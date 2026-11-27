use std::collections::BTreeMap;

use clipper2_rust::{
    ClipType, Clipper64, FillRule, Paths64, Point64, difference_64, union_subjects_64,
};
use gerber_types::Polarity;

use crate::fab_gerber::GerberBuilder;
use crate::geom::{
    EPS_NM, ensure_ccw, local_to_board, polygon_area, polygon_violates_clearance, pts_to_path64,
    remap_layer_for_side, sin_cos_deg,
};
use crate::ir::{BBoxNm, BoardIr, Graphic, Nm, Placement, PourPoly, Pt, Side};
use crate::shapes::{ResolvedShape, circle_poly};
use crate::zone_scene::{paths_to_pour_polys, pour_poly_area, shape_inflated_poly};

const PIN1_DOT_RADIUS_NM: Nm = 120_000;
const MIN_CLIPPED_SEG_NM: f64 = 50_000.0;
const MIN_FILLED_AREA_NM2: f64 = 50_000.0 * 50_000.0;

pub(crate) fn is_silk_layer(layer: &str, side: Side) -> bool {
    match side {
        Side::Top => matches!(layer, "F.Silk" | "F.SilkS" | "F.Silkscreen"),
        Side::Bottom => matches!(layer, "B.Silk" | "B.SilkS" | "B.Silkscreen"),
    }
}

pub(crate) fn is_bottom_layer(layer: &str) -> bool {
    layer.starts_with("B.")
}

pub(crate) fn text_graphic_local_segments(g: &Graphic) -> Option<(Vec<(Pt, Pt)>, Nm)> {
    let Graphic::Text {
        layer,
        text,
        pos,
        height_nm,
        stroke_nm,
        rot_deg,
    } = g
    else {
        return None;
    };
    let segs =
        text_stroke_segments_transformed(text, *pos, *height_nm, *rot_deg, is_bottom_layer(layer));
    Some((segs, (*stroke_nm).max(10_000)))
}

pub(crate) fn text_graphic_world_segments(
    g: &Graphic,
    pl: Option<&Placement>,
    out_segs: &mut Vec<(Pt, Pt, Nm)>,
) {
    let Some((segs, w)) = text_graphic_local_segments(g) else {
        return;
    };
    for (a, b) in segs {
        let (wa, wb) = match pl {
            Some(pl) => (
                local_to_board(a, pl.pos.x, pl.pos.y, pl.rot_deg, pl.side),
                local_to_board(b, pl.pos.x, pl.pos.y, pl.rot_deg, pl.side),
            ),
            None => (a, b),
        };
        out_segs.push((wa, wb, w));
    }
}

/// Decomposes a graphic into world-space stroke segments (unfilled) or a `ResolvedShape` (filled).
pub(crate) fn graphic_world_geom(
    g: &Graphic,
    pl: Option<&Placement>,
    out_segs: &mut Vec<(Pt, Pt, Nm)>,
) -> Option<ResolvedShape> {
    let map_pt = |p: Pt| match pl {
        Some(pl) => local_to_board(p, pl.pos.x, pl.pos.y, pl.rot_deg, pl.side),
        None => p,
    };
    match g {
        Graphic::Line { a, b, width, .. } => {
            out_segs.push((map_pt(*a), map_pt(*b), *width));
            None
        }
        Graphic::Rect {
            min,
            max,
            width,
            fill,
            ..
        } => {
            let c = [
                map_pt(*min),
                map_pt(Pt::new(max.x, min.y)),
                map_pt(*max),
                map_pt(Pt::new(min.x, max.y)),
            ];
            if *fill {
                Some(ResolvedShape::polygon(c.to_vec()))
            } else {
                for i in 0..4 {
                    out_segs.push((c[i], c[(i + 1) % 4], *width));
                }
                None
            }
        }
        Graphic::Circle {
            center,
            radius,
            width,
            fill,
            ..
        } => {
            if *radius <= 0 {
                return None;
            }
            let c = map_pt(*center);
            if *fill {
                Some(ResolvedShape::Circle {
                    center: c,
                    radius: *radius,
                })
            } else {
                let ring = circle_poly(c, *radius, 32);
                for i in 0..ring.len() {
                    out_segs.push((ring[i], ring[(i + 1) % ring.len()], *width));
                }
                None
            }
        }
        Graphic::Poly {
            verts, width, fill, ..
        } => {
            let pts: Vec<Pt> = verts.iter().map(|&v| map_pt(v)).collect();
            if *fill {
                (pts.len() >= 3).then(|| ResolvedShape::polygon(pts))
            } else {
                if pts.len() >= 2 {
                    for i in 0..pts.len() {
                        out_segs.push((pts[i], pts[(i + 1) % pts.len()], *width));
                    }
                }
                None
            }
        }
        Graphic::Text { .. } => {
            text_graphic_world_segments(g, pl, out_segs);
            None
        }
    }
}

pub(crate) fn push_graphic_mask_shapes(
    g: &Graphic,
    pl: Option<&Placement>,
    out: &mut Vec<ResolvedShape>,
) {
    let mut segs = Vec::new();
    if let Some(sh) = graphic_world_geom(g, pl, &mut segs) {
        out.push(sh);
    }
    for (a, b, w) in segs {
        out.push(ResolvedShape::Capsule {
            a,
            b,
            radius: w.max(10_000) / 2,
        });
    }
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

pub(crate) fn text_stroke_segments_transformed(
    text: &str,
    center_local: Pt,
    char_height_nm: Nm,
    rot_deg: f64,
    mirror_x: bool,
) -> Vec<(Pt, Pt)> {
    if char_height_nm <= 0 || !rot_deg.is_finite() {
        return Vec::new();
    }
    let lines: Vec<Vec<char>> = text
        .split('\n')
        .map(|l| {
            l.trim_end_matches('\r')
                .chars()
                .filter(|c| !c.is_control())
                .collect::<Vec<_>>()
        })
        .collect();
    if lines.iter().all(|l| l.is_empty()) {
        return Vec::new();
    }
    let h = char_height_nm as f64;
    let scale_x = h * 0.65 / 4.0;
    let scale_y = h / 6.0;
    let char_w = 4.0 * scale_x;
    let advance = char_w * 1.45;
    let line_pitch = h * 1.5;
    let total_h = (lines.len() - 1) as f64 * line_pitch + h;
    let (s, c) = sin_cos_deg(rot_deg);
    let x_sign = if mirror_x { -1.0 } else { 1.0 };

    let to_pt = |lx: f64, ly: f64| {
        let mx = lx * x_sign;
        let rx = mx * c - ly * s;
        let ry = mx * s + ly * c;
        Pt::new(
            center_local.x + rx.round() as Nm,
            center_local.y + ry.round() as Nm,
        )
    };

    let mut segs = Vec::new();
    for (line_idx, chars) in lines.iter().enumerate() {
        if chars.is_empty() {
            continue;
        }
        let total_w = (chars.len() as f64 - 1.0) * advance + char_w;
        let x0 = -total_w * 0.5;
        let y0 = total_h * 0.5 - h - (line_idx as f64) * line_pitch;
        for (i, ch) in chars.iter().enumerate() {
            let cx = x0 + i as f64 * advance;
            for poly in glyph_strokes(*ch) {
                for w in poly.windows(2) {
                    let a = to_pt(cx + w[0].0 as f64 * scale_x, y0 + w[0].1 as f64 * scale_y);
                    let b = to_pt(cx + w[1].0 as f64 * scale_x, y0 + w[1].1 as f64 * scale_y);
                    segs.push((a, b));
                }
            }
        }
    }
    segs
}

pub(crate) fn text_stroke_segments(
    text: &str,
    center_local: Pt,
    char_height_nm: Nm,
) -> Vec<(Pt, Pt)> {
    text_stroke_segments_transformed(text, center_local, char_height_nm, 0.0, false)
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

fn point_hits_mask(p: Pt, radius: Nm, openings: &[ResolvedShape], clr: Nm) -> bool {
    let pad = radius + clr;
    let sb = BBoxNm::from_pt(p, pad);
    openings
        .iter()
        .any(|op| op.bbox().intersects(sb) && op.dist_to_circle(p, pad) < 0.0)
}

fn shape_hits_opening(sh: &ResolvedShape, op: &ResolvedShape, clr: Nm) -> bool {
    if !op.bbox().expand(clr).intersects(sh.bbox()) {
        return false;
    }
    if clr == 0
        && let (ResolvedShape::Poly { verts: a, .. }, ResolvedShape::Poly { verts: b, .. }) =
            (op, sh)
    {
        return polygon_violates_clearance(a, b, 0);
    }
    op.dist_to_shape(sh) < clr as f64 - EPS_NM
}

fn clip_filled_shape(
    shape: &ResolvedShape,
    openings: &[ResolvedShape],
    clr: Nm,
) -> Option<Vec<PourPoly>> {
    let mut clip = Paths64::new();
    for op in openings {
        if shape_hits_opening(shape, op, clr) {
            let mut poly = shape_inflated_poly(op, clr);
            if poly.len() >= 3 {
                ensure_ccw(&mut poly);
                clip.push(pts_to_path64(&poly));
            }
        }
    }
    if clip.is_empty() {
        return None;
    }
    let mut subj_pts = shape.to_polygon(16);
    if subj_pts.len() < 3 {
        return Some(Vec::new());
    }
    ensure_ccw(&mut subj_pts);
    let diff = difference_64(&vec![pts_to_path64(&subj_pts)], &clip, FillRule::NonZero);
    let mut polys = paths_to_pour_polys(&diff);
    polys.retain(|p| pour_poly_area(p) >= MIN_FILLED_AREA_NM2);
    Some(polys)
}

fn emit_clipped_open_path(builder: &mut GerberBuilder, path: &[Point64], width: Nm) {
    if path.len() < 2 {
        return;
    }
    let mut start = Pt::new(path[0].x, path[0].y);
    let mut prev = Pt::new(path[1].x, path[1].y);
    for pt in &path[2..] {
        let cur = Pt::new(pt.x, pt.y);
        let dx1 = (prev.x - start.x) as i128;
        let dy1 = (prev.y - start.y) as i128;
        let dx2 = (cur.x - prev.x) as i128;
        let dy2 = (cur.y - prev.y) as i128;
        if dx1 * dy2 == dy1 * dx2 && dx1 * dx2 + dy1 * dy2 >= 0 {
            prev = cur;
        } else {
            if start.dist(prev) >= MIN_CLIPPED_SEG_NM {
                builder.stroke_segment(start, prev, width);
            }
            start = prev;
            prev = cur;
        }
    }
    if start.dist(prev) >= MIN_CLIPPED_SEG_NM {
        builder.stroke_segment(start, prev, width);
    }
}

fn emit_clipped_strokes(
    builder: &mut GerberBuilder,
    raw_segs: Vec<(Pt, Pt, Nm)>,
    openings: &[ResolvedShape],
    clr: Nm,
) {
    let mut by_width: BTreeMap<Nm, Vec<(Pt, Pt)>> = BTreeMap::new();
    for (a, b, width) in raw_segs {
        let w = width.max(10_000);
        let r = w / 2;
        if a == b {
            if !point_hits_mask(a, r, openings, clr) {
                builder.stroke_segment(a, b, w);
            }
            continue;
        }
        let delta = r + clr;
        let sb = BBoxNm::from_pts(&[a, b]).expand(delta + 2);
        let hits = openings
            .iter()
            .any(|op| op.bbox().intersects(sb) && op.dist_to_capsule(a, b, delta) < EPS_NM);
        if hits {
            by_width.entry(w).or_default().push((a, b));
        } else {
            builder.stroke_segment(a, b, w);
        }
    }

    for (w, segs) in by_width {
        let delta = w / 2 + clr;
        let mut group_bb = BBoxNm::from_pts(&[segs[0].0, segs[0].1]);
        for &(a, b) in &segs[1..] {
            group_bb = group_bb.union(BBoxNm::from_pts(&[a, b]));
        }
        let query_bb = group_bb.expand(delta + 2_000);
        let mut raw_clips = Paths64::new();
        for op in openings {
            if op.bbox().intersects(query_bb) {
                let mut poly = shape_inflated_poly(op, delta);
                if poly.len() >= 3 {
                    ensure_ccw(&mut poly);
                    raw_clips.push(pts_to_path64(&poly));
                }
            }
        }
        // Union overlapping clips first so shared interior edges never confuse open-path difference.
        let clips = union_subjects_64(&raw_clips, FillRule::NonZero);
        let open_subjects: Paths64 = segs
            .iter()
            .map(|&(a, b)| vec![Point64::new(a.x, a.y), Point64::new(b.x, b.y)])
            .collect();
        let mut clipper = Clipper64::new();
        clipper.add_open_subject(&open_subjects);
        clipper.add_clip(&clips);
        let mut closed_out = Paths64::new();
        let mut open_out = Paths64::new();
        clipper.execute(
            ClipType::Difference,
            FillRule::NonZero,
            &mut closed_out,
            Some(&mut open_out),
        );
        for path in &open_out {
            emit_clipped_open_path(builder, path, w);
        }
    }
}

pub(crate) fn emit_silk_layer(
    builder: &mut GerberBuilder,
    board: &BoardIr,
    side: Side,
    mask_openings: &[ResolvedShape],
) {
    let clr = board.rules.silk_clearance_nm.max(0);
    let mut raw_segs: Vec<(Pt, Pt, Nm)> = Vec::new();
    let mut filled_graphics: Vec<(&Graphic, Option<&Placement>, ResolvedShape)> = Vec::new();
    let mut pin1_dots: Vec<Pt> = Vec::new();

    for pl in &board.placements {
        let Some(fp) = board.footprints.get(&pl.footprint) else {
            continue;
        };
        let mut has_silk_dot = false;
        for g in &fp.graphics {
            let layer = remap_layer_for_side(g.layer(), pl.side);
            if !is_silk_layer(&layer, side) {
                continue;
            }
            if matches!(g, Graphic::Circle { fill: true, .. }) {
                has_silk_dot = true;
            }
            if let Some(sh) = graphic_world_geom(g, Some(pl), &mut raw_segs) {
                filled_graphics.push((g, Some(pl), sh));
            }
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
                pin1_dots.push(local_to_board(
                    dot_local, pl.pos.x, pl.pos.y, pl.rot_deg, pl.side,
                ));
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
        if is_silk_layer(g.layer(), side)
            && let Some(sh) = graphic_world_geom(g, None, &mut raw_segs)
        {
            filled_graphics.push((g, None, sh));
        }
    }

    // Emit clipped filled regions (with Clear holes) first so Clear polarity never erases strokes or dots.
    let mut clipped_polys: Vec<PourPoly> = Vec::new();
    let mut unclipped_filled: Vec<(&Graphic, Option<&Placement>)> = Vec::new();
    for (g, pl, sh) in filled_graphics {
        if !mask_openings.is_empty()
            && let Some(polys) = clip_filled_shape(&sh, mask_openings, clr)
        {
            clipped_polys.extend(polys);
        } else {
            unclipped_filled.push((g, pl));
        }
    }
    clipped_polys.sort_by(|a, b| polygon_area(&b.outer).total_cmp(&polygon_area(&a.outer)));
    for poly in &clipped_polys {
        builder.fill_region(&poly.outer, Polarity::Dark);
        for hole in &poly.holes {
            builder.fill_region(hole, Polarity::Clear);
        }
    }
    for (g, pl) in unclipped_filled {
        builder.emit_graphic(g, pl);
    }

    for dot_world in pin1_dots {
        if mask_openings.is_empty()
            || !point_hits_mask(dot_world, PIN1_DOT_RADIUS_NM, mask_openings, clr)
        {
            builder.flash_circle(dot_world, PIN1_DOT_RADIUS_NM * 2);
        }
    }

    if mask_openings.is_empty() {
        for (a, b, w) in raw_segs {
            builder.stroke_segment(a, b, w);
        }
    } else {
        emit_clipped_strokes(builder, raw_segs, mask_openings, clr);
    }
}
