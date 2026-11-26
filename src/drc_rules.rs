use crate::drc::{Severity, Violation, ViolationKind};
use crate::drc_scene::{
    ClearanceHit, DrillItem, PadItem, PourItem, TrackItem, ViaItem, push_clearance_violation,
};
use crate::geom::{EPS_NM, SpatialGrid, closest_pt_on_segment};
use crate::ir::{BBoxNm, BoardIr, Nm, PadKind, Pt, same_named_net};
use crate::poly_index::{IndexedRing, indexed_shape_edge_clearance};
use crate::shapes::ResolvedShape;

pub(crate) fn check_drill_rules(
    board: &BoardIr,
    tracks: &[TrackItem<'_>],
    pads: &[PadItem],
    vias: &[ViaItem],
    drills: &[DrillItem],
    pours: &[PourItem<'_>],
    max_clr: Nm,
) -> Vec<Violation> {
    let mut out = Vec::new();
    for via in vias {
        let annular = (via.dia - via.drill) / 2;
        if annular < board.rules.min_annular_ring_nm {
            out.push(Violation {
                kind: ViolationKind::MinAnnularRing,
                severity: Severity::Error,
                layer: via.layers.0.clone(),
                pos_nm: via.pos,
                measured_nm: annular,
                required_nm: board.rules.min_annular_ring_nm,
                item_a: via.label.clone(),
                item_b: None,
                net_a: via.net.clone(),
                net_b: String::new(),
                message: format!(
                    "{} annular ring {annular} nm < {} nm",
                    via.label, board.rules.min_annular_ring_nm
                ),
            });
        }
    }
    for pad in pads {
        if pad.kind == PadKind::Tht
            && let Some(drill) = pad.drill_nm
            && drill > 0
        {
            let annular = (pad.min_dim_nm - drill) / 2;
            if annular < board.rules.min_annular_ring_nm {
                let layer = pad.layers.first().map(String::as_str).unwrap_or("F.Cu");
                out.push(Violation {
                    kind: ViolationKind::MinAnnularRing,
                    severity: Severity::Error,
                    layer: layer.to_string(),
                    pos_nm: pad.center,
                    measured_nm: annular,
                    required_nm: board.rules.min_annular_ring_nm,
                    item_a: pad.label.clone(),
                    item_b: None,
                    net_a: pad.net.clone(),
                    net_b: String::new(),
                    message: format!(
                        "{} annular ring {annular} nm < {} nm",
                        pad.label, board.rules.min_annular_ring_nm
                    ),
                });
            }
        }
    }

    let drill_bboxes: Vec<BBoxNm> = drills.iter().map(|d| d.bbox).collect();
    let mut drill_grid = SpatialGrid::new(&drill_bboxes, 2_000_000);
    let mut candidates = Vec::new();
    for i in 0..drills.len() {
        let da = &drills[i];
        drill_grid.query(da.bbox.expand(board.rules.hole_to_hole_nm), &mut candidates);
        for &j in &candidates {
            if j <= i {
                continue;
            }
            let db = &drills[j];
            if !da.layers.iter().any(|l| db.layers.contains(l)) {
                continue;
            }
            let dist = da.pos.dist(db.pos) - (da.radius + db.radius) as f64;
            if dist < board.rules.hole_to_hole_nm as f64 - EPS_NM {
                let meas = dist.max(0.0).round() as Nm;
                out.push(Violation {
                    kind: ViolationKind::HoleToHole,
                    severity: Severity::Error,
                    layer: String::new(),
                    pos_nm: da.pos.midpoint(db.pos),
                    measured_nm: meas,
                    required_nm: board.rules.hole_to_hole_nm,
                    item_a: da.label.clone(),
                    item_b: Some(db.label.clone()),
                    net_a: da.net.clone(),
                    net_b: db.net.clone(),
                    message: format!(
                        "hole {} to {} spacing {meas} nm < {} nm",
                        da.label, db.label, board.rules.hole_to_hole_nm
                    ),
                });
            }
        }
    }

    let track_bboxes: Vec<BBoxNm> = tracks.iter().map(|t| t.bbox).collect();
    let pad_bboxes: Vec<BBoxNm> = pads.iter().map(|p| p.bbox).collect();
    let via_bboxes: Vec<BBoxNm> = vias.iter().map(|v| v.bbox).collect();
    let mut track_grid = SpatialGrid::new(&track_bboxes, 2_000_000);
    let mut pad_grid = SpatialGrid::new(&pad_bboxes, 2_000_000);
    let mut via_grid = SpatialGrid::new(&via_bboxes, 2_000_000);

    let hole_clr = board.rules.effective_hole_clearance_nm();
    for d in drills {
        let d_net = if d.is_npth { "" } else { d.net.as_str() };
        let d_shape = ResolvedShape::Circle {
            center: d.pos,
            radius: d.radius,
        };
        let d_exp = d.bbox.expand(max_clr);

        track_grid.query(d_exp, &mut candidates);
        for &ti in &candidates {
            let t = &tracks[ti];
            if (!d.is_npth && same_named_net(&d.net, t.net))
                || !d.layers.iter().any(|l| l == t.layer)
                || !t.bbox.intersects(d_exp)
            {
                continue;
            }
            let req = board.pair_clearance(d_net, t.net, None).max(hole_clr);
            let dist = d_shape.dist_to_capsule(t.a, t.b, t.width / 2);
            if dist < req as f64 - EPS_NM {
                let cp = closest_pt_on_segment(d.pos, t.a, t.b);
                push_clearance_violation(
                    &mut out,
                    ClearanceHit {
                        kind: ViolationKind::DrillToCopper,
                        layer: t.layer,
                        pos_nm: cp.midpoint(d.pos),
                        dist,
                        required_nm: req,
                        item_a: &d.label,
                        item_b: &t.label,
                        net_a: &d.net,
                        net_b: t.net,
                        prefix: "drill ",
                    },
                );
            }
        }

        pad_grid.query(d_exp, &mut candidates);
        for &pi in &candidates {
            let pad = &pads[pi];
            if pad.kind == PadKind::Npth
                || pad.layers.is_empty()
                || d.pad_idx == Some(pi)
                || (!d.is_npth && same_named_net(&d.net, &pad.net))
                || !pad.bbox.intersects(d_exp)
            {
                continue;
            }
            let Some(layer) = d.layers.iter().find(|l| pad.layers.contains(l)) else {
                continue;
            };
            let req = board.pair_clearance(d_net, &pad.net, None).max(hole_clr);
            let dist = pad.shape.dist_to_circle(d.pos, d.radius);
            if dist < req as f64 - EPS_NM {
                push_clearance_violation(
                    &mut out,
                    ClearanceHit {
                        kind: ViolationKind::DrillToCopper,
                        layer,
                        pos_nm: d.pos.midpoint(pad.center),
                        dist,
                        required_nm: req,
                        item_a: &d.label,
                        item_b: &pad.label,
                        net_a: &d.net,
                        net_b: &pad.net,
                        prefix: "drill ",
                    },
                );
            }
        }

        via_grid.query(d_exp, &mut candidates);
        for &vi in &candidates {
            let via = &vias[vi];
            if d.via_idx == Some(vi)
                || (!d.is_npth && same_named_net(&d.net, &via.net))
                || !via.bbox.intersects(d_exp)
            {
                continue;
            }
            let Some(layer) = d
                .layers
                .iter()
                .find(|l| board.vias[vi].touches_layer(l, &board.layers))
            else {
                continue;
            };
            let req = board.pair_clearance(d_net, &via.net, None).max(hole_clr);
            let dist = d.pos.dist(via.pos) - (d.radius + via.dia / 2) as f64;
            if dist < req as f64 - EPS_NM {
                push_clearance_violation(
                    &mut out,
                    ClearanceHit {
                        kind: ViolationKind::DrillToCopper,
                        layer,
                        pos_nm: d.pos.midpoint(via.pos),
                        dist,
                        required_nm: req,
                        item_a: &d.label,
                        item_b: &via.label,
                        net_a: &d.net,
                        net_b: &via.net,
                        prefix: "drill ",
                    },
                );
            }
        }

        for pour in pours {
            if (!d.is_npth && same_named_net(&d.net, pour.net))
                || !d.layers.iter().any(|l| l == pour.layer)
                || !pour.bbox.intersects(d_exp)
            {
                continue;
            }
            let req = board
                .pour_pair_clearance(pour.net, pour.clearance_override, d_net)
                .max(hole_clr);
            let dist = pour.indexed.dist_to_shape_bounded(&d_shape, req);
            if dist < req as f64 - EPS_NM {
                push_clearance_violation(
                    &mut out,
                    ClearanceHit {
                        kind: ViolationKind::DrillToCopper,
                        layer: pour.layer,
                        pos_nm: d.pos,
                        dist,
                        required_nm: req,
                        item_a: &d.label,
                        item_b: &pour.label,
                        net_a: &d.net,
                        net_b: pour.net,
                        prefix: "drill ",
                    },
                );
            }
        }
    }
    out
}

pub(crate) fn check_edge_clearances(
    board: &BoardIr,
    tracks: &[TrackItem<'_>],
    pads: &[PadItem],
    vias: &[ViaItem],
    pours: &[PourItem<'_>],
    out: &mut Vec<Violation>,
) {
    if board.outline.len() < 3 {
        return;
    }
    let outline_idx = IndexedRing::new(&board.outline);
    let cutout_idxs: Vec<IndexedRing> = board
        .cutouts
        .iter()
        .filter(|c| c.len() >= 3)
        .map(|c| IndexedRing::new(c))
        .collect();
    let req = board.rules.edge_clearance_nm;
    let push_edge = |dist: f64,
                     off_board: bool,
                     pos: Pt,
                     layer: &str,
                     label: String,
                     net: &str,
                     out: &mut Vec<Violation>| {
        if off_board || dist < req as f64 - EPS_NM {
            let meas = if off_board {
                0
            } else {
                dist.max(0.0).round() as Nm
            };
            let msg = format!("{label} edge clearance {meas} nm < {req} nm");
            out.push(Violation {
                kind: ViolationKind::EdgeClearance,
                severity: Severity::Error,
                layer: layer.to_string(),
                pos_nm: pos,
                measured_nm: meas,
                required_nm: req,
                item_a: label,
                item_b: Some("board_edge".into()),
                net_a: net.to_string(),
                net_b: String::new(),
                message: msg,
            });
        }
    };

    for tr in tracks {
        let (dist, off) = indexed_shape_edge_clearance(&tr.shape, &outline_idx, &cutout_idxs, req);
        push_edge(
            dist,
            off,
            tr.shape.center(),
            tr.layer,
            tr.label.clone(),
            tr.net,
            out,
        );
    }
    for via in vias {
        let (dist, off) = indexed_shape_edge_clearance(&via.shape, &outline_idx, &cutout_idxs, req);
        push_edge(
            dist,
            off,
            via.pos,
            &via.layers.0,
            via.label.clone(),
            &via.net,
            out,
        );
    }
    for pad in pads {
        if pad.kind != PadKind::Npth
            && let Some(layer) = pad.layers.first()
        {
            let (dist, off) =
                indexed_shape_edge_clearance(&pad.shape, &outline_idx, &cutout_idxs, req);
            push_edge(
                dist,
                off,
                pad.center,
                layer,
                pad.label.clone(),
                &pad.net,
                out,
            );
        }
    }
    for pour in pours {
        let Some(&ref_pt) = pour.indexed.outer.pts.first() else {
            continue;
        };
        let (dist, off) = pour
            .indexed
            .edge_clearance(pour.poly, &outline_idx, &cutout_idxs, req);
        push_edge(
            dist,
            off,
            ref_pt,
            pour.layer,
            pour.label.clone(),
            pour.net,
            out,
        );
    }
}
