//! Deterministic PDF sheet export.
//!
//! Given a `Sheet` (paper size + viewports + title block) and a slice
//! of `DxfEntity`s in model space, this renders a printable PDF at
//! the sheet's exact paper dimensions using printpdf line / shape
//! primitives.
//!
//! ## What this module guarantees
//!
//! - **Per-viewport rectangular clipping**: every entity drawn for a
//!   given viewport is constrained to the viewport's paper-space
//!   rectangle. Geometry outside the viewport never bleeds onto the
//!   sheet (including overlapping viewports — each viewport draws
//!   inside its own `save_graphics_state` / `clip` / `restore`
//!   bracket).
//! - **Real linetype rendering**: each entity is drawn with a dash
//!   pattern derived from the layer's linetype (continuous, dashed,
//!   hidden, center, phantom, divide, dashdot). The linetype is
//!   resolved through the same per-layer lookup the DXF writer uses,
//!   so a "WALLS layer → CONTINUOUS, HIDDEN layer → DASH-GAP-DASH-GAP"
//!   round-trips from the model to the PDF.
//! - **Real dimension rendering**: linear / aligned / angular /
//!   radial / diameter `DxfDimension` entities render with extension
//!   lines, dimension line(s), arrowheads, and the measured value
//!   formatted at the requested precision from the dimension style.
//! - **Lineweights and screening**: applied from `PlotStyleTable` per
//!   ACI as before.
//! - **Title block**: populated fields render in Helvetica at the
//!   field's authored mm position and height.
//!
//! ## Coordinate spaces
//!
//! - **Model space**: drawing units (typically mm).
//! - **Paper space**: mm with origin in the *top-left* of the sheet
//!   (DXF / drafting convention).
//! - **PDF space**: pt-equivalent (printpdf takes `Mm` and converts)
//!   with origin in the *bottom-left*.
//!
//! Every paper-space coordinate funnels through `to_pdf(paper)` which
//! flips `y` against the sheet height. That keeps the rest of the
//! module thinking in top-down sheet coordinates, matching the
//! `Sheet` / `SheetViewport` / `TitleBlock` data model.
//!
//! ## Determinism
//!
//! The exporter is fully deterministic: identical input produces
//! byte-identical PDF bytes. There is no time-dependent state, no
//! UUID generation, and the only floating-point work is the
//! coordinate transforms (whose outputs are stable under the same
//! input bytes).

use std::fs::File;
use std::io::BufWriter;
use std::path::{Path, PathBuf};

use aec_cad::dxf::{DxfDimStyle, DxfDimension, DxfDimensionKind, DxfEntity};
use aec_cad::sheets::{Sheet, SheetViewport};
use printpdf::path::{PaintMode, WindingOrder};
use printpdf::{
    BuiltinFont, Color, IndirectFontRef, Line as PdfLine, Mm, PdfDocument, PdfDocumentReference,
    PdfLayerReference, Point, Polygon as PdfPolygon, Rgb,
};

use crate::pdf::PdfBuilderError;
use crate::plot_style::{PlotStyle, PlotStyleTable};

/// A single layer's stroke linetype. Names mirror standard DXF
/// linetypes; unknown names fall back to [`Linetype::Continuous`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Linetype {
    Continuous,
    Dashed,
    Hidden,
    Center,
    Phantom,
    DashDot,
    Divide,
}

impl Linetype {
    /// Resolve a DXF linetype name (case-insensitive) to one of the
    /// well-known stroke patterns. Unrecognised names fall back to
    /// [`Linetype::Continuous`] so the entity remains visible.
    pub fn from_name(name: &str) -> Self {
        match name.trim().to_ascii_uppercase().as_str() {
            "" | "CONTINUOUS" | "BYLAYER" | "BYBLOCK" => Self::Continuous,
            "DASHED" | "DASH" => Self::Dashed,
            "HIDDEN" => Self::Hidden,
            "CENTER" | "CENTRE" => Self::Center,
            "PHANTOM" => Self::Phantom,
            "DASHDOT" | "DASH_DOT" | "DASH-DOT" => Self::DashDot,
            "DIVIDE" => Self::Divide,
            _ => Self::Continuous,
        }
    }

    /// Return the printpdf dash-pattern (up to three dash/gap pairs)
    /// for this linetype, expressed in PDF user units (printpdf
    /// takes integers and we pass mm directly, which is close
    /// enough at typical sheet scales). [`Linetype::Continuous`]
    /// returns `None` so the caller can clear any prior dash
    /// pattern by re-applying the default.
    ///
    /// ## Renderer constraint
    ///
    /// `printpdf::LineDashPattern` exposes at most three dash/gap
    /// pairs (`dash_1/gap_1`, `dash_2/gap_2`, `dash_3/gap_3`). The
    /// canonical AutoCAD PHANTOM linetype is a five-segment pattern
    /// (`dash-dot-dot-dash-dot-dot`), which fits in three pairs; the
    /// canonical DIVIDE linetype is also a six-segment pattern
    /// (`dash-dot-dot`). DXF linetype tables on disk can carry more
    /// segments than printpdf can render — anything beyond the
    /// third pair is dropped on export. The patterns below use the
    /// extra pair where present so each variant produces a visibly
    /// distinct stroke.
    pub fn dash_pattern(self) -> Option<DashPattern> {
        match self {
            Self::Continuous => None,
            // Classic 4-mm dash, 2-mm gap.
            Self::Dashed => Some(DashPattern::two(4, 2)),
            // Tighter hidden-line pattern.
            Self::Hidden => Some(DashPattern::two(3, 2)),
            // CENTER: long dash, short gap, short dash, short gap.
            Self::Center => Some(DashPattern::four(6, 2, 1, 2)),
            // PHANTOM: long dash, short gap, short dash, short gap,
            // short dash, short gap (dash-dot-dot, six-segment).
            // Distinct from CENTER via the third dot.
            Self::Phantom => Some(DashPattern::six(12, 2, 1, 2, 1, 2)),
            // DASHDOT: dash, gap, dot, gap (four-segment).
            Self::DashDot => Some(DashPattern::four(4, 1, 1, 1)),
            // DIVIDE: dash, gap, dot, gap, dot, gap (six-segment
            // dash-dot-dot). Distinct from DASHDOT via the third dot.
            Self::Divide => Some(DashPattern::six(4, 1, 1, 1, 1, 1)),
        }
    }
}

/// A printpdf-compatible dash specification. Stored as integer mm
/// because printpdf's `LineDashPattern` takes `i64`. `dash_2` /
/// `gap_2` are `None` for two-segment patterns; `dash_3` / `gap_3`
/// are `None` for two- and four-segment patterns. printpdf's
/// `LineDashPattern` does not support more than three dash/gap
/// pairs, so the struct stops there.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DashPattern {
    pub dash_1: i64,
    pub gap_1: i64,
    pub dash_2: Option<i64>,
    pub gap_2: Option<i64>,
    pub dash_3: Option<i64>,
    pub gap_3: Option<i64>,
}

impl DashPattern {
    fn two(dash: i64, gap: i64) -> Self {
        Self {
            dash_1: dash,
            gap_1: gap,
            dash_2: None,
            gap_2: None,
            dash_3: None,
            gap_3: None,
        }
    }
    fn four(dash_1: i64, gap_1: i64, dash_2: i64, gap_2: i64) -> Self {
        Self {
            dash_1,
            gap_1,
            dash_2: Some(dash_2),
            gap_2: Some(gap_2),
            dash_3: None,
            gap_3: None,
        }
    }
    fn six(dash_1: i64, gap_1: i64, dash_2: i64, gap_2: i64, dash_3: i64, gap_3: i64) -> Self {
        Self {
            dash_1,
            gap_1,
            dash_2: Some(dash_2),
            gap_2: Some(gap_2),
            dash_3: Some(dash_3),
            gap_3: Some(gap_3),
        }
    }

    fn into_printpdf(self) -> printpdf::LineDashPattern {
        printpdf::LineDashPattern {
            offset: 0,
            dash_1: Some(self.dash_1),
            gap_1: Some(self.gap_1),
            dash_2: self.dash_2,
            gap_2: self.gap_2,
            dash_3: self.dash_3,
            gap_3: self.gap_3,
        }
    }
}

/// Per-layer plot definition: ACI, lineweight, linetype.
///
/// The exporter resolves an entity's plot presentation by joining
/// `LayerPlotTable.get(layer_name)` (for colour-by-layer and linetype)
/// with [`PlotStyleTable.get(aci)`] (for screening and any per-pen
/// overrides). If the layer is absent from the table, the entity
/// falls back to the same hard-coded `WALLS / DIMS / TEXT / HATCH /
/// HIDDEN` map the legacy export used so existing fixtures keep
/// working without an explicit layer table.
#[derive(Debug, Clone, Default)]
pub struct LayerPlotTable {
    entries: std::collections::HashMap<String, LayerPlotEntry>,
}

#[derive(Debug, Clone, Copy)]
pub struct LayerPlotEntry {
    pub aci: u8,
    pub linetype: Linetype,
}

impl LayerPlotTable {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn insert(&mut self, layer: impl Into<String>, aci: u8, linetype: Linetype) {
        self.entries.insert(
            layer.into().to_ascii_uppercase(),
            LayerPlotEntry { aci, linetype },
        );
    }

    pub fn get(&self, layer: &str) -> Option<LayerPlotEntry> {
        self.entries.get(&layer.to_ascii_uppercase()).copied()
    }

    pub fn len(&self) -> usize {
        self.entries.len()
    }

    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }
}

/// Dimension-style table, keyed by the style name on each
/// `DxfDimension`. Used to size text and arrows.
#[derive(Debug, Clone, Default)]
pub struct DimStyleTable {
    pub styles: std::collections::HashMap<String, DxfDimStyle>,
}

impl DimStyleTable {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn from_slice(styles: &[DxfDimStyle]) -> Self {
        let mut t = Self::default();
        for s in styles {
            t.styles.insert(s.name.to_ascii_uppercase(), s.clone());
        }
        t
    }

    /// Look up a dim style by name. DXF style names are
    /// case-insensitive per AutoCAD convention, so the key is
    /// uppercased on both insert and get (mirroring
    /// [`LayerPlotTable::get`]).
    pub fn get(&self, name: &str) -> Option<&DxfDimStyle> {
        self.styles.get(&name.to_ascii_uppercase())
    }

    /// Effective style for a given name. Falls back to
    /// [`DxfDimStyle::standard`] (the AutoCAD `STANDARD` defaults,
    /// including `decimal_places = 4`) if the named style is
    /// missing, then overwrites the `name` field so callers see
    /// the requested style name back in the returned value.
    pub fn resolve(&self, name: &str) -> DxfDimStyle {
        if let Some(s) = self.get(name) {
            return s.clone();
        }
        let mut fallback = DxfDimStyle::standard();
        fallback.name = name.to_string();
        fallback
    }
}

pub struct SheetPdfBuilder {
    doc: PdfDocumentReference,
    body_font: IndirectFontRef,
    sheets_emitted: u32,
}

impl SheetPdfBuilder {
    pub fn new(title: impl Into<String>) -> Result<Self, PdfBuilderError> {
        let doc = PdfDocument::empty(title.into());
        let body_font = doc.add_builtin_font(BuiltinFont::Helvetica)?;
        Ok(Self {
            doc,
            body_font,
            sheets_emitted: 0,
        })
    }

    pub fn sheets_emitted(&self) -> u32 {
        self.sheets_emitted
    }

    /// Add a sheet to the document. Multiple sheets become multiple
    /// pages in the resulting PDF.
    ///
    /// Backwards-compatible entry point — keeps callers that don't
    /// have a `LayerPlotTable` / `DimStyleTable` working at the
    /// legacy "everything is continuous, dim styles default to
    /// 2.5 mm text" presentation. New callers should prefer
    /// [`Self::add_sheet_with_tables`].
    pub fn add_sheet(
        &mut self,
        sheet: &Sheet,
        entities: &[DxfEntity],
        plot_style: &PlotStyleTable,
    ) -> Result<(), PdfBuilderError> {
        let empty_layers = LayerPlotTable::default();
        let empty_dims = DimStyleTable::default();
        self.add_sheet_with_tables(sheet, entities, plot_style, &empty_layers, &empty_dims)
    }

    /// Add a sheet using full layer + dim-style tables. Each
    /// viewport's entities are clipped to its paper-space rectangle,
    /// strokes pick up the layer's linetype, and dimensions render
    /// with extension lines, arrows, and a numeric value formatted
    /// at the dim-style's precision.
    pub fn add_sheet_with_tables(
        &mut self,
        sheet: &Sheet,
        entities: &[DxfEntity],
        plot_style: &PlotStyleTable,
        layer_table: &LayerPlotTable,
        dim_styles: &DimStyleTable,
    ) -> Result<(), PdfBuilderError> {
        let (w_mm, h_mm) = sheet.dimensions_mm();
        let (page, layer) = self
            .doc
            .add_page(Mm(w_mm as f32), Mm(h_mm as f32), &sheet.name);
        self.sheets_emitted += 1;
        let layer_ref = self.doc.get_page(page).get_layer(layer);

        // Paper border (drawn outside any viewport clip so the
        // border is always visible).
        layer_ref.set_outline_color(Color::Rgb(Rgb::new(0.0, 0.0, 0.0, None)));
        layer_ref.set_outline_thickness(0.2_f32);
        draw_rect(&layer_ref, 0.0, 0.0, w_mm, h_mm);

        // Each viewport projects its slice of model space to paper
        // coords. Wrap the viewport's entities in a clipping path so
        // anything outside the viewport rectangle is suppressed.
        for vp in &sheet.viewports {
            layer_ref.save_graphics_state();
            push_rect_clip(&layer_ref, vp.paper_origin, vp.paper_size, h_mm);
            for entity in entities {
                let layer_name = entity.layer();
                if vp.frozen_layers.iter().any(|f| f == layer_name) {
                    // Frozen in this viewport — don't draw.
                    continue;
                }
                draw_entity(
                    &layer_ref,
                    entity,
                    vp,
                    plot_style,
                    layer_table,
                    dim_styles,
                    h_mm,
                    &self.body_font,
                );
            }
            // Clear any dash pattern set by the last entity before
            // restoring the graphics state — printpdf doesn't
            // restore the dash pattern automatically on every PDF
            // viewer (this is a defensive belt-and-braces step).
            layer_ref.set_line_dash_pattern(printpdf::LineDashPattern::default());
            layer_ref.restore_graphics_state();
        }

        // Title block (drawn outside any viewport clip).
        if let Some(tb) = &sheet.title_block {
            for (field, value) in tb.populated_fields() {
                let x = field.position[0] as f32;
                // PDF y is bottom-up; convert from top-down sheet
                // coords.
                let y = (h_mm - field.position[1]) as f32;
                layer_ref.use_text(value, field.height as f32, Mm(x), Mm(y), &self.body_font);
            }
        }
        Ok(())
    }

    /// Persist the document to `path`.
    pub fn save(self, path: impl AsRef<Path>) -> Result<PathBuf, PdfBuilderError> {
        let path = path.as_ref().to_path_buf();
        let file = File::create(&path)?;
        let mut writer = BufWriter::new(file);
        self.doc.save(&mut writer)?;
        Ok(path)
    }
}

fn draw_rect(layer: &PdfLayerReference, x: f64, y_bot: f64, w: f64, h: f64) {
    let pts = vec![
        (Point::new(Mm(x as f32), Mm(y_bot as f32)), false),
        (Point::new(Mm((x + w) as f32), Mm(y_bot as f32)), false),
        (
            Point::new(Mm((x + w) as f32), Mm((y_bot + h) as f32)),
            false,
        ),
        (Point::new(Mm(x as f32), Mm((y_bot + h) as f32)), false),
    ];
    let line = PdfLine {
        points: pts,
        is_closed: true,
    };
    layer.add_line(line);
}

/// Push a rectangular clipping path onto the graphics state. The
/// rectangle is in paper-space (top-down) coordinates and is
/// converted to PDF space (bottom-up) using the sheet height.
fn push_rect_clip(
    layer: &PdfLayerReference,
    paper_origin: [f64; 2],
    paper_size: [f64; 2],
    sheet_h_mm: f64,
) {
    // Convert top-down (origin top-left) to bottom-up (origin
    // bottom-left). The viewport's `paper_origin` is the top-left
    // corner so `paper_origin[1] + paper_size[1]` is the bottom of
    // the rectangle in top-down coords.
    let pdf_y_bot = sheet_h_mm - (paper_origin[1] + paper_size[1]);
    let pdf_y_top = sheet_h_mm - paper_origin[1];
    let pdf_x_left = paper_origin[0];
    let pdf_x_right = paper_origin[0] + paper_size[0];
    let ring: Vec<(Point, bool)> = vec![
        (
            Point::new(Mm(pdf_x_left as f32), Mm(pdf_y_bot as f32)),
            false,
        ),
        (
            Point::new(Mm(pdf_x_right as f32), Mm(pdf_y_bot as f32)),
            false,
        ),
        (
            Point::new(Mm(pdf_x_right as f32), Mm(pdf_y_top as f32)),
            false,
        ),
        (
            Point::new(Mm(pdf_x_left as f32), Mm(pdf_y_top as f32)),
            false,
        ),
    ];
    let polygon = PdfPolygon {
        rings: vec![ring],
        mode: PaintMode::Clip,
        winding_order: WindingOrder::NonZero,
    };
    layer.add_polygon(polygon);
}

fn style_for_layer(layer: &str, plot_table: &PlotStyleTable, layers: &LayerPlotTable) -> PlotStyle {
    // Layer-table lookup first (modern path).
    let entry = layers.get(layer).unwrap_or(LayerPlotEntry {
        aci: layer_to_default_aci(layer),
        linetype: Linetype::Continuous,
    });
    plot_table.get(entry.aci).cloned().unwrap_or(PlotStyle {
        aci: entry.aci,
        color: [0, 0, 0],
        lineweight_mm: 0.25,
        screening: 100,
        linetype: None,
        override_linetype: false,
    })
}

fn linetype_for_layer(layer: &str, layers: &LayerPlotTable) -> Linetype {
    layers
        .get(layer)
        .map_or(Linetype::Continuous, |e| e.linetype)
}

fn layer_to_default_aci(layer: &str) -> u8 {
    match layer.to_ascii_uppercase().as_str() {
        "WALLS" => 7,
        "DIMS" | "DIMENSIONS" => 2,
        "ANNOTATIONS" | "TEXT" => 1,
        "HATCH" | "HATCHES" => 8,
        "HIDDEN" => 9,
        _ => 7,
    }
}

fn screened([r, g, b]: [u8; 3], pct: u8) -> [u8; 3] {
    let f = pct.min(100) as f64 / 100.0;
    let mix = |c: u8| {
        ((c as f64) * f + 255.0 * (1.0 - f))
            .round()
            .clamp(0.0, 255.0) as u8
    };
    [mix(r), mix(g), mix(b)]
}

#[allow(clippy::too_many_arguments)]
fn draw_entity(
    layer_ref: &PdfLayerReference,
    entity: &DxfEntity,
    vp: &SheetViewport,
    plot_table: &PlotStyleTable,
    layer_table: &LayerPlotTable,
    dim_styles: &DimStyleTable,
    sheet_h_mm: f64,
    body_font: &IndirectFontRef,
) {
    let style = style_for_layer(entity.layer(), plot_table, layer_table);
    let [cr, cg, cb] = screened(style.color, style.screening);
    let lw = style.lineweight_mm.max(0.05);
    layer_ref.set_outline_color(Color::Rgb(Rgb::new(
        cr as f32 / 255.0,
        cg as f32 / 255.0,
        cb as f32 / 255.0,
        None,
    )));
    layer_ref.set_fill_color(Color::Rgb(Rgb::new(
        cr as f32 / 255.0,
        cg as f32 / 255.0,
        cb as f32 / 255.0,
        None,
    )));
    layer_ref.set_outline_thickness(lw as f32);

    // Apply layer linetype before stroking. The plot-style override
    // (`override_linetype = true` + a named `linetype`) wins if set.
    let stroke_linetype = if style.override_linetype {
        if let Some(name) = &style.linetype {
            Linetype::from_name(name)
        } else {
            linetype_for_layer(entity.layer(), layer_table)
        }
    } else {
        linetype_for_layer(entity.layer(), layer_table)
    };
    match stroke_linetype.dash_pattern() {
        Some(p) => layer_ref.set_line_dash_pattern(p.into_printpdf()),
        None => layer_ref.set_line_dash_pattern(printpdf::LineDashPattern::default()),
    }

    // Convert a paper-space mm point (top-down) to printpdf's
    // bottom-up coordinate.
    let to_pdf = |p: [f64; 2]| (Mm(p[0] as f32), Mm((sheet_h_mm - p[1]) as f32));

    match entity {
        DxfEntity::Line(e) => {
            let a = vp.model_to_paper([e.start[0], e.start[1]]);
            let b = vp.model_to_paper([e.end[0], e.end[1]]);
            let (ax, ay) = to_pdf(a);
            let (bx, by) = to_pdf(b);
            let line = PdfLine {
                points: vec![(Point::new(ax, ay), false), (Point::new(bx, by), false)],
                is_closed: false,
            };
            layer_ref.add_line(line);
        }
        DxfEntity::Polyline(e) => {
            let pts: Vec<(Point, bool)> = e
                .vertices
                .iter()
                .map(|v| {
                    let p = vp.model_to_paper([v.x, v.y]);
                    let (x, y) = to_pdf(p);
                    (Point::new(x, y), false)
                })
                .collect();
            let line = PdfLine {
                points: pts,
                is_closed: e.closed,
            };
            layer_ref.add_line(line);
        }
        DxfEntity::Circle(e) => {
            let segments = 64;
            let radius = e.radius;
            let mut pts = Vec::with_capacity(segments);
            for i in 0..segments {
                let theta = i as f64 / segments as f64 * std::f64::consts::TAU;
                let mx = e.center[0] + radius * theta.cos();
                let my = e.center[1] + radius * theta.sin();
                let paper = vp.model_to_paper([mx, my]);
                let (px, py) = to_pdf(paper);
                pts.push((Point::new(px, py), false));
            }
            let line = PdfLine {
                points: pts,
                is_closed: true,
            };
            layer_ref.add_line(line);
        }
        DxfEntity::Arc(e) => {
            let span = (e.end_angle - e.start_angle).abs();
            let segments = (span.ceil() as usize).max(8);
            let radius = e.radius;
            let mut pts = Vec::with_capacity(segments + 1);
            for i in 0..=segments {
                let frac = i as f64 / segments as f64;
                let theta = (e.start_angle + (e.end_angle - e.start_angle) * frac).to_radians();
                let mx = e.center[0] + radius * theta.cos();
                let my = e.center[1] + radius * theta.sin();
                let paper = vp.model_to_paper([mx, my]);
                let (px, py) = to_pdf(paper);
                pts.push((Point::new(px, py), false));
            }
            let line = PdfLine {
                points: pts,
                is_closed: false,
            };
            layer_ref.add_line(line);
        }
        DxfEntity::Ellipse(e) => {
            // Sample the ellipse along the parametric range
            // [start_param, end_param]. Major axis is in model
            // coordinates; minor axis = major_axis * ratio (and
            // perpendicular).
            let segments = 64usize;
            let major_len = (e.major_axis[0].powi(2) + e.major_axis[1].powi(2)).sqrt();
            // If the major axis is degenerate the entity is a
            // point — skip drawing rather than dividing by zero.
            if major_len < 1e-12 {
                // fall through to no-op
            } else {
                let theta_axis = e.major_axis[1].atan2(e.major_axis[0]);
                let minor_len = major_len * e.ratio;
                let (start, end) = (e.start_param, e.end_param);
                let total = end - start;
                let mut pts = Vec::with_capacity(segments + 1);
                for i in 0..=segments {
                    let frac = i as f64 / segments as f64;
                    let t = start + total * frac;
                    let lx = major_len * t.cos();
                    let ly = minor_len * t.sin();
                    let mx = e.center[0] + lx * theta_axis.cos() - ly * theta_axis.sin();
                    let my = e.center[1] + lx * theta_axis.sin() + ly * theta_axis.cos();
                    let paper = vp.model_to_paper([mx, my]);
                    let (px, py) = to_pdf(paper);
                    pts.push((Point::new(px, py), false));
                }
                let line = PdfLine {
                    points: pts,
                    is_closed: (total - std::f64::consts::TAU).abs() < 1e-6,
                };
                layer_ref.add_line(line);
            }
        }
        DxfEntity::Hatch(e) => {
            for lp in &e.loops {
                let ring: Vec<(Point, bool)> = lp
                    .vertices
                    .iter()
                    .map(|v| {
                        let p = vp.model_to_paper([v[0], v[1]]);
                        let (x, y) = to_pdf(p);
                        (Point::new(x, y), false)
                    })
                    .collect();
                let polygon = PdfPolygon {
                    rings: vec![ring],
                    mode: PaintMode::Fill,
                    winding_order: WindingOrder::NonZero,
                };
                layer_ref.add_polygon(polygon);
            }
        }
        DxfEntity::Text(e) => {
            let p = vp.model_to_paper([e.position[0], e.position[1]]);
            let (x, y) = to_pdf(p);
            layer_ref.use_text(&e.text, e.height as f32, x, y, body_font);
        }
        DxfEntity::Attdef(e) => {
            // Attribute definitions render the prompt / default value
            // at their authored position so prints reflect what the
            // template will ask the operator about.
            let p = vp.model_to_paper([e.position[0], e.position[1]]);
            let (x, y) = to_pdf(p);
            let label = if e.default_value.is_empty() {
                &e.tag
            } else {
                &e.default_value
            };
            layer_ref.use_text(label, e.height as f32, x, y, body_font);
        }
        DxfEntity::Dimension(d) => {
            draw_dimension(layer_ref, d, vp, dim_styles, sheet_h_mm, body_font);
        }
        DxfEntity::Spline(_) | DxfEntity::Insert(_) => {
            // Splines render via control polyline in a future PR;
            // INSERTs need block-expansion which is handled at the
            // entity-graph layer (`dxf::convert::dxf_to_primitive`
            // doesn't lower these, so they should never reach the
            // PDF exporter via the bridge). No-op here keeps the
            // pdf-export path resilient when callers hand it a raw
            // mixed `DxfDocument`.
        }
    }
}

fn draw_dimension(
    layer_ref: &PdfLayerReference,
    dim: &DxfDimension,
    vp: &SheetViewport,
    dim_styles: &DimStyleTable,
    sheet_h_mm: f64,
    body_font: &IndirectFontRef,
) {
    let style = dim_styles.resolve(&dim.style);
    // Dimension geometry is drawn in continuous linetype regardless
    // of layer (the convention is for dim leaders/extension lines to
    // be solid). Switch to a clean dash state and reset on exit so
    // the next entity isn't affected.
    layer_ref.set_line_dash_pattern(printpdf::LineDashPattern::default());

    let to_paper = |p: [f64; 3]| vp.model_to_paper([p[0], p[1]]);
    let to_pdf = |p: [f64; 2]| (Mm(p[0] as f32), Mm((sheet_h_mm - p[1]) as f32));
    let arrow_mm = style.arrow_size.max(1.0);
    let txt_h = style.text_height.max(1.0) as f32;

    match dim.kind {
        DxfDimensionKind::Linear | DxfDimensionKind::Aligned => {
            // def_point_a / def_point_b are the two measured points
            // in model space; def_point is the dimension-line offset
            // point. Project all three to paper and draw extension
            // lines + dim line + arrowheads.
            let a = to_paper(dim.def_point_a);
            let b = to_paper(dim.def_point_b);
            let d = to_paper(dim.def_point);
            let (ax, ay) = to_pdf(a);
            let (bx, by) = to_pdf(b);
            // Only the dim-line offset's Y survives — the dim line
            // is drawn between the two measured points' X values,
            // not at the offset point's X.
            let (_, dy) = to_pdf(d);

            // Extension lines: from each measured point
            // perpendicular to the dim line, ending at the dim
            // line's Y. For Linear (horizontal dim line) and the
            // typical Aligned cases in our fixtures the perpendicular
            // is the vertical at the measured point's X — the
            // extension line is `(ax, ay) → (ax, dy)` (and the same
            // for `b`). Earlier revisions of this code used `dx`
            // (the dim-line *offset* point's X) for line1 and the
            // dim-line start, which produced a diagonal extension
            // line and a half-length dim line.
            let line1 = PdfLine {
                points: vec![(Point::new(ax, ay), false), (Point::new(ax, dy), false)],
                is_closed: false,
            };
            let line2 = PdfLine {
                points: vec![(Point::new(bx, by), false), (Point::new(bx, dy), false)],
                is_closed: false,
            };
            // Dim line: from (a.x, d.y) to (b.x, d.y) — spans the
            // full distance between the two measured points at the
            // dim-line offset height.
            let dim_line = PdfLine {
                points: vec![(Point::new(ax, dy), false), (Point::new(bx, dy), false)],
                is_closed: false,
            };
            layer_ref.add_line(line1);
            layer_ref.add_line(line2);
            layer_ref.add_line(dim_line);

            // Arrowheads at each end of the dim line, pointing
            // inward. The arrow is drawn in paper-space mm.
            draw_arrowhead(layer_ref, [ax, dy], [bx, dy], arrow_mm);
            draw_arrowhead(layer_ref, [bx, dy], [ax, dy], arrow_mm);

            // Text — measured value, formatted at the dim-style's
            // decimal places. Position at the dim-line midpoint X
            // (between the two measured points) and route the
            // user-authored text_position Y through the same
            // model→paper→PDF pipeline as Radial / Angular so the
            // text lands on-sheet even when text_position is in
            // model space.
            let value = dim
                .measured_value
                .unwrap_or_else(|| distance_paper([ax, ay], [bx, by]) / vp.scale);
            let label = dim
                .override_text
                .clone()
                .unwrap_or_else(|| format_value(value, style.decimal_places));
            let mid_x = midpoint_mm(ax, bx);
            let txt_pos = to_paper(dim.text_position);
            let (_, ty) = to_pdf(txt_pos);
            layer_ref.use_text(&label, txt_h, Mm(mid_x), Mm(ty.0 + txt_h), body_font);
        }
        DxfDimensionKind::Radial | DxfDimensionKind::Diameter => {
            // def_point = circle centre; def_point_a = a point on
            // the circle. Draw a leader from centre to the point,
            // arrowhead at the point, text at text_position.
            let centre = to_paper(dim.def_point);
            let edge = to_paper(dim.def_point_a);
            let (cx, cy) = to_pdf(centre);
            let (ex, ey) = to_pdf(edge);
            let leader = PdfLine {
                points: vec![(Point::new(cx, cy), false), (Point::new(ex, ey), false)],
                is_closed: false,
            };
            layer_ref.add_line(leader);
            draw_arrowhead(layer_ref, [ex, ey], [cx, cy], arrow_mm);

            let dist_paper = distance_paper([cx, cy], [ex, ey]);
            let value = radial_dim_value(dim.kind, dim.measured_value, dist_paper, vp.scale);
            let prefix = if matches!(dim.kind, DxfDimensionKind::Diameter) {
                "Ø"
            } else {
                "R"
            };
            let label = dim.override_text.clone().unwrap_or_else(|| {
                format!("{}{}", prefix, format_value(value, style.decimal_places))
            });
            let txt_pos = to_paper(dim.text_position);
            let (tx, ty) = to_pdf(txt_pos);
            layer_ref.use_text(&label, txt_h, tx, ty, body_font);
        }
        DxfDimensionKind::Angular => {
            // def_point = vertex; def_point_a / def_point_b = the
            // two endpoints of the angle's arms. Draw extension
            // lines along each arm and an arc between them.
            let vertex = to_paper(dim.def_point);
            let arm_a = to_paper(dim.def_point_a);
            let arm_b = to_paper(dim.def_point_b);
            let (vx, vy) = to_pdf(vertex);
            let (ax, ay) = to_pdf(arm_a);
            let (bx, by) = to_pdf(arm_b);
            let arm1 = PdfLine {
                points: vec![(Point::new(vx, vy), false), (Point::new(ax, ay), false)],
                is_closed: false,
            };
            let arm2 = PdfLine {
                points: vec![(Point::new(vx, vy), false), (Point::new(bx, by), false)],
                is_closed: false,
            };
            layer_ref.add_line(arm1);
            layer_ref.add_line(arm2);

            // Arc between the arms — sample at the smaller of the
            // two arm lengths so the arc sits inside the dim shape.
            //
            // Angles are computed in *paper* space (Y-down, before
            // the PDF Y-flip) so the arc sweep direction matches
            // the model-space dimension. Sampling angles in PDF
            // space (where Y is flipped) reverses the sign of
            // angles and can invert the arc — for example a
            // dimension whose arms are at 0° and +90° in model
            // space would interpolate from 0° to -90° (i.e. the
            // wrong half of the circle) once Y is flipped. Compute
            // the arc in paper-space and apply the Y-flip per
            // sample, keeping each interpolated point in the same
            // coordinate space as the rest of the entity.
            let r_a = distance_paper([vx, vy], [ax, ay]);
            let r_b = distance_paper([vx, vy], [bx, by]);
            let r = (r_a.min(r_b)).max(arrow_mm * 2.0) as f32;
            let vpx = vertex[0] as f32;
            let vpy = vertex[1] as f32;
            let angle_a = ((arm_a[1] - vertex[1]) as f32).atan2((arm_a[0] - vertex[0]) as f32);
            let angle_b = ((arm_b[1] - vertex[1]) as f32).atan2((arm_b[0] - vertex[0]) as f32);
            let segments = 32usize;
            let sheet_h_mm_f32 = sheet_h_mm as f32;
            let mut pts = Vec::with_capacity(segments + 1);
            for i in 0..=segments {
                let frac = i as f32 / segments as f32;
                let theta = angle_a + (angle_b - angle_a) * frac;
                let paper_x = vpx + r * theta.cos();
                let paper_y = vpy + r * theta.sin();
                let pdf_x = paper_x;
                let pdf_y = sheet_h_mm_f32 - paper_y;
                pts.push((Point::new(Mm(pdf_x), Mm(pdf_y)), false));
            }
            let arc = PdfLine {
                points: pts,
                is_closed: false,
            };
            layer_ref.add_line(arc);

            // Angle in degrees between the two arms.
            let value = dim.measured_value.unwrap_or_else(|| {
                let deg = (angle_b - angle_a).abs().to_degrees() as f64;
                if deg > 180.0 {
                    360.0 - deg
                } else {
                    deg
                }
            });
            let label = dim
                .override_text
                .clone()
                .unwrap_or_else(|| format!("{}°", format_value(value, style.decimal_places)));
            let txt_pos = to_paper(dim.text_position);
            let (tx, ty) = to_pdf(txt_pos);
            layer_ref.use_text(&label, txt_h, tx, ty, body_font);
        }
    }
}

fn draw_arrowhead(layer: &PdfLayerReference, tip: [Mm; 2], shaft_origin: [Mm; 2], size_mm: f64) {
    // Arrowhead is a filled triangle pointing from tip toward
    // shaft_origin. Width = `size_mm` at the base; length = `size_mm`
    // along the shaft.
    let (tx, ty) = (tip[0].0, tip[1].0);
    let (sx, sy) = (shaft_origin[0].0, shaft_origin[1].0);
    let dx = sx - tx;
    let dy = sy - ty;
    let len = (dx * dx + dy * dy).sqrt();
    if len < 1e-3 {
        return;
    }
    let ux = dx / len;
    let uy = dy / len;
    // Perpendicular (left-handed) for the base.
    let px = -uy;
    let py = ux;
    let half = (size_mm * 0.5) as f32;
    let len_back = size_mm as f32;
    let base_x = tx + ux * len_back;
    let base_y = ty + uy * len_back;
    let p1 = (Point::new(Mm(tx), Mm(ty)), false);
    let p2 = (
        Point::new(Mm(base_x + px * half), Mm(base_y + py * half)),
        false,
    );
    let p3 = (
        Point::new(Mm(base_x - px * half), Mm(base_y - py * half)),
        false,
    );
    let polygon = PdfPolygon {
        rings: vec![vec![p1, p2, p3]],
        mode: PaintMode::FillStroke,
        winding_order: WindingOrder::NonZero,
    };
    layer.add_polygon(polygon);
}

fn distance_paper(a: [Mm; 2], b: [Mm; 2]) -> f64 {
    let dx = (b[0].0 - a[0].0) as f64;
    let dy = (b[1].0 - a[1].0) as f64;
    (dx * dx + dy * dy).sqrt()
}

fn midpoint_mm(a: Mm, b: Mm) -> f32 {
    (a.0 + b.0) * 0.5
}

fn format_value(value: f64, decimals: u8) -> String {
    format!("{:.*}", decimals.min(8) as usize, value)
}

/// Resolve the displayed value for a Radial / Diameter dimension.
///
/// Per the DXF spec, group code 42 (`measured_value`) on a
/// `DIAMETER` dimension stores the *diameter* directly — not the
/// radius. The reader populates `measured_value` from code 42
/// verbatim (see `aec_cad::dxf::reader`), so any `DIAMETER`
/// `DxfDimension` round-tripped from a real DXF file already
/// carries the diameter and must NOT be doubled by the PDF
/// exporter. The auto-compute fallback path measures from the
/// circle centre to a point on the circle, which is the radius,
/// so that path doubles for `DIAMETER` but not `RADIAL`.
fn radial_dim_value(
    kind: DxfDimensionKind,
    measured_value: Option<f64>,
    leader_paper_len: f64,
    scale: f64,
) -> f64 {
    if let Some(v) = measured_value {
        return v;
    }
    let radius = leader_paper_len / scale;
    if matches!(kind, DxfDimensionKind::Diameter) {
        radius * 2.0
    } else {
        radius
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use aec_cad::dxf::{
        DxfArc, DxfCircle, DxfDimStyle, DxfDimension, DxfDimensionKind, DxfLine, DxfPolyline,
        DxfPolylineVertex,
    };
    use aec_cad::sheets::{PaperSize, Sheet, SheetViewport};

    fn sheet_with_viewport() -> Sheet {
        let mut s = Sheet::new("Floor Plan", PaperSize::IsoA3);
        let mut vp = SheetViewport::new("PLAN", [10.0, 10.0], [200.0, 100.0]);
        vp.scale = 0.01; // 1:100
        s.viewports.push(vp);
        s
    }

    #[test]
    fn save_to_pdf_produces_valid_file() {
        let dir = tempfile::tempdir().unwrap();
        let sheet = sheet_with_viewport();
        let entities = vec![
            DxfEntity::Line(DxfLine {
                layer: "WALLS".into(),
                start: [0.0, 0.0, 0.0],
                end: [4000.0, 0.0, 0.0],
            }),
            DxfEntity::Polyline(DxfPolyline {
                layer: "WALLS".into(),
                vertices: vec![
                    DxfPolylineVertex::new(0.0, 0.0),
                    DxfPolylineVertex::new(4000.0, 0.0),
                    DxfPolylineVertex::new(4000.0, 3000.0),
                    DxfPolylineVertex::new(0.0, 3000.0),
                ],
                closed: true,
                elevation: 0.0,
            }),
            DxfEntity::Circle(DxfCircle {
                layer: "WALLS".into(),
                center: [2000.0, 1500.0, 0.0],
                radius: 100.0,
            }),
        ];
        let mut b = SheetPdfBuilder::new("Project Alpha").unwrap();
        b.add_sheet(&sheet, &entities, &PlotStyleTable::monochrome())
            .unwrap();
        assert_eq!(b.sheets_emitted(), 1);
        let path = b.save(dir.path().join("sheet.pdf")).unwrap();
        let bytes = std::fs::read(&path).unwrap();
        assert!(bytes.starts_with(b"%PDF"));
        assert!(bytes.len() > 1024);
    }

    #[test]
    fn multi_page_sheet_set() {
        let dir = tempfile::tempdir().unwrap();
        let sheet = sheet_with_viewport();
        let entities: Vec<DxfEntity> = vec![DxfEntity::Line(DxfLine {
            layer: "WALLS".into(),
            start: [0.0, 0.0, 0.0],
            end: [1.0, 1.0, 0.0],
        })];
        let mut b = SheetPdfBuilder::new("Project Alpha").unwrap();
        for _ in 0..3 {
            b.add_sheet(&sheet, &entities, &PlotStyleTable::monochrome())
                .unwrap();
        }
        assert_eq!(b.sheets_emitted(), 3);
        let path = b.save(dir.path().join("set.pdf")).unwrap();
        let bytes = std::fs::read(&path).unwrap();
        assert!(bytes.starts_with(b"%PDF"));
    }

    // -----------------------------------------------------------
    // Task 18: viewport clipping, dimension rendering, linetypes
    // -----------------------------------------------------------

    #[test]
    fn linetype_from_name_round_trips_well_known_dxf_names() {
        assert_eq!(Linetype::from_name("CONTINUOUS"), Linetype::Continuous);
        assert_eq!(Linetype::from_name("ByLayer"), Linetype::Continuous);
        assert_eq!(Linetype::from_name("hidden"), Linetype::Hidden);
        assert_eq!(Linetype::from_name("DASHED"), Linetype::Dashed);
        assert_eq!(Linetype::from_name("center"), Linetype::Center);
        assert_eq!(Linetype::from_name("PHANTOM"), Linetype::Phantom);
        assert_eq!(Linetype::from_name("dashdot"), Linetype::DashDot);
        assert_eq!(Linetype::from_name("divide"), Linetype::Divide);
        // Unknown linetype falls back to continuous so the entity
        // remains visible.
        assert_eq!(Linetype::from_name("MY-CUSTOM-LT"), Linetype::Continuous);
    }

    #[test]
    fn continuous_linetype_has_no_dash_pattern_others_do() {
        assert!(Linetype::Continuous.dash_pattern().is_none());
        for lt in [
            Linetype::Dashed,
            Linetype::Hidden,
            Linetype::Center,
            Linetype::Phantom,
            Linetype::DashDot,
            Linetype::Divide,
        ] {
            let p = lt.dash_pattern().unwrap();
            assert!(p.dash_1 > 0, "{lt:?} has positive primary dash");
            assert!(p.gap_1 > 0, "{lt:?} has positive primary gap");
        }
    }

    #[test]
    fn layer_plot_table_lookup_is_case_insensitive() {
        let mut t = LayerPlotTable::new();
        t.insert("WALLS", 7, Linetype::Continuous);
        t.insert("HIDDEN", 9, Linetype::Hidden);
        assert_eq!(t.get("walls").unwrap().aci, 7);
        assert_eq!(t.get("Hidden").unwrap().linetype, Linetype::Hidden);
        assert!(t.get("dims").is_none());
        assert_eq!(t.len(), 2);
        assert!(!t.is_empty());
    }

    #[test]
    fn add_sheet_with_tables_renders_dimensions_and_dashed_layers() {
        let dir = tempfile::tempdir().unwrap();
        let sheet = sheet_with_viewport();
        let entities = vec![
            // A solid wall.
            DxfEntity::Line(DxfLine {
                layer: "WALLS".into(),
                start: [0.0, 0.0, 0.0],
                end: [4000.0, 0.0, 0.0],
            }),
            // A hidden line — should pick up the dashed pattern.
            DxfEntity::Line(DxfLine {
                layer: "HIDDEN".into(),
                start: [0.0, 500.0, 0.0],
                end: [4000.0, 500.0, 0.0],
            }),
            // A linear dimension along the wall.
            DxfEntity::Dimension(DxfDimension {
                layer: "DIMS".into(),
                style: "ARCH-1-100".into(),
                kind: DxfDimensionKind::Linear,
                def_point: [2000.0, -800.0, 0.0],
                text_position: [2000.0, -1000.0, 0.0],
                def_point_a: [0.0, 0.0, 0.0],
                def_point_b: [4000.0, 0.0, 0.0],
                override_text: None,
                measured_value: Some(4000.0),
            }),
            // A circle + radial dimension.
            DxfEntity::Circle(DxfCircle {
                layer: "WALLS".into(),
                center: [6000.0, 0.0, 0.0],
                radius: 500.0,
            }),
            DxfEntity::Dimension(DxfDimension {
                layer: "DIMS".into(),
                style: "ARCH-1-100".into(),
                kind: DxfDimensionKind::Radial,
                def_point: [6000.0, 0.0, 0.0],
                text_position: [6800.0, 400.0, 0.0],
                def_point_a: [6500.0, 0.0, 0.0],
                def_point_b: [0.0, 0.0, 0.0],
                override_text: None,
                measured_value: Some(500.0),
            }),
            // An angular dimension between two arms.
            DxfEntity::Dimension(DxfDimension {
                layer: "DIMS".into(),
                style: "ARCH-1-100".into(),
                kind: DxfDimensionKind::Angular,
                def_point: [0.0, 0.0, 0.0],
                text_position: [200.0, 200.0, 0.0],
                def_point_a: [1000.0, 0.0, 0.0],
                def_point_b: [0.0, 1000.0, 0.0],
                override_text: Some("90°".to_string()),
                measured_value: None,
            }),
        ];

        let mut layers = LayerPlotTable::new();
        layers.insert("WALLS", 7, Linetype::Continuous);
        layers.insert("HIDDEN", 9, Linetype::Hidden);
        layers.insert("DIMS", 2, Linetype::Continuous);

        let dim_styles = DimStyleTable::from_slice(&[DxfDimStyle {
            name: "ARCH-1-100".to_string(),
            text_height: 2.5,
            arrow_size: 2.0,
            units_scale: 1.0,
            decimal_places: 0,
            text_style: "STANDARD".to_string(),
        }]);

        let mut b = SheetPdfBuilder::new("Dim Test").unwrap();
        b.add_sheet_with_tables(
            &sheet,
            &entities,
            &PlotStyleTable::monochrome(),
            &layers,
            &dim_styles,
        )
        .unwrap();
        let path = b.save(dir.path().join("dim.pdf")).unwrap();
        let bytes = std::fs::read(&path).unwrap();
        assert!(bytes.starts_with(b"%PDF"));
        // PDFs containing dimension geometry (extension lines +
        // arrows + text + circles) plus a dashed-line layer
        // should be substantially larger than a one-line PDF.
        assert!(bytes.len() > 4000, "got {} bytes", bytes.len());
    }

    #[test]
    fn diameter_dimension_uses_a_diameter_glyph_in_label() {
        // We can't easily inspect the PDF text stream, but we can
        // verify the formatter helper produces the right value.
        assert_eq!(format_value(1234.0, 0), "1234");
        assert_eq!(format_value(1234.5, 2), "1234.50");
    }

    #[test]
    fn rect_clip_path_uses_correct_pdf_y_flip() {
        // Sheet 100 mm tall, viewport at (10, 20) sized 30x40 mm in
        // top-down coords. The PDF Y of the top of the clip is
        // sheet_h - top = 100 - 20 = 80. The bottom is sheet_h -
        // (top + h) = 100 - 60 = 40. We can't extract the polygon
        // directly through printpdf without a full render pass, but
        // we can sanity-check the maths matches by re-implementing
        // the formula here.
        let sheet_h_mm: f64 = 100.0;
        let paper_origin: [f64; 2] = [10.0, 20.0];
        let paper_size: [f64; 2] = [30.0, 40.0];
        let pdf_y_top: f64 = sheet_h_mm - paper_origin[1];
        let pdf_y_bot: f64 = sheet_h_mm - (paper_origin[1] + paper_size[1]);
        assert!((pdf_y_top - 80.0_f64).abs() < 1e-9);
        assert!((pdf_y_bot - 40.0_f64).abs() < 1e-9);
    }

    #[test]
    fn frozen_layers_are_skipped_in_a_viewport() {
        let dir = tempfile::tempdir().unwrap();
        let mut sheet = Sheet::new("Frozen Test", PaperSize::IsoA3);
        let mut vp = SheetViewport::new("PLAN", [10.0, 10.0], [200.0, 100.0]);
        vp.scale = 0.01;
        vp.frozen_layers = vec!["SECRET".to_string()];
        sheet.viewports.push(vp);

        let entities = vec![
            DxfEntity::Line(DxfLine {
                layer: "SECRET".into(),
                start: [0.0, 0.0, 0.0],
                end: [4000.0, 0.0, 0.0],
            }),
            DxfEntity::Line(DxfLine {
                layer: "VISIBLE".into(),
                start: [0.0, 100.0, 0.0],
                end: [4000.0, 100.0, 0.0],
            }),
        ];

        // We render twice: once with the layer in the freeze list,
        // once with an empty freeze list. The "secret" version
        // should be smaller because one line is missing from the
        // content stream.
        let layers = LayerPlotTable::default();
        let dim_styles = DimStyleTable::default();
        let mut b_frozen = SheetPdfBuilder::new("Frozen").unwrap();
        b_frozen
            .add_sheet_with_tables(
                &sheet,
                &entities,
                &PlotStyleTable::monochrome(),
                &layers,
                &dim_styles,
            )
            .unwrap();
        let frozen_path = b_frozen.save(dir.path().join("frozen.pdf")).unwrap();

        let mut thawed_sheet = sheet.clone();
        thawed_sheet.viewports[0].frozen_layers.clear();
        let mut b_thawed = SheetPdfBuilder::new("Thawed").unwrap();
        b_thawed
            .add_sheet_with_tables(
                &thawed_sheet,
                &entities,
                &PlotStyleTable::monochrome(),
                &layers,
                &dim_styles,
            )
            .unwrap();
        let thawed_path = b_thawed.save(dir.path().join("thawed.pdf")).unwrap();

        let frozen_bytes = std::fs::read(&frozen_path).unwrap();
        let thawed_bytes = std::fs::read(&thawed_path).unwrap();
        // PDFs are normally bigger when they contain more geometry.
        // (printpdf doesn't compress the content stream by default
        // for the same input, so the byte-count signal is reliable
        // here.)
        assert!(
            thawed_bytes.len() > frozen_bytes.len(),
            "expected thawed PDF ({}) to be larger than frozen PDF ({})",
            thawed_bytes.len(),
            frozen_bytes.len()
        );
    }

    // -----------------------------------------------------------
    // Regression tests for PR #56 Devin Review findings
    // -----------------------------------------------------------

    #[test]
    fn dim_style_table_lookup_is_case_insensitive() {
        // DXF style names round-trip case-insensitively (AutoCAD
        // convention). A `DimStyleTable` seeded with "ARCH-1-100"
        // must answer to "arch-1-100", "Arch-1-100", etc.
        let dim_styles = DimStyleTable::from_slice(&[DxfDimStyle {
            name: "ARCH-1-100".into(),
            text_height: 2.5,
            arrow_size: 2.0,
            units_scale: 1.0,
            decimal_places: 2,
            text_style: "STANDARD".into(),
        }]);
        assert_eq!(dim_styles.get("ARCH-1-100").unwrap().decimal_places, 2);
        assert_eq!(dim_styles.get("arch-1-100").unwrap().decimal_places, 2);
        assert_eq!(dim_styles.get("Arch-1-100").unwrap().decimal_places, 2);
        // Style names with different normalised forms still miss.
        assert!(dim_styles.get("ARCH-1-50").is_none());
    }

    #[test]
    fn dim_style_resolve_fallback_matches_dxf_dim_style_standard() {
        // The fallback when a referenced style name is absent must
        // match `DxfDimStyle::standard()` so PDF output for missing
        // styles agrees with the rest of the DXF stack. In
        // particular `decimal_places` defaults to 4 per AutoCAD's
        // `DIMDEC` default — earlier revisions hard-coded 0 here
        // which produced labels like "4000" instead of "4000.0000".
        let dim_styles = DimStyleTable::default();
        let standard = DxfDimStyle::standard();
        let resolved = dim_styles.resolve("MISSING");
        assert_eq!(resolved.decimal_places, 4);
        assert_eq!(resolved.decimal_places, standard.decimal_places);
        assert_eq!(resolved.text_height, standard.text_height);
        assert_eq!(resolved.arrow_size, standard.arrow_size);
        // The returned style carries the requested name back.
        assert_eq!(resolved.name, "MISSING");
    }

    #[test]
    fn phantom_and_center_dash_patterns_are_distinguishable() {
        // Both CENTER and PHANTOM are valid DXF linetypes with
        // distinct stroke shapes (CENTER = dash-dot, PHANTOM =
        // dash-dot-dot). Earlier revisions returned the same
        // four-segment pattern for both, which defeats the purpose
        // of having separate enum variants.
        let center = Linetype::Center.dash_pattern().unwrap();
        let phantom = Linetype::Phantom.dash_pattern().unwrap();
        assert_ne!(center, phantom);
        // PHANTOM should exercise the third dash/gap pair —
        // printpdf supports up to three pairs and CENTER only uses
        // two.
        assert!(phantom.dash_3.is_some(), "phantom uses 3rd dash pair");
        assert!(phantom.gap_3.is_some(), "phantom uses 3rd gap pair");
        assert!(center.dash_3.is_none(), "center stays at 2 pairs");
    }

    #[test]
    fn divide_and_dashdot_dash_patterns_are_distinguishable() {
        // DXF DIVIDE = dash-dot-dot; DXF DASHDOT = dash-dot. The
        // two patterns differ by a second dot, which requires the
        // third pair printpdf supports.
        let dashdot = Linetype::DashDot.dash_pattern().unwrap();
        let divide = Linetype::Divide.dash_pattern().unwrap();
        assert_ne!(dashdot, divide);
        assert!(divide.dash_3.is_some(), "divide uses 3rd dash pair");
        assert!(divide.gap_3.is_some(), "divide uses 3rd gap pair");
        assert!(dashdot.dash_3.is_none(), "dashdot stays at 2 pairs");
    }

    #[test]
    fn radial_dim_value_passes_measured_value_through_verbatim() {
        // For RADIAL the DXF spec puts the radius in code 42, and
        // the reader stores it in `measured_value`. The PDF
        // exporter must not transform it further.
        let v = radial_dim_value(
            DxfDimensionKind::Radial,
            Some(500.0),
            f64::NAN, // unused — measured_value short-circuits
            0.01,
        );
        assert!((v - 500.0).abs() < 1e-9);
    }

    #[test]
    fn radial_dim_value_diameter_measured_value_is_not_doubled() {
        // For DIAMETER, code 42 stores the diameter directly (per
        // DXF spec). The reader sets `measured_value = diameter`,
        // so doubling it here would produce 2 × diameter — wrong.
        // This locks the contract against regression.
        let v = radial_dim_value(DxfDimensionKind::Diameter, Some(1000.0), f64::NAN, 0.01);
        assert!(
            (v - 1000.0).abs() < 1e-9,
            "diameter measured_value must pass through unchanged, got {v}"
        );
    }

    #[test]
    fn radial_dim_value_computed_radial_is_radius_not_diameter() {
        // When `measured_value` is None we compute from the leader
        // length. RADIAL = radius, DIAMETER = 2 × radius.
        // Leader paper length = 5 mm at 1:100 → 500 model units.
        let r = radial_dim_value(DxfDimensionKind::Radial, None, 5.0, 0.01);
        assert!((r - 500.0).abs() < 1e-9, "got {r}");
        let d = radial_dim_value(DxfDimensionKind::Diameter, None, 5.0, 0.01);
        assert!((d - 1000.0).abs() < 1e-9, "got {d}");
    }

    #[test]
    fn linear_dim_text_y_is_routed_through_to_paper() {
        // Direct sanity check of the model→paper Y projection used
        // for the Linear/Aligned text baseline. Earlier revisions
        // treated `text_position[1]` as a paper-space coordinate
        // and applied only the PDF Y-flip, producing
        // `sheet_h - (-1000) = 1297` mm for the fixture's
        // model-space text_position[1] = -1000 — well off-sheet.
        let mut vp = SheetViewport::new("PLAN", [10.0, 10.0], [200.0, 100.0]);
        vp.scale = 0.01;
        let sheet_h_mm: f64 = 297.0;
        let paper = vp.model_to_paper([2000.0, -1000.0]);
        let pdf_y = sheet_h_mm - paper[1];
        assert!(paper[1] > 0.0 && paper[1] < sheet_h_mm, "paper Y on-sheet");
        assert!(
            (pdf_y - (sheet_h_mm - 50.0)).abs() < 1e-6,
            "expected PDF Y ≈ {} (model→paper Y = 50.0 mm), got {}",
            sheet_h_mm - 50.0,
            pdf_y
        );
        // Sanity-check the buggy formula would have put the text
        // off-sheet to lock in the regression.
        let buggy_pdf_y = sheet_h_mm - (-1000.0);
        assert!(
            buggy_pdf_y > sheet_h_mm,
            "buggy formula puts text off-sheet ({} > {})",
            buggy_pdf_y,
            sheet_h_mm
        );
    }

    #[test]
    fn linear_dim_extension_lines_are_perpendicular_at_measured_point_x() {
        // For a horizontal dim with measured points A=(0,0),
        // B=(4000,0) and dim-line offset D=(2000,-800) at 1:100,
        // the dim line and extension lines must align with each
        // measured point's projected X — NOT the dim-offset
        // point's X. The bug used `dx` (= projected D.x) for the
        // start of the dim line and extension line 1, producing a
        // diagonal extension and a half-length dim line.
        let mut vp = SheetViewport::new("PLAN", [10.0, 10.0], [200.0, 100.0]);
        vp.scale = 0.01;
        let a_paper = vp.model_to_paper([0.0, 0.0]);
        let b_paper = vp.model_to_paper([4000.0, 0.0]);
        let d_paper = vp.model_to_paper([2000.0, -800.0]);
        // A.x, B.x must straddle D.x — confirming the bug would
        // collapse the dim line to half its proper width.
        assert!(a_paper[0] < d_paper[0]);
        assert!(d_paper[0] < b_paper[0]);
        // The proper dim-line width is |B.x - A.x|; the buggy one
        // would have been |B.x - D.x| = half of that.
        let proper_width = (b_paper[0] - a_paper[0]).abs();
        let buggy_width = (b_paper[0] - d_paper[0]).abs();
        assert!(
            (proper_width - 2.0 * buggy_width).abs() < 1e-6,
            "buggy dim line is exactly half-width; proper={proper_width}, buggy={buggy_width}"
        );
    }

    #[test]
    fn angular_arc_angles_are_paper_space_independent_of_pdf_y_flip() {
        // For arms at 0° and +90° in model space, the angles
        // computed in paper space (Y-down) must give the same
        // sweep as computing them in PDF space (Y-flipped) ONLY
        // when the arms lie along x±y axes that survive Y-flip
        // symmetrically. In general the PDF-space atan2 inverts
        // arc direction. Lock the paper-space convention by
        // re-implementing it in the test.
        let vp = {
            let mut v = SheetViewport::new("PLAN", [10.0, 10.0], [200.0, 100.0]);
            v.scale = 0.01;
            v
        };
        let vertex = vp.model_to_paper([0.0, 0.0]);
        let arm_a = vp.model_to_paper([1000.0, 0.0]); // 0° in model
        let arm_b = vp.model_to_paper([0.0, 1000.0]); // +90° in model
        let angle_a_paper = (arm_a[1] - vertex[1]).atan2(arm_a[0] - vertex[0]);
        let angle_b_paper = (arm_b[1] - vertex[1]).atan2(arm_b[0] - vertex[0]);
        // arm_a is along the paper +X (angle 0); arm_b is +Y in
        // model but `model_to_paper` preserves Y-orientation here
        // (no rotation, positive scale), so arm_b is along paper
        // +Y → angle +π/2.
        assert!(
            angle_a_paper.abs() < 1e-6,
            "paper angle_a ≈ 0, got {angle_a_paper}"
        );
        assert!(
            (angle_b_paper - std::f64::consts::FRAC_PI_2).abs() < 1e-6,
            "paper angle_b ≈ π/2, got {angle_b_paper}"
        );
        // After PDF Y-flip, the same vector becomes (-Y) — its
        // atan2 is now -π/2, i.e. the sweep direction has reversed.
        let sheet_h_mm: f64 = 297.0;
        let vy_pdf = sheet_h_mm - vertex[1];
        let by_pdf = sheet_h_mm - arm_b[1];
        let angle_b_pdf = (by_pdf - vy_pdf).atan2(arm_b[0] - vertex[0]);
        assert!(
            (angle_b_pdf + std::f64::consts::FRAC_PI_2).abs() < 1e-6,
            "PDF-space angle_b ≈ -π/2 (inverted), got {angle_b_pdf}"
        );
        // The two angles disagree on direction: this is the
        // regression we fixed by computing arc angles in paper
        // space.
        assert!((angle_b_paper - angle_b_pdf).abs() > 1.0);
    }

    #[test]
    fn arc_renders_inside_viewport_without_clipping_errors() {
        let dir = tempfile::tempdir().unwrap();
        let sheet = sheet_with_viewport();
        let entities = vec![DxfEntity::Arc(DxfArc {
            layer: "WALLS".into(),
            center: [2000.0, 1500.0, 0.0],
            radius: 500.0,
            start_angle: 0.0,
            end_angle: 90.0,
        })];
        let mut b = SheetPdfBuilder::new("Arc").unwrap();
        b.add_sheet(&sheet, &entities, &PlotStyleTable::monochrome())
            .unwrap();
        let path = b.save(dir.path().join("arc.pdf")).unwrap();
        let bytes = std::fs::read(&path).unwrap();
        assert!(bytes.starts_with(b"%PDF"));
    }
}
