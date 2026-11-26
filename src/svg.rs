use std::fmt::Write as _;
use std::fs;
use std::path::{Path, PathBuf};

use crate::drc::DrcReport;
use crate::fab_assembly::render_assembly_svg;
use crate::fab_drills::render_drill_map_svg;
use crate::fab_silk::{is_silk_layer, refdes_local_pos, text_stroke_segments};
use crate::geom::{local_to_board, pad_copper_layers, remap_layer_for_side};
use crate::ir::{BBoxNm, BoardIr, Graphic, PadKind, Pt, Side, mm, nm_to_mm, safe_slug};
use crate::shapes::{ResolvedShape, pad_to_world_shape};

pub(crate) const DEFAULT_SVG_MARGIN_MM: f64 = 2.0;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SvgView {
    Top,
    Bottom,
    Layer(String),
    AssemblyTop,
    AssemblyBottom,
    DrillMap,
}

#[derive(Debug, Clone)]
pub struct SvgOptions {
    pub view: SvgView,
    pub show_hotspots: bool,
    pub show_silk: bool,
    pub show_drills: bool,
    pub margin_mm: f64,
}

impl Default for SvgOptions {
    fn default() -> Self {
        Self {
            view: SvgView::Top,
            show_hotspots: true,
            show_silk: true,
            show_drills: true,
            margin_mm: DEFAULT_SVG_MARGIN_MM,
        }
    }
}

pub(crate) fn xml_escape(s: &str) -> String {
    s.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
}

pub(crate) fn svg_pt(pt: Pt, mirror_x: bool) -> (f64, f64) {
    let x = nm_to_mm(pt.x);
    let y = nm_to_mm(pt.y);
    (if mirror_x { -x } else { x }, -y)
}

pub(crate) fn poly_path_d(pts: &[Pt], mirror_x: bool) -> String {
    let mut d = String::new();
    for (i, &p) in pts.iter().enumerate() {
        let (x, y) = svg_pt(p, mirror_x);
        let cmd = if i == 0 { 'M' } else { 'L' };
        let _ = write!(d, "{cmd}{x:.4},{y:.4} ");
    }
    if !pts.is_empty() {
        d.push('Z');
    }
    d
}

fn layer_color(layer: &str, idx: usize) -> &'static str {
    if layer == "F.Cu" {
        "#d9483b"
    } else if layer == "B.Cu" {
        "#3b82f6"
    } else {
        match idx % 4 {
            0 => "#eab308",
            1 => "#10b981",
            2 => "#a855f7",
            _ => "#06b6d4",
        }
    }
}

pub(crate) fn render_shape_svg(
    out: &mut String,
    shape: &ResolvedShape,
    color: &str,
    mirror_x: bool,
) {
    match shape {
        ResolvedShape::Circle { center, radius } => {
            let (cx, cy) = svg_pt(*center, mirror_x);
            let _ = writeln!(
                out,
                r#"  <circle cx="{cx:.4}" cy="{cy:.4}" r="{:.4}" fill="{color}" />"#,
                nm_to_mm(*radius)
            );
        }
        ResolvedShape::Capsule { a, b, radius } => {
            let (ax, ay) = svg_pt(*a, mirror_x);
            let (bx, by) = svg_pt(*b, mirror_x);
            let _ = writeln!(
                out,
                r#"  <line x1="{ax:.4}" y1="{ay:.4}" x2="{bx:.4}" y2="{by:.4}" stroke="{color}" stroke-width="{:.4}" stroke-linecap="round" />"#,
                nm_to_mm(*radius * 2)
            );
        }
        ResolvedShape::Poly { verts, .. } if verts.len() >= 3 => {
            let d = poly_path_d(verts, mirror_x);
            let _ = writeln!(out, r#"  <path d="{d}" fill="{color}" />"#);
        }
        _ => {}
    }
}

pub(crate) fn render_graphic_svg(
    out: &mut String,
    g: &Graphic,
    pl: Option<&crate::ir::Placement>,
    color: &str,
    mirror_x: bool,
) {
    let map_pt = |p: Pt| match pl {
        Some(pl) => local_to_board(p, pl.pos.x, pl.pos.y, pl.rot_deg, pl.side),
        None => p,
    };
    match g {
        Graphic::Line { a, b, width, .. } => {
            let (ax, ay) = svg_pt(map_pt(*a), mirror_x);
            let (bx, by) = svg_pt(map_pt(*b), mirror_x);
            let _ = writeln!(
                out,
                r#"  <line x1="{ax:.4}" y1="{ay:.4}" x2="{bx:.4}" y2="{by:.4}" stroke="{color}" stroke-width="{:.4}" stroke-linecap="round" />"#,
                nm_to_mm((*width).max(50_000))
            );
        }
        Graphic::Rect {
            min,
            max,
            width,
            fill,
            ..
        } => {
            let pts = [
                map_pt(*min),
                map_pt(Pt::new(max.x, min.y)),
                map_pt(*max),
                map_pt(Pt::new(min.x, max.y)),
            ];
            let d = poly_path_d(&pts, mirror_x);
            if *fill {
                let _ = writeln!(out, r#"  <path d="{d}" fill="{color}" />"#);
            } else {
                let _ = writeln!(
                    out,
                    r#"  <path d="{d}" fill="none" stroke="{color}" stroke-width="{:.4}" stroke-linejoin="round" />"#,
                    nm_to_mm((*width).max(50_000))
                );
            }
        }
        Graphic::Circle {
            center,
            radius,
            width,
            fill,
            ..
        } if *radius > 0 => {
            let (cx, cy) = svg_pt(map_pt(*center), mirror_x);
            if *fill {
                let _ = writeln!(
                    out,
                    r#"  <circle cx="{cx:.4}" cy="{cy:.4}" r="{:.4}" fill="{color}" />"#,
                    nm_to_mm(*radius)
                );
            } else {
                let _ = writeln!(
                    out,
                    r#"  <circle cx="{cx:.4}" cy="{cy:.4}" r="{:.4}" fill="none" stroke="{color}" stroke-width="{:.4}" />"#,
                    nm_to_mm(*radius),
                    nm_to_mm((*width).max(50_000))
                );
            }
        }
        Graphic::Poly {
            verts, width, fill, ..
        } if verts.len() >= 2 => {
            let pts: Vec<Pt> = verts.iter().map(|&v| map_pt(v)).collect();
            let d = poly_path_d(&pts, mirror_x);
            if *fill {
                let _ = writeln!(out, r#"  <path d="{d}" fill="{color}" />"#);
            } else {
                let _ = writeln!(
                    out,
                    r#"  <path d="{d}" fill="none" stroke="{color}" stroke-width="{:.4}" stroke-linejoin="round" />"#,
                    nm_to_mm((*width).max(50_000))
                );
            }
        }
        _ => {}
    }
}

pub(crate) fn sanitize_margin_mm(margin_mm: f64) -> f64 {
    if margin_mm.is_finite() {
        margin_mm.max(0.0)
    } else {
        DEFAULT_SVG_MARGIN_MM
    }
}

pub(crate) fn board_svg_bounds(board: &BoardIr, mirror_x: bool) -> (f64, f64, f64, f64) {
    let bb = if !board.outline.is_empty() {
        BBoxNm::from_pts(&board.outline)
    } else {
        BBoxNm::new(0, 0, mm(40.0), mm(30.0))
    };
    let (min_x, max_x) = if mirror_x {
        (-nm_to_mm(bb.max_x), -nm_to_mm(bb.min_x))
    } else {
        (nm_to_mm(bb.min_x), nm_to_mm(bb.max_x))
    };
    (min_x, max_x, -nm_to_mm(bb.max_y), -nm_to_mm(bb.min_y))
}

pub(crate) fn begin_svg(title: &str, min_x: f64, min_y: f64, vw: f64, vh: f64, bg: &str) -> String {
    let mut out = String::new();
    let _ = writeln!(
        out,
        r#"<svg xmlns="http://www.w3.org/2000/svg" viewBox="{min_x:.4} {min_y:.4} {vw:.4} {vh:.4}" width="{:.0}px" height="{:.0}px">"#,
        (vw * 20.0).clamp(320.0, 2400.0),
        (vh * 20.0).clamp(240.0, 2400.0)
    );
    let _ = writeln!(out, "  <title>{}</title>", xml_escape(title));
    let _ = writeln!(
        out,
        r#"  <rect x="{min_x:.4}" y="{min_y:.4}" width="{vw:.4}" height="{vh:.4}" fill="{bg}" />"#
    );
    out
}

pub(crate) fn write_board_outline(
    out: &mut String,
    board: &BoardIr,
    mirror_x: bool,
    fill: &str,
    stroke: &str,
    stroke_width: &str,
) {
    if board.outline.is_empty() {
        return;
    }
    let mut d = poly_path_d(&board.outline, mirror_x);
    for cut in &board.cutouts {
        d.push(' ');
        d.push_str(&poly_path_d(cut, mirror_x));
    }
    let _ = writeln!(
        out,
        r#"  <path id="board_outline" d="{d}" fill="{fill}" fill-rule="evenodd" stroke="{stroke}" stroke-width="{stroke_width}" />"#
    );
}

fn draw_copper_layer(
    out: &mut String,
    board: &BoardIr,
    cu_layers: &[String],
    layer: &str,
    color: &str,
    mirror_x: bool,
    opacity: f64,
) {
    let _ = writeln!(
        out,
        r#"  <g id="layer_{}" opacity="{opacity:.2}">"#,
        xml_escape(layer)
    );
    for pour in &board.filled_pours {
        if pour.layer != layer {
            continue;
        }
        for poly in &pour.polys {
            let mut d = poly_path_d(&poly.outer, mirror_x);
            for hole in &poly.holes {
                d.push(' ');
                d.push_str(&poly_path_d(hole, mirror_x));
            }
            let _ = writeln!(
                out,
                r#"  <path d="{d}" fill="{color}" fill-opacity="0.38" fill-rule="evenodd" stroke="{color}" stroke-width="0.03" />"#
            );
        }
    }
    for tr in &board.tracks {
        if tr.layer == layer {
            let (ax, ay) = svg_pt(tr.a, mirror_x);
            let (bx, by) = svg_pt(tr.b, mirror_x);
            let _ = writeln!(
                out,
                r#"  <line x1="{ax:.4}" y1="{ay:.4}" x2="{bx:.4}" y2="{by:.4}" stroke="{color}" stroke-width="{:.4}" stroke-linecap="round" />"#,
                nm_to_mm(tr.width)
            );
        }
    }
    for pl in &board.placements {
        let Some(fp) = board.footprints.get(&pl.footprint) else {
            continue;
        };
        for pad in &fp.pads {
            if pad.kind == PadKind::Npth {
                continue;
            }
            if pad_copper_layers(pad, pl.side, cu_layers)
                .iter()
                .any(|l| l == layer)
            {
                let shape = pad_to_world_shape(pad, pl, 16);
                render_shape_svg(out, &shape, color, mirror_x);
            }
        }
    }
    for via in &board.vias {
        if via.touches_layer(layer, &board.layers) {
            let (cx, cy) = svg_pt(via.pos, mirror_x);
            let _ = writeln!(
                out,
                r#"  <circle cx="{cx:.4}" cy="{cy:.4}" r="{:.4}" fill="{color}" />"#,
                nm_to_mm(via.diameter / 2)
            );
        }
    }
    out.push_str("  </g>\n");
}

/// Renders a composite Top/Bottom, single-layer, assembly, or drill-map 2D SVG.
pub fn render_board_svg(board: &BoardIr, drc: Option<&DrcReport>, cfg: &SvgOptions) -> String {
    match cfg.view {
        SvgView::AssemblyTop => return render_assembly_svg(board, Side::Top, cfg.margin_mm),
        SvgView::AssemblyBottom => return render_assembly_svg(board, Side::Bottom, cfg.margin_mm),
        SvgView::DrillMap => return render_drill_map_svg(board, cfg.margin_mm),
        _ => {}
    }
    let cu_layers = board.copper_layer_names();
    let mirror_x = matches!(cfg.view, SvgView::Bottom);
    let margin = sanitize_margin_mm(cfg.margin_mm);
    let (bx0, bx1, by0, by1) = board_svg_bounds(board, mirror_x);
    let min_x = bx0 - margin;
    let min_y = by0 - margin;
    let vw = (bx1 - bx0 + 2.0 * margin).max(1.0);
    let vh = (by1 - by0 + 2.0 * margin).max(1.0);

    let mut out = begin_svg(&board.name, min_x, min_y, vw, vh, "#0d1117");
    write_board_outline(&mut out, board, mirror_x, "#14291d", "#facc15", "0.12");

    let mut draw_order: Vec<(usize, &str, f64)> = Vec::new();
    match &cfg.view {
        SvgView::Layer(target) => {
            let idx = cu_layers.iter().position(|l| l == target).unwrap_or(0);
            draw_order.push((idx, target.as_str(), 0.95));
        }
        SvgView::Top | SvgView::Bottom => {
            let (back, front) = if matches!(cfg.view, SvgView::Top) {
                ("B.Cu", "F.Cu")
            } else {
                ("F.Cu", "B.Cu")
            };
            if let Some((i, n)) = cu_layers.iter().enumerate().find(|(_, l)| *l == back) {
                draw_order.push((i, n.as_str(), 0.35));
            }
            for (i, n) in cu_layers.iter().enumerate() {
                if n != "F.Cu" && n != "B.Cu" {
                    draw_order.push((i, n.as_str(), 0.40));
                }
            }
            if let Some((i, n)) = cu_layers.iter().enumerate().find(|(_, l)| *l == front) {
                draw_order.push((i, n.as_str(), 0.92));
            }
        }
        SvgView::AssemblyTop | SvgView::AssemblyBottom | SvgView::DrillMap => unreachable!(),
    }
    for (idx, name, opacity) in draw_order {
        draw_copper_layer(
            &mut out,
            board,
            &cu_layers,
            name,
            layer_color(name, idx),
            mirror_x,
            opacity,
        );
    }

    if cfg.show_drills {
        out.push_str("  <g id=\"drills\">\n");
        for via in &board.vias {
            let (cx, cy) = svg_pt(via.pos, mirror_x);
            let _ = writeln!(
                out,
                r##"  <circle cx="{cx:.4}" cy="{cy:.4}" r="{:.4}" fill="#0b0f14" stroke="#94a3b8" stroke-width="0.02" />"##,
                nm_to_mm(via.drill / 2)
            );
        }
        for pl in &board.placements {
            let Some(fp) = board.footprints.get(&pl.footprint) else {
                continue;
            };
            for pad in &fp.pads {
                if let Some(drill) = pad.drill_nm
                    && drill > 0
                {
                    let pos = local_to_board(pad.pos, pl.pos.x, pl.pos.y, pl.rot_deg, pl.side);
                    let (cx, cy) = svg_pt(pos, mirror_x);
                    let _ = writeln!(
                        out,
                        r##"  <circle cx="{cx:.4}" cy="{cy:.4}" r="{:.4}" fill="#0b0f14" stroke="#cbd5e1" stroke-width="0.03" />"##,
                        nm_to_mm(drill / 2)
                    );
                }
            }
        }
        out.push_str("  </g>\n");
    }

    if cfg.show_silk {
        let silk_side = match cfg.view {
            SvgView::Bottom => Some(Side::Bottom),
            SvgView::Top => Some(Side::Top),
            SvgView::Layer(ref l) if l == "B.Cu" => Some(Side::Bottom),
            SvgView::Layer(ref l) if l == "F.Cu" => Some(Side::Top),
            _ => None,
        };
        if let Some(side) = silk_side {
            out.push_str("  <g id=\"silkscreen\">\n");
            for pl in &board.placements {
                let Some(fp) = board.footprints.get(&pl.footprint) else {
                    continue;
                };
                for g in &fp.graphics {
                    if is_silk_layer(&remap_layer_for_side(g.layer(), pl.side), side) {
                        render_graphic_svg(&mut out, g, Some(pl), "#f1f5f9", mirror_x);
                    }
                }
                if pl.side == side && !pl.dnp && !pl.refdes.is_empty() {
                    let c = refdes_local_pos(board, pl);
                    for (a, b) in text_stroke_segments(&pl.refdes, c, 700_000) {
                        let wa = local_to_board(a, pl.pos.x, pl.pos.y, pl.rot_deg, pl.side);
                        let wb = local_to_board(b, pl.pos.x, pl.pos.y, pl.rot_deg, pl.side);
                        let (ax, ay) = svg_pt(wa, mirror_x);
                        let (bx, by) = svg_pt(wb, mirror_x);
                        let _ = writeln!(
                            out,
                            r##"  <line x1="{ax:.4}" y1="{ay:.4}" x2="{bx:.4}" y2="{by:.4}" stroke="#f1f5f9" stroke-width="0.10" stroke-linecap="round" />"##
                        );
                    }
                }
            }
            for g in &board.graphics {
                if is_silk_layer(g.layer(), side) {
                    render_graphic_svg(&mut out, g, None, "#f1f5f9", mirror_x);
                }
            }
            out.push_str("  </g>\n");
        }
    }

    if cfg.show_hotspots
        && let Some(report) = drc
        && !report.hotspots.is_empty()
    {
        out.push_str("  <g id=\"drc_hotspots\">\n");
        for (i, hs) in report.hotspots.iter().enumerate() {
            let (cx, cy) = svg_pt(hs.centroid_nm, mirror_x);
            let r = nm_to_mm(hs.radius_nm.max(600_000));
            let _ = writeln!(
                out,
                r##"  <circle cx="{cx:.4}" cy="{cy:.4}" r="{r:.4}" fill="rgba(239,68,68,0.18)" stroke="#ef4444" stroke-width="0.16" />"##
            );
            let label = format!("#{} ({}) {}", i + 1, hs.violation_count, hs.root_cause_item);
            let _ = writeln!(
                out,
                r##"  <text x="{:.4}" y="{:.4}" fill="#fca5a5" font-family="monospace" font-size="0.80">{}</text>"##,
                cx + r + 0.2,
                cy,
                xml_escape(&label)
            );
        }
        out.push_str("  </g>\n");
    }

    out.push_str("</svg>\n");
    out
}

/// Convenience wrapper rendering a single layer SVG with optional DRC hotspot overlay.
pub fn render_layer_svg(board: &BoardIr, layer: &str, drc: Option<&DrcReport>) -> String {
    render_board_svg(
        board,
        drc,
        &SvgOptions {
            view: SvgView::Layer(layer.to_string()),
            ..Default::default()
        },
    )
}

/// Writes Top, Bottom, and per-copper-layer SVGs into `out_dir` and returns their paths.
pub fn write_board_svgs(
    board: &BoardIr,
    drc: Option<&DrcReport>,
    out_dir: &Path,
) -> Result<Vec<PathBuf>, String> {
    fs::create_dir_all(out_dir)
        .map_err(|e| format!("failed to create svg dir '{}': {e}", out_dir.display()))?;
    let slug = safe_slug(&board.name);
    let mut written = Vec::new();

    for (suffix, view) in [("top", SvgView::Top), ("bottom", SvgView::Bottom)] {
        let path = out_dir.join(format!("{slug}_{suffix}.svg"));
        let svg = render_board_svg(
            board,
            drc,
            &SvgOptions {
                view,
                ..Default::default()
            },
        );
        fs::write(&path, svg).map_err(|e| format!("failed to write '{}': {e}", path.display()))?;
        written.push(path);
    }

    for layer in board.copper_layer_names() {
        let path = out_dir.join(format!("{slug}_{}.svg", safe_slug(&layer)));
        let svg = render_layer_svg(board, &layer, drc);
        fs::write(&path, svg).map_err(|e| format!("failed to write '{}': {e}", path.display()))?;
        written.push(path);
    }

    Ok(written)
}
