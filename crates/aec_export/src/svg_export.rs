//! Deterministic SVG export of CAD entities arranged on a sheet.
//!
//! Given a `Sheet` (paper size + viewports + optional title block) and a
//! slice of `DxfEntity`s in model space, this produces an SVG document
//! whose output is byte-identical for the same input regardless of
//! platform or wall-clock time.
//!
//! Feature coverage (Phase 11 Group C Task 19):
//! - Every `DxfEntity` variant: Line / Polyline / Arc / Circle / Ellipse
//!   / Spline / Hatch / Text / Insert / Dimension (all 5 kinds).
//! - Per-viewport rectangular clipping via `<clipPath>` so geometry
//!   outside a viewport never bleeds.
//! - Layer visibility (`Layer::is_visible()`) — frozen or off layers
//!   skipped, plus per-viewport `frozen_layers` overlay.
//! - Layer colors used as fall-back when the plot-style table has no
//!   entry; layer colors mix with screening if the plot style requests
//!   it. Plot-style overrides win when present.
//! - Linetypes (Continuous, Hidden, Dashed, Center, Phantom, DashDot,
//!   Divide) translated to `stroke-dasharray`.
//! - Hatch fill patterns — solid hatches fill the polygon; named DXF
//!   patterns (ANSI31, ANSI32, ANSI33, ANSI34, ANSI35, ANSI36, ANSI37,
//!   ANSI38, DOTS) are rendered as SVG `<pattern>` defs.
//! - Title block: border + populated fields rendered outside any
//!   viewport's clip group.
//! - Block expansion — `INSERT` entities are expanded against an
//!   optional block table; missing blocks render as a labelled bbox
//!   marker so the operator can still see where the symbol was placed.

use std::collections::{BTreeMap, BTreeSet};
use std::fmt::Write;

use aec_cad::dxf::{DxfDimStyle, DxfDimensionKind, DxfEntity};
use aec_cad::layers::Layer;
use aec_cad::sheets::{PaperSize, Sheet, SheetViewport};
use serde::{Deserialize, Serialize};

use crate::plot_style::{PlotStyle, PlotStyleTable};

#[derive(Debug, thiserror::Error)]
pub enum SvgExportError {
    #[error("write: {0}")]
    Fmt(#[from] std::fmt::Error),
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SvgExportOptions {
    /// Background fill of the paper (None = no fill).
    pub paper_fill: Option<[u8; 3]>,
    /// Border stroke around the paper.
    pub draw_border: bool,
    /// Maximum number of decimal places to emit for coordinates.
    pub coordinate_precision: u8,
    /// If true, frozen/off layers are skipped. (Default true.)
    pub respect_layer_visibility: bool,
    /// If true, viewports are wrapped in a `<clipPath>` so model-space
    /// geometry can't bleed outside the viewport rectangle.
    pub clip_viewports: bool,
}

impl Default for SvgExportOptions {
    fn default() -> Self {
        Self {
            paper_fill: Some([255, 255, 255]),
            draw_border: true,
            coordinate_precision: 4,
            respect_layer_visibility: true,
            clip_viewports: true,
        }
    }
}

/// SVG-side linetype model. Mirrors the printpdf `Linetype` in
/// `pdf_sheet` but emits as `stroke-dasharray` values in mm.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum SvgLinetype {
    Continuous,
    Dashed,
    Hidden,
    Center,
    Phantom,
    DashDot,
    Divide,
}

impl SvgLinetype {
    /// Resolve a DXF linetype name to its enum value. Unknown names
    /// (and `BYLAYER`, `BYBLOCK`) fall back to `Continuous` so the
    /// renderer never produces an invalid `stroke-dasharray`.
    pub fn from_name(name: &str) -> Self {
        match name.trim().to_ascii_uppercase().as_str() {
            "" | "CONTINUOUS" | "BYLAYER" | "BYBLOCK" | "SOLID" => Self::Continuous,
            "DASHED" | "DASH" => Self::Dashed,
            "HIDDEN" | "HID" => Self::Hidden,
            "CENTER" | "CEN" => Self::Center,
            "PHANTOM" | "PHA" => Self::Phantom,
            "DASHDOT" | "DASH_DOT" | "DASHDOT2" => Self::DashDot,
            "DIVIDE" | "DIV" | "DIVIDE2" => Self::Divide,
            _ => Self::Continuous,
        }
    }

    /// Emits the `stroke-dasharray` attribute value, or `None` for
    /// continuous (which means "no dasharray attribute at all").
    pub fn dasharray_mm(self) -> Option<&'static str> {
        match self {
            Self::Continuous => None,
            Self::Dashed => Some("4,2"),
            Self::Hidden => Some("2.5,1.25"),
            Self::Center => Some("12,1.5,3,1.5"),
            Self::Phantom => Some("16,2,4,2,4,2"),
            // SVG draws zero-length dashes as dots when `stroke-linecap=round`,
            // which our renderer uses globally for stylistic consistency.
            Self::DashDot => Some("5,2,0.001,2"),
            Self::Divide => Some("4,2,0.001,2,0.001,2"),
        }
    }
}

/// Case-insensitive layer lookup. Distinct from the project-level
/// `LayerSystem` because the exporter receives only the layer slice
/// the caller wants to honour (e.g. drawn from the project's active
/// layer table at plot time).
#[derive(Debug, Clone, Default)]
pub struct LayerTable {
    by_name: BTreeMap<String, Layer>,
}

impl LayerTable {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn from_slice(layers: &[Layer]) -> Self {
        let mut t = Self::new();
        for l in layers {
            t.insert(l.clone());
        }
        t
    }

    pub fn insert(&mut self, layer: Layer) {
        let key = layer.name.to_ascii_uppercase();
        self.by_name.insert(key, layer);
    }

    pub fn get(&self, name: &str) -> Option<&Layer> {
        self.by_name.get(&name.to_ascii_uppercase())
    }

    pub fn len(&self) -> usize {
        self.by_name.len()
    }

    pub fn is_empty(&self) -> bool {
        self.by_name.is_empty()
    }
}

/// Dimension-style table, mirrors the pdf_sheet `DimStyleTable`.
#[derive(Debug, Clone, Default)]
pub struct DimStyleTable {
    by_name: BTreeMap<String, DxfDimStyle>,
}

impl DimStyleTable {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn from_slice(styles: &[DxfDimStyle]) -> Self {
        let mut t = Self::new();
        for s in styles {
            t.insert(s.clone());
        }
        t
    }

    pub fn insert(&mut self, style: DxfDimStyle) {
        self.by_name.insert(style.name.to_ascii_uppercase(), style);
    }

    pub fn resolve(&self, name: &str) -> DxfDimStyle {
        self.by_name
            .get(&name.to_ascii_uppercase())
            .cloned()
            .unwrap_or_else(DxfDimStyle::standard)
    }
}

/// Block-definition table. INSERT entities are expanded against this.
/// Missing blocks render as a labelled bbox marker so the operator can
/// still see where the symbol was placed.
///
/// DXF block names are case-insensitive per AutoCAD convention
/// (`Window` and `WINDOW` refer to the same block). Keys are
/// normalized to uppercase on both `insert` and `get`, matching the
/// case-insensitivity already implemented by [`LayerTable`] and
/// [`DimStyleTable`].
#[derive(Debug, Clone, Default)]
pub struct BlockTable {
    by_name: BTreeMap<String, Vec<DxfEntity>>,
}

impl BlockTable {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn insert(&mut self, name: impl Into<String>, body: Vec<DxfEntity>) {
        self.by_name.insert(name.into().to_ascii_uppercase(), body);
    }

    pub fn get(&self, name: &str) -> Option<&[DxfEntity]> {
        self.by_name
            .get(&name.to_ascii_uppercase())
            .map(Vec::as_slice)
    }
}

/// Maximum block-expansion recursion depth. INSERT entities that
/// reference each other in a cycle (A inserts B, B inserts A) would
/// otherwise blow the stack — at the limit we render a labelled
/// placeholder so the operator still sees where the symbol was
/// placed and which cycle hit the wall.
///
/// 16 is well above what real drawings need (deep nesting in DXF is
/// almost always 2-3 levels: title-block → sub-symbol → leaf), but
/// shallow enough that a runaway cycle is caught before it produces
/// gigabytes of SVG.
pub const BLOCK_EXPANSION_MAX_DEPTH: u32 = 16;

/// Composed insert transform applied to a block body. Captures
/// translation, rotation, and per-axis scale; supplies the geometric
/// transforms needed by `transform_entity` so that radii, text
/// heights, and direction vectors all move with the insert (not just
/// the entity center points).
#[derive(Debug, Clone, Copy)]
struct BlockTransform {
    pos: [f64; 3],
    scale: [f64; 3],
    rot_deg: f64,
    cos_t: f64,
    sin_t: f64,
}

impl BlockTransform {
    fn new(pos: [f64; 3], scale: [f64; 3], rot_deg: f64) -> Self {
        let rot = rot_deg.to_radians();
        Self {
            pos,
            scale,
            rot_deg,
            cos_t: rot.cos(),
            sin_t: rot.sin(),
        }
    }

    /// Transform a model-space point: scale → rotate → translate.
    fn point3(&self, pt: &[f64; 3]) -> [f64; 3] {
        let sx = pt[0] * self.scale[0];
        let sy = pt[1] * self.scale[1];
        let rx = sx * self.cos_t - sy * self.sin_t;
        let ry = sx * self.sin_t + sy * self.cos_t;
        [
            self.pos[0] + rx,
            self.pos[1] + ry,
            pt[2] * self.scale[2] + self.pos[2],
        ]
    }

    fn point2(&self, pt: &[f64; 2]) -> [f64; 2] {
        let p = self.point3(&[pt[0], pt[1], 0.0]);
        [p[0], p[1]]
    }

    /// Transform a direction vector (e.g. ellipse `major_axis`):
    /// scale + rotate, but NO translation. The vector encodes both
    /// length (`||v||`) and rotation (`atan2(v.y, v.x)`); without
    /// applying scale+rotate to it, ellipses inside rotated /
    /// non-unit-scale blocks render at the wrong size and angle.
    fn vec3(&self, v: &[f64; 3]) -> [f64; 3] {
        let sx = v[0] * self.scale[0];
        let sy = v[1] * self.scale[1];
        let rx = sx * self.cos_t - sy * self.sin_t;
        let ry = sx * self.sin_t + sy * self.cos_t;
        [rx, ry, v[2] * self.scale[2]]
    }

    /// Scalar XY scale for radii (Circle, Arc). Uses the mean of
    /// |sx| and |sy|; for the common AutoCAD case of uniform insert
    /// scale this is exact. Under non-uniform scaling a Circle
    /// technically becomes an ellipse, but `DxfCircle` has no
    /// major-axis vector so the mean is the best we can offer
    /// without changing the entity model.
    fn uniform_xy_scale(&self) -> f64 {
        (self.scale[0].abs() + self.scale[1].abs()) / 2.0
    }

    /// Text/Attdef height scale. Mirrors AutoCAD's convention of
    /// using the Y-scale for text height inside blocks.
    fn text_height_scale(&self) -> f64 {
        self.scale[1].abs()
    }

    fn rot_deg(&self) -> f64 {
        self.rot_deg
    }
}

/// Existing entry point — emits an SVG using only a plot-style table.
/// Layer visibility / linetypes / dimensions / blocks fall back to
/// safe defaults. Prefer `render_sheet_svg_full` for new callers.
pub fn render_sheet_svg(
    sheet: &Sheet,
    entities: &[DxfEntity],
    plot_style: &PlotStyleTable,
    options: &SvgExportOptions,
) -> Result<String, SvgExportError> {
    render_sheet_svg_full(
        sheet,
        entities,
        plot_style,
        &LayerTable::new(),
        &DimStyleTable::new(),
        &BlockTable::new(),
        options,
    )
}

/// Full-featured SVG renderer. Honours layer visibility, viewport
/// clipping, per-layer linetypes, dimensions, hatch patterns, block
/// expansion, and the sheet's title block.
pub fn render_sheet_svg_full(
    sheet: &Sheet,
    entities: &[DxfEntity],
    plot_style: &PlotStyleTable,
    layer_table: &LayerTable,
    dim_styles: &DimStyleTable,
    blocks: &BlockTable,
    options: &SvgExportOptions,
) -> Result<String, SvgExportError> {
    let (w_mm, h_mm) = sheet.dimensions_mm();
    let mut out = String::new();
    writeln!(out, r#"<?xml version="1.0" encoding="UTF-8"?>"#)?;
    writeln!(
        out,
        r#"<svg xmlns="http://www.w3.org/2000/svg" width="{w_mm}mm" height="{h_mm}mm" viewBox="0 0 {w_mm} {h_mm}" stroke-linecap="round" stroke-linejoin="round">"#,
    )?;

    // <defs>: arrowhead marker + per-viewport clip-paths + hatch
    // patterns. Patterns iterate sorted so the output is deterministic.
    write_defs(
        &mut out,
        sheet,
        entities,
        options.clip_viewports,
        options.coordinate_precision,
    )?;

    if let Some([r, g, b]) = options.paper_fill {
        writeln!(
            out,
            r#"  <rect x="0" y="0" width="{w_mm}" height="{h_mm}" fill="rgb({r},{g},{b})" />"#,
        )?;
    }
    if options.draw_border {
        writeln!(
            out,
            r#"  <rect x="0" y="0" width="{w_mm}" height="{h_mm}" fill="none" stroke="black" stroke-width="0.2" />"#,
        )?;
    }

    // Each viewport: optionally clip, then emit visible entities.
    for (idx, vp) in sheet.viewports.iter().enumerate() {
        let clip_attr = if options.clip_viewports {
            format!(r#" clip-path="url(#vp-clip-{idx})""#)
        } else {
            String::new()
        };
        writeln!(
            out,
            r#"  <g class="viewport" id="vp-{idx}" data-name="{name}"{clip_attr}>"#,
            name = escape_xml(&vp.name),
        )?;
        for entity in entities {
            if !is_layer_visible_in(vp, entity.layer(), layer_table, options) {
                continue;
            }
            emit_entity(
                &mut out,
                entity,
                vp,
                plot_style,
                layer_table,
                dim_styles,
                blocks,
                options,
                BLOCK_EXPANSION_MAX_DEPTH,
            )?;
        }
        writeln!(out, "  </g>")?;
    }

    // Title block (outside any viewport clip).
    if let Some(title) = sheet.title_block.as_ref() {
        write_title_block(&mut out, sheet, title, options)?;
    }

    writeln!(out, "</svg>")?;
    Ok(out)
}

/// Builds the `<defs>` block — arrowhead marker, viewport clip-paths,
/// and any hatch patterns referenced by entities.
fn write_defs(
    out: &mut String,
    sheet: &Sheet,
    entities: &[DxfEntity],
    clip_viewports: bool,
    prec: u8,
) -> Result<(), SvgExportError> {
    writeln!(out, "  <defs>")?;
    // Closed-triangle arrowhead marker, 2.5 mm long. Used for
    // dimension arrows.
    writeln!(
        out,
        r#"    <marker id="dim-arrow" viewBox="0 0 10 10" refX="10" refY="5" markerUnits="userSpaceOnUse" markerWidth="2.5" markerHeight="2.5" orient="auto-start-reverse"><path d="M 0 0 L 10 5 L 0 10 z" fill="currentColor" /></marker>"#,
    )?;

    // Viewport clip rects (only when clipping is enabled — emitting an
    // unreferenced `<clipPath>` would just bloat the file and confuse
    // SVG editors that flag dangling defs).
    if clip_viewports {
        for (idx, vp) in sheet.viewports.iter().enumerate() {
            writeln!(
                out,
                r#"    <clipPath id="vp-clip-{idx}"><rect x="{x}" y="{y}" width="{w}" height="{h}" /></clipPath>"#,
                // Use `fmt_coord` for the same precision as every other
                // numeric attribute in the document. Rust's default
                // `Display` for f64 is deterministic but emits a variable
                // number of decimals, which breaks the visual
                // "same-precision-everywhere" rule that downstream SVG
                // tooling relies on.
                x = fmt_coord(vp.paper_origin[0], prec),
                y = fmt_coord(vp.paper_origin[1], prec),
                w = fmt_coord(vp.paper_size[0], prec),
                h = fmt_coord(vp.paper_size[1], prec),
            )?;
        }
    }

    // Hatch patterns — collect unique names referenced.
    let mut needed: BTreeSet<String> = BTreeSet::new();
    for e in entities {
        if let DxfEntity::Hatch(h) = e {
            if !h.solid {
                needed.insert(h.pattern_name.to_ascii_uppercase());
            }
        }
    }
    for name in &needed {
        write_hatch_pattern_def(out, name)?;
    }
    writeln!(out, "  </defs>")?;
    Ok(())
}

/// Defines a single named DXF hatch pattern as an SVG `<pattern>`.
/// Names not in the well-known set get a generic 45° cross-hatch.
fn write_hatch_pattern_def(out: &mut String, name: &str) -> Result<(), SvgExportError> {
    // Build a 4mm × 4mm tile, then `patternTransform` applies the
    // angle/scale at use sites.
    let pid = format!("hatch-{}", sanitize_id(name));
    match name {
        // 45° lines (most common ANSI31 — "iron" pattern).
        "ANSI31" => writeln!(
            out,
            r#"    <pattern id="{pid}" width="3" height="3" patternUnits="userSpaceOnUse" patternTransform="rotate(45)"><line x1="0" y1="0" x2="0" y2="3" stroke="currentColor" stroke-width="0.15" /></pattern>"#,
        )?,
        // 135° lines (ANSI32 — "steel").
        "ANSI32" => writeln!(
            out,
            r#"    <pattern id="{pid}" width="3" height="3" patternUnits="userSpaceOnUse" patternTransform="rotate(135)"><line x1="0" y1="0" x2="0" y2="3" stroke="currentColor" stroke-width="0.15" /></pattern>"#,
        )?,
        // 0° horizontal lines (ANSI33 — "bronze, brass").
        "ANSI33" => writeln!(
            out,
            r#"    <pattern id="{pid}" width="3" height="3" patternUnits="userSpaceOnUse"><line x1="0" y1="0" x2="3" y2="0" stroke="currentColor" stroke-width="0.15" /></pattern>"#,
        )?,
        // 0° + 90° cross-hatch (ANSI37 — "lead, zinc, white metal").
        "ANSI37" => writeln!(
            out,
            r#"    <pattern id="{pid}" width="3" height="3" patternUnits="userSpaceOnUse"><line x1="0" y1="0" x2="3" y2="0" stroke="currentColor" stroke-width="0.15" /><line x1="0" y1="0" x2="0" y2="3" stroke="currentColor" stroke-width="0.15" /></pattern>"#,
        )?,
        // 45° + 135° cross-hatch (ANSI38 — "magnesium").
        "ANSI38" => writeln!(
            out,
            r#"    <pattern id="{pid}" width="3" height="3" patternUnits="userSpaceOnUse" patternTransform="rotate(45)"><line x1="0" y1="0" x2="3" y2="0" stroke="currentColor" stroke-width="0.15" /><line x1="0" y1="0" x2="0" y2="3" stroke="currentColor" stroke-width="0.15" /></pattern>"#,
        )?,
        // DOTS — small dots.
        "DOTS" => writeln!(
            out,
            r#"    <pattern id="{pid}" width="2" height="2" patternUnits="userSpaceOnUse"><circle cx="1" cy="1" r="0.2" fill="currentColor" /></pattern>"#,
        )?,
        // Generic — 45° lines like ANSI31. Better than nothing.
        _ => writeln!(
            out,
            r#"    <pattern id="{pid}" width="3" height="3" patternUnits="userSpaceOnUse" patternTransform="rotate(45)"><line x1="0" y1="0" x2="0" y2="3" stroke="currentColor" stroke-width="0.15" /></pattern>"#,
        )?,
    }
    Ok(())
}

fn sanitize_id(name: &str) -> String {
    name.chars()
        .map(|c| if c.is_ascii_alphanumeric() { c } else { '-' })
        .collect()
}

/// True if entities on the given layer should be drawn in this
/// viewport. Honours global frozen/off + per-viewport `frozen_layers`.
fn is_layer_visible_in(
    vp: &SheetViewport,
    layer_name: &str,
    layer_table: &LayerTable,
    options: &SvgExportOptions,
) -> bool {
    // Per-viewport freeze always wins.
    if vp
        .frozen_layers
        .iter()
        .any(|n| n.eq_ignore_ascii_case(layer_name))
    {
        return false;
    }
    if !options.respect_layer_visibility {
        return true;
    }
    // Unknown layers default to visible (the caller may simply not
    // have populated the layer table).
    match layer_table.get(layer_name) {
        Some(l) => l.is_visible(),
        None => true,
    }
}

fn linetype_for(layer_name: &str, layer_table: &LayerTable, style: &PlotStyle) -> SvgLinetype {
    if style.override_linetype {
        if let Some(name) = &style.linetype {
            return SvgLinetype::from_name(name);
        }
    }
    match layer_table.get(layer_name) {
        Some(l) => SvgLinetype::from_name(&l.linetype),
        None => SvgLinetype::Continuous,
    }
}

/// Resolve plot style for a layer. We prefer (in order):
///   1. The plot style for the layer's *current* ACI (when known via
///      the layer table).
///   2. The plot style for the layer's default ACI (the legacy fallback
///      shipped in v0.1.0 — keeps existing tests happy).
///   3. A synthetic black 0.25mm style.
fn style_for_layer(
    layer_name: &str,
    layer_table: &LayerTable,
    plot_table: &PlotStyleTable,
) -> PlotStyle {
    let layer = layer_table.get(layer_name);
    let aci = match layer {
        Some(l) => aci_for_layer_color(l.color),
        None => legacy_default_aci(layer_name),
    };
    if let Some(style) = plot_table.get(aci).cloned() {
        return style;
    }
    // No plot-style entry: synthesise one from the layer's own colour
    // (via the ACI palette) and lineweight, so the renderer still
    // honours layer colours even when no CTB is loaded.
    let color = layer.map_or([0, 0, 0], |l| l.color.to_srgb());
    let lineweight_mm = layer.and_then(|l| l.lineweight.to_mm()).unwrap_or(0.25);
    PlotStyle {
        aci,
        color,
        lineweight_mm,
        screening: 100,
        linetype: None,
        override_linetype: false,
    }
}

/// Maps a `LayerColor` to an ACI byte. ByLayer / ByBlock / out-of-
/// range values bucket to 7 so the plot-style table always has an
/// entry to look up.
fn aci_for_layer_color(color: aec_cad::layers::LayerColor) -> u8 {
    if color.0 <= 0 || color.0 > 255 {
        7
    } else {
        color.0 as u8
    }
}

/// Pre-`LayerTable` behaviour — maps a layer name to a default ACI.
/// Kept for backwards compatibility with the legacy `render_sheet_svg`
/// entry point, where callers pass no layer table.
fn legacy_default_aci(layer: &str) -> u8 {
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

/// SVG coordinates have Y down; sheet paper coordinates have Y down too
/// (origin top-left). Both match, so we just emit the paper coords.
fn fmt_coord(v: f64, precision: u8) -> String {
    format!("{0:.1$}", v, precision as usize)
}

#[allow(clippy::too_many_arguments)]
fn emit_entity(
    out: &mut String,
    entity: &DxfEntity,
    vp: &SheetViewport,
    plot_table: &PlotStyleTable,
    layer_table: &LayerTable,
    dim_styles: &DimStyleTable,
    blocks: &BlockTable,
    opts: &SvgExportOptions,
    depth_remaining: u32,
) -> Result<(), SvgExportError> {
    let style = style_for_layer(entity.layer(), layer_table, plot_table);
    let [cr, cg, cb] = screened(style.color, style.screening);
    let lw = style.lineweight_mm.max(0.05);
    let prec = opts.coordinate_precision;
    let lt = linetype_for(entity.layer(), layer_table, &style);
    let dash_attr = lt
        .dasharray_mm()
        .map(|d| format!(r#" stroke-dasharray="{d}""#))
        .unwrap_or_default();

    let stroke = format!("rgb({cr},{cg},{cb})");
    match entity {
        DxfEntity::Line(e) => {
            let a = vp.model_to_paper([e.start[0], e.start[1]]);
            let b = vp.model_to_paper([e.end[0], e.end[1]]);
            writeln!(
                out,
                r#"    <line x1="{}" y1="{}" x2="{}" y2="{}" stroke="{stroke}" stroke-width="{lw}" fill="none"{dash_attr} />"#,
                fmt_coord(a[0], prec),
                fmt_coord(a[1], prec),
                fmt_coord(b[0], prec),
                fmt_coord(b[1], prec),
            )?;
        }
        DxfEntity::Polyline(e) => {
            let mut pts = String::new();
            for v in &e.vertices {
                let p = vp.model_to_paper([v.x, v.y]);
                if !pts.is_empty() {
                    pts.push(' ');
                }
                write!(pts, "{},{}", fmt_coord(p[0], prec), fmt_coord(p[1], prec))?;
            }
            let tag = if e.closed { "polygon" } else { "polyline" };
            writeln!(
                out,
                r#"    <{tag} points="{pts}" stroke="{stroke}" stroke-width="{lw}" fill="none"{dash_attr} />"#,
            )?;
        }
        DxfEntity::Circle(e) => {
            let c = vp.model_to_paper([e.center[0], e.center[1]]);
            let r = e.radius * vp.scale;
            writeln!(
                out,
                r#"    <circle cx="{}" cy="{}" r="{}" stroke="{stroke}" stroke-width="{lw}" fill="none"{dash_attr} />"#,
                fmt_coord(c[0], prec),
                fmt_coord(c[1], prec),
                fmt_coord(r, prec),
            )?;
        }
        DxfEntity::Arc(e) => {
            // Sample the arc as a polyline at 1° steps for fidelity.
            let segments = ((e.end_angle - e.start_angle).abs().ceil() as i32).max(8);
            let mut pts = String::new();
            for i in 0..=segments {
                let frac = i as f64 / segments as f64;
                let theta = (e.start_angle + (e.end_angle - e.start_angle) * frac).to_radians();
                let mx = e.center[0] + e.radius * theta.cos();
                let my = e.center[1] + e.radius * theta.sin();
                let paper = vp.model_to_paper([mx, my]);
                if !pts.is_empty() {
                    pts.push(' ');
                }
                write!(
                    pts,
                    "{},{}",
                    fmt_coord(paper[0], prec),
                    fmt_coord(paper[1], prec),
                )?;
            }
            writeln!(
                out,
                r#"    <polyline points="{pts}" stroke="{stroke}" stroke-width="{lw}" fill="none"{dash_attr} />"#,
            )?;
        }
        DxfEntity::Ellipse(e) => {
            // Sample the ellipse along start_param..end_param.
            let span = e.end_param - e.start_param;
            let segments = (span.abs().to_degrees().ceil() as i32).max(32);
            let mut pts = String::new();
            let mx_len = (e.major_axis[0].powi(2) + e.major_axis[1].powi(2)).sqrt();
            // Major-axis rotation about Z (DXF stores the axis vector;
            // we ignore Z because the renderer is 2D).
            let major_angle = e.major_axis[1].atan2(e.major_axis[0]);
            let minor_len = mx_len * e.ratio;
            for i in 0..=segments {
                let frac = i as f64 / segments as f64;
                let t = e.start_param + span * frac;
                // Untransformed ellipse point (parametric).
                let ux = mx_len * t.cos();
                let uy = minor_len * t.sin();
                // Rotate by major_angle, translate to center.
                let mx = e.center[0] + ux * major_angle.cos() - uy * major_angle.sin();
                let my = e.center[1] + ux * major_angle.sin() + uy * major_angle.cos();
                let paper = vp.model_to_paper([mx, my]);
                if !pts.is_empty() {
                    pts.push(' ');
                }
                write!(
                    pts,
                    "{},{}",
                    fmt_coord(paper[0], prec),
                    fmt_coord(paper[1], prec),
                )?;
            }
            // Use polygon if it's a full ellipse (span = 2π), polyline otherwise.
            let tag = if (span.abs() - std::f64::consts::TAU).abs() < 1e-6 {
                "polygon"
            } else {
                "polyline"
            };
            writeln!(
                out,
                r#"    <{tag} points="{pts}" stroke="{stroke}" stroke-width="{lw}" fill="none"{dash_attr} />"#,
            )?;
        }
        DxfEntity::Spline(e) => {
            // Sample the spline using De Boor's algorithm.
            let pts: Vec<[f64; 2]> = sample_spline(e);
            let mut buf = String::new();
            for p in &pts {
                let paper = vp.model_to_paper([p[0], p[1]]);
                if !buf.is_empty() {
                    buf.push(' ');
                }
                write!(
                    buf,
                    "{},{}",
                    fmt_coord(paper[0], prec),
                    fmt_coord(paper[1], prec),
                )?;
            }
            let tag = if e.closed { "polygon" } else { "polyline" };
            writeln!(
                out,
                r#"    <{tag} points="{buf}" stroke="{stroke}" stroke-width="{lw}" fill="none"{dash_attr} />"#,
            )?;
        }
        DxfEntity::Text(e) => {
            let p = vp.model_to_paper([e.position[0], e.position[1]]);
            let rot_attr = if e.rotation.abs() > 1e-9 {
                format!(
                    r#" transform="rotate({rot} {x} {y})""#,
                    rot = fmt_coord(-e.rotation, prec),
                    x = fmt_coord(p[0], prec),
                    y = fmt_coord(p[1], prec),
                )
            } else {
                String::new()
            };
            writeln!(
                out,
                r#"    <text x="{}" y="{}" font-size="{}" fill="{stroke}"{rot_attr}>{}</text>"#,
                fmt_coord(p[0], prec),
                fmt_coord(p[1], prec),
                e.height,
                escape_xml(&e.text),
            )?;
        }
        DxfEntity::Hatch(e) => {
            let fill = if e.solid {
                stroke.clone()
            } else {
                format!(
                    "url(#hatch-{})",
                    sanitize_id(&e.pattern_name.to_ascii_uppercase())
                )
            };
            for lp in &e.loops {
                let mut pts = String::new();
                for v in &lp.vertices {
                    let p = vp.model_to_paper([v[0], v[1]]);
                    if !pts.is_empty() {
                        pts.push(' ');
                    }
                    write!(pts, "{},{}", fmt_coord(p[0], prec), fmt_coord(p[1], prec))?;
                }
                writeln!(
                    out,
                    r#"    <polygon points="{pts}" fill="{fill}" color="{stroke}" stroke="none" />"#,
                )?;
            }
        }
        DxfEntity::Insert(e) => {
            // Block expansion. Three branches:
            //   1. Depth limit reached — emit a recursion-limit
            //      placeholder. Cyclic block definitions (A inserts
            //      B, B inserts A) would otherwise blow the stack.
            //   2. Block found — recurse into `emit_block_body` with
            //      the depth counter decremented.
            //   3. Block not in the table — emit the labelled
            //      diamond marker so the operator still sees where
            //      the symbol was placed.
            let p = vp.model_to_paper([e.position[0], e.position[1]]);
            if depth_remaining == 0 {
                writeln!(
                    out,
                    r#"    <g class="insert-recursion-limit" data-block="{name}" data-max-depth="{max_depth}"><circle cx="{x}" cy="{y}" r="1.5" stroke="{stroke}" stroke-width="{lw}" fill="none" /><text x="{xp2}" y="{y}" font-size="2" fill="{stroke}">[{name} ↻]</text></g>"#,
                    max_depth = BLOCK_EXPANSION_MAX_DEPTH,
                    name = escape_xml(&e.block_name),
                    x = fmt_coord(p[0], prec),
                    y = fmt_coord(p[1], prec),
                    xp2 = fmt_coord(p[0] + 2.0, prec),
                )?;
            } else if let Some(body) = blocks.get(&e.block_name) {
                emit_block_body(
                    out,
                    body,
                    vp,
                    plot_table,
                    layer_table,
                    dim_styles,
                    blocks,
                    opts,
                    &e.position,
                    &e.scale,
                    e.rotation,
                    depth_remaining - 1,
                )?;
            } else {
                // Diamond marker at insertion point + label.
                writeln!(
                    out,
                    r#"    <g class="insert-missing" data-block="{name}"><polyline points="{x},{ym} {xp},{y} {x},{yp} {xm},{y} {x},{ym}" stroke="{stroke}" stroke-width="{lw}" fill="none" /><text x="{xp2}" y="{y}" font-size="2" fill="{stroke}">[{name}]</text></g>"#,
                    name = escape_xml(&e.block_name),
                    x = fmt_coord(p[0], prec),
                    y = fmt_coord(p[1], prec),
                    xp = fmt_coord(p[0] + 1.0, prec),
                    xm = fmt_coord(p[0] - 1.0, prec),
                    yp = fmt_coord(p[1] + 1.0, prec),
                    ym = fmt_coord(p[1] - 1.0, prec),
                    xp2 = fmt_coord(p[0] + 2.0, prec),
                )?;
            }
        }
        DxfEntity::Dimension(d) => {
            emit_dimension(out, d, vp, dim_styles, &stroke, lw, prec, &dash_attr)?;
        }
        DxfEntity::Attdef(_) => {
            // ATTDEFs only render when an INSERT expands its block
            // body — the attribute lives inside the BLOCK record, not
            // at the top-level entity table. A bare top-level ATTDEF
            // has no expansion site so we skip it deliberately.
        }
    }
    Ok(())
}

/// Emit a single dimension entity. Covers all 5 `DxfDimensionKind`s.
#[allow(clippy::too_many_arguments)]
#[allow(clippy::many_single_char_names)]
fn emit_dimension(
    out: &mut String,
    d: &aec_cad::dxf::DxfDimension,
    vp: &SheetViewport,
    dim_styles: &DimStyleTable,
    stroke: &str,
    lw: f64,
    prec: u8,
    dash_attr: &str,
) -> Result<(), SvgExportError> {
    let ds = dim_styles.resolve(&d.style);
    let arrow_size = ds.arrow_size.max(0.5);
    let text_h = ds.text_height.max(0.5);

    writeln!(
        out,
        r#"    <g class="dimension" data-kind="{kind:?}" color="{stroke}">"#,
        kind = d.kind,
    )?;

    match d.kind {
        DxfDimensionKind::Linear | DxfDimensionKind::Aligned => {
            // Project the two definition points onto the dimension
            // line (passing through def_point in the direction of
            // (def_point_b - def_point_a) for Aligned, or the X/Y
            // axis for Linear depending on which is dominant).
            let a = [d.def_point_a[0], d.def_point_a[1]];
            let b = [d.def_point_b[0], d.def_point_b[1]];
            let p = [d.def_point[0], d.def_point[1]];

            let (axis_x, axis_y) = if matches!(d.kind, DxfDimensionKind::Aligned) {
                let dx = b[0] - a[0];
                let dy = b[1] - a[1];
                let len = (dx * dx + dy * dy).sqrt().max(1e-9);
                (dx / len, dy / len)
            } else if (b[0] - a[0]).abs() >= (b[1] - a[1]).abs() {
                (1.0, 0.0)
            } else {
                (0.0, 1.0)
            };

            // Project a/b onto the line through `p` parallel to axis.
            let project = |q: [f64; 2]| {
                let dx = q[0] - p[0];
                let dy = q[1] - p[1];
                let t = dx * axis_x + dy * axis_y;
                [p[0] + axis_x * t, p[1] + axis_y * t]
            };
            let pa = project(a);
            let pb = project(b);

            // Extension lines: from def_point_a/b out to pa/pb.
            let a_paper = vp.model_to_paper(a);
            let b_paper = vp.model_to_paper(b);
            let pa_paper = vp.model_to_paper(pa);
            let pb_paper = vp.model_to_paper(pb);

            writeln!(
                out,
                r#"      <line x1="{}" y1="{}" x2="{}" y2="{}" stroke="{stroke}" stroke-width="{lw}"{dash_attr} />"#,
                fmt_coord(a_paper[0], prec),
                fmt_coord(a_paper[1], prec),
                fmt_coord(pa_paper[0], prec),
                fmt_coord(pa_paper[1], prec),
            )?;
            writeln!(
                out,
                r#"      <line x1="{}" y1="{}" x2="{}" y2="{}" stroke="{stroke}" stroke-width="{lw}"{dash_attr} />"#,
                fmt_coord(b_paper[0], prec),
                fmt_coord(b_paper[1], prec),
                fmt_coord(pb_paper[0], prec),
                fmt_coord(pb_paper[1], prec),
            )?;

            // Dim line between pa and pb, with arrow markers at each end.
            writeln!(
                out,
                r#"      <line x1="{}" y1="{}" x2="{}" y2="{}" stroke="{stroke}" stroke-width="{lw}" marker-start="url(#dim-arrow)" marker-end="url(#dim-arrow)" />"#,
                fmt_coord(pa_paper[0], prec),
                fmt_coord(pa_paper[1], prec),
                fmt_coord(pb_paper[0], prec),
                fmt_coord(pb_paper[1], prec),
            )?;

            // Text label at midpoint.
            let measured = if let Some(value) = d.measured_value {
                value
            } else {
                let dx = b[0] - a[0];
                let dy = b[1] - a[1];
                if matches!(d.kind, DxfDimensionKind::Aligned) {
                    (dx * dx + dy * dy).sqrt()
                } else if axis_x.abs() > axis_y.abs() {
                    dx.abs()
                } else {
                    dy.abs()
                }
            };
            let label = d
                .override_text
                .clone()
                .unwrap_or_else(|| format!("{:.2}", measured * ds.units_scale));
            let mid = vp.model_to_paper([(pa[0] + pb[0]) * 0.5, (pa[1] + pb[1]) * 0.5]);
            writeln!(
                out,
                r#"      <text x="{x}" y="{y}" font-size="{text_h}" fill="{stroke}" text-anchor="middle" dy="-{dy:.3}">{label}</text>"#,
                x = fmt_coord(mid[0], prec),
                y = fmt_coord(mid[1], prec),
                dy = arrow_size,
                label = escape_xml(&label),
            )?;
        }
        DxfDimensionKind::Radial | DxfDimensionKind::Diameter => {
            // Leader from center → def_point_a (assumed on rim).
            let c = vp.model_to_paper([d.def_point[0], d.def_point[1]]);
            let r = vp.model_to_paper([d.def_point_a[0], d.def_point_a[1]]);
            writeln!(
                out,
                r#"      <line x1="{}" y1="{}" x2="{}" y2="{}" stroke="{stroke}" stroke-width="{lw}" marker-end="url(#dim-arrow)" />"#,
                fmt_coord(c[0], prec),
                fmt_coord(c[1], prec),
                fmt_coord(r[0], prec),
                fmt_coord(r[1], prec),
            )?;
            // Measured value = distance from center to rim.
            let mut measured = if let Some(value) = d.measured_value {
                value
            } else {
                let dx = d.def_point_a[0] - d.def_point[0];
                let dy = d.def_point_a[1] - d.def_point[1];
                (dx * dx + dy * dy).sqrt()
            };
            let prefix = if matches!(d.kind, DxfDimensionKind::Diameter) {
                measured *= 2.0;
                "\u{00f8}"
            } else {
                "R"
            };
            let label = d
                .override_text
                .clone()
                .unwrap_or_else(|| format!("{prefix}{:.2}", measured * ds.units_scale));
            let text_pos = vp.model_to_paper([d.text_position[0], d.text_position[1]]);
            writeln!(
                out,
                r#"      <text x="{}" y="{}" font-size="{text_h}" fill="{stroke}">{}</text>"#,
                fmt_coord(text_pos[0], prec),
                fmt_coord(text_pos[1], prec),
                escape_xml(&label),
            )?;
        }
        DxfDimensionKind::Angular => {
            // Two arms from def_point to def_point_a / def_point_b,
            // plus an arc between them.
            let v = [d.def_point[0], d.def_point[1]];
            let a = [d.def_point_a[0], d.def_point_a[1]];
            let b = [d.def_point_b[0], d.def_point_b[1]];
            let v_paper = vp.model_to_paper(v);
            let a_paper = vp.model_to_paper(a);
            let b_paper = vp.model_to_paper(b);

            writeln!(
                out,
                r#"      <line x1="{}" y1="{}" x2="{}" y2="{}" stroke="{stroke}" stroke-width="{lw}"{dash_attr} />"#,
                fmt_coord(v_paper[0], prec),
                fmt_coord(v_paper[1], prec),
                fmt_coord(a_paper[0], prec),
                fmt_coord(a_paper[1], prec),
            )?;
            writeln!(
                out,
                r#"      <line x1="{}" y1="{}" x2="{}" y2="{}" stroke="{stroke}" stroke-width="{lw}"{dash_attr} />"#,
                fmt_coord(v_paper[0], prec),
                fmt_coord(v_paper[1], prec),
                fmt_coord(b_paper[0], prec),
                fmt_coord(b_paper[1], prec),
            )?;

            let ang_a = (a[1] - v[1]).atan2(a[0] - v[0]);
            let ang_b = (b[1] - v[1]).atan2(b[0] - v[0]);
            let arc_r = {
                let ra = ((a[0] - v[0]).powi(2) + (a[1] - v[1]).powi(2)).sqrt();
                let rb = ((b[0] - v[0]).powi(2) + (b[1] - v[1]).powi(2)).sqrt();
                ra.min(rb).max(arrow_size * 2.0) * 0.7
            };
            // Sample the arc as a polyline.
            let segments = 32;
            let mut pts = String::new();
            let mut span = ang_b - ang_a;
            // Normalise to the shorter arc, [-π, π].
            while span > std::f64::consts::PI {
                span -= std::f64::consts::TAU;
            }
            while span < -std::f64::consts::PI {
                span += std::f64::consts::TAU;
            }
            for i in 0..=segments {
                let t = i as f64 / segments as f64;
                let theta = ang_a + span * t;
                let mx = v[0] + arc_r * theta.cos();
                let my = v[1] + arc_r * theta.sin();
                let paper = vp.model_to_paper([mx, my]);
                if !pts.is_empty() {
                    pts.push(' ');
                }
                write!(
                    pts,
                    "{},{}",
                    fmt_coord(paper[0], prec),
                    fmt_coord(paper[1], prec),
                )?;
            }
            writeln!(
                out,
                r#"      <polyline points="{pts}" stroke="{stroke}" stroke-width="{lw}" fill="none" marker-start="url(#dim-arrow)" marker-end="url(#dim-arrow)" />"#,
            )?;
            // Label at the arc midpoint.
            let mid_angle = ang_a + span * 0.5;
            let mid_r = arc_r + arrow_size;
            let mid = vp.model_to_paper([
                v[0] + mid_r * mid_angle.cos(),
                v[1] + mid_r * mid_angle.sin(),
            ]);
            let deg = d.measured_value.unwrap_or_else(|| span.abs().to_degrees());
            let label = d
                .override_text
                .clone()
                .unwrap_or_else(|| format!("{:.1}\u{00b0}", deg));
            writeln!(
                out,
                r#"      <text x="{}" y="{}" font-size="{text_h}" fill="{stroke}" text-anchor="middle">{}</text>"#,
                fmt_coord(mid[0], prec),
                fmt_coord(mid[1], prec),
                escape_xml(&label),
            )?;
        }
    }

    writeln!(out, "    </g>")?;
    Ok(())
}

/// Emit a block body at the given insert position/scale/rotation.
///
/// `depth_remaining` is decremented at every nested INSERT;
/// `emit_entity` renders a recursion-limit placeholder when it hits
/// zero so cyclic block definitions can't blow the stack.
#[allow(clippy::too_many_arguments)]
fn emit_block_body(
    out: &mut String,
    body: &[DxfEntity],
    vp: &SheetViewport,
    plot_table: &PlotStyleTable,
    layer_table: &LayerTable,
    dim_styles: &DimStyleTable,
    blocks: &BlockTable,
    opts: &SvgExportOptions,
    insert_pos: &[f64; 3],
    insert_scale: &[f64; 3],
    insert_rot_deg: f64,
    depth_remaining: u32,
) -> Result<(), SvgExportError> {
    // Apply the insertion transform to every entity in the block,
    // then delegate to `emit_entity`. We transform the entity in
    // model space (before model_to_paper).
    let xform = BlockTransform::new(*insert_pos, *insert_scale, insert_rot_deg);
    for e in body {
        let transformed = transform_entity(e, &xform);
        emit_entity(
            out,
            &transformed,
            vp,
            plot_table,
            layer_table,
            dim_styles,
            blocks,
            opts,
            depth_remaining,
        )?;
    }
    Ok(())
}

/// Apply an insert-transform to an entity. Translation, scale, and
/// rotation all flow through `BlockTransform`, which provides:
///
/// - `point3` / `point2`: scale → rotate → translate (positions),
/// - `vec3`: scale + rotate, no translation (direction vectors such
///   as the ellipse `major_axis`),
/// - `uniform_xy_scale`: scalar multiplier for radii (Circle, Arc),
/// - `text_height_scale`: scalar multiplier for text heights
///   (matches AutoCAD's Y-scale convention),
/// - `rot_deg`: degree offset added to entity-stored angles.
///
/// Without these, radii, text heights, ellipse axes, and stored
/// rotation angles would all stay at their authored values when an
/// INSERT was rotated or scaled — producing wrong-size geometry.
fn transform_entity(entity: &DxfEntity, x: &BlockTransform) -> DxfEntity {
    let rot = x.rot_deg();
    let uniform = x.uniform_xy_scale();
    let h_scale = x.text_height_scale();
    match entity {
        DxfEntity::Line(e) => DxfEntity::Line(aec_cad::dxf::DxfLine {
            layer: e.layer.clone(),
            start: x.point3(&e.start),
            end: x.point3(&e.end),
        }),
        DxfEntity::Polyline(e) => {
            let new_v: Vec<_> = e
                .vertices
                .iter()
                .map(|v| {
                    let p = x.point2(&[v.x, v.y]);
                    aec_cad::dxf::DxfPolylineVertex {
                        x: p[0],
                        y: p[1],
                        bulge: v.bulge,
                    }
                })
                .collect();
            DxfEntity::Polyline(aec_cad::dxf::DxfPolyline {
                layer: e.layer.clone(),
                vertices: new_v,
                closed: e.closed,
                elevation: e.elevation,
            })
        }
        DxfEntity::Arc(e) => DxfEntity::Arc(aec_cad::dxf::DxfArc {
            layer: e.layer.clone(),
            center: x.point3(&e.center),
            radius: e.radius * uniform,
            start_angle: e.start_angle + rot,
            end_angle: e.end_angle + rot,
        }),
        DxfEntity::Circle(e) => DxfEntity::Circle(aec_cad::dxf::DxfCircle {
            layer: e.layer.clone(),
            center: x.point3(&e.center),
            radius: e.radius * uniform,
        }),
        DxfEntity::Ellipse(e) => DxfEntity::Ellipse(aec_cad::dxf::DxfEllipse {
            layer: e.layer.clone(),
            center: x.point3(&e.center),
            // major_axis is a direction-and-length vector — must be
            // scaled and rotated (but NOT translated). The ellipse
            // renderer reads its length and atan2 to size & orient
            // the curve.
            major_axis: x.vec3(&e.major_axis),
            ratio: e.ratio,
            start_param: e.start_param,
            end_param: e.end_param,
        }),
        DxfEntity::Spline(e) => DxfEntity::Spline(aec_cad::dxf::DxfSpline {
            layer: e.layer.clone(),
            degree: e.degree,
            knots: e.knots.clone(),
            control_points: e.control_points.iter().map(|c| x.point3(c)).collect(),
            closed: e.closed,
        }),
        DxfEntity::Hatch(e) => {
            let new_loops: Vec<_> = e
                .loops
                .iter()
                .map(|l| aec_cad::dxf::DxfHatchLoop {
                    vertices: l.vertices.iter().map(|v| x.point2(v)).collect(),
                })
                .collect();
            DxfEntity::Hatch(aec_cad::dxf::DxfHatch {
                layer: e.layer.clone(),
                pattern_name: e.pattern_name.clone(),
                solid: e.solid,
                scale: e.scale,
                angle: e.angle,
                elevation: e.elevation,
                loops: new_loops,
            })
        }
        DxfEntity::Text(e) => DxfEntity::Text(aec_cad::dxf::DxfText {
            layer: e.layer.clone(),
            position: x.point3(&e.position),
            // Text height scales with the block's Y-scale and rotation
            // composes additively with the insert's rotation.
            height: e.height * h_scale,
            rotation: e.rotation + rot,
            text: e.text.clone(),
        }),
        DxfEntity::Insert(e) => DxfEntity::Insert(aec_cad::dxf::DxfInsert {
            layer: e.layer.clone(),
            block_name: e.block_name.clone(),
            position: x.point3(&e.position),
            // Nested-insert scale composes multiplicatively;
            // rotation composes additively.
            scale: [
                e.scale[0] * x.scale[0],
                e.scale[1] * x.scale[1],
                e.scale[2] * x.scale[2],
            ],
            rotation: e.rotation + rot,
        }),
        DxfEntity::Dimension(e) => DxfEntity::Dimension(aec_cad::dxf::DxfDimension {
            layer: e.layer.clone(),
            style: e.style.clone(),
            kind: e.kind,
            def_point: x.point3(&e.def_point),
            text_position: x.point3(&e.text_position),
            def_point_a: x.point3(&e.def_point_a),
            def_point_b: x.point3(&e.def_point_b),
            override_text: e.override_text.clone(),
            // measured_value reflects the dimensioned distance in
            // model space; under a scaled insert the visible distance
            // between def_point_a/b scales with `uniform_xy_scale`,
            // so the cached measurement must scale to match.
            measured_value: e.measured_value.map(|m| m * uniform),
        }),
        DxfEntity::Attdef(e) => DxfEntity::Attdef(aec_cad::dxf::DxfAttdef {
            layer: e.layer.clone(),
            position: x.point3(&e.position),
            height: e.height * h_scale,
            rotation: e.rotation + rot,
            default_value: e.default_value.clone(),
            tag: e.tag.clone(),
            prompt: e.prompt.clone(),
            flags: e.flags,
            text_style: e.text_style.clone(),
        }),
    }
}

/// De Boor's algorithm: sample a B-spline at `n` evenly-spaced
/// parameter values across the valid range `[knots[degree],
/// knots[len-degree-1]]`. Falls back to the control polygon when the
/// spline data is invalid (degree 0, fewer control points than
/// degree+1, malformed knots).
fn sample_spline(s: &aec_cad::dxf::DxfSpline) -> Vec<[f64; 2]> {
    let n_ctrl = s.control_points.len();
    let p = s.degree.max(0) as usize;
    let n_samples = 64;

    if p == 0 || n_ctrl < p + 1 || s.knots.len() < n_ctrl + p + 1 {
        // Fall back to the control polygon as a polyline.
        return s.control_points.iter().map(|c| [c[0], c[1]]).collect();
    }

    let t_lo = s.knots[p];
    let t_hi = s.knots[n_ctrl];
    if !t_hi.is_finite() || !t_lo.is_finite() || (t_hi - t_lo).abs() < 1e-9 {
        return s.control_points.iter().map(|c| [c[0], c[1]]).collect();
    }

    let mut samples = Vec::with_capacity(n_samples + 1);
    for i in 0..=n_samples {
        let t = t_lo + (t_hi - t_lo) * (i as f64 / n_samples as f64);
        samples.push(de_boor(p, &s.knots, &s.control_points, t));
    }
    samples
}

/// Evaluate a B-spline at parameter t using De Boor's recursive
/// algorithm. Returns the (x, y) projection — the input is `[f64; 3]`
/// but we only care about XY for SVG output.
fn de_boor(p: usize, knots: &[f64], ctrl: &[[f64; 3]], t: f64) -> [f64; 2] {
    let n_ctrl = ctrl.len();
    // Find the knot span `k` such that knots[k] <= t < knots[k+1].
    let mut k = p;
    for i in p..n_ctrl {
        if t >= knots[i] && t < knots[i + 1] {
            k = i;
            break;
        }
        if i == n_ctrl - 1 {
            k = i;
        }
    }

    // Working array of degree+1 control points around index k.
    let mut d: Vec<[f64; 2]> = (0..=p)
        .map(|j| {
            let idx = (k + j).saturating_sub(p);
            [ctrl[idx][0], ctrl[idx][1]]
        })
        .collect();

    for r in 1..=p {
        for j in (r..=p).rev() {
            let idx_left = k + j - p;
            let idx_right = k + 1 + j - r;
            let denom = knots[idx_right] - knots[idx_left];
            if denom.abs() < 1e-12 {
                continue;
            }
            let alpha = (t - knots[idx_left]) / denom;
            d[j] = [
                (1.0 - alpha) * d[j - 1][0] + alpha * d[j][0],
                (1.0 - alpha) * d[j - 1][1] + alpha * d[j][1],
            ];
        }
    }

    d[p]
}

/// Render a title block (border + populated fields) on the sheet.
fn write_title_block(
    out: &mut String,
    sheet: &Sheet,
    title: &aec_cad::sheets::TitleBlock,
    opts: &SvgExportOptions,
) -> Result<(), SvgExportError> {
    let (w, h) = sheet.dimensions_mm();
    let [bx, by] = title.border_offset;
    let prec = opts.coordinate_precision;
    writeln!(out, r#"  <g class="title-block">"#)?;
    writeln!(
        out,
        r#"    <rect x="{}" y="{}" width="{}" height="{}" fill="none" stroke="black" stroke-width="0.3" />"#,
        fmt_coord(bx, prec),
        fmt_coord(by, prec),
        fmt_coord((w - 2.0 * bx).max(0.0), prec),
        fmt_coord((h - 2.0 * by).max(0.0), prec),
    )?;
    for (field, value) in title.populated_fields() {
        writeln!(
            out,
            r#"    <text x="{}" y="{}" font-size="{}" fill="black">{}: {}</text>"#,
            fmt_coord(field.position[0], prec),
            fmt_coord(field.position[1], prec),
            field.height,
            escape_xml(&field.label),
            escape_xml(value),
        )?;
    }
    writeln!(out, "  </g>")?;
    Ok(())
}

fn escape_xml(s: &str) -> String {
    s.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
        .replace('\'', "&apos;")
}

/// Convenience: convert a `PaperSize` to a tuple for callers who want
/// raw mm (helps mock sheets in tests without depending on aec_cad).
pub fn paper_size_dimensions_mm(p: PaperSize) -> (f64, f64) {
    p.dimensions_mm()
}

#[cfg(test)]
mod tests {
    use super::*;
    use aec_cad::dxf::{
        DxfArc, DxfCircle, DxfDimStyle, DxfDimension, DxfDimensionKind, DxfEllipse, DxfHatch,
        DxfHatchLoop, DxfInsert, DxfLine, DxfPolyline, DxfPolylineVertex, DxfSpline, DxfText,
    };
    use aec_cad::layers::{Layer, LayerColor, LayerLineweight};
    use aec_cad::sheets::{PaperSize, Sheet, SheetViewport, TitleBlock};

    fn make_sheet() -> Sheet {
        let mut s = Sheet::new("A1", PaperSize::IsoA3);
        s.viewports
            .push(SheetViewport::new("VP1", [10.0, 10.0], [200.0, 100.0]));
        s
    }

    fn make_layer(name: &str, aci: u8, visible: bool, linetype: &str) -> Layer {
        let mut l = Layer::new(name).unwrap();
        l.color = LayerColor(aci as i16);
        l.linetype = linetype.into();
        l.lineweight = LayerLineweight::from_mm(0.25);
        l.on = visible;
        l
    }

    fn count_tags(svg: &str, tag: &str) -> usize {
        let needle = format!("<{tag}");
        svg.matches(&needle).count()
    }

    #[test]
    fn empty_entities_produces_valid_svg() {
        let s = make_sheet();
        let svg = render_sheet_svg(
            &s,
            &[],
            &PlotStyleTable::monochrome(),
            &SvgExportOptions::default(),
        )
        .unwrap();
        assert!(svg.starts_with("<?xml"));
        assert!(svg.contains("<svg"));
        assert!(svg.contains("</svg>"));
    }

    #[test]
    fn deterministic_output_for_same_input() {
        let s = make_sheet();
        let entities = vec![DxfEntity::Line(DxfLine {
            layer: "WALLS".into(),
            start: [0.0, 0.0, 0.0],
            end: [100.0, 50.0, 0.0],
        })];
        let table = PlotStyleTable::monochrome();
        let opts = SvgExportOptions::default();
        let a = render_sheet_svg(&s, &entities, &table, &opts).unwrap();
        let b = render_sheet_svg(&s, &entities, &table, &opts).unwrap();
        assert_eq!(a, b);
    }

    #[test]
    fn closed_polyline_uses_polygon_element() {
        let s = make_sheet();
        let entities = vec![DxfEntity::Polyline(DxfPolyline {
            layer: "WALLS".into(),
            vertices: vec![
                DxfPolylineVertex::new(0.0, 0.0),
                DxfPolylineVertex::new(10.0, 0.0),
                DxfPolylineVertex::new(10.0, 5.0),
                DxfPolylineVertex::new(0.0, 5.0),
            ],
            closed: true,
            elevation: 0.0,
        })];
        let svg = render_sheet_svg(
            &s,
            &entities,
            &PlotStyleTable::monochrome(),
            &SvgExportOptions::default(),
        )
        .unwrap();
        assert!(svg.contains("<polygon"));
    }

    #[test]
    fn open_polyline_uses_polyline_element() {
        let s = make_sheet();
        let entities = vec![DxfEntity::Polyline(DxfPolyline {
            layer: "WALLS".into(),
            vertices: vec![
                DxfPolylineVertex::new(0.0, 0.0),
                DxfPolylineVertex::new(10.0, 0.0),
            ],
            closed: false,
            elevation: 0.0,
        })];
        let svg = render_sheet_svg(
            &s,
            &entities,
            &PlotStyleTable::monochrome(),
            &SvgExportOptions::default(),
        )
        .unwrap();
        assert!(svg.contains("<polyline"));
        assert!(!svg.contains("<polygon"));
    }

    #[test]
    fn text_is_xml_escaped() {
        let s = make_sheet();
        let entities = vec![DxfEntity::Text(DxfText {
            layer: "TEXT".into(),
            position: [0.0, 0.0, 0.0],
            height: 2.5,
            rotation: 0.0,
            text: "A < B & C".into(),
        })];
        let svg = render_sheet_svg(
            &s,
            &entities,
            &PlotStyleTable::monochrome(),
            &SvgExportOptions::default(),
        )
        .unwrap();
        assert!(svg.contains("A &lt; B &amp; C"));
    }

    #[test]
    fn paper_size_dimensions_mm_matches_iso_a3() {
        let (w, h) = paper_size_dimensions_mm(PaperSize::IsoA3);
        // ISO A3 portrait: 297 × 420 mm.
        assert!((w - 297.0).abs() < 1e-6);
        assert!((h - 420.0).abs() < 1e-6);
    }

    #[test]
    fn linetype_from_name_resolves_well_known_dxf_names() {
        assert_eq!(
            SvgLinetype::from_name("CONTINUOUS"),
            SvgLinetype::Continuous
        );
        assert_eq!(
            SvgLinetype::from_name("Continuous"),
            SvgLinetype::Continuous
        );
        assert_eq!(SvgLinetype::from_name("ByLayer"), SvgLinetype::Continuous);
        assert_eq!(SvgLinetype::from_name("BYBLOCK"), SvgLinetype::Continuous);
        assert_eq!(SvgLinetype::from_name(""), SvgLinetype::Continuous);
        assert_eq!(SvgLinetype::from_name("DASHED"), SvgLinetype::Dashed);
        assert_eq!(SvgLinetype::from_name("hidden"), SvgLinetype::Hidden);
        assert_eq!(SvgLinetype::from_name("CENTER"), SvgLinetype::Center);
        assert_eq!(SvgLinetype::from_name("PHANTOM"), SvgLinetype::Phantom);
        assert_eq!(SvgLinetype::from_name("DASHDOT"), SvgLinetype::DashDot);
        assert_eq!(SvgLinetype::from_name("DIVIDE"), SvgLinetype::Divide);
        assert_eq!(SvgLinetype::from_name("garbage"), SvgLinetype::Continuous);
    }

    #[test]
    fn linetype_dasharray_continuous_has_no_attribute() {
        assert!(SvgLinetype::Continuous.dasharray_mm().is_none());
        for lt in [
            SvgLinetype::Dashed,
            SvgLinetype::Hidden,
            SvgLinetype::Center,
            SvgLinetype::Phantom,
            SvgLinetype::DashDot,
            SvgLinetype::Divide,
        ] {
            assert!(lt.dasharray_mm().is_some(), "{lt:?} missing dasharray");
            assert!(!lt.dasharray_mm().unwrap().is_empty());
        }
    }

    #[test]
    fn layer_table_lookup_is_case_insensitive() {
        let mut t = LayerTable::new();
        t.insert(make_layer("WALLS", 7, true, "CONTINUOUS"));
        assert!(t.get("walls").is_some());
        assert!(t.get("WALLS").is_some());
        assert!(t.get("Walls").is_some());
        assert_eq!(t.len(), 1);
        assert!(!t.is_empty());
    }

    #[test]
    fn frozen_layer_entity_is_skipped() {
        let s = make_sheet();
        let entities = vec![DxfEntity::Line(DxfLine {
            layer: "HIDDEN-WALLS".into(),
            start: [0.0, 0.0, 0.0],
            end: [10.0, 10.0, 0.0],
        })];
        let layers = vec![
            // Layer marked frozen — should be skipped.
            {
                let mut l = make_layer("HIDDEN-WALLS", 7, true, "CONTINUOUS");
                l.frozen = true;
                l
            },
        ];
        let lt = LayerTable::from_slice(&layers);
        let svg = render_sheet_svg_full(
            &s,
            &entities,
            &PlotStyleTable::monochrome(),
            &lt,
            &DimStyleTable::new(),
            &BlockTable::new(),
            &SvgExportOptions::default(),
        )
        .unwrap();
        assert!(!svg.contains("<line x1=\"0"));
    }

    #[test]
    fn off_layer_entity_is_skipped() {
        let s = make_sheet();
        let entities = vec![DxfEntity::Line(DxfLine {
            layer: "OFF-LAYER".into(),
            start: [0.0, 0.0, 0.0],
            end: [10.0, 10.0, 0.0],
        })];
        let layers = vec![make_layer("OFF-LAYER", 7, false, "CONTINUOUS")];
        let lt = LayerTable::from_slice(&layers);
        let svg = render_sheet_svg_full(
            &s,
            &entities,
            &PlotStyleTable::monochrome(),
            &lt,
            &DimStyleTable::new(),
            &BlockTable::new(),
            &SvgExportOptions::default(),
        )
        .unwrap();
        assert!(!svg.contains("<line x1=\"0"));
    }

    #[test]
    fn per_viewport_frozen_layer_is_skipped_in_that_viewport_only() {
        let mut s = Sheet::new("A1", PaperSize::IsoA3);
        let mut vp_normal = SheetViewport::new("VP1", [10.0, 10.0], [200.0, 100.0]);
        let mut vp_frozen = SheetViewport::new("VP2", [10.0, 120.0], [200.0, 100.0]);
        vp_normal.scale = 1.0 / 50.0;
        vp_frozen.scale = 1.0 / 50.0;
        vp_frozen.frozen_layers.push("WALLS".into());
        s.viewports = vec![vp_normal, vp_frozen];
        let entities = vec![DxfEntity::Line(DxfLine {
            layer: "WALLS".into(),
            start: [0.0, 0.0, 0.0],
            end: [10.0, 0.0, 0.0],
        })];
        let svg = render_sheet_svg_full(
            &s,
            &entities,
            &PlotStyleTable::monochrome(),
            &LayerTable::new(),
            &DimStyleTable::new(),
            &BlockTable::new(),
            &SvgExportOptions::default(),
        )
        .unwrap();
        // VP1 group contains a <line>, VP2 group does not.
        let vp1_block = svg
            .split(r#"<g class="viewport" id="vp-0""#)
            .nth(1)
            .unwrap()
            .split("</g>")
            .next()
            .unwrap();
        assert!(vp1_block.contains("<line"));
        let vp2_block = svg
            .split(r#"<g class="viewport" id="vp-1""#)
            .nth(1)
            .unwrap()
            .split("</g>")
            .next()
            .unwrap();
        assert!(!vp2_block.contains("<line"));
    }

    #[test]
    fn dashed_layer_linetype_produces_stroke_dasharray() {
        let s = make_sheet();
        let entities = vec![DxfEntity::Line(DxfLine {
            layer: "DASHED-LINES".into(),
            start: [0.0, 0.0, 0.0],
            end: [10.0, 10.0, 0.0],
        })];
        let lt = LayerTable::from_slice(&[make_layer("DASHED-LINES", 1, true, "DASHED")]);
        let svg = render_sheet_svg_full(
            &s,
            &entities,
            &PlotStyleTable::monochrome(),
            &lt,
            &DimStyleTable::new(),
            &BlockTable::new(),
            &SvgExportOptions::default(),
        )
        .unwrap();
        assert!(svg.contains("stroke-dasharray=\"4,2\""));
    }

    #[test]
    fn ellipse_is_rendered_as_polyline_or_polygon() {
        let s = make_sheet();
        let entities = vec![DxfEntity::Ellipse(DxfEllipse {
            layer: "WALLS".into(),
            center: [50.0, 25.0, 0.0],
            major_axis: [20.0, 0.0, 0.0],
            ratio: 0.5,
            start_param: 0.0,
            end_param: std::f64::consts::TAU,
        })];
        let svg = render_sheet_svg(
            &s,
            &entities,
            &PlotStyleTable::monochrome(),
            &SvgExportOptions::default(),
        )
        .unwrap();
        // Full ellipse => polygon (we emit polygon for full revolution).
        assert!(svg.contains("<polygon"));
    }

    #[test]
    fn partial_ellipse_arc_is_rendered_as_polyline() {
        let s = make_sheet();
        let entities = vec![DxfEntity::Ellipse(DxfEllipse {
            layer: "WALLS".into(),
            center: [50.0, 25.0, 0.0],
            major_axis: [20.0, 0.0, 0.0],
            ratio: 0.5,
            start_param: 0.0,
            end_param: std::f64::consts::PI,
        })];
        let svg = render_sheet_svg(
            &s,
            &entities,
            &PlotStyleTable::monochrome(),
            &SvgExportOptions::default(),
        )
        .unwrap();
        // Half ellipse => polyline (not full revolution).
        assert!(svg.contains("<polyline"));
    }

    #[test]
    fn spline_falls_back_to_control_polygon_when_invalid() {
        // Degree 0 spline; should emit control polygon as polyline.
        let s = make_sheet();
        let entities = vec![DxfEntity::Spline(DxfSpline {
            layer: "WALLS".into(),
            degree: 0,
            knots: vec![],
            control_points: vec![[0.0, 0.0, 0.0], [10.0, 0.0, 0.0], [10.0, 10.0, 0.0]],
            closed: false,
        })];
        let svg = render_sheet_svg(
            &s,
            &entities,
            &PlotStyleTable::monochrome(),
            &SvgExportOptions::default(),
        )
        .unwrap();
        assert!(svg.contains("<polyline"));
    }

    #[test]
    fn cubic_spline_with_valid_knot_vector_samples_curve() {
        // Open uniform cubic B-spline through 4 control points.
        // Knot vector: [0,0,0,0, 1,1,1,1] (cubic Bezier-equivalent).
        let s = make_sheet();
        let entities = vec![DxfEntity::Spline(DxfSpline {
            layer: "WALLS".into(),
            degree: 3,
            knots: vec![0.0, 0.0, 0.0, 0.0, 1.0, 1.0, 1.0, 1.0],
            control_points: vec![
                [0.0, 0.0, 0.0],
                [10.0, 30.0, 0.0],
                [30.0, 30.0, 0.0],
                [40.0, 0.0, 0.0],
            ],
            closed: false,
        })];
        let svg = render_sheet_svg(
            &s,
            &entities,
            &PlotStyleTable::monochrome(),
            &SvgExportOptions::default(),
        )
        .unwrap();
        assert!(svg.contains("<polyline"));
        // 65 sampled points → 64 spaces.
        let polyline_block = svg.split("<polyline").nth(1).unwrap();
        assert!(polyline_block.matches(',').count() >= 60);
    }

    #[test]
    fn linear_dimension_emits_extension_lines_dim_line_text() {
        let s = make_sheet();
        let entities = vec![DxfEntity::Dimension(DxfDimension {
            layer: "DIMS".into(),
            style: "STANDARD".into(),
            kind: DxfDimensionKind::Linear,
            def_point: [0.0, 20.0, 0.0],
            text_position: [50.0, 21.0, 0.0],
            def_point_a: [0.0, 0.0, 0.0],
            def_point_b: [100.0, 0.0, 0.0],
            override_text: None,
            measured_value: None,
        })];
        let svg = render_sheet_svg(
            &s,
            &entities,
            &PlotStyleTable::monochrome(),
            &SvgExportOptions::default(),
        )
        .unwrap();
        // Two extension lines + one dim line = at least 3 <line> tags
        // inside the dimension group.
        let dim_block = svg
            .split(r#"<g class="dimension""#)
            .nth(1)
            .unwrap()
            .split("</g>")
            .next()
            .unwrap();
        assert!(count_tags(dim_block, "line") >= 3);
        assert!(dim_block.contains("<text"));
        // No override → numeric label.
        assert!(dim_block.contains("100"));
    }

    #[test]
    fn radial_dimension_uses_r_prefix() {
        let s = make_sheet();
        let entities = vec![DxfEntity::Dimension(DxfDimension {
            layer: "DIMS".into(),
            style: "STANDARD".into(),
            kind: DxfDimensionKind::Radial,
            def_point: [0.0, 0.0, 0.0],
            text_position: [10.0, 0.0, 0.0],
            def_point_a: [5.0, 0.0, 0.0],
            def_point_b: [0.0, 0.0, 0.0],
            override_text: None,
            measured_value: None,
        })];
        let svg = render_sheet_svg(
            &s,
            &entities,
            &PlotStyleTable::monochrome(),
            &SvgExportOptions::default(),
        )
        .unwrap();
        assert!(svg.contains(">R5"));
    }

    #[test]
    fn diameter_dimension_uses_oslash_prefix() {
        let s = make_sheet();
        let entities = vec![DxfEntity::Dimension(DxfDimension {
            layer: "DIMS".into(),
            style: "STANDARD".into(),
            kind: DxfDimensionKind::Diameter,
            def_point: [0.0, 0.0, 0.0],
            text_position: [10.0, 0.0, 0.0],
            def_point_a: [5.0, 0.0, 0.0],
            def_point_b: [0.0, 0.0, 0.0],
            override_text: None,
            measured_value: None,
        })];
        let svg = render_sheet_svg(
            &s,
            &entities,
            &PlotStyleTable::monochrome(),
            &SvgExportOptions::default(),
        )
        .unwrap();
        // Measured = 5 then doubled to 10 for diameter; Ø prefix.
        assert!(svg.contains("\u{00f8}10"));
    }

    #[test]
    fn angular_dimension_emits_two_arms_and_an_arc() {
        let s = make_sheet();
        let entities = vec![DxfEntity::Dimension(DxfDimension {
            layer: "DIMS".into(),
            style: "STANDARD".into(),
            kind: DxfDimensionKind::Angular,
            def_point: [0.0, 0.0, 0.0],
            text_position: [3.0, 3.0, 0.0],
            def_point_a: [10.0, 0.0, 0.0],
            def_point_b: [0.0, 10.0, 0.0],
            override_text: None,
            measured_value: None,
        })];
        let svg = render_sheet_svg(
            &s,
            &entities,
            &PlotStyleTable::monochrome(),
            &SvgExportOptions::default(),
        )
        .unwrap();
        let dim_block = svg
            .split(r#"<g class="dimension""#)
            .nth(1)
            .unwrap()
            .split("</g>")
            .next()
            .unwrap();
        assert!(count_tags(dim_block, "line") >= 2);
        assert!(dim_block.contains("<polyline"));
        // 90° angle.
        assert!(dim_block.contains("90"));
    }

    #[test]
    fn insert_without_matching_block_renders_placeholder() {
        let s = make_sheet();
        let entities = vec![DxfEntity::Insert(DxfInsert {
            layer: "WALLS".into(),
            block_name: "MISSING".into(),
            position: [50.0, 25.0, 0.0],
            scale: [1.0, 1.0, 1.0],
            rotation: 0.0,
        })];
        let svg = render_sheet_svg(
            &s,
            &entities,
            &PlotStyleTable::monochrome(),
            &SvgExportOptions::default(),
        )
        .unwrap();
        assert!(svg.contains(r#"class="insert-missing""#));
        assert!(svg.contains("[MISSING]"));
    }

    #[test]
    fn insert_with_matching_block_expands_body_with_transform() {
        let mut blocks = BlockTable::new();
        blocks.insert(
            "BOX",
            vec![DxfEntity::Polyline(DxfPolyline {
                layer: "WALLS".into(),
                vertices: vec![
                    DxfPolylineVertex::new(0.0, 0.0),
                    DxfPolylineVertex::new(10.0, 0.0),
                    DxfPolylineVertex::new(10.0, 5.0),
                    DxfPolylineVertex::new(0.0, 5.0),
                ],
                closed: true,
                elevation: 0.0,
            })],
        );
        let s = make_sheet();
        let entities = vec![DxfEntity::Insert(DxfInsert {
            layer: "WALLS".into(),
            block_name: "BOX".into(),
            position: [50.0, 25.0, 0.0],
            scale: [2.0, 2.0, 1.0],
            rotation: 0.0,
        })];
        let svg = render_sheet_svg_full(
            &s,
            &entities,
            &PlotStyleTable::monochrome(),
            &LayerTable::new(),
            &DimStyleTable::new(),
            &blocks,
            &SvgExportOptions::default(),
        )
        .unwrap();
        assert!(!svg.contains("insert-missing"));
        // The expanded block is a closed polyline → polygon.
        assert!(svg.contains("<polygon"));
    }

    #[test]
    fn block_table_lookup_is_case_insensitive() {
        // DXF block names are case-insensitive per AutoCAD convention.
        // An INSERT referencing "window" must match a block stored as
        // "WINDOW" (and vice versa).
        let mut blocks = BlockTable::new();
        blocks.insert(
            "WINDOW",
            vec![DxfEntity::Line(DxfLine {
                layer: "WALLS".into(),
                start: [0.0, 0.0, 0.0],
                end: [1.0, 0.0, 0.0],
            })],
        );
        // get() must hit on every case variant.
        assert!(blocks.get("WINDOW").is_some());
        assert!(blocks.get("window").is_some());
        assert!(blocks.get("Window").is_some());
        assert!(blocks.get("wIndOW").is_some());
        // And miss when the name truly doesn't match.
        assert!(blocks.get("door").is_none());
    }

    #[test]
    fn insert_referencing_block_with_different_case_expands_body() {
        // The exporter must expand the block body (not render the
        // missing-block placeholder) when the INSERT and block name
        // differ only in casing.
        let mut blocks = BlockTable::new();
        blocks.insert(
            "WINDOW",
            vec![DxfEntity::Polyline(DxfPolyline {
                layer: "WALLS".into(),
                vertices: vec![
                    DxfPolylineVertex::new(0.0, 0.0),
                    DxfPolylineVertex::new(2.0, 0.0),
                    DxfPolylineVertex::new(2.0, 1.0),
                    DxfPolylineVertex::new(0.0, 1.0),
                ],
                closed: true,
                elevation: 0.0,
            })],
        );
        let s = make_sheet();
        let entities = vec![DxfEntity::Insert(DxfInsert {
            layer: "WALLS".into(),
            block_name: "window".into(), // lowercase!
            position: [50.0, 25.0, 0.0],
            scale: [1.0, 1.0, 1.0],
            rotation: 0.0,
        })];
        let svg = render_sheet_svg_full(
            &s,
            &entities,
            &PlotStyleTable::monochrome(),
            &LayerTable::new(),
            &DimStyleTable::new(),
            &blocks,
            &SvgExportOptions::default(),
        )
        .unwrap();
        assert!(
            !svg.contains("insert-missing"),
            "expected expanded body, got missing-placeholder: {svg}",
        );
        assert!(svg.contains("<polygon"));
    }

    #[test]
    fn self_referencing_block_does_not_overflow_stack() {
        // Cyclic block definitions are invalid DXF but must not crash
        // the exporter. The recursion-limit placeholder is emitted
        // when `BLOCK_EXPANSION_MAX_DEPTH` is exhausted.
        let mut blocks = BlockTable::new();
        // CYCLE -> [INSERT(CYCLE)] : direct self-reference.
        blocks.insert(
            "CYCLE",
            vec![DxfEntity::Insert(DxfInsert {
                layer: "WALLS".into(),
                block_name: "CYCLE".into(),
                position: [0.0, 0.0, 0.0],
                scale: [1.0, 1.0, 1.0],
                rotation: 0.0,
            })],
        );
        let s = make_sheet();
        let entities = vec![DxfEntity::Insert(DxfInsert {
            layer: "WALLS".into(),
            block_name: "CYCLE".into(),
            position: [50.0, 25.0, 0.0],
            scale: [1.0, 1.0, 1.0],
            rotation: 0.0,
        })];
        // Should return Ok(_) (no stack overflow) and the SVG should
        // contain the recursion-limit placeholder.
        let svg = render_sheet_svg_full(
            &s,
            &entities,
            &PlotStyleTable::monochrome(),
            &LayerTable::new(),
            &DimStyleTable::new(),
            &blocks,
            &SvgExportOptions::default(),
        )
        .unwrap();
        assert!(
            svg.contains(r#"class="insert-recursion-limit""#),
            "expected recursion-limit marker in: {svg}",
        );
        assert!(svg.contains("[CYCLE"));
    }

    #[test]
    fn mutually_cyclic_blocks_do_not_overflow_stack() {
        // Indirect cycle: A inserts B, B inserts A. The first INSERT
        // in the top-level entity slice expands; further nesting
        // unwinds the depth counter and stops at the limit.
        let mut blocks = BlockTable::new();
        blocks.insert(
            "A",
            vec![DxfEntity::Insert(DxfInsert {
                layer: "WALLS".into(),
                block_name: "B".into(),
                position: [0.0, 0.0, 0.0],
                scale: [1.0, 1.0, 1.0],
                rotation: 0.0,
            })],
        );
        blocks.insert(
            "B",
            vec![DxfEntity::Insert(DxfInsert {
                layer: "WALLS".into(),
                block_name: "A".into(),
                position: [0.0, 0.0, 0.0],
                scale: [1.0, 1.0, 1.0],
                rotation: 0.0,
            })],
        );
        let s = make_sheet();
        let entities = vec![DxfEntity::Insert(DxfInsert {
            layer: "WALLS".into(),
            block_name: "A".into(),
            position: [50.0, 25.0, 0.0],
            scale: [1.0, 1.0, 1.0],
            rotation: 0.0,
        })];
        let svg = render_sheet_svg_full(
            &s,
            &entities,
            &PlotStyleTable::monochrome(),
            &LayerTable::new(),
            &DimStyleTable::new(),
            &blocks,
            &SvgExportOptions::default(),
        )
        .unwrap();
        // The placeholder may be tagged with either "A" or "B"
        // depending on which side of the cycle hits the wall first.
        assert!(svg.contains(r#"class="insert-recursion-limit""#));
    }

    #[test]
    fn circle_inside_scaled_insert_uses_scaled_radius() {
        // A Circle of radius 5 inside a block inserted at scale 2.0
        // must render at radius 10, not 5.
        let mut blocks = BlockTable::new();
        blocks.insert(
            "PILE",
            vec![DxfEntity::Circle(DxfCircle {
                layer: "WALLS".into(),
                center: [0.0, 0.0, 0.0],
                radius: 5.0,
            })],
        );
        let s = make_sheet();
        let entities = vec![DxfEntity::Insert(DxfInsert {
            layer: "WALLS".into(),
            block_name: "PILE".into(),
            position: [0.0, 0.0, 0.0],
            scale: [2.0, 2.0, 1.0],
            rotation: 0.0,
        })];
        let svg = render_sheet_svg_full(
            &s,
            &entities,
            &PlotStyleTable::monochrome(),
            &LayerTable::new(),
            &DimStyleTable::new(),
            &blocks,
            &SvgExportOptions::default(),
        )
        .unwrap();
        // The viewport in make_sheet() has scale 0.01 (1:100), so a
        // model-space radius of 10 -> paper-space radius of 0.1 mm.
        // fmt_coord emits 4 decimal places by default.
        assert!(
            svg.contains(r#"r="0.1000""#),
            "expected r=0.1000 (10 mm * vp.scale 0.01) in: {svg}",
        );
        // And NOT the unscaled radius of 0.05 (= 5 mm * 0.01).
        assert!(
            !svg.contains(r#"r="0.0500""#),
            "found unscaled radius — Circle.radius is not being multiplied by insert scale: {svg}",
        );
    }

    #[test]
    fn arc_inside_rotated_insert_offsets_start_and_end_angles() {
        // An Arc with start=0°/end=90° inside a block inserted at
        // rotation=45° must render the arc sweep from 45° to 135°.
        // Arc is sampled as a polyline; the first/last samples are
        // the easiest assertion points (they sit on the arc endpoints
        // in model space → transformed → paper-space).
        let mut blocks = BlockTable::new();
        blocks.insert(
            "QUARTER",
            vec![DxfEntity::Arc(DxfArc {
                layer: "WALLS".into(),
                center: [0.0, 0.0, 0.0],
                radius: 10.0,
                start_angle: 0.0,
                end_angle: 90.0,
            })],
        );
        let s = make_sheet();
        // Render with rotation=0 and rotation=45; the two outputs
        // must differ. (If start_angle/end_angle were ignored, the
        // arc would land in the same place either way.)
        let no_rot = vec![DxfEntity::Insert(DxfInsert {
            layer: "WALLS".into(),
            block_name: "QUARTER".into(),
            position: [50.0, 25.0, 0.0],
            scale: [1.0, 1.0, 1.0],
            rotation: 0.0,
        })];
        let with_rot = vec![DxfEntity::Insert(DxfInsert {
            layer: "WALLS".into(),
            block_name: "QUARTER".into(),
            position: [50.0, 25.0, 0.0],
            scale: [1.0, 1.0, 1.0],
            rotation: 45.0,
        })];
        let svg_no_rot = render_sheet_svg_full(
            &s,
            &no_rot,
            &PlotStyleTable::monochrome(),
            &LayerTable::new(),
            &DimStyleTable::new(),
            &blocks,
            &SvgExportOptions::default(),
        )
        .unwrap();
        let svg_with_rot = render_sheet_svg_full(
            &s,
            &with_rot,
            &PlotStyleTable::monochrome(),
            &LayerTable::new(),
            &DimStyleTable::new(),
            &blocks,
            &SvgExportOptions::default(),
        )
        .unwrap();
        assert_ne!(
            svg_no_rot, svg_with_rot,
            "expected arc sweep to shift with INSERT rotation",
        );
    }

    #[test]
    fn ellipse_inside_rotated_insert_transforms_major_axis() {
        // An Ellipse whose major_axis points along +X must rotate
        // with the INSERT — render at rotation=0 and rotation=90,
        // expect different SVG output. (If major_axis were copied
        // unchanged, the ellipse would render identically.)
        let mut blocks = BlockTable::new();
        blocks.insert(
            "OVAL",
            vec![DxfEntity::Ellipse(DxfEllipse {
                layer: "WALLS".into(),
                center: [0.0, 0.0, 0.0],
                major_axis: [5.0, 0.0, 0.0],
                ratio: 0.5,
                start_param: 0.0,
                end_param: std::f64::consts::TAU,
            })],
        );
        let s = make_sheet();
        let no_rot = vec![DxfEntity::Insert(DxfInsert {
            layer: "WALLS".into(),
            block_name: "OVAL".into(),
            position: [50.0, 25.0, 0.0],
            scale: [1.0, 1.0, 1.0],
            rotation: 0.0,
        })];
        let rotated = vec![DxfEntity::Insert(DxfInsert {
            layer: "WALLS".into(),
            block_name: "OVAL".into(),
            position: [50.0, 25.0, 0.0],
            scale: [1.0, 1.0, 1.0],
            rotation: 90.0,
        })];
        let a = render_sheet_svg_full(
            &s,
            &no_rot,
            &PlotStyleTable::monochrome(),
            &LayerTable::new(),
            &DimStyleTable::new(),
            &blocks,
            &SvgExportOptions::default(),
        )
        .unwrap();
        let b = render_sheet_svg_full(
            &s,
            &rotated,
            &PlotStyleTable::monochrome(),
            &LayerTable::new(),
            &DimStyleTable::new(),
            &blocks,
            &SvgExportOptions::default(),
        )
        .unwrap();
        assert_ne!(
            a, b,
            "expected ellipse to rotate with insert; major_axis is not being transformed",
        );
    }

    #[test]
    fn text_inside_scaled_insert_uses_scaled_height() {
        // Text inside a block with scale=2 must render at 2× the
        // authored height. The renderer emits height as the font-size
        // attribute directly, so we can assert on that value.
        let mut blocks = BlockTable::new();
        blocks.insert(
            "LABEL",
            vec![DxfEntity::Text(DxfText {
                layer: "TEXT".into(),
                position: [0.0, 0.0, 0.0],
                height: 2.5,
                rotation: 0.0,
                text: "X".into(),
            })],
        );
        let s = make_sheet();
        let entities = vec![DxfEntity::Insert(DxfInsert {
            layer: "TEXT".into(),
            block_name: "LABEL".into(),
            position: [50.0, 25.0, 0.0],
            scale: [2.0, 2.0, 1.0],
            rotation: 0.0,
        })];
        let svg = render_sheet_svg_full(
            &s,
            &entities,
            &PlotStyleTable::monochrome(),
            &LayerTable::new(),
            &DimStyleTable::new(),
            &blocks,
            &SvgExportOptions::default(),
        )
        .unwrap();
        // Height 2.5 * Y-scale 2.0 = 5.
        assert!(
            svg.contains(r#"font-size="5""#),
            "expected scaled font-size 5 in: {svg}",
        );
    }

    #[test]
    fn clip_path_coordinates_use_fmt_coord_precision() {
        // Viewport with fractional paper coordinates that would yield
        // a long decimal representation under Display. fmt_coord
        // clamps to the configured precision; Display would emit far
        // more digits.
        let mut s = make_sheet();
        // Replace the default viewport with one whose origin and
        // size both have many decimal places after multiplication.
        s.viewports.clear();
        s.viewports.push(SheetViewport::new(
            "VP_FRAC",
            [10.0_f64 / 3.0, 25.0_f64 / 7.0],
            [100.1234567_f64, 50.7654321_f64],
        ));
        let svg = render_sheet_svg(
            &s,
            &[],
            &PlotStyleTable::monochrome(),
            &SvgExportOptions::default(),
        )
        .unwrap();
        // Find the clipPath line.
        let clip_line = svg
            .lines()
            .find(|l| l.contains("<clipPath id=\"vp-clip-0\""))
            .expect("clipPath line should be present");
        // Default coordinate_precision is 4 → at most 4 decimals on
        // each coordinate. Display for 10.0/3.0 would emit 17 digits.
        // Check that no coordinate value in the clip rect has more
        // than 4 digits past the decimal point.
        for cap in clip_line.split('"') {
            if let Ok(v) = cap.parse::<f64>() {
                let s = format!("{v}");
                if let Some((_, frac)) = s.split_once('.') {
                    assert!(
                        frac.len() <= 4,
                        "clip-path coord {s} has more than 4 decimals — fmt_coord not applied",
                    );
                }
            }
        }
    }

    #[test]
    fn lib_re_exports_render_sheet_svg_full_and_tables() {
        // Compile-time check: the full SVG renderer + its supporting
        // tables must be re-exported from the crate root so external
        // callers don't need to reach into `svg_export::` directly.
        use crate::{
            render_sheet_svg_full, BlockTable, DimStyleTable, LayerTable, SvgExportOptions,
            BLOCK_EXPANSION_MAX_DEPTH,
        };
        let _ = BLOCK_EXPANSION_MAX_DEPTH;
        let s = make_sheet();
        let svg = render_sheet_svg_full(
            &s,
            &[],
            &PlotStyleTable::monochrome(),
            &LayerTable::new(),
            &DimStyleTable::new(),
            &BlockTable::new(),
            &SvgExportOptions::default(),
        )
        .unwrap();
        assert!(svg.starts_with("<?xml"));
    }

    #[test]
    fn solid_hatch_fills_polygon_named_hatch_uses_pattern_url() {
        let s = make_sheet();
        let solid = DxfHatch {
            layer: "HATCH".into(),
            pattern_name: "SOLID".into(),
            solid: true,
            scale: 1.0,
            angle: 0.0,
            elevation: 0.0,
            loops: vec![DxfHatchLoop {
                vertices: vec![[0.0, 0.0], [10.0, 0.0], [10.0, 10.0], [0.0, 10.0]],
            }],
        };
        let named = DxfHatch {
            layer: "HATCH".into(),
            pattern_name: "ANSI31".into(),
            solid: false,
            scale: 1.0,
            angle: 0.0,
            elevation: 0.0,
            loops: vec![DxfHatchLoop {
                vertices: vec![[20.0, 0.0], [30.0, 0.0], [30.0, 10.0], [20.0, 10.0]],
            }],
        };
        let entities = vec![DxfEntity::Hatch(solid), DxfEntity::Hatch(named)];
        let svg = render_sheet_svg(
            &s,
            &entities,
            &PlotStyleTable::monochrome(),
            &SvgExportOptions::default(),
        )
        .unwrap();
        assert!(svg.contains("<pattern id=\"hatch-ANSI31\""));
        assert!(svg.contains("fill=\"url(#hatch-ANSI31)\""));
    }

    #[test]
    fn viewport_clip_path_is_emitted_when_clipping_enabled() {
        let s = make_sheet();
        let svg = render_sheet_svg(
            &s,
            &[],
            &PlotStyleTable::monochrome(),
            &SvgExportOptions::default(),
        )
        .unwrap();
        assert!(svg.contains(r#"<clipPath id="vp-clip-0">"#));
        assert!(svg.contains(r#"clip-path="url(#vp-clip-0)""#));
    }

    #[test]
    fn viewport_clip_path_is_omitted_when_disabled() {
        let s = make_sheet();
        let opts = SvgExportOptions {
            clip_viewports: false,
            ..Default::default()
        };
        let svg = render_sheet_svg(&s, &[], &PlotStyleTable::monochrome(), &opts).unwrap();
        assert!(!svg.contains("<clipPath"));
        assert!(!svg.contains("clip-path="));
    }

    #[test]
    fn title_block_renders_border_and_populated_fields() {
        let mut s = make_sheet();
        let mut t = TitleBlock::standard();
        t.set("project.name", "Test Project");
        t.set("date", "2025-05-25");
        s.title_block = Some(t);
        let svg = render_sheet_svg(
            &s,
            &[],
            &PlotStyleTable::monochrome(),
            &SvgExportOptions::default(),
        )
        .unwrap();
        let block = svg
            .split(r#"<g class="title-block">"#)
            .nth(1)
            .unwrap()
            .split("</g>")
            .next()
            .unwrap();
        assert!(block.contains("<rect"));
        assert!(block.contains("Test Project"));
        assert!(block.contains("2025-05-25"));
    }

    #[test]
    fn mixed_primitive_export_contains_every_entity_kind() {
        // Comprehensive test: every kind of entity in one drawing.
        // Confirms the round-trip pipeline doesn't skip any.
        let mut s = Sheet::new("A1", PaperSize::IsoA1);
        s.viewports
            .push(SheetViewport::new("VP1", [10.0, 10.0], [800.0, 500.0]));
        let entities = vec![
            DxfEntity::Line(DxfLine {
                layer: "WALLS".into(),
                start: [0.0, 0.0, 0.0],
                end: [100.0, 0.0, 0.0],
            }),
            DxfEntity::Polyline(DxfPolyline {
                layer: "WALLS".into(),
                vertices: vec![
                    DxfPolylineVertex::new(0.0, 0.0),
                    DxfPolylineVertex::new(10.0, 0.0),
                ],
                closed: false,
                elevation: 0.0,
            }),
            DxfEntity::Arc(DxfArc {
                layer: "WALLS".into(),
                center: [0.0, 0.0, 0.0],
                radius: 10.0,
                start_angle: 0.0,
                end_angle: 90.0,
            }),
            DxfEntity::Circle(DxfCircle {
                layer: "WALLS".into(),
                center: [50.0, 50.0, 0.0],
                radius: 5.0,
            }),
            DxfEntity::Ellipse(DxfEllipse {
                layer: "WALLS".into(),
                center: [10.0, 10.0, 0.0],
                major_axis: [5.0, 0.0, 0.0],
                ratio: 0.5,
                start_param: 0.0,
                end_param: std::f64::consts::TAU,
            }),
            DxfEntity::Spline(DxfSpline {
                layer: "WALLS".into(),
                degree: 3,
                knots: vec![0.0, 0.0, 0.0, 0.0, 1.0, 1.0, 1.0, 1.0],
                control_points: vec![
                    [0.0, 0.0, 0.0],
                    [5.0, 10.0, 0.0],
                    [15.0, 10.0, 0.0],
                    [20.0, 0.0, 0.0],
                ],
                closed: false,
            }),
            DxfEntity::Hatch(DxfHatch {
                layer: "HATCH".into(),
                pattern_name: "ANSI31".into(),
                solid: false,
                scale: 1.0,
                angle: 0.0,
                elevation: 0.0,
                loops: vec![DxfHatchLoop {
                    vertices: vec![[0.0, 0.0], [10.0, 0.0], [10.0, 10.0], [0.0, 10.0]],
                }],
            }),
            DxfEntity::Text(DxfText {
                layer: "TEXT".into(),
                position: [50.0, 50.0, 0.0],
                height: 2.5,
                rotation: 0.0,
                text: "hello".into(),
            }),
            DxfEntity::Insert(DxfInsert {
                layer: "WALLS".into(),
                block_name: "MISSING".into(),
                position: [80.0, 80.0, 0.0],
                scale: [1.0, 1.0, 1.0],
                rotation: 0.0,
            }),
            DxfEntity::Dimension(DxfDimension {
                layer: "DIMS".into(),
                style: "STANDARD".into(),
                kind: DxfDimensionKind::Linear,
                def_point: [0.0, 30.0, 0.0],
                text_position: [50.0, 31.0, 0.0],
                def_point_a: [0.0, 0.0, 0.0],
                def_point_b: [100.0, 0.0, 0.0],
                override_text: None,
                measured_value: Some(100.0),
            }),
        ];
        let dim_styles = DimStyleTable::from_slice(&[DxfDimStyle::standard()]);
        let svg = render_sheet_svg_full(
            &s,
            &entities,
            &PlotStyleTable::monochrome(),
            &LayerTable::new(),
            &dim_styles,
            &BlockTable::new(),
            &SvgExportOptions::default(),
        )
        .unwrap();
        // Top-level sanity: well-formed structure.
        assert!(svg.starts_with("<?xml"));
        assert!(svg.contains("</svg>"));
        // Each entity kind is represented.
        assert!(svg.contains("<line")); // Line + extension/dim lines
        assert!(svg.contains("<polyline")); // Polyline, partial spline, arc sample
        assert!(svg.contains("<circle")); // Circle
        assert!(svg.contains("<polygon")); // Closed ellipse
        assert!(svg.contains("<text")); // Text + dim label
        assert!(svg.contains(r#"class="dimension""#));
        assert!(svg.contains(r#"class="insert-missing""#));
        // Hatch pattern definition present.
        assert!(svg.contains("<pattern id=\"hatch-ANSI31\""));
    }
}
