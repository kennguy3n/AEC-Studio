//! Deterministic SVG export of CAD entities arranged on a sheet.
//!
//! Given a `Sheet` (paper size + viewports + title block) and a slice
//! of `DxfEntity`s in model space, this produces an SVG document whose
//! output is byte-identical for the same input regardless of platform
//! or wall-clock time.

use std::fmt::Write;

use aec_cad::dxf::DxfEntity;
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
}

impl Default for SvgExportOptions {
    fn default() -> Self {
        Self {
            paper_fill: Some([255, 255, 255]),
            draw_border: true,
            coordinate_precision: 4,
        }
    }
}

/// Render a sheet plus a flat entity list to SVG. The entities are
/// expected to be in *model* coordinates; each viewport on the sheet
/// declares the mapping from model → paper.
pub fn render_sheet_svg(
    sheet: &Sheet,
    entities: &[DxfEntity],
    plot_style: &PlotStyleTable,
    options: &SvgExportOptions,
) -> Result<String, SvgExportError> {
    let (w_mm, h_mm) = sheet.dimensions_mm();
    let mut out = String::new();
    writeln!(out, r#"<?xml version="1.0" encoding="UTF-8"?>"#)?;
    writeln!(
        out,
        r#"<svg xmlns="http://www.w3.org/2000/svg" width="{w_mm}mm" height="{h_mm}mm" viewBox="0 0 {w_mm} {h_mm}">"#,
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
    // For each viewport, project model→paper and emit the visible
    // entities, applying plot-style colour and lineweight.
    for vp in &sheet.viewports {
        writeln!(out, r#"  <g class="viewport" id="{name}">"#, name = vp.name)?;
        for entity in entities {
            emit_entity(&mut out, entity, vp, plot_style, options)?;
        }
        writeln!(out, "  </g>")?;
    }
    writeln!(out, "</svg>")?;
    Ok(out)
}

/// SVG coordinates have Y down; sheet paper coordinates have Y down too
/// (origin top-left). Both match, so we just emit the paper coords.
fn fmt_coord(v: f64, precision: u8) -> String {
    format!("{0:.1$}", v, precision as usize)
}

fn style_for_layer(layer: &str, table: &PlotStyleTable) -> PlotStyle {
    // Without a proper layer→ACI map, default to ACI 7 (black).
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

fn emit_entity(
    out: &mut String,
    entity: &DxfEntity,
    vp: &SheetViewport,
    table: &PlotStyleTable,
    opts: &SvgExportOptions,
) -> Result<(), SvgExportError> {
    let style = style_for_layer(entity.layer(), table);
    let [cr, cg, cb] = screened(style.color, style.screening);
    let lw = style.lineweight_mm.max(0.05);
    let prec = opts.coordinate_precision;
    match entity {
        DxfEntity::Line(e) => {
            let a = vp.model_to_paper([e.start[0], e.start[1]]);
            let b = vp.model_to_paper([e.end[0], e.end[1]]);
            writeln!(
                out,
                r#"    <line x1="{}" y1="{}" x2="{}" y2="{}" stroke="rgb({},{},{})" stroke-width="{}" fill="none" />"#,
                fmt_coord(a[0], prec),
                fmt_coord(a[1], prec),
                fmt_coord(b[0], prec),
                fmt_coord(b[1], prec),
                cr,
                cg,
                cb,
                lw,
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
                r#"    <{tag} points="{pts}" stroke="rgb({cr},{cg},{cb})" stroke-width="{lw}" fill="none" />"#,
            )?;
        }
        DxfEntity::Circle(e) => {
            let c = vp.model_to_paper([e.center[0], e.center[1]]);
            let r = e.radius * vp.scale;
            writeln!(
                out,
                r#"    <circle cx="{}" cy="{}" r="{}" stroke="rgb({},{},{})" stroke-width="{}" fill="none" />"#,
                fmt_coord(c[0], prec),
                fmt_coord(c[1], prec),
                fmt_coord(r, prec),
                cr,
                cg,
                cb,
                lw,
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
                    fmt_coord(paper[1], prec)
                )?;
            }
            writeln!(
                out,
                r#"    <polyline points="{pts}" stroke="rgb({cr},{cg},{cb})" stroke-width="{lw}" fill="none" />"#,
            )?;
        }
        DxfEntity::Text(e) => {
            let p = vp.model_to_paper([e.position[0], e.position[1]]);
            writeln!(
                out,
                r#"    <text x="{}" y="{}" font-size="{}" fill="rgb({},{},{})">{}</text>"#,
                fmt_coord(p[0], prec),
                fmt_coord(p[1], prec),
                e.height,
                cr,
                cg,
                cb,
                escape_xml(&e.text),
            )?;
        }
        DxfEntity::Hatch(e) => {
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
                    r#"    <polygon points="{pts}" fill="rgb({cr},{cg},{cb})" stroke="none" />"#,
                )?;
            }
        }
        DxfEntity::Ellipse(_)
        | DxfEntity::Spline(_)
        | DxfEntity::Insert(_)
        | DxfEntity::Dimension(_)
        | DxfEntity::Attdef(_) => {
            // Out of scope for v1 SVG — these are exported via the
            // engineering PDF path which has more control over
            // multi-curve drawing. ATTDEFs only render when an
            // INSERT expands its block body.
        }
    }
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
    use aec_cad::dxf::{DxfLine, DxfPolylineVertex};
    use aec_cad::sheets::{PaperSize, Sheet, SheetViewport};

    fn make_sheet() -> Sheet {
        let mut s = Sheet::new("A1", PaperSize::IsoA3);
        s.viewports
            .push(SheetViewport::new("VP1", [10.0, 10.0], [200.0, 100.0]));
        s
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
        let entities = vec![DxfEntity::Polyline(aec_cad::dxf::DxfPolyline {
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
        let entities = vec![DxfEntity::Polyline(aec_cad::dxf::DxfPolyline {
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
        let entities = vec![DxfEntity::Text(aec_cad::dxf::DxfText {
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
}
