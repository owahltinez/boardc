use crate::drc::{Severity, Violation, ViolationKind};
use crate::drc_scene::{
    ClearanceHit, PadItem, PourItem, TrackItem, ViaItem, push_clearance_violation,
};
use crate::geom::{EPS_NM, SpatialGrid, closest_pt_on_segment, segment_pair_midpoint};
use crate::ir::{BBoxNm, BoardIr, Nm, PadKind, Pt, same_named_net};

pub(crate) fn check_track_rules_and_clearances(
    board: &BoardIr,
    tracks: &[TrackItem<'_>],
    pads: &[PadItem],
    vias: &[ViaItem],
    max_clr: Nm,
    out: &mut Vec<Violation>,
) {
    let mut track_bboxes = Vec::with_capacity(tracks.len());
    for tr in tracks {
        if tr.width < board.rules.track_width_nm {
            out.push(Violation {
                kind: ViolationKind::MinTraceWidth,
                severity: Severity::Error,
                layer: tr.layer.to_string(),
                pos_nm: tr.a.midpoint(tr.b),
                measured_nm: tr.width,
                required_nm: board.rules.track_width_nm,
                item_a: tr.label.clone(),
                item_b: None,
                net_a: tr.net.to_string(),
                net_b: String::new(),
                message: format!(
                    "{} width {} nm < min {} nm",
                    tr.label, tr.width, board.rules.track_width_nm
                ),
            });
        }
        track_bboxes.push(tr.bbox);
    }

    let mut track_grid = SpatialGrid::new(&track_bboxes, 2_000_000);
    let mut candidates = Vec::new();
    for i in 0..tracks.len() {
        let ti = &tracks[i];
        track_grid.query(ti.bbox.expand(max_clr), &mut candidates);
        for &j in &candidates {
            if j <= i {
                continue;
            }
            let tj = &tracks[j];
            if ti.layer != tj.layer || same_named_net(ti.net, tj.net) {
                continue;
            }
            let req = board.pair_clearance(ti.net, tj.net, None);
            let dist = ti.shape.dist_to_shape(&tj.shape);
            if dist < req as f64 - EPS_NM {
                push_clearance_violation(
                    out,
                    ClearanceHit {
                        kind: ViolationKind::Clearance,
                        layer: ti.layer,
                        pos_nm: segment_pair_midpoint(ti.a, ti.b, tj.a, tj.b),
                        dist,
                        required_nm: req,
                        item_a: &ti.label,
                        item_b: &tj.label,
                        net_a: ti.net,
                        net_b: tj.net,
                        prefix: "",
                    },
                );
            }
        }
    }

    for pad in pads {
        if pad.kind == PadKind::Npth || pad.layers.is_empty() {
            continue;
        }
        track_grid.query(pad.bbox.expand(max_clr), &mut candidates);
        for &ti_idx in &candidates {
            let t = &tracks[ti_idx];
            if same_named_net(t.net, &pad.net) || !pad.layers.iter().any(|l| l == t.layer) {
                continue;
            }
            let req = board.pair_clearance(t.net, &pad.net, None);
            let dist = pad.shape.dist_to_capsule(t.a, t.b, t.width / 2);
            if dist < req as f64 - EPS_NM {
                let cp = closest_pt_on_segment(pad.center, t.a, t.b);
                push_clearance_violation(
                    out,
                    ClearanceHit {
                        kind: ViolationKind::Clearance,
                        layer: t.layer,
                        pos_nm: cp.midpoint(pad.center),
                        dist,
                        required_nm: req,
                        item_a: &t.label,
                        item_b: &pad.label,
                        net_a: t.net,
                        net_b: &pad.net,
                        prefix: "",
                    },
                );
            }
        }
    }

    for (vi, via) in vias.iter().enumerate() {
        let raw_via = &board.vias[vi];
        track_grid.query(via.bbox.expand(max_clr), &mut candidates);
        for &ti_idx in &candidates {
            let t = &tracks[ti_idx];
            if same_named_net(t.net, &via.net) || !raw_via.touches_layer(t.layer, &board.layers) {
                continue;
            }
            let req = board.pair_clearance(t.net, &via.net, board.rules.via_to_track_nm);
            let dist = t.shape.dist_to_circle(via.pos, via.dia / 2);
            if dist < req as f64 - EPS_NM {
                let cp = closest_pt_on_segment(via.pos, t.a, t.b);
                push_clearance_violation(
                    out,
                    ClearanceHit {
                        kind: ViolationKind::Clearance,
                        layer: t.layer,
                        pos_nm: cp.midpoint(via.pos),
                        dist,
                        required_nm: req,
                        item_a: &t.label,
                        item_b: &via.label,
                        net_a: t.net,
                        net_b: &via.net,
                        prefix: "",
                    },
                );
            }
        }
    }
}

pub(crate) fn check_pad_to_pad_clearances(
    board: &BoardIr,
    pads: &[PadItem],
    max_clr: Nm,
    out: &mut Vec<Violation>,
) {
    let pad_bboxes: Vec<BBoxNm> = pads.iter().map(|p| p.bbox).collect();
    let mut grid = SpatialGrid::new(&pad_bboxes, 2_000_000);
    let mut candidates = Vec::new();
    for i in 0..pads.len() {
        let pa = &pads[i];
        if pa.kind == PadKind::Npth || pa.layers.is_empty() {
            continue;
        }
        grid.query(pa.bbox.expand(max_clr), &mut candidates);
        for &j in &candidates {
            if j <= i {
                continue;
            }
            let pb = &pads[j];
            if pb.kind == PadKind::Npth || pb.layers.is_empty() || same_named_net(&pa.net, &pb.net)
            {
                continue;
            }
            if pa.pl_idx == pb.pl_idx && pa.net.is_empty() && pb.net.is_empty() {
                continue;
            }
            let Some(shared_layer) = pa.layers.iter().find(|l| pb.layers.contains(l)) else {
                continue;
            };
            let rule_ov = (pa.kind == PadKind::Smd && pb.kind == PadKind::Smd)
                .then_some(board.rules.smd_to_smd_nm)
                .flatten();
            let req = board.pair_clearance(&pa.net, &pb.net, rule_ov);
            let dist = pa.shape.dist_to_shape(&pb.shape);
            if dist < req as f64 - EPS_NM {
                push_clearance_violation(
                    out,
                    ClearanceHit {
                        kind: ViolationKind::Clearance,
                        layer: shared_layer,
                        pos_nm: pa.center.midpoint(pb.center),
                        dist,
                        required_nm: req,
                        item_a: &pa.label,
                        item_b: &pb.label,
                        net_a: &pa.net,
                        net_b: &pb.net,
                        prefix: "",
                    },
                );
            }
        }
    }
}

pub(crate) fn check_via_to_via_and_pad(
    board: &BoardIr,
    pads: &[PadItem],
    vias: &[ViaItem],
    cu_layers: &[String],
    max_clr: Nm,
    out: &mut Vec<Violation>,
) {
    let via_bboxes: Vec<BBoxNm> = vias.iter().map(|v| v.bbox).collect();
    let mut via_grid = SpatialGrid::new(&via_bboxes, 2_000_000);
    let mut candidates = Vec::new();

    for i in 0..vias.len() {
        let va = &vias[i];
        via_grid.query(va.bbox.expand(max_clr), &mut candidates);
        for &j in &candidates {
            if j <= i {
                continue;
            }
            let vb = &vias[j];
            if same_named_net(&va.net, &vb.net) {
                continue;
            }
            let Some(layer) = cu_layers.iter().find(|l| {
                board.vias[i].touches_layer(l, &board.layers)
                    && board.vias[j].touches_layer(l, &board.layers)
            }) else {
                continue;
            };
            let req = board.pair_clearance(&va.net, &vb.net, board.rules.via_to_via_nm);
            let dist = va.pos.dist(vb.pos) - (va.dia + vb.dia) as f64 / 2.0;
            if dist < req as f64 - EPS_NM {
                push_clearance_violation(
                    out,
                    ClearanceHit {
                        kind: ViolationKind::Clearance,
                        layer,
                        pos_nm: va.pos.midpoint(vb.pos),
                        dist,
                        required_nm: req,
                        item_a: &va.label,
                        item_b: &vb.label,
                        net_a: &va.net,
                        net_b: &vb.net,
                        prefix: "",
                    },
                );
            }
        }
    }

    for pad in pads {
        if pad.kind == PadKind::Npth || pad.layers.is_empty() {
            continue;
        }
        via_grid.query(pad.bbox.expand(max_clr), &mut candidates);
        for &vi in &candidates {
            let via = &vias[vi];
            let Some(layer) = pad
                .layers
                .iter()
                .find(|l| board.vias[vi].touches_layer(l, &board.layers))
            else {
                continue;
            };
            if same_named_net(&pad.net, &via.net) {
                if pad.kind == PadKind::Smd
                    && !board.rules.via_at_smd
                    && pad.min_dim_nm < 1_500_000
                    && pad.shape.dist_to_circle(via.pos, via.drill / 2) < -EPS_NM
                {
                    out.push(Violation {
                        kind: ViolationKind::ViaAtSmd,
                        severity: Severity::Error,
                        layer: layer.clone(),
                        pos_nm: via.pos,
                        measured_nm: 0,
                        required_nm: 1,
                        item_a: via.label.clone(),
                        item_b: Some(pad.label.clone()),
                        net_a: via.net.clone(),
                        net_b: pad.net.clone(),
                        message: format!(
                            "{} inside SMD pad {} with via_at_smd=false",
                            via.label, pad.label
                        ),
                    });
                }
                continue;
            }
            let rule_ov = (pad.kind == PadKind::Smd)
                .then_some(board.rules.via_to_smd_nm)
                .flatten();
            let req = board.pair_clearance(&pad.net, &via.net, rule_ov);
            let dist = pad.shape.dist_to_circle(via.pos, via.dia / 2);
            if dist < req as f64 - EPS_NM {
                push_clearance_violation(
                    out,
                    ClearanceHit {
                        kind: ViolationKind::Clearance,
                        layer,
                        pos_nm: pad.center.midpoint(via.pos),
                        dist,
                        required_nm: req,
                        item_a: &via.label,
                        item_b: &pad.label,
                        net_a: &via.net,
                        net_b: &pad.net,
                        prefix: "",
                    },
                );
            }
        }
    }
}

pub(crate) fn check_pour_clearances(
    board: &BoardIr,
    tracks: &[TrackItem<'_>],
    pads: &[PadItem],
    vias: &[ViaItem],
    pours: &[PourItem<'_>],
    max_clr: Nm,
    out: &mut Vec<Violation>,
) {
    if pours.is_empty() {
        return;
    }
    for (pi, pour) in pours.iter().enumerate() {
        let p_exp = pour.bbox.expand(max_clr);
        for tr in tracks {
            if tr.layer != pour.layer
                || same_named_net(pour.net, tr.net)
                || !tr.bbox.intersects(p_exp)
            {
                continue;
            }
            let req = board.pour_pair_clearance(pour.net, pour.clearance_override, tr.net);
            let dist = pour.indexed.dist_to_shape_bounded(&tr.shape, req);
            if dist < req as f64 - EPS_NM {
                push_clearance_violation(
                    out,
                    ClearanceHit {
                        kind: ViolationKind::Clearance,
                        layer: pour.layer,
                        pos_nm: tr.shape.center(),
                        dist,
                        required_nm: req,
                        item_a: &tr.label,
                        item_b: &pour.label,
                        net_a: tr.net,
                        net_b: pour.net,
                        prefix: "",
                    },
                );
            }
        }

        for pad in pads {
            if pad.kind == PadKind::Npth
                || same_named_net(pour.net, &pad.net)
                || !pad.layers.iter().any(|l| l == pour.layer)
                || !pad.bbox.intersects(p_exp)
            {
                continue;
            }
            let req = board.pour_pair_clearance(pour.net, pour.clearance_override, &pad.net);
            let dist = pour.indexed.dist_to_shape_bounded(&pad.shape, req);
            if dist < req as f64 - EPS_NM {
                push_clearance_violation(
                    out,
                    ClearanceHit {
                        kind: ViolationKind::Clearance,
                        layer: pour.layer,
                        pos_nm: pad.center,
                        dist,
                        required_nm: req,
                        item_a: &pad.label,
                        item_b: &pour.label,
                        net_a: &pad.net,
                        net_b: pour.net,
                        prefix: "",
                    },
                );
            }
        }

        for (vi, via) in vias.iter().enumerate() {
            if same_named_net(pour.net, &via.net)
                || !via.bbox.intersects(p_exp)
                || !board.vias[vi].touches_layer(pour.layer, &board.layers)
            {
                continue;
            }
            let req = board.pour_pair_clearance(pour.net, pour.clearance_override, &via.net);
            let dist = pour.indexed.dist_to_shape_bounded(&via.shape, req);
            if dist < req as f64 - EPS_NM {
                push_clearance_violation(
                    out,
                    ClearanceHit {
                        kind: ViolationKind::Clearance,
                        layer: pour.layer,
                        pos_nm: via.pos,
                        dist,
                        required_nm: req,
                        item_a: &via.label,
                        item_b: &pour.label,
                        net_a: &via.net,
                        net_b: pour.net,
                        prefix: "",
                    },
                );
            }
        }

        for (pj, other) in pours.iter().enumerate() {
            if pj <= pi
                || other.layer != pour.layer
                || same_named_net(pour.net, other.net)
                || !other.bbox.intersects(p_exp)
            {
                continue;
            }
            let req = board
                .pour_pair_clearance(pour.net, pour.clearance_override, other.net)
                .max(board.pour_pair_clearance(other.net, other.clearance_override, pour.net));
            let dist = pour.indexed.dist_to_pour_bounded(&other.indexed, req);
            if dist < req as f64 - EPS_NM {
                let pos = Pt::new(
                    (pour.bbox.min_x.max(other.bbox.min_x) + pour.bbox.max_x.min(other.bbox.max_x))
                        / 2,
                    (pour.bbox.min_y.max(other.bbox.min_y) + pour.bbox.max_y.min(other.bbox.max_y))
                        / 2,
                );
                push_clearance_violation(
                    out,
                    ClearanceHit {
                        kind: ViolationKind::Clearance,
                        layer: pour.layer,
                        pos_nm: pos,
                        dist,
                        required_nm: req,
                        item_a: &pour.label,
                        item_b: &other.label,
                        net_a: pour.net,
                        net_b: other.net,
                        prefix: "",
                    },
                );
            }
        }
    }
}
