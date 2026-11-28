use std::f64::consts::PI;

use clipper2_rust::{
    ClipType, EndType, FillRule, JoinType, Path64, Paths64, PolyTree64, boolean_op_tree_64,
    difference_64, inflate_paths_64, intersect_64,
};

use crate::fab_gerber::pad_world_shape_with_margin;
use crate::geom::{
    ensure_ccw, local_to_board, pad_copper_layers, pad_to_world_shape, pad_world_layers,
    path64_to_pts, point_in_polygon, polygon_area, polygon_signed_area_2, pts_to_path64,
    remap_layer_for_side,
};
use crate::ir::{
    BBoxNm, BoardIr, FilledPour, KeepoutKind, Nm, PadKind, PourPoly, PourSpec, Pt, Side,
};
use crate::poly_index::IndexedPour;
use crate::shapes::{ResolvedShape, capsule_poly, circle_poly};
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

pub(crate) fn shape_inflated_poly(shape: &ResolvedShape, delta: Nm) -> Vec<Pt> {
    match shape {
        ResolvedShape::Circle { center, radius } => circle_poly_outer(*center, *radius + delta),
        ResolvedShape::Capsule { a, b, radius } => {
            let pad_r = ((*radius + delta) as f64 / (PI / 32.0).cos()).ceil() as Nm;
            capsule_poly(*a, *b, pad_r, 32)
        }
        ResolvedShape::Poly { .. } => {
            let path = pts_to_path64(&shape.to_polygon(16));
            if delta <= 0 {
                return path64_to_pts(&path);
            }
            let inflated = inflate_round_outer(&vec![path], delta);
            inflated.first().map(path64_to_pts).unwrap_or_default()
        }
    }
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
                push_keepout_obstacle(&mut obstacles, format!("{}.keepout", pl.refdes), &pts);
            }
        }
        for pad in &fp.pads {
            let layers = pad_copper_layers(pad, pl.side, &cu_layers);
            let on_layer = layers.iter().any(|l| l == layer);
            let pad_net = pl.pin_nets.get(&pad.name).map(String::as_str).unwrap_or("");
            let is_same_net = !pour_net.is_empty() && pad_net == pour_net;
            let center = local_to_board(pad.pos, pl.pos.x, pl.pos.y, pl.rot_deg, pl.side);
            let shape = pad_to_world_shape(pad, pl, 16);

            if on_layer {
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
                let pts = circle_poly_outer(center, drill / 2 + clr);
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

            if let (Some(mask_layer), Some(m2c)) = (mask_layer_opt, m2c_opt)
                && (!is_same_net || pad.kind == PadKind::Npth)
                && pad_world_layers(pad, pl.side, &cu_layers)
                    .iter()
                    .any(|l| l == mask_layer)
            {
                let margin = pad.mask_margin_nm.unwrap_or(board.rules.mask_expansion_nm);
                if let Some(mask_shape) = pad_world_shape_with_margin(pad, pl, margin) {
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

pub(crate) fn morphological_open(paths: &Paths64, min_width_nm: Nm) -> Paths64 {
    if paths.is_empty() || min_width_nm <= 0 {
        return paths.clone();
    }
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
    let dilated = inflate_paths_64(
        &eroded,
        delta,
        JoinType::Miter,
        EndType::Polygon,
        2.0,
        ARC_TOLERANCE,
    );
    intersect_64(&dilated, paths, FillRule::NonZero)
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

pub(crate) fn paths_to_pour_polys(paths: &Paths64) -> Vec<PourPoly> {
    if paths.is_empty() {
        return Vec::new();
    }
    let mut tree = PolyTree64::new();
    boolean_op_tree_64(
        ClipType::Union,
        FillRule::NonZero,
        paths,
        &Paths64::new(),
        &mut tree,
    );
    let mut out = Vec::new();
    for &child_idx in tree.root().children() {
        collect_polytree_node(&tree, child_idx, &mut out);
    }
    out
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

pub(crate) fn point_in_pour_poly(p: Pt, poly: &PourPoly) -> bool {
    point_in_polygon(p, &poly.outer) && !poly.holes.iter().any(|h| point_in_polygon(p, h))
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
