use std::collections::{BTreeMap, BTreeSet};

use clipper2_rust::{
    ClipType, Clipper64, FillRule, Paths64, Point64, difference_64, union_subjects_64,
};
use gerber_types::Polarity;

use crate::fab_gerber::GerberBuilder;
use crate::geom::{
    EPS_NM, GridQueryScratch, SpatialGrid, ensure_ccw, local_to_board, polygon_area,
    polygon_violates_clearance, pts_to_path64, remap_layer_for_side,
};
use crate::ir::{
    AutoRefdes, BBoxNm, BoardIr, FabConfig, FootprintDef, Graphic, Nm, Placement, PourPoly, Pt,
    Side,
};
use crate::shapes::{ResolvedShape, circle_poly};
use crate::stroke_text::{text_graphic_segments, text_stroke_segments};
use crate::zone_scene::{paths_to_pour_polys, pour_poly_area, shape_inflated_poly};

const PIN1_DOT_RADIUS_NM: Nm = 120_000;
const MIN_CLIPPED_SEG_NM: f64 = 50_000.0;
const MIN_FILLED_AREA_NM2: f64 = 50_000.0 * 50_000.0;

pub(crate) enum SilkElement<'a> {
    Graphic {
        pl: Option<&'a Placement>,
        idx: usize,
        graphic: &'a Graphic,
    },
    Pin1Dot {
        refdes: &'a str,
        center: Pt,
        radius: Nm,
    },
    Refdes {
        refdes: &'a str,
        segs: Vec<(Pt, Pt)>,
        stroke_nm: Nm,
    },
}

pub(crate) fn for_each_silk_element<'a>(
    board: &'a BoardIr,
    side: Side,
    fab: FabConfig,
    mut visit: impl FnMut(SilkElement<'a>),
) {
    for pl in &board.placements {
        let Some(fp) = board.footprints.get(&pl.footprint) else {
            continue;
        };
        let mut has_silk_dot = false;
        for (idx, graphic) in fp.graphics.iter().enumerate() {
            let layer = remap_layer_for_side(graphic.layer(), pl.side);
            if !is_silk_layer(&layer, side) {
                continue;
            }
            if matches!(graphic, Graphic::Circle { fill: true, .. }) {
                has_silk_dot = true;
            }
            visit(SilkElement::Graphic {
                pl: Some(pl),
                idx,
                graphic,
            });
        }
        if pl.side == side && !pl.dnp && fab.auto_refdes != AutoRefdes::Off {
            if !has_silk_dot && let Some(center) = pin1_dot_world(fp, pl) {
                visit(SilkElement::Pin1Dot {
                    refdes: &pl.refdes,
                    center,
                    radius: PIN1_DOT_RADIUS_NM,
                });
            }
            let segs = refdes_world_segments(fp, pl, fab.auto_refdes);
            if !segs.is_empty() {
                visit(SilkElement::Refdes {
                    refdes: &pl.refdes,
                    segs,
                    stroke_nm: fab.refdes_stroke_nm,
                });
            }
        }
    }
    for (idx, graphic) in board.graphics.iter().enumerate() {
        if is_silk_layer(graphic.layer(), side) {
            visit(SilkElement::Graphic {
                pl: None,
                idx,
                graphic,
            });
        }
    }
}

pub(crate) fn is_silk_layer(layer: &str, side: Side) -> bool {
    match side {
        Side::Top => matches!(layer, "F.Silk" | "F.SilkS" | "F.Silkscreen"),
        Side::Bottom => matches!(layer, "B.Silk" | "B.SilkS" | "B.Silkscreen"),
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
            if let Some((segs, w)) = text_graphic_segments(g) {
                for (a, b) in segs {
                    out_segs.push((map_pt(a), map_pt(b), w));
                }
            }
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

fn placement_local_bbox(fp: &FootprintDef) -> BBoxNm {
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

fn pin1_dot_world(fp: &FootprintDef, pl: &Placement) -> Option<Pt> {
    let pad1 = fp.pads.first()?;
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
    Some(local_to_board(
        dot_local, pl.pos.x, pl.pos.y, pl.rot_deg, pl.side,
    ))
}

fn needs_upright_flip(rot_deg: f64, side: Side) -> bool {
    let view_rot = match side {
        Side::Top => rot_deg.rem_euclid(360.0),
        Side::Bottom => (180.0 - rot_deg).rem_euclid(360.0),
    };
    view_rot > 90.0 + 1e-6 && view_rot <= 270.0 + 1e-6
}

pub(crate) fn refdes_world_segments(
    fp: &FootprintDef,
    pl: &Placement,
    mode: AutoRefdes,
) -> Vec<(Pt, Pt)> {
    if mode == AutoRefdes::Off || pl.refdes.is_empty() {
        return Vec::new();
    }
    let b = placement_local_bbox(fp);
    let local_center = Pt::new((b.min_x + b.max_x) / 2, b.max_y + 450_000);
    let world_center = local_to_board(local_center, pl.pos.x, pl.pos.y, pl.rot_deg, pl.side);
    let eff_rot = if mode == AutoRefdes::Upright && needs_upright_flip(pl.rot_deg, pl.side) {
        pl.rot_deg + 180.0
    } else {
        pl.rot_deg
    };
    text_stroke_segments(&pl.refdes, Pt::new(0, 0), 700_000, 0.0, false)
        .into_iter()
        .map(|(a, b)| {
            (
                local_to_board(a, world_center.x, world_center.y, eff_rot, pl.side),
                local_to_board(b, world_center.x, world_center.y, eff_rot, pl.side),
            )
        })
        .collect()
}

fn point_hits_mask(
    p: Pt,
    radius: Nm,
    openings: &[ResolvedShape],
    grid: &SpatialGrid,
    scratch: &mut GridQueryScratch,
    cands: &mut Vec<usize>,
    clr: Nm,
) -> bool {
    let pad = radius + clr;
    let sb = BBoxNm::from_pt(p, pad);
    grid.query_with(sb, scratch, cands);
    cands.iter().any(|&idx| {
        let op = &openings[idx];
        op.bbox().intersects(sb) && op.dist_to_circle(p, pad) < 0.0
    })
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
    grid: &SpatialGrid,
    scratch: &mut GridQueryScratch,
    cands: &mut Vec<usize>,
    clr: Nm,
) -> Option<Vec<PourPoly>> {
    grid.query_with(shape.bbox().expand(clr), scratch, cands);
    cands.sort_unstable();
    let mut clip = Paths64::new();
    for &idx in cands.iter() {
        let op = &openings[idx];
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

#[derive(Default)]
struct HitStrokeGroup {
    segs: Vec<(Pt, Pt)>,
    hit_openings: BTreeSet<usize>,
}

fn emit_clipped_strokes(
    builder: &mut GerberBuilder,
    raw_segs: Vec<(Pt, Pt, Nm)>,
    openings: &[ResolvedShape],
    grid: &SpatialGrid,
    scratch: &mut GridQueryScratch,
    cands: &mut Vec<usize>,
    clr: Nm,
) {
    let mut by_width: BTreeMap<Nm, HitStrokeGroup> = BTreeMap::new();
    for (a, b, width) in raw_segs {
        let w = width.max(10_000);
        let r = w / 2;
        if a == b {
            if !point_hits_mask(a, r, openings, grid, scratch, cands, clr) {
                builder.stroke_segment(a, b, w);
            }
            continue;
        }
        let delta = r + clr;
        let sb = BBoxNm::from_pts(&[a, b]).expand(delta + 2);
        grid.query_with(sb, scratch, cands);
        cands.retain(|&idx| {
            let op = &openings[idx];
            op.bbox().intersects(sb) && op.dist_to_capsule(a, b, delta) < EPS_NM
        });
        if cands.is_empty() {
            builder.stroke_segment(a, b, w);
        } else {
            let group = by_width.entry(w).or_default();
            group.segs.push((a, b));
            group.hit_openings.extend(cands.iter().copied());
        }
    }

    for (w, group) in by_width {
        let delta = w / 2 + clr;
        let mut raw_clips = Paths64::new();
        for idx in group.hit_openings {
            let mut poly = shape_inflated_poly(&openings[idx], delta);
            if poly.len() >= 3 {
                ensure_ccw(&mut poly);
                raw_clips.push(pts_to_path64(&poly));
            }
        }
        // Union overlapping clips first so shared interior edges never confuse open-path difference.
        let clips = union_subjects_64(&raw_clips, FillRule::NonZero);
        let open_subjects: Paths64 = group
            .segs
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
    fab: FabConfig,
) {
    let clr = board.rules.silk_clearance_nm.max(0);
    let mut raw_segs: Vec<(Pt, Pt, Nm)> = Vec::new();
    let mut filled_graphics: Vec<(&Graphic, Option<&Placement>, ResolvedShape)> = Vec::new();
    let mut pin1_dots: Vec<(Pt, Nm)> = Vec::new();

    for_each_silk_element(board, side, fab, |elem| match elem {
        SilkElement::Graphic { pl, graphic, .. } => {
            if let Some(sh) = graphic_world_geom(graphic, pl, &mut raw_segs) {
                filled_graphics.push((graphic, pl, sh));
            }
        }
        SilkElement::Pin1Dot { center, radius, .. } => pin1_dots.push((center, radius)),
        SilkElement::Refdes {
            segs, stroke_nm, ..
        } => {
            for (wa, wb) in segs {
                raw_segs.push((wa, wb, stroke_nm));
            }
        }
    });

    if mask_openings.is_empty() {
        for (g, pl, _) in filled_graphics {
            builder.emit_graphic(g, pl);
        }
        for (dot_world, radius) in pin1_dots {
            builder.flash_circle(dot_world, radius * 2);
        }
        for (a, b, w) in raw_segs {
            builder.stroke_segment(a, b, w);
        }
        return;
    }

    let grid = SpatialGrid::new(mask_openings.iter().map(ResolvedShape::bbox), 2_000_000);
    let mut scratch = GridQueryScratch::default();
    let mut cands = Vec::new();

    // Emit clipped filled regions (with Clear holes) first so Clear polarity never erases strokes or dots.
    let mut clipped_polys: Vec<PourPoly> = Vec::new();
    let mut unclipped_filled: Vec<(&Graphic, Option<&Placement>)> = Vec::new();
    for (g, pl, sh) in filled_graphics {
        if let Some(polys) =
            clip_filled_shape(&sh, mask_openings, &grid, &mut scratch, &mut cands, clr)
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

    for (dot_world, radius) in pin1_dots {
        if !point_hits_mask(
            dot_world,
            radius,
            mask_openings,
            &grid,
            &mut scratch,
            &mut cands,
            clr,
        ) {
            builder.flash_circle(dot_world, radius * 2);
        }
    }

    emit_clipped_strokes(
        builder,
        raw_segs,
        mask_openings,
        &grid,
        &mut scratch,
        &mut cands,
        clr,
    );
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ir::{LayerDef, LayerKind, mm};
    use gerber_types::FileFunction;

    #[test]
    fn hit_only_mask_inflation_leaves_distant_openings_untouched() {
        let mut board = BoardIr {
            name: "silk_hit_test".into(),
            title: None,
            revision: None,
            outline: vec![
                Pt::new(0, 0),
                Pt::new(mm(30.0), 0),
                Pt::new(mm(30.0), mm(10.0)),
                Pt::new(0, mm(10.0)),
            ],
            cutouts: Vec::new(),
            layers: vec![LayerDef {
                name: "F.Cu".into(),
                kind: LayerKind::Signal,
                side: Some(Side::Top),
                thickness_nm: None,
            }],
            footprints: BTreeMap::new(),
            placements: Vec::new(),
            keepouts: Vec::new(),
            pours: Vec::new(),
            filled_pours: Vec::new(),
            tracks: Vec::new(),
            vias: Vec::new(),
            net_classes: BTreeMap::new(),
            nets: BTreeMap::new(),
            rules: Default::default(),
            constraints: Default::default(),
            graphics: vec![
                Graphic::Line {
                    layer: "F.SilkS".into(),
                    a: Pt::new(mm(2.0), mm(5.0)),
                    b: Pt::new(mm(28.0), mm(5.0)),
                    width: mm(0.15),
                },
                Graphic::Line {
                    layer: "F.SilkS".into(),
                    a: Pt::new(mm(12.0), mm(2.0)),
                    b: Pt::new(mm(18.0), mm(2.0)),
                    width: mm(0.15),
                },
            ],
            stackup: Default::default(),
            fab: FabConfig {
                auto_refdes: AutoRefdes::Off,
                ..Default::default()
            },
        };
        board.rules.silk_clearance_nm = mm(0.1);

        let hit_left = ResolvedShape::Circle {
            center: Pt::new(mm(4.0), mm(5.0)),
            radius: mm(0.5),
        };
        let hit_right = ResolvedShape::Circle {
            center: Pt::new(mm(26.0), mm(5.0)),
            radius: mm(0.5),
        };
        // Inside Line 1's bbox, 2um outside clearance so circumscribed 32-gon would clip if inflated.
        let unhit_inside_bb = ResolvedShape::Circle {
            center: Pt::new(mm(15.0), mm(4.323)),
            radius: mm(0.5),
        };
        let distant_outside = ResolvedShape::Circle {
            center: Pt::new(mm(15.0), mm(9.0)),
            radius: mm(1.0),
        };

        let tmp =
            std::env::temp_dir().join(format!("boardc_silk_hit_test_{}.gbr", std::process::id()));
        let render = |openings: &[ResolvedShape]| -> String {
            let mut b = GerberBuilder::new(FileFunction::Legend {
                pos: gerber_types::Position::Top,
                index: None,
            });
            emit_silk_layer(&mut b, &board, Side::Top, openings, board.fab);
            b.write_file(&tmp).unwrap();
            let s = std::fs::read_to_string(&tmp).unwrap();
            let _ = std::fs::remove_file(&tmp);
            s
        };

        let with_hit_only = render(&[hit_left.clone(), hit_right.clone()]);
        let with_distant = render(&[hit_left, hit_right, unhit_inside_bb, distant_outside]);
        assert_eq!(with_hit_only, with_distant);
        assert!(with_hit_only.contains("D01*"));
    }
}
