use std::f64::consts::PI;

use clipper2_rust::{
    ClipType, EndType, FillRule, JoinType, Path64, Paths64, PolyTree64, boolean_op_tree_64,
    difference_64, inflate_paths_64, intersect_64,
};

use crate::fab_gerber::pad_world_shape_with_margin;
use crate::geom::{
    ensure_ccw, local_to_board, pad_copper_layers, pad_to_world_shape, pad_world_layers,
    path64_to_pts, polygon_area, polygon_signed_area_2, pts_to_path64, remap_layer_for_side,
};
use crate::ir::{
    BBoxNm, BoardIr, FilledPour, KeepoutKind, Nm, PadKind, PourPoly, PourSpec, Pt, Side,
};
use crate::poly_index::IndexedPour;
use crate::shapes::{ResolvedShape, capsule_poly, circle_poly, roundrect_corners_poly};
use crate::zones::BlockerKind;

pub(crate) const ARC_TOLERANCE: f64 = 1_500.0;
const CLEARANCE_CIRCLE_SEGS: usize = 32;

pub(crate) struct Terminal {
    pub label: String,
    pub center: Pt,
    pub pts: Vec<Pt>,
    pub poly: Path64,
    pub bbox: BBoxNm,
    pub axes: (f64, f64),
    pub thermal_relief: bool,
}

pub(crate) struct Obstacle {
    pub kind: BlockerKind,
    pub net: String,
    pub label: String,
    pub pos: Pt,
    pub cleared_poly: Path64,
    pub cleared_bbox: BBoxNm,
}

pub(crate) struct LayerScene {
    pub terminals: Vec<Terminal>,
    pub obstacles: Vec<Obstacle>,
}

pub(crate) fn match_pour_specs<'a>(
    board: &'a BoardIr,
    filled: &[FilledPour],
) -> Vec<Option<&'a PourSpec>> {
    let mut used = vec![false; board.pours.len()];
    let mut matched = Vec::with_capacity(filled.len());
    for fp in filled {
        let found = board
            .pours
            .iter()
            .enumerate()
            .find(|(idx, sp)| {
                !used[*idx]
                    && sp.net == fp.net
                    && sp.layer == fp.layer
                    && sp.priority == fp.priority
            })
            .or_else(|| {
                board
                    .pours
                    .iter()
                    .enumerate()
                    .find(|(idx, sp)| !used[*idx] && sp.net == fp.net && sp.layer == fp.layer)
            });
        if let Some((idx, sp)) = found {
            used[idx] = true;
            matched.push(Some(sp));
        } else {
            matched.push(None);
        }
    }
    matched
}

// Circumscribe circles by 1/cos(pi/n) so chord midpoints never dip inside clearance radius.
pub(crate) fn circle_poly_outer(center: Pt, radius: Nm) -> Vec<Pt> {
    if radius <= 0 {
        return Vec::new();
    }
    let pad_r = (radius as f64 / (PI / CLEARANCE_CIRCLE_SEGS as f64).cos()).ceil() as Nm;
    circle_poly(center, pad_r, CLEARANCE_CIRCLE_SEGS)
}

pub(crate) fn inflate_round_outer(paths: &Paths64, delta: Nm) -> Paths64 {
    if delta <= 0 {
        return paths.clone();
    }
    inflate_paths_64(
        paths,
        delta as f64 + ARC_TOLERANCE + 2.0,
        JoinType::Round,
        EndType::Polygon,
        2.0,
        ARC_TOLERANCE,
    )
}

fn is_rect_4(verts: &[Pt]) -> bool {
    if verts.len() != 4 {
        return false;
    }
    let d01x = verts[1].x - verts[0].x;
    let d01y = verts[1].y - verts[0].y;
    let d12x = verts[2].x - verts[1].x;
    let d12y = verts[2].y - verts[1].y;
    let d23x = verts[3].x - verts[2].x;
    let d23y = verts[3].y - verts[2].y;
    let len01_sq = (d01x as f64) * (d01x as f64) + (d01y as f64) * (d01y as f64);
    let len12_sq = (d12x as f64) * (d12x as f64) + (d12y as f64) * (d12y as f64);
    if len01_sq <= 0.0 || len12_sq <= 0.0 {
        return false;
    }
    if (d01x + d23x).abs() > 4 || (d01y + d23y).abs() > 4 {
        return false;
    }
    let dot = (d01x as f64) * (d12x as f64) + (d01y as f64) * (d12y as f64);
    dot.abs() <= (len01_sq.sqrt() * len12_sq.sqrt() * 1e-4).max(1.0)
}

pub(crate) fn shape_inflated_poly(shape: &ResolvedShape, delta: Nm) -> Vec<Pt> {
    match shape {
        ResolvedShape::Circle { center, radius } => circle_poly_outer(*center, *radius + delta),
        ResolvedShape::Capsule { a, b, radius } => {
            let pad_r = ((*radius + delta) as f64 / (PI / 32.0).cos()).ceil() as Nm;
            capsule_poly(*a, *b, pad_r, 32)
        }
        ResolvedShape::RoundRect {
            corners, radius, ..
        } => {
            let r_exact = *radius + delta.max(0);
            let r_circ = if r_exact > 0 {
                (r_exact as f64 / (PI / (4.0 * 16.0)).cos()).ceil() as Nm + 1
            } else {
                0
            };
            roundrect_corners_poly(corners, r_circ, 16)
        }
        ResolvedShape::Poly { verts, .. } => {
            if delta <= 0 {
                return verts.clone();
            }
            if is_rect_4(verts) {
                let r_circ = (delta as f64 / (PI / 64.0).cos()).ceil() as Nm + 1;
                let corners = [verts[0], verts[1], verts[2], verts[3]];
                return roundrect_corners_poly(&corners, r_circ, 16);
            }
            let path = pts_to_path64(verts);
            let inflated = inflate_round_outer(&vec![path], delta);
            inflated.first().map(path64_to_pts).unwrap_or_default()
        }
    }
}

fn inflated_shape_intersects(shape: &ResolvedShape, delta: Nm, ob: BBoxNm) -> bool {
    let bb = shape.bbox();
    let half_extent = bb.width().max(bb.height()) / 2;
    let d = delta.max(0);
    let overhang = (half_extent + d) / 100 + 20_000;
    bb.expand(d + overhang).intersects(ob)
}

fn keepout_applies_to_pour(kind: KeepoutKind, layers: &[String], layer: &str) -> bool {
    kind.blocks_copper() && (layers.is_empty() || layers.iter().any(|l| l == layer || l == "*.Cu"))
}

fn unit_axis_for_pad(pad_rot_deg: f64, pl_rot_deg: f64, side: Side) -> (f64, f64) {
    let total_deg = match side {
        Side::Top => pl_rot_deg + pad_rot_deg,
        Side::Bottom => pl_rot_deg - pad_rot_deg,
    };
    let rad = total_deg.to_radians();
    (rad.cos(), rad.sin())
}

fn push_keepout_obstacle(obstacles: &mut Vec<Obstacle>, label: String, raw_pts: &[Pt]) {
    if raw_pts.len() < 3 {
        return;
    }
    let mut pts = raw_pts.to_vec();
    ensure_ccw(&mut pts);
    let cleared_bbox = BBoxNm::from_pts(&pts);
    obstacles.push(Obstacle {
        kind: BlockerKind::Keepout,
        net: String::new(),
        label,
        pos: cleared_bbox.center(),
        cleared_poly: pts_to_path64(&pts),
        cleared_bbox,
    });
}

pub(crate) fn collect_layer_scene(
    board: &BoardIr,
    layer: &str,
    pour_net: &str,
    pour_clr_opt: Option<Nm>,
    outline_bbox: Option<BBoxNm>,
) -> LayerScene {
    let cu_layers = board.copper_layer_names();
    let mask_layer_opt = match layer {
        "F.Cu" => Some("F.Mask"),
        "B.Cu" => Some("B.Mask"),
        _ => None,
    };
    let m2c_opt = mask_layer_opt.and(board.rules.mask_to_copper_nm.filter(|&v| v >= 0));
    let mut terminals = Vec::new();
    let mut obstacles = Vec::new();

    for ko in &board.keepouts {
        if keepout_applies_to_pour(ko.kind, &ko.layers, layer) {
            if outline_bbox.is_some_and(|ob| !BBoxNm::from_pts(&ko.poly).intersects(ob)) {
                continue;
            }
            push_keepout_obstacle(&mut obstacles, "keepout".into(), &ko.poly);
        }
    }

    for pl in &board.placements {
        let Some(fp) = board.footprints.get(&pl.footprint) else {
            continue;
        };
        for ko in &fp.keepouts {
            let mapped: Vec<String> = ko
                .layers
                .iter()
                .map(|l| remap_layer_for_side(l, pl.side))
                .collect();
            if keepout_applies_to_pour(ko.kind, &mapped, layer) {
                let pts: Vec<Pt> = ko
                    .poly
                    .iter()
                    .map(|&p| local_to_board(p, pl.pos.x, pl.pos.y, pl.rot_deg, pl.side))
                    .collect();
                if outline_bbox.is_some_and(|ob| !BBoxNm::from_pts(&pts).intersects(ob)) {
                    continue;
                }
                push_keepout_obstacle(&mut obstacles, format!("{}.keepout", pl.refdes), &pts);
            }
        }
        for pad in &fp.pads {
            let layers = pad_copper_layers(pad, pl.side, &cu_layers);
            let on_layer = layers.iter().any(|l| l == layer);
            let pad_net = pl.pin_nets.get(&pad.name).map(String::as_str).unwrap_or("");
            let is_same_net = !pour_net.is_empty() && pad_net == pour_net;
            let center = local_to_board(pad.pos, pl.pos.x, pl.pos.y, pl.rot_deg, pl.side);

            if on_layer {
                let shape = pad_to_world_shape(pad, pl, 16);
                if is_same_net && pad.kind != PadKind::Npth {
                    let pts = shape.to_polygon(16);
                    let bbox = BBoxNm::from_pts(&pts);
                    let poly = pts_to_path64(&pts);
                    terminals.push(Terminal {
                        label: format!("{}.{}", pl.refdes, pad.name),
                        center,
                        pts,
                        poly,
                        bbox,
                        axes: unit_axis_for_pad(pad.rot_deg, pl.rot_deg, pl.side),
                        thermal_relief: true,
                    });
                } else {
                    let clr = board.pour_pair_clearance(pour_net, pour_clr_opt, pad_net);
                    if outline_bbox.is_none_or(|ob| inflated_shape_intersects(&shape, clr, ob)) {
                        let pts = shape_inflated_poly(&shape, clr);
                        if pts.len() >= 3 {
                            let cleared_bbox = BBoxNm::from_pts(&pts);
                            obstacles.push(Obstacle {
                                kind: BlockerKind::Pad,
                                net: pad_net.to_string(),
                                label: format!("{}.{}", pl.refdes, pad.name),
                                pos: center,
                                cleared_poly: pts_to_path64(&pts),
                                cleared_bbox,
                            });
                        }
                    }
                }
            }

            // Drills for THT/NPTH penetrate all copper layers, so clear non-same-net drills everywhere.
            if let Some(drill) = pad.drill_nm
                && drill > 0
                && (!is_same_net || pad.kind == PadKind::Npth)
            {
                let clr = board
                    .pour_pair_clearance(
                        pour_net,
                        pour_clr_opt,
                        if pad.kind == PadKind::Npth {
                            ""
                        } else {
                            pad_net
                        },
                    )
                    .max(board.rules.effective_hole_clearance_nm());
                let rad = drill / 2 + clr;
                let pad_r = (rad as f64 / (PI / CLEARANCE_CIRCLE_SEGS as f64).cos()).ceil() as Nm;
                if outline_bbox.is_none_or(|ob| BBoxNm::from_pt(center, pad_r).intersects(ob)) {
                    let pts = circle_poly_outer(center, rad);
                    if pts.len() >= 3 {
                        let cleared_bbox = BBoxNm::from_pts(&pts);
                        obstacles.push(Obstacle {
                            kind: BlockerKind::Pad,
                            net: pad_net.to_string(),
                            label: format!("{}.{}", pl.refdes, pad.name),
                            pos: center,
                            cleared_poly: pts_to_path64(&pts),
                            cleared_bbox,
                        });
                    }
                }
            }

            if let (Some(mask_layer), Some(m2c)) = (mask_layer_opt, m2c_opt)
                && (!is_same_net || pad.kind == PadKind::Npth)
                && pad_world_layers(pad, pl.side, &cu_layers)
                    .iter()
                    .any(|l| l == mask_layer)
            {
                let margin = pad.mask_margin_nm.unwrap_or(board.rules.mask_expansion_nm);
                if let Some(mask_shape) = pad_world_shape_with_margin(pad, pl, margin)
                    && outline_bbox.is_none_or(|ob| inflated_shape_intersects(&mask_shape, m2c, ob))
                {
                    let pts = shape_inflated_poly(&mask_shape, m2c);
                    if pts.len() >= 3 {
                        let cleared_bbox = BBoxNm::from_pts(&pts);
                        obstacles.push(Obstacle {
                            kind: BlockerKind::Pad,
                            net: if pad.kind == PadKind::Npth {
                                String::new()
                            } else {
                                pad_net.to_string()
                            },
                            label: format!("{}.{}", pl.refdes, pad.name),
                            pos: center,
                            cleared_poly: pts_to_path64(&pts),
                            cleared_bbox,
                        });
                    }
                }
            }
        }
    }

    for (idx, track) in board.tracks.iter().enumerate() {
        if track.layer != layer {
            continue;
        }
        if !pour_net.is_empty() && track.net == pour_net {
            let pts = capsule_poly(track.a, track.b, (track.width / 2).max(1), 8);
            if pts.len() >= 3 {
                let bbox = BBoxNm::from_pts(&pts);
                let poly = pts_to_path64(&pts);
                terminals.push(Terminal {
                    label: format!("track#{idx}:{}", track.net),
                    center: track.a.midpoint(track.b),
                    pts,
                    poly,
                    bbox,
                    axes: (1.0, 0.0),
                    thermal_relief: false,
                });
            }
            continue;
        }
        let clr = board.pour_pair_clearance(pour_net, pour_clr_opt, &track.net);
        let pad_r = ((track.width / 2 + clr) as f64 / (PI / 32.0).cos()).ceil() as Nm;
        if outline_bbox.is_some_and(|ob| {
            !BBoxNm::from_pts(&[track.a, track.b])
                .expand(pad_r)
                .intersects(ob)
        }) {
            continue;
        }
        let pts = capsule_poly(track.a, track.b, pad_r, 32);
        if pts.len() >= 3 {
            let cleared_bbox = BBoxNm::from_pts(&pts);
            obstacles.push(Obstacle {
                kind: BlockerKind::Track,
                net: track.net.clone(),
                label: format!("track#{idx}:{}", track.net),
                pos: track.a.midpoint(track.b),
                cleared_poly: pts_to_path64(&pts),
                cleared_bbox,
            });
        }
    }

    for (idx, via) in board.vias.iter().enumerate() {
        if !via.touches_layer(layer, &board.layers) {
            continue;
        }
        if !pour_net.is_empty() && via.net == pour_net {
            let pts = circle_poly(via.pos, via.diameter / 2, 16);
            if pts.len() >= 3 {
                let bbox = BBoxNm::from_pts(&pts);
                let poly = pts_to_path64(&pts);
                terminals.push(Terminal {
                    label: format!("via#{idx}:{}", via.net),
                    center: via.pos,
                    pts,
                    poly,
                    bbox,
                    axes: (1.0, 0.0),
                    thermal_relief: false,
                });
            }
        } else {
            let clr = board.pour_pair_clearance(pour_net, pour_clr_opt, &via.net);
            let hole_clr = clr.max(board.rules.effective_hole_clearance_nm());
            // Full via pads are flashed on every spanned layer, so clear both pad and drill.
            let mut radius = (via.diameter / 2 + clr).max(via.drill / 2 + hole_clr);
            if let Some(m2c) = m2c_opt
                && !via.tented
            {
                let mask_dia = via.diameter + 2 * board.rules.mask_expansion_nm;
                if mask_dia > 0 {
                    radius = radius.max(mask_dia / 2 + m2c);
                }
            }
            let pad_r = (radius as f64 / (PI / CLEARANCE_CIRCLE_SEGS as f64).cos()).ceil() as Nm;
            if outline_bbox.is_some_and(|ob| !BBoxNm::from_pt(via.pos, pad_r).intersects(ob)) {
                continue;
            }
            let pts = circle_poly_outer(via.pos, radius);
            if pts.len() >= 3 {
                let cleared_bbox = BBoxNm::from_pts(&pts);
                obstacles.push(Obstacle {
                    kind: BlockerKind::Via,
                    net: via.net.clone(),
                    label: format!("via#{idx}:{}", via.net),
                    pos: via.pos,
                    cleared_poly: pts_to_path64(&pts),
                    cleared_bbox,
                });
            }
        }
    }

    LayerScene {
        terminals,
        obstacles,
    }
}

pub(crate) fn add_cutout_obstacles(board: &BoardIr, obstacles: &mut Vec<Obstacle>) {
    let edge_clr = board.rules.edge_clearance_nm;
    for (idx, cutout) in board.cutouts.iter().enumerate() {
        if cutout.len() < 3 {
            continue;
        }
        let mut pts = cutout.clone();
        ensure_ccw(&mut pts);
        let raw = vec![pts_to_path64(&pts)];
        let cleared = inflate_round_outer(&raw, edge_clr);
        for p in cleared {
            let cpts = path64_to_pts(&p);
            let bbox = BBoxNm::from_pts(&cpts);
            obstacles.push(Obstacle {
                kind: BlockerKind::Cutout,
                net: String::new(),
                label: format!("cutout#{idx}"),
                pos: bbox.center(),
                cleared_poly: p,
                cleared_bbox: bbox,
            });
        }
    }
}

pub(crate) fn build_board_copper_region(board: &BoardIr) -> Paths64 {
    let mut outline = board.outline.clone();
    ensure_ccw(&mut outline);
    let outline_paths = vec![pts_to_path64(&outline)];
    let edge_clr = board.rules.edge_clearance_nm;
    let deflated = if edge_clr > 0 {
        inflate_paths_64(
            &outline_paths,
            -(edge_clr as f64),
            JoinType::Miter,
            EndType::Polygon,
            2.0,
            ARC_TOLERANCE,
        )
    } else {
        outline_paths
    };
    if deflated.is_empty() || board.cutouts.is_empty() {
        return deflated;
    }
    let mut cutout_clips = Paths64::new();
    for c in &board.cutouts {
        if c.len() < 3 {
            continue;
        }
        let mut pts = c.clone();
        ensure_ccw(&mut pts);
        cutout_clips.extend(inflate_round_outer(&vec![pts_to_path64(&pts)], edge_clr));
    }
    if cutout_clips.is_empty() {
        deflated
    } else {
        difference_64(&deflated, &cutout_clips, FillRule::NonZero)
    }
}

pub(crate) fn erode_dilate(paths: &Paths64, min_width_nm: Nm) -> Paths64 {
    let delta = min_width_nm as f64 / 2.0;
    let eroded = inflate_paths_64(
        paths,
        -delta,
        JoinType::Miter,
        EndType::Polygon,
        2.0,
        ARC_TOLERANCE,
    );
    if eroded.is_empty() {
        return Paths64::new();
    }
    inflate_paths_64(
        &eroded,
        delta,
        JoinType::Miter,
        EndType::Polygon,
        2.0,
        ARC_TOLERANCE,
    )
}

pub(crate) fn morphological_open(paths: &Paths64, min_width_nm: Nm) -> Paths64 {
    if paths.is_empty() || min_width_nm <= 0 {
        return paths.clone();
    }
    let dilated = erode_dilate(paths, min_width_nm);
    if dilated.is_empty() {
        Paths64::new()
    } else {
        intersect_64(&dilated, paths, FillRule::NonZero)
    }
}

fn collect_polytree_node(tree: &PolyTree64, idx: usize, out: &mut Vec<PourPoly>) {
    let node = &tree.nodes[idx];
    if !tree.is_hole(idx) && node.polygon().len() >= 3 {
        let mut outer = path64_to_pts(node.polygon());
        ensure_ccw(&mut outer);
        let mut holes = Vec::new();
        for &child_idx in node.children() {
            let child = &tree.nodes[child_idx];
            if tree.is_hole(child_idx) && child.polygon().len() >= 3 {
                let mut hole = path64_to_pts(child.polygon());
                if polygon_signed_area_2(&hole) > 0 {
                    hole.reverse();
                }
                holes.push(hole);
                for &grandchild_idx in child.children() {
                    collect_polytree_node(tree, grandchild_idx, out);
                }
            }
        }
        if polygon_area(&outer) > 0.0 {
            out.push(PourPoly { outer, holes });
        }
    } else {
        for &child_idx in node.children() {
            collect_polytree_node(tree, child_idx, out);
        }
    }
}

pub(crate) fn boolean_op_to_pour_polys(
    clip_type: ClipType,
    subjects: &Paths64,
    clips: &Paths64,
) -> Vec<PourPoly> {
    if subjects.is_empty() {
        return Vec::new();
    }
    let mut tree = PolyTree64::new();
    boolean_op_tree_64(clip_type, FillRule::NonZero, subjects, clips, &mut tree);
    let mut out = Vec::new();
    for &child_idx in tree.root().children() {
        collect_polytree_node(&tree, child_idx, &mut out);
    }
    out
}

pub(crate) fn paths_to_pour_polys(paths: &Paths64) -> Vec<PourPoly> {
    boolean_op_to_pour_polys(ClipType::Union, paths, &Paths64::new())
}

pub(crate) fn pour_poly_to_paths64(poly: &PourPoly) -> Paths64 {
    let mut paths = Vec::with_capacity(1 + poly.holes.len());
    let mut outer = poly.outer.clone();
    ensure_ccw(&mut outer);
    paths.push(pts_to_path64(&outer));
    for h in &poly.holes {
        let mut hole = h.clone();
        if polygon_signed_area_2(&hole) > 0 {
            hole.reverse();
        }
        paths.push(pts_to_path64(&hole));
    }
    paths
}

pub(crate) fn pour_poly_area(poly: &PourPoly) -> f64 {
    let outer_area = polygon_area(&poly.outer);
    let hole_area: f64 = poly.holes.iter().map(|h| polygon_area(h)).sum();
    (outer_area - hole_area).max(0.0)
}

pub(crate) fn indexed_pour_intersects_terminal(pour: &IndexedPour, term: &Terminal) -> bool {
    if !pour.outer.bbox.intersects(term.bbox) {
        return false;
    }
    if pour.contains_pt(term.center) {
        return true;
    }
    pour.intersects_poly(&term.pts, term.bbox)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ir::{FootprintDef, PadDef, PadShape, Placement};

    #[test]
    fn shape_inflated_poly_roundrect_zero_delta_stays_circumscribed() {
        let corners = [
            Pt::new(-300_000, -200_000),
            Pt::new(300_000, -200_000),
            Pt::new(300_000, 200_000),
            Pt::new(-300_000, 200_000),
        ];
        let rr = ResolvedShape::RoundRect {
            corners,
            radius: 150_000,
            bbox: BBoxNm::from_pts(&corners).expand(150_000),
        };
        let poly = shape_inflated_poly(&rr, 0);
        assert!(poly.len() >= 16);
        for i in 0..poly.len() {
            let a = poly[i];
            let b = poly[(i + 1) % poly.len()];
            assert!(
                rr.dist_to_capsule(a, b, 0) >= 0.0,
                "edge ({a:?}, {b:?}) dipped inside analytic RoundRect"
            );
        }
    }

    #[test]
    fn collect_layer_scene_keeps_large_pad_within_circumscription_overhang() {
        let mut board: BoardIr = serde_json::from_str(
            r#"{"name":"t","outline":[],"layers":[],"footprints":{},"placements":[]}"#,
        )
        .unwrap();
        board.footprints.insert(
            "MTG".into(),
            FootprintDef {
                name: "MTG".into(),
                pads: vec![PadDef {
                    name: "1".into(),
                    kind: PadKind::Smd,
                    shape: PadShape::Circle {
                        diameter: 12_000_000,
                    },
                    pos: Pt::new(0, 0),
                    rot_deg: 0.0,
                    drill_nm: None,
                    layers: vec!["F.Cu".into()],
                    mask_margin_nm: None,
                    paste_margin_nm: None,
                }],
                courtyard: Vec::new(),
                graphics: Vec::new(),
                keepouts: Vec::new(),
                models: Vec::new(),
                pin_types: Default::default(),
            },
        );
        board.placements.push(Placement {
            refdes: "H1".into(),
            footprint: "MTG".into(),
            pos: Pt::new(0, 0),
            side: Side::Top,
            ..Default::default()
        });
        // Pad radius is 6mm, circumscription overhang is ~29um; place pour outline 20um outside pad bbox.
        let ob = BBoxNm::new(6_020_000, -1_000_000, 10_000_000, 1_000_000);
        board.rules.clearance_nm = 0;
        let scene = collect_layer_scene(&board, "F.Cu", "GND", Some(0), Some(ob));
        assert_eq!(scene.obstacles.len(), 1);
    }
}
