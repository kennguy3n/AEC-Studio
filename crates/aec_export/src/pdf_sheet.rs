//! Deterministic PDF sheet export.
//!
//! Given a `Sheet` (paper size + viewports + title block) and a slice
//! of `DxfEntity`s in model space, this renders a printable PDF at the
//! sheet's exact paper dimensions using printpdf line/shape primitives.
//!
//! Per-entity colour and lineweight come from a `PlotStyleTable`. Lines,
//! polylines, arcs (sampled), circles, and hatch loops are emitted as
//! native vector paths. Text is emitted via Helvetica.

use std::fs::File;
use std::io::BufWriter;
use std::path::{Path, PathBuf};

use aec_cad::dxf::DxfEntity;
use aec_cad::sheets::{Sheet, SheetViewport};
use printpdf::path::{PaintMode, WindingOrder};
use printpdf::{
    BuiltinFont, Color, IndirectFontRef, Line as PdfLine, Mm, PdfDocument, PdfDocumentReference,
    Point, Polygon as PdfPolygon, Rgb,
};

use crate::pdf::PdfBuilderError;
use crate::plot_style::{PlotStyle, PlotStyleTable};

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
    pub fn add_sheet(
        &mut self,
        sheet: &Sheet,
        entities: &[DxfEntity],
        plot_style: &PlotStyleTable,
    ) -> Result<(), PdfBuilderError> {
        let (w_mm, h_mm) = sheet.dimensions_mm();
        let (page, layer) = self
            .doc
            .add_page(Mm(w_mm as f32), Mm(h_mm as f32), &sheet.name);
        self.sheets_emitted += 1;
        let layer_ref = self.doc.get_page(page).get_layer(layer);

        // Paper border.
        layer_ref.set_outline_color(Color::Rgb(Rgb::new(0.0, 0.0, 0.0, None)));
        layer_ref.set_outline_thickness(0.2_f32);
        draw_rect(&layer_ref, 0.0, 0.0, w_mm, h_mm);

        // Each viewport projects its slice of model space to paper coords.
        for vp in &sheet.viewports {
            for entity in entities {
                draw_entity(&layer_ref, entity, vp, plot_style, h_mm, &self.body_font);
            }
        }

        // Title block.
        if let Some(tb) = &sheet.title_block {
            for (field, value) in tb.populated_fields() {
                let x = field.position[0] as f32;
                // PDF y is bottom-up; convert from top-down sheet coords.
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

fn draw_rect(layer: &printpdf::PdfLayerReference, x: f64, y_bot: f64, w: f64, h: f64) {
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

fn style_for_layer(layer: &str, table: &PlotStyleTable) -> PlotStyle {
    let aci = layer_to_default_aci(layer);
    table.get(aci).cloned().unwrap_or(PlotStyle {
        aci,
        color: [0, 0, 0],
        lineweight_mm: 0.25,
        screening: 100,
        linetype: None,
        override_linetype: false,
    })
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

fn draw_entity(
    layer_ref: &printpdf::PdfLayerReference,
    entity: &DxfEntity,
    vp: &SheetViewport,
    table: &PlotStyleTable,
    sheet_h_mm: f64,
    body_font: &IndirectFontRef,
) {
    let style = style_for_layer(entity.layer(), table);
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

    // Convert a paper-space mm point (top-down) to printpdf's bottom-up coordinate.
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
        DxfEntity::Ellipse(_)
        | DxfEntity::Spline(_)
        | DxfEntity::Insert(_)
        | DxfEntity::Dimension(_)
        | DxfEntity::Attdef(_) => {
            // Not yet rendered. ATTDEF lives inside blocks; the
            // top-level PDF render walks INSERT references separately.
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use aec_cad::dxf::{DxfCircle, DxfLine, DxfPolyline, DxfPolylineVertex};
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
}
