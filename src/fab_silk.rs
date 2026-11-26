use crate::fab_gerber::{GerberBuilder, pad_world_shape_with_margin};
use crate::geom::{EPS_NM, local_to_board, pad_world_layers, remap_layer_for_side};
use crate::ir::{BBoxNm, BoardIr, Graphic, Nm, Placement, Pt, Side};
use crate::shapes::ResolvedShape;

pub(crate) fn is_silk_layer(layer: &str, side: Side) -> bool {
    match side {
        Side::Top => matches!(layer, "F.Silk" | "F.SilkS" | "F.Silkscreen"),
        Side::Bottom => matches!(layer, "B.Silk" | "B.SilkS" | "B.Silkscreen"),
    }
}

pub(crate) fn collect_mask_openings(
    board: &BoardIr,
    side: Side,
    cu_layers: &[String],
) -> Vec<ResolvedShape> {
    let mask_name = match side {
        Side::Top => "F.Mask",
        Side::Bottom => "B.Mask",
    };
    let cu_name = side.cu_layer();
    let mut openings = Vec::new();
    for pl in &board.placements {
        let Some(fp) = board.footprints.get(&pl.footprint) else {
            continue;
        };
        for pad in &fp.pads {
            if !pad_world_layers(pad, pl.side, cu_layers)
                .iter()
                .any(|l| l == mask_name)
            {
                continue;
            }
            let margin = pad.mask_margin_nm.unwrap_or(board.rules.mask_expansion_nm);
            if let Some(shape) = pad_world_shape_with_margin(pad, pl, margin) {
                openings.push(shape);
            }
        }
    }
    for via in &board.vias {
        if !via.tented && via.touches_layer(cu_name, &board.layers) {
            let r = via.diameter / 2 + board.rules.mask_expansion_nm;
            if r > 0 {
                openings.push(ResolvedShape::Circle {
                    center: via.pos,
                    radius: r,
                });
            }
        }
    }
    openings
}

/// Returns a 5x7 stroke polyline glyph (coordinates in `[0..=4] x [0..=6]`) for ASCII chars.
fn glyph_strokes(ch: char) -> &'static [&'static [(u8, u8)]] {
    match ch.to_ascii_uppercase() {
        '0' => &[
            &[
                (0, 1),
                (0, 5),
                (1, 6),
                (3, 6),
                (4, 5),
                (4, 1),
                (3, 0),
                (1, 0),
                (0, 1),
            ],
            &[(0, 1), (4, 5)],
        ],
        '1' => &[&[(1, 5), (2, 6), (2, 0)], &[(1, 0), (3, 0)]],
        '2' => &[&[(0, 5), (1, 6), (3, 6), (4, 5), (4, 4), (0, 0), (4, 0)]],
        '3' => &[
            &[(0, 5), (1, 6), (3, 6), (4, 5), (4, 4), (3, 3), (1, 3)],
            &[(3, 3), (4, 2), (4, 1), (3, 0), (1, 0), (0, 1)],
        ],
        '4' => &[&[(3, 0), (3, 6), (0, 2), (4, 2)]],
        '5' => &[&[
            (4, 6),
            (0, 6),
            (0, 3),
            (3, 3),
            (4, 2),
            (4, 1),
            (3, 0),
            (1, 0),
            (0, 1),
        ]],
        '6' => &[&[
            (3, 6),
            (1, 6),
            (0, 5),
            (0, 1),
            (1, 0),
            (3, 0),
            (4, 1),
            (4, 2),
            (3, 3),
            (0, 3),
        ]],
        '7' => &[&[(0, 6), (4, 6), (1, 0)]],
        '8' => &[&[
            (1, 3),
            (0, 4),
            (0, 5),
            (1, 6),
            (3, 6),
            (4, 5),
            (4, 4),
            (3, 3),
            (1, 3),
            (0, 2),
            (0, 1),
            (1, 0),
            (3, 0),
            (4, 1),
            (4, 2),
            (3, 3),
        ]],
        '9' => &[&[
            (4, 3),
            (1, 3),
            (0, 4),
            (0, 5),
            (1, 6),
            (3, 6),
            (4, 5),
            (4, 1),
            (3, 0),
            (1, 0),
        ]],
        'A' => &[&[(0, 0), (0, 4), (2, 6), (4, 4), (4, 0)], &[(0, 2), (4, 2)]],
        'B' => &[
            &[(0, 0), (0, 6), (3, 6), (4, 5), (4, 4), (3, 3), (0, 3)],
            &[(3, 3), (4, 2), (4, 1), (3, 0), (0, 0)],
        ],
        'C' => &[&[
            (4, 5),
            (3, 6),
            (1, 6),
            (0, 5),
            (0, 1),
            (1, 0),
            (3, 0),
            (4, 1),
        ]],
        'D' => &[&[(0, 0), (0, 6), (3, 6), (4, 5), (4, 1), (3, 0), (0, 0)]],
        'E' => &[&[(4, 6), (0, 6), (0, 0), (4, 0)], &[(0, 3), (3, 3)]],
        'F' => &[&[(4, 6), (0, 6), (0, 0)], &[(0, 3), (3, 3)]],
        'G' => &[&[
            (4, 5),
            (3, 6),
            (1, 6),
            (0, 5),
            (0, 1),
            (1, 0),
            (3, 0),
            (4, 1),
            (4, 3),
            (2, 3),
        ]],
        'H' => &[&[(0, 0), (0, 6)], &[(4, 0), (4, 6)], &[(0, 3), (4, 3)]],
        'I' => &[&[(1, 6), (3, 6)], &[(2, 6), (2, 0)], &[(1, 0), (3, 0)]],
        'J' => &[&[(4, 6), (4, 1), (3, 0), (1, 0), (0, 1)]],
        'K' => &[&[(0, 0), (0, 6)], &[(4, 6), (0, 3), (4, 0)]],
        'L' => &[&[(0, 6), (0, 0), (4, 0)]],
        'M' => &[&[(0, 0), (0, 6), (2, 3), (4, 6), (4, 0)]],
        'N' => &[&[(0, 0), (0, 6), (4, 0), (4, 6)]],
        'O' => &[&[
            (1, 0),
            (0, 1),
            (0, 5),
            (1, 6),
            (3, 6),
            (4, 5),
            (4, 1),
            (3, 0),
            (1, 0),
        ]],
        'P' => &[&[(0, 0), (0, 6), (3, 6), (4, 5), (4, 4), (3, 3), (0, 3)]],
        'Q' => &[
            &[
                (1, 0),
                (0, 1),
                (0, 5),
                (1, 6),
                (3, 6),
                (4, 5),
                (4, 1),
                (3, 0),
                (1, 0),
            ],
            &[(2, 2), (4, 0)],
        ],
        'R' => &[
            &[(0, 0), (0, 6), (3, 6), (4, 5), (4, 4), (3, 3), (0, 3)],
            &[(2, 3), (4, 0)],
        ],
        'S' => &[&[
            (4, 5),
            (3, 6),
            (1, 6),
            (0, 5),
            (0, 4),
            (4, 2),
            (4, 1),
            (3, 0),
            (1, 0),
            (0, 1),
        ]],
        'T' => &[&[(0, 6), (4, 6)], &[(2, 6), (2, 0)]],
        'U' => &[&[(0, 6), (0, 1), (1, 0), (3, 0), (4, 1), (4, 6)]],
        'V' => &[&[(0, 6), (2, 0), (4, 6)]],
        'W' => &[&[(0, 6), (1, 0), (2, 3), (3, 0), (4, 6)]],
        'X' => &[&[(0, 6), (4, 0)], &[(0, 0), (4, 6)]],
        'Y' => &[&[(0, 6), (2, 3), (4, 6)], &[(2, 3), (2, 0)]],
        'Z' => &[&[(0, 6), (4, 6), (0, 0), (4, 0)]],
        '-' => &[&[(0, 3), (4, 3)]],
        '_' => &[&[(0, 0), (4, 0)]],
        '.' => &[&[(2, 0), (2, 1)]],
        '+' => &[&[(0, 3), (4, 3)], &[(2, 1), (2, 5)]],
        '/' => &[&[(0, 0), (4, 6)]],
        ':' => &[&[(2, 1), (2, 2)], &[(2, 4), (2, 5)]],
        '#' => &[
            &[(1, 0), (1, 6)],
            &[(3, 0), (3, 6)],
            &[(0, 2), (4, 2)],
            &[(0, 4), (4, 4)],
        ],
        _ => &[],
    }
}

pub(crate) fn text_stroke_segments(
    text: &str,
    center_local: Pt,
    char_height_nm: Nm,
) -> Vec<(Pt, Pt)> {
    let chars: Vec<char> = text.chars().filter(|c| !c.is_control()).collect();
    if chars.is_empty() {
        return Vec::new();
    }
    let scale_x = (char_height_nm as f64) * 0.65 / 4.0;
    let scale_y = (char_height_nm as f64) / 6.0;
    let char_w = 4.0 * scale_x;
    let advance = char_w * 1.45;
    let total_w = (chars.len() as f64 - 1.0) * advance + char_w;
    let x0 = center_local.x as f64 - total_w * 0.5;
    let y0 = center_local.y as f64 - (char_height_nm as f64) * 0.5;

    let mut segs = Vec::new();
    for (i, ch) in chars.iter().enumerate() {
        let cx = x0 + i as f64 * advance;
        for poly in glyph_strokes(*ch) {
            for w in poly.windows(2) {
                let a = Pt::new(
                    (cx + w[0].0 as f64 * scale_x).round() as Nm,
                    (y0 + w[0].1 as f64 * scale_y).round() as Nm,
                );
                let b = Pt::new(
                    (cx + w[1].0 as f64 * scale_x).round() as Nm,
                    (y0 + w[1].1 as f64 * scale_y).round() as Nm,
                );
                segs.push((a, b));
            }
        }
    }
    segs
}

fn segment_hits_mask(a: Pt, b: Pt, radius: Nm, openings: &[ResolvedShape], clr: Nm) -> bool {
    let req = (radius + clr) as f64;
    let sb = BBoxNm::from_pts(&[a, b]).expand(radius + clr);
    openings.iter().any(|op| {
        if !op.bbox().intersects(sb) {
            return false;
        }
        if a == b {
            op.dist_to_point(a) < req
        } else {
            op.dist_to_capsule(a, b, radius + clr) < 0.0
        }
    })
}

fn point_hits_mask(p: Pt, radius: Nm, openings: &[ResolvedShape], clr: Nm) -> bool {
    let sb = BBoxNm::from_pt(p, radius + clr);
    openings
        .iter()
        .any(|op| op.bbox().intersects(sb) && op.dist_to_circle(p, radius + clr) < 0.0)
}

fn emit_clipped_segment(
    builder: &mut GerberBuilder,
    a: Pt,
    b: Pt,
    width: Nm,
    openings: &[ResolvedShape],
    clr: Nm,
    depth: u8,
) {
    let r = (width / 2).max(10_000);
    if !segment_hits_mask(a, b, r, openings, clr) {
        builder.stroke_segment(a, b, width);
        return;
    }
    if depth >= 3 || a.dist(b) < (width.max(100_000) as f64) {
        return;
    }
    let mid = Pt::new((a.x + b.x) / 2, (a.y + b.y) / 2);
    emit_clipped_segment(builder, a, mid, width, openings, clr, depth + 1);
    emit_clipped_segment(builder, mid, b, width, openings, clr, depth + 1);
}

fn placement_local_bbox(fp: &crate::ir::FootprintDef) -> BBoxNm {
    if !fp.courtyard.is_empty() {
        return BBoxNm::from_pts(&fp.courtyard);
    }
    let mut pts = Vec::new();
    for pad in &fp.pads {
        pts.push(pad.pos);
    }
    if pts.is_empty() {
        BBoxNm::new(-500_000, -500_000, 500_000, 500_000)
    } else {
        BBoxNm::from_pts(&pts).expand(400_000)
    }
}

pub(crate) fn refdes_local_pos(board: &BoardIr, pl: &Placement) -> Pt {
    let Some(fp) = board.footprints.get(&pl.footprint) else {
        return Pt::new(0, 0);
    };
    let b = placement_local_bbox(fp);
    Pt::new((b.min_x + b.max_x) / 2, b.max_y + 450_000)
}

pub(crate) fn emit_silk_layer(
    builder: &mut GerberBuilder,
    board: &BoardIr,
    side: Side,
    cu_layers: &[String],
    clip_to_mask: bool,
) {
    let openings = if clip_to_mask {
        collect_mask_openings(board, side, cu_layers)
    } else {
        Vec::new()
    };
    let clr = board.rules.silk_clearance_nm;
    let mut raw_segs: Vec<(Pt, Pt, Nm)> = Vec::new();

    for pl in &board.placements {
        let Some(fp) = board.footprints.get(&pl.footprint) else {
            continue;
        };
        let mut has_silk_dot = false;
        for g in &fp.graphics {
            let layer = match g {
                Graphic::Line { layer, .. }
                | Graphic::Rect { layer, .. }
                | Graphic::Circle { layer, .. }
                | Graphic::Poly { layer, .. } => remap_layer_for_side(layer, pl.side),
            };
            if !is_silk_layer(&layer, side) {
                continue;
            }
            if matches!(g, Graphic::Circle { fill: true, .. }) {
                has_silk_dot = true;
            }
            collect_graphic_strokes_or_emit(
                builder,
                g,
                Some(pl),
                &openings,
                clr,
                clip_to_mask,
                &mut raw_segs,
            );
        }

        if pl.side == side && !pl.dnp {
            if !has_silk_dot && let Some(pad1) = fp.pads.first() {
                let b = placement_local_bbox(fp);
                let dx = if pad1.pos.x <= (b.min_x + b.max_x) / 2 {
                    -250_000
                } else {
                    250_000
                };
                let dy = if pad1.pos.y >= (b.min_y + b.max_y) / 2 {
                    250_000
                } else {
                    -250_000
                };
                let dot_local = Pt::new(b.min_x.min(pad1.pos.x) + dx, pad1.pos.y + dy);
                let dot_world = local_to_board(dot_local, pl.pos.x, pl.pos.y, pl.rot_deg, pl.side);
                let dot_r = 120_000;
                if !clip_to_mask || !point_hits_mask(dot_world, dot_r, &openings, clr) {
                    builder.flash_circle(dot_world, dot_r * 2);
                }
            }
            if !pl.refdes.is_empty() {
                let text_center = refdes_local_pos(board, pl);
                for (a, b) in text_stroke_segments(&pl.refdes, text_center, 700_000) {
                    let wa = local_to_board(a, pl.pos.x, pl.pos.y, pl.rot_deg, pl.side);
                    let wb = local_to_board(b, pl.pos.x, pl.pos.y, pl.rot_deg, pl.side);
                    raw_segs.push((wa, wb, 120_000));
                }
            }
        }
    }

    for g in &board.graphics {
        let layer = match g {
            Graphic::Line { layer, .. }
            | Graphic::Rect { layer, .. }
            | Graphic::Circle { layer, .. }
            | Graphic::Poly { layer, .. } => layer.as_str(),
        };
        if is_silk_layer(layer, side) {
            collect_graphic_strokes_or_emit(
                builder,
                g,
                None,
                &openings,
                clr,
                clip_to_mask,
                &mut raw_segs,
            );
        }
    }

    for (a, b, w) in raw_segs {
        if clip_to_mask {
            emit_clipped_segment(builder, a, b, w, &openings, clr, 0);
        } else {
            builder.stroke_segment(a, b, w);
        }
    }
}

fn collect_graphic_strokes_or_emit(
    builder: &mut GerberBuilder,
    g: &Graphic,
    pl: Option<&Placement>,
    openings: &[ResolvedShape],
    clr: Nm,
    clip_to_mask: bool,
    out_segs: &mut Vec<(Pt, Pt, Nm)>,
) {
    let map_pt = |p: Pt| match pl {
        Some(pl) => local_to_board(p, pl.pos.x, pl.pos.y, pl.rot_deg, pl.side),
        None => p,
    };
    match g {
        Graphic::Line { a, b, width, .. } => out_segs.push((map_pt(*a), map_pt(*b), *width)),
        Graphic::Rect {
            min,
            max,
            width,
            fill: false,
            ..
        } => {
            let c = [
                map_pt(*min),
                map_pt(Pt::new(max.x, min.y)),
                map_pt(*max),
                map_pt(Pt::new(min.x, max.y)),
            ];
            for i in 0..4 {
                out_segs.push((c[i], c[(i + 1) % 4], *width));
            }
        }
        Graphic::Circle {
            center,
            radius,
            width,
            fill: false,
            ..
        } => {
            let ring = crate::shapes::circle_poly(map_pt(*center), *radius, 24);
            for i in 0..ring.len() {
                out_segs.push((ring[i], ring[(i + 1) % ring.len()], *width));
            }
        }
        Graphic::Poly {
            verts,
            width,
            fill: false,
            ..
        } if verts.len() >= 2 => {
            let pts: Vec<Pt> = verts.iter().map(|&v| map_pt(v)).collect();
            for i in 0..pts.len() {
                out_segs.push((pts[i], pts[(i + 1) % pts.len()], *width));
            }
        }
        Graphic::Circle {
            center,
            radius,
            fill: true,
            ..
        } => {
            let c = map_pt(*center);
            if !clip_to_mask || !point_hits_mask(c, *radius, openings, clr) {
                builder.flash_circle(c, *radius * 2);
            }
        }
        other => {
            let shape_opt = match other {
                Graphic::Rect { min, max, .. } => Some(ResolvedShape::polygon(vec![
                    map_pt(*min),
                    map_pt(Pt::new(max.x, min.y)),
                    map_pt(*max),
                    map_pt(Pt::new(min.x, max.y)),
                ])),
                Graphic::Poly { verts, .. } if verts.len() >= 3 => Some(ResolvedShape::polygon(
                    verts.iter().map(|&v| map_pt(v)).collect(),
                )),
                _ => None,
            };
            let blocked = clip_to_mask
                && shape_opt.is_some_and(|sh| {
                    openings
                        .iter()
                        .any(|op| op.dist_to_shape(&sh) < clr as f64 - EPS_NM)
                });
            if !blocked {
                builder.emit_graphic(other, pl);
            }
        }
    }
}
