use std::fmt::Write as _;

use crate::fab_silk::text_graphic_local_segments;
use crate::geom::{local_to_board, pad_copper_layers, remap_layer_for_side};
use crate::ir::{BBoxNm, BoardIr, FootprintDef, Graphic, PadKind, Placement, Pt, Side, nm_to_mm};
use crate::shapes::pad_to_world_shape;
use crate::svg::{
    begin_svg, board_svg_bounds, poly_path_d, render_graphic_svg, render_shape_svg,
    sanitize_margin_mm, svg_pt, write_board_outline, xml_escape,
};

const FALLBACK_FP_BBOX: BBoxNm = BBoxNm::new(-500_000, -500_000, 500_000, 500_000);

pub(crate) fn has_bottom_assembly(board: &BoardIr) -> bool {
    board.placements.iter().any(|pl| {
        pl.side == Side::Bottom
            || board.footprints.get(&pl.footprint).is_some_and(|fp| {
                fp.graphics.iter().any(|g| {
                    matches!(
                        remap_layer_for_side(g.layer(), pl.side).as_str(),
                        "B.Fab" | "B.CrtYd"
                    )
                })
            })
    }) || board
        .graphics
        .iter()
        .any(|g| matches!(g.layer(), "B.Fab" | "B.CrtYd"))
}

fn push_graphic_local_pts(g: &Graphic, pts: &mut Vec<Pt>) {
    match g {
        Graphic::Line { a, b, .. } => {
            pts.push(*a);
            pts.push(*b);
        }
        Graphic::Rect { min, max, .. } => {
            pts.push(*min);
            pts.push(*max);
        }
        Graphic::Circle { center, radius, .. } => {
            if *radius > 0 {
                pts.push(Pt::new(center.x - *radius, center.y - *radius));
                pts.push(Pt::new(center.x + *radius, center.y + *radius));
            }
        }
        Graphic::Poly { verts, .. } => pts.extend_from_slice(verts),
        Graphic::Text { pos, .. } => {
            if let Some((segs, _)) = text_graphic_local_segments(g)
                && !segs.is_empty()
            {
                for (a, b) in segs {
                    pts.push(a);
                    pts.push(b);
                }
            } else {
                pts.push(*pos);
            }
        }
    }
}

fn fp_layer_graphics_bbox(fp: &FootprintDef, pl_side: Side, target_layer: &str) -> Option<BBoxNm> {
    let mut pts = Vec::new();
    for g in &fp.graphics {
        if remap_layer_for_side(g.layer(), pl_side) == target_layer {
            push_graphic_local_pts(g, &mut pts);
        }
    }
    (!pts.is_empty()).then(|| BBoxNm::from_pts(&pts))
}

fn fp_courtyard_local_bbox(fp: &FootprintDef, pl_side: Side, crtyd_layer: &str) -> Option<BBoxNm> {
    let poly_bb = (!fp.courtyard.is_empty()).then(|| BBoxNm::from_pts(&fp.courtyard));
    let gfx_bb = fp_layer_graphics_bbox(fp, pl_side, crtyd_layer);
    match (poly_bb, gfx_bb) {
        (Some(a), Some(b)) => Some(a.union(b)),
        (a, b) => a.or(b),
    }
}

fn fp_pads_local_bbox(fp: &FootprintDef) -> Option<BBoxNm> {
    let origin = Placement::default();
    let mut iter = fp
        .pads
        .iter()
        .map(|p| pad_to_world_shape(p, &origin, 8).bbox());
    let first = iter.next()?;
    Some(iter.fold(first, BBoxNm::union))
}

fn fp_assembly_local_bbox(
    fp: &FootprintDef,
    pl_side: Side,
    fab_layer: &str,
    crtyd_layer: &str,
) -> BBoxNm {
    fp_layer_graphics_bbox(fp, pl_side, fab_layer)
        .or_else(|| fp_courtyard_local_bbox(fp, pl_side, crtyd_layer))
        .or_else(|| fp_pads_local_bbox(fp))
        .unwrap_or(FALLBACK_FP_BBOX)
}

fn fp_dnp_local_bbox(
    fp: &FootprintDef,
    pl_side: Side,
    fab_layer: &str,
    crtyd_layer: &str,
) -> BBoxNm {
    fp_courtyard_local_bbox(fp, pl_side, crtyd_layer)
        .unwrap_or_else(|| fp_assembly_local_bbox(fp, pl_side, fab_layer, crtyd_layer))
}

fn local_bbox_svg_corners(lb: BBoxNm, pl: &Placement, mirror_x: bool) -> [(f64, f64); 4] {
    [
        Pt::new(lb.min_x, lb.min_y),
        Pt::new(lb.max_x, lb.min_y),
        Pt::new(lb.max_x, lb.max_y),
        Pt::new(lb.min_x, lb.max_y),
    ]
    .map(|p| {
        svg_pt(
            local_to_board(p, pl.pos.x, pl.pos.y, pl.rot_deg, pl.side),
            mirror_x,
        )
    })
}

pub(crate) fn render_assembly_svg(board: &BoardIr, side: Side, margin_mm: f64) -> String {
    let margin = sanitize_margin_mm(margin_mm);
    let mirror_x = side == Side::Bottom;
    let cu_layers = board.copper_layer_names();
    let cu_layer = side.cu_layer();
    let (fab_layer, crtyd_layer, side_title) = match side {
        Side::Top => ("F.Fab", "F.CrtYd", "ASSEMBLY TOP"),
        Side::Bottom => ("B.Fab", "B.CrtYd", "ASSEMBLY BOTTOM (MIRRORED)"),
    };
    let (board_min_x, board_max_x, board_min_y, board_max_y) = board_svg_bounds(board, mirror_x);
    let board_w = (board_max_x - board_min_x).max(1.0);
    let header_h = (board_w * 0.045).clamp(1.6, 4.0);
    let title_text = format!("{} - {side_title}", board.name);
    let title_chars = title_text.chars().count().max(1) as f64;
    let title_fs = (header_h * 0.55)
        .min(board_w / (title_chars * 0.64))
        .clamp(0.35, 2.4);
    let title_w = title_chars * title_fs * 0.64;

    let min_x = board_min_x - margin;
    let max_x = board_max_x.max(board_min_x + title_w) + margin;
    let min_y = board_min_y - margin - header_h;
    let max_y = board_max_y + margin;
    let vw = (max_x - min_x).max(1.0);
    let vh = (max_y - min_y).max(1.0);

    let mut out = begin_svg(&title_text, min_x, min_y, vw, vh, "#ffffff");
    let _ = writeln!(
        out,
        r##"  <text x="{board_min_x:.4}" y="{:.4}" fill="#0f172a" font-family="monospace" font-size="{title_fs:.4}" font-weight="700">{}</text>"##,
        min_y + header_h * 0.72,
        xml_escape(&title_text)
    );
    write_board_outline(&mut out, board, mirror_x, "#f8fafc", "#111827", "0.15");

    // Subtle pad outlines on this side help verify pin-1 and orientation against component bodies.
    out.push_str("  <g id=\"pads\" opacity=\"0.85\">\n");
    for pl in &board.placements {
        let Some(fp) = board.footprints.get(&pl.footprint) else {
            continue;
        };
        for pad in &fp.pads {
            if pad.kind != PadKind::Npth
                && pad_copper_layers(pad, pl.side, &cu_layers)
                    .iter()
                    .any(|l| l == cu_layer)
            {
                let shape = pad_to_world_shape(pad, pl, 16);
                render_shape_svg(&mut out, &shape, "#e2e8f0", mirror_x);
            }
            if pad.kind != PadKind::Smd
                && let Some(drill) = pad.drill_nm
                && drill > 0
            {
                let pos = local_to_board(pad.pos, pl.pos.x, pl.pos.y, pl.rot_deg, pl.side);
                let (cx, cy) = svg_pt(pos, mirror_x);
                let _ = writeln!(
                    out,
                    r##"  <circle cx="{cx:.4}" cy="{cy:.4}" r="{:.4}" fill="#ffffff" stroke="#94a3b8" stroke-width="0.04" />"##,
                    nm_to_mm(drill / 2)
                );
            }
        }
    }
    out.push_str("  </g>\n");

    // Courtyard outlines (`F.CrtYd` / `B.CrtYd`).
    out.push_str("  <g id=\"courtyard\" stroke-dasharray=\"0.28,0.18\">\n");
    for pl in &board.placements {
        let Some(fp) = board.footprints.get(&pl.footprint) else {
            continue;
        };
        if pl.side == side && !fp.courtyard.is_empty() {
            let pts: Vec<Pt> = fp
                .courtyard
                .iter()
                .map(|&p| local_to_board(p, pl.pos.x, pl.pos.y, pl.rot_deg, pl.side))
                .collect();
            let d = poly_path_d(&pts, mirror_x);
            let _ = writeln!(
                out,
                r##"  <path d="{d}" fill="none" stroke="#64748b" stroke-width="0.06" stroke-linejoin="round" />"##
            );
        }
        for g in &fp.graphics {
            if remap_layer_for_side(g.layer(), pl.side) == crtyd_layer {
                render_graphic_svg(&mut out, g, Some(pl), "#64748b", mirror_x);
            }
        }
    }
    for g in &board.graphics {
        if g.layer() == crtyd_layer {
            render_graphic_svg(&mut out, g, None, "#64748b", mirror_x);
        }
    }
    out.push_str("  </g>\n");

    // Fabrication outlines (`F.Fab` / `B.Fab`).
    out.push_str("  <g id=\"fab\">\n");
    for pl in &board.placements {
        let Some(fp) = board.footprints.get(&pl.footprint) else {
            continue;
        };
        let mut has_dnp_fab = false;
        for g in &fp.graphics {
            if remap_layer_for_side(g.layer(), pl.side) != fab_layer {
                continue;
            }
            if pl.dnp {
                if !has_dnp_fab {
                    out.push_str("  <g stroke-dasharray=\"0.20,0.15\">\n");
                    has_dnp_fab = true;
                }
                render_graphic_svg(&mut out, g, Some(pl), "#94a3b8", mirror_x);
            } else {
                render_graphic_svg(&mut out, g, Some(pl), "#0f172a", mirror_x);
            }
        }
        if has_dnp_fab {
            out.push_str("  </g>\n");
        }
    }
    for g in &board.graphics {
        if g.layer() == fab_layer {
            render_graphic_svg(&mut out, g, None, "#0f172a", mirror_x);
        }
    }
    out.push_str("  </g>\n");

    // DNP crossout X marks across courtyard/body bbox.
    let mut dnp_opened = false;
    for pl in board
        .placements
        .iter()
        .filter(|pl| pl.side == side && pl.dnp)
    {
        if !dnp_opened {
            out.push_str(
                "  <g id=\"dnp\" stroke=\"#dc2626\" stroke-opacity=\"0.65\" stroke-width=\"0.08\">\n",
            );
            dnp_opened = true;
        }
        let lb = board
            .footprints
            .get(&pl.footprint)
            .map(|fp| fp_dnp_local_bbox(fp, pl.side, fab_layer, crtyd_layer))
            .unwrap_or(FALLBACK_FP_BBOX);
        let c = local_bbox_svg_corners(lb, pl, mirror_x);
        for (a, b) in [(c[0], c[2]), (c[1], c[3])] {
            let _ = writeln!(
                out,
                r#"  <line x1="{:.4}" y1="{:.4}" x2="{:.4}" y2="{:.4}" />"#,
                a.0, a.1, b.0, b.1
            );
        }
    }
    if dnp_opened {
        out.push_str("  </g>\n");
    }

    // Reference designators centered in each component body, always upright and readable.
    out.push_str(
        "  <g id=\"refdes\" font-family=\"monospace\" font-weight=\"600\" text-anchor=\"middle\" dominant-baseline=\"central\">\n",
    );
    for pl in &board.placements {
        if pl.side != side || pl.refdes.is_empty() {
            continue;
        }
        let lb = board
            .footprints
            .get(&pl.footprint)
            .map(|fp| fp_assembly_local_bbox(fp, pl.side, fab_layer, crtyd_layer))
            .unwrap_or(FALLBACK_FP_BBOX);
        let c = local_bbox_svg_corners(lb, pl, mirror_x);
        let (min_sx, max_sx, min_sy, max_sy) = c.iter().fold(
            (
                f64::INFINITY,
                f64::NEG_INFINITY,
                f64::INFINITY,
                f64::NEG_INFINITY,
            ),
            |(x0, x1, y0, y1), &(x, y)| (x0.min(x), x1.max(x), y0.min(y), y1.max(y)),
        );
        let sw = (max_sx - min_sx).max(0.5);
        let sh = (max_sy - min_sy).max(0.5);
        let (cx, cy) = svg_pt(
            local_to_board(lb.center(), pl.pos.x, pl.pos.y, pl.rot_deg, pl.side),
            mirror_x,
        );
        let chars = pl.refdes.chars().count();
        let vertical = sh > sw * 1.4 && chars >= 2;
        let (text_span, text_cap) = if vertical { (sh, sw) } else { (sw, sh) };
        let fs = ((text_span * 0.80) / (chars.max(1) as f64 * 0.62))
            .min(text_cap * 0.68)
            .clamp(0.30, 1.60);
        let (fill, strike) = if pl.dnp {
            ("#dc2626", r#" text-decoration="line-through""#)
        } else {
            ("#1d4ed8", "")
        };
        let _ = write!(
            out,
            r#"  <text x="{cx:.4}" y="{cy:.4}" fill="{fill}" font-size="{fs:.4}"{strike}"#
        );
        if vertical {
            let _ = write!(out, r#" transform="rotate(-90 {cx:.4} {cy:.4})""#);
        }
        let _ = writeln!(out, ">{}</text>", xml_escape(&pl.refdes));
    }
    out.push_str("  </g>\n");

    out.push_str("</svg>\n");
    out
}
