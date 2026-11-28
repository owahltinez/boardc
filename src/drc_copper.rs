use crate::drc::{Severity, Violation, ViolationKind};
use crate::drc_scene::{ClearanceHit, DrcInputs, TrackItem, push_clearance_violation};
use crate::geom::{EPS_NM, GridQueryScratch, closest_pt_on_segment, segment_pair_midpoint};
use crate::ir::{BoardIr, PadKind, same_named_net};

#[inline]
fn masks_share_layer(
    a_mask: u64,
    b_mask: u64,
    a_layers: &[String],
    b_layers: &[String],
    use_mask: bool,
) -> bool {
    if use_mask {
        (a_mask & b_mask) != 0
    } else {
        a_layers.iter().any(|l| b_layers.contains(l))
    }
}

pub(crate) fn check_track_rules_and_clearances(
    board: &BoardIr,
    inputs: &DrcInputs<'_>,
    out: &mut Vec<Violation>,
) {
    let (cu_layers, tracks, pads, vias, grids, max_clr) = (
        inputs.cu_layers,
        inputs.tracks,
        inputs.pads,
        inputs.vias,
        inputs.grids,
        inputs.max_clr,
    );
    let base_clr = board.rules.clearance_nm;
    let use_mask = cu_layers.len() <= 64;
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
    }

    let mut scratch = GridQueryScratch::default();
    let mut candidates = Vec::new();
    for (layer_idxs, layer_grid) in &grids.tracks_by_layer {
        for (pos_i, &i) in layer_idxs.iter().enumerate() {
            let ti = &tracks[i];
            let ti_exp = ti.bbox.expand(max_clr);
            layer_grid.query_with(ti_exp, &mut scratch, &mut candidates);
            for &loc_j in &candidates {
                if loc_j <= pos_i {
                    continue;
                }
                let j = layer_idxs[loc_j];
                let tj = &tracks[j];
                if same_named_net(ti.net, tj.net) || !tj.bbox.intersects(ti_exp) {
                    continue;
                }
                let req = ti.clr.pair(tj.clr, base_clr, None);
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
    }

    for pad in pads {
        if pad.kind == PadKind::Npth || pad.layers.is_empty() {
            continue;
        }
        let pad_exp = pad.bbox.expand(max_clr);
        let mut check = |t: &TrackItem<'_>| {
            if same_named_net(t.net, &pad.net) || !t.bbox.intersects(pad_exp) {
                return;
            }
            let req = t.clr.pair(pad.clr, base_clr, None);
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
        };
        if use_mask {
            let mut m = pad.layer_mask;
            while m != 0 {
                let li = m.trailing_zeros() as usize;
                m &= m - 1;
                let (layer_idxs, layer_grid) = &grids.tracks_by_layer[li];
                layer_grid.query_with(pad_exp, &mut scratch, &mut candidates);
                for &loc_ti in &candidates {
                    check(&tracks[layer_idxs[loc_ti]]);
                }
            }
        } else {
            grids
                .track_grid
                .query_with(pad_exp, &mut scratch, &mut candidates);
            for &ti_idx in &candidates {
                let t = &tracks[ti_idx];
                if pad.layers.iter().any(|l| l == t.layer) {
                    check(t);
                }
            }
        }
    }

    for (vi, via) in vias.iter().enumerate() {
        let via_exp = via.bbox.expand(max_clr);
        let mut check = |t: &TrackItem<'_>| {
            if same_named_net(t.net, &via.net) || !t.bbox.intersects(via_exp) {
                return;
            }
            let req = t.clr.pair(via.clr, base_clr, board.rules.via_to_track_nm);
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
        };
        if use_mask {
            let mut m = via.layer_mask;
            while m != 0 {
                let li = m.trailing_zeros() as usize;
                m &= m - 1;
                let (layer_idxs, layer_grid) = &grids.tracks_by_layer[li];
                layer_grid.query_with(via_exp, &mut scratch, &mut candidates);
                for &loc_ti in &candidates {
                    check(&tracks[layer_idxs[loc_ti]]);
                }
            }
        } else {
            let raw_via = &board.vias[vi];
            grids
                .track_grid
                .query_with(via_exp, &mut scratch, &mut candidates);
            for &ti_idx in &candidates {
                let t = &tracks[ti_idx];
                if raw_via.touches_layer(t.layer, &board.layers) {
                    check(t);
                }
            }
        }
    }
}

pub(crate) fn check_pad_to_pad_clearances(
    board: &BoardIr,
    inputs: &DrcInputs<'_>,
    out: &mut Vec<Violation>,
) {
    let (cu_layers, pads, grids, max_clr) =
        (inputs.cu_layers, inputs.pads, inputs.grids, inputs.max_clr);
    let base_clr = board.rules.clearance_nm;
    let use_mask = cu_layers.len() <= 64;
    let mut scratch = GridQueryScratch::default();
    let mut candidates = Vec::new();
    for i in 0..pads.len() {
        let pa = &pads[i];
        if pa.kind == PadKind::Npth || pa.layers.is_empty() {
            continue;
        }
        let pa_exp = pa.bbox.expand(max_clr);
        grids
            .pad_grid
            .query_with(pa_exp, &mut scratch, &mut candidates);
        for &j in &candidates {
            if j <= i {
                continue;
            }
            let pb = &pads[j];
            if pb.kind == PadKind::Npth
                || pb.layers.is_empty()
                || same_named_net(&pa.net, &pb.net)
                || !pb.bbox.intersects(pa_exp)
                || (pa.pl_idx == pb.pl_idx && pa.net.is_empty() && pb.net.is_empty())
                || !masks_share_layer(
                    pa.layer_mask,
                    pb.layer_mask,
                    &pa.layers,
                    &pb.layers,
                    use_mask,
                )
            {
                continue;
            }
            let rule_ov = (pa.kind == PadKind::Smd && pb.kind == PadKind::Smd)
                .then_some(board.rules.smd_to_smd_nm)
                .flatten();
            let req = pa.clr.pair(pb.clr, base_clr, rule_ov);
            let dist = pa.shape.dist_to_shape(&pb.shape);
            if dist < req as f64 - EPS_NM {
                let shared_layer = pa
                    .layers
                    .iter()
                    .find(|l| pb.layers.contains(l))
                    .map(String::as_str)
                    .unwrap_or(&cu_layers[0]);
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
    inputs: &DrcInputs<'_>,
    out: &mut Vec<Violation>,
) {
    let (cu_layers, pads, vias, grids, max_clr) = (
        inputs.cu_layers,
        inputs.pads,
        inputs.vias,
        inputs.grids,
        inputs.max_clr,
    );
    let base_clr = board.rules.clearance_nm;
    let use_mask = cu_layers.len() <= 64;
    let mut scratch = GridQueryScratch::default();
    let mut candidates = Vec::new();

    for i in 0..vias.len() {
        let va = &vias[i];
        let va_exp = va.bbox.expand(max_clr);
        grids
            .via_grid
            .query_with(va_exp, &mut scratch, &mut candidates);
        for &j in &candidates {
            if j <= i {
                continue;
            }
            let vb = &vias[j];
            if same_named_net(&va.net, &vb.net) || !vb.bbox.intersects(va_exp) {
                continue;
            }
            let layer: &str = if use_mask {
                let shared = va.layer_mask & vb.layer_mask;
                if shared == 0 {
                    continue;
                }
                &cu_layers[shared.trailing_zeros() as usize]
            } else {
                let Some(l) = cu_layers.iter().find(|l| {
                    board.vias[i].touches_layer(l, &board.layers)
                        && board.vias[j].touches_layer(l, &board.layers)
                }) else {
                    continue;
                };
                l
            };
            let req = va.clr.pair(vb.clr, base_clr, board.rules.via_to_via_nm);
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
        let pad_exp = pad.bbox.expand(max_clr);
        grids
            .via_grid
            .query_with(pad_exp, &mut scratch, &mut candidates);
        for &vi in &candidates {
            let via = &vias[vi];
            if !via.bbox.intersects(pad_exp) {
                continue;
            }
            if use_mask && (pad.layer_mask & via.layer_mask) == 0 {
                continue;
            }
            let layer = if use_mask && pad.layers.len() == 1 {
                &pad.layers[0]
            } else {
                let Some(l) = pad
                    .layers
                    .iter()
                    .find(|l| board.vias[vi].touches_layer(l, &board.layers))
                else {
                    continue;
                };
                l
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
            let req = pad.clr.pair(via.clr, base_clr, rule_ov);
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

fn check_single_pour_clearances(
    board: &BoardIr,
    inputs: &DrcInputs<'_>,
    pi: usize,
) -> Vec<Violation> {
    let (cu_layers, tracks, pads, vias, pours, grids, max_clr) = (
        inputs.cu_layers,
        inputs.tracks,
        inputs.pads,
        inputs.vias,
        inputs.pours,
        inputs.grids,
        inputs.max_clr,
    );
    let mut out = Vec::new();
    let pour = &pours[pi];
    let p_exp = pour.bbox.expand(max_clr);
    let base_clr = board.rules.clearance_nm;
    let use_mask = cu_layers.len() <= 64;
    let mut scratch = GridQueryScratch::default();
    let mut candidates = Vec::new();

    {
        let mut check_track = |tr: &TrackItem<'_>| {
            if same_named_net(pour.net, tr.net) || !tr.bbox.intersects(p_exp) {
                return;
            }
            let req = pour
                .clr
                .pour_pair(pour.clearance_override, tr.clr, base_clr);
            let dist = pour.indexed.dist_to_shape_bounded(&tr.shape, req);
            if dist < req as f64 - EPS_NM {
                push_clearance_violation(
                    &mut out,
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
        };
        if let Some(li) = pour.layer_idx {
            let (layer_idxs, layer_grid) = &grids.tracks_by_layer[li];
            layer_grid.query_with(p_exp, &mut scratch, &mut candidates);
            for &loc_ti in &candidates {
                check_track(&tracks[layer_idxs[loc_ti]]);
            }
        } else {
            for tr in tracks {
                if tr.layer == pour.layer {
                    check_track(tr);
                }
            }
        }
    }

    grids
        .pad_grid
        .query_with(p_exp, &mut scratch, &mut candidates);
    for &pad_i in &candidates {
        let pad = &pads[pad_i];
        if pad.kind == PadKind::Npth
            || same_named_net(pour.net, &pad.net)
            || !pad.bbox.intersects(p_exp)
            || (use_mask && (pad.layer_mask & pour.layer_mask) == 0)
            || (!use_mask && !pad.layers.iter().any(|l| l == pour.layer))
        {
            continue;
        }
        let req = pour
            .clr
            .pour_pair(pour.clearance_override, pad.clr, base_clr);
        let dist = pour.indexed.dist_to_shape_bounded(&pad.shape, req);
        if dist < req as f64 - EPS_NM {
            push_clearance_violation(
                &mut out,
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

    grids
        .via_grid
        .query_with(p_exp, &mut scratch, &mut candidates);
    for &vi in &candidates {
        let via = &vias[vi];
        if same_named_net(pour.net, &via.net)
            || !via.bbox.intersects(p_exp)
            || (use_mask && (via.layer_mask & pour.layer_mask) == 0)
            || (!use_mask && !board.vias[vi].touches_layer(pour.layer, &board.layers))
        {
            continue;
        }
        let req = pour
            .clr
            .pour_pair(pour.clearance_override, via.clr, base_clr);
        let dist = pour.indexed.dist_to_shape_bounded(&via.shape, req);
        if dist < req as f64 - EPS_NM {
            push_clearance_violation(
                &mut out,
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
        let req = pour
            .clr
            .pour_pair(pour.clearance_override, other.clr, base_clr)
            .max(
                other
                    .clr
                    .pour_pair(other.clearance_override, pour.clr, base_clr),
            );
        let dist = pour.indexed.dist_to_pour_bounded(&other.indexed, req);
        if dist < req as f64 - EPS_NM {
            let pos = crate::ir::Pt::new(
                (pour.bbox.min_x.max(other.bbox.min_x) + pour.bbox.max_x.min(other.bbox.max_x)) / 2,
                (pour.bbox.min_y.max(other.bbox.min_y) + pour.bbox.max_y.min(other.bbox.max_y)) / 2,
            );
            push_clearance_violation(
                &mut out,
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
    out
}

pub(crate) fn check_pour_clearances(
    board: &BoardIr,
    inputs: &DrcInputs<'_>,
    out: &mut Vec<Violation>,
) {
    if inputs.pours.is_empty() {
        return;
    }
    if inputs.pours.len() == 1 {
        out.extend(check_single_pour_clearances(board, inputs, 0));
        return;
    }
    std::thread::scope(|s| {
        let handles: Vec<_> = (0..inputs.pours.len())
            .map(|pi| s.spawn(move || check_single_pour_clearances(board, inputs, pi)))
            .collect();
        for h in handles {
            out.extend(h.join().expect("pour clearance thread panicked"));
        }
    });
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::drc_scene::{collect_drc_grids, collect_pours, compute_max_clearance};
    use crate::geom::SpatialGrid;
    use crate::ir::{FilledPour, NetClass, NetDef, PourPoly, PourSpec, Pt};

    #[test]
    fn pour_to_pour_clearance_respects_non_default_net_class_with_override() {
        let mut board: BoardIr = serde_json::from_str(
            r#"{"name":"t","outline":[],"layers":[],"footprints":{},"placements":[]}"#,
        )
        .unwrap();
        board.rules.clearance_nm = 100_000;
        board.net_classes.insert(
            "HV".into(),
            NetClass {
                name: "HV".into(),
                track_width_nm: 150_000,
                clearance_nm: 500_000,
                via_dia_nm: 450_000,
                via_drill_nm: 200_000,
                diff_pair_width_nm: None,
                diff_pair_gap_nm: None,
            },
        );
        board.nets.insert(
            "N2".into(),
            NetDef {
                class: "HV".into(),
                ..Default::default()
            },
        );
        let p1_poly = vec![
            Pt::new(0, 0),
            Pt::new(2_000_000, 0),
            Pt::new(2_000_000, 2_000_000),
            Pt::new(0, 2_000_000),
        ];
        let p2_poly = vec![
            Pt::new(2_300_000, 0),
            Pt::new(4_300_000, 0),
            Pt::new(4_300_000, 2_000_000),
            Pt::new(2_300_000, 2_000_000),
        ];
        board.pours.push(PourSpec {
            name: "P1".into(),
            net: "N1".into(),
            layer: "F.Cu".into(),
            clearance_nm: Some(150_000),
            outline: p1_poly.clone(),
            ..Default::default()
        });
        board.pours.push(PourSpec {
            name: "P2".into(),
            net: "N2".into(),
            layer: "F.Cu".into(),
            clearance_nm: Some(150_000),
            outline: p2_poly.clone(),
            ..Default::default()
        });
        board.filled_pours.push(FilledPour {
            net: "N1".into(),
            layer: "F.Cu".into(),
            priority: 0,
            polys: vec![PourPoly {
                outer: p1_poly,
                holes: Vec::new(),
            }],
        });
        board.filled_pours.push(FilledPour {
            net: "N2".into(),
            layer: "F.Cu".into(),
            priority: 0,
            polys: vec![PourPoly {
                outer: p2_poly,
                holes: Vec::new(),
            }],
        });

        let expected = board.pour_pair_clearance("N1", Some(150_000), "N2");
        assert_eq!(expected, 500_000);

        let cu_layers = board.copper_layer_names();
        let pours = collect_pours(&board);
        let pad_grid = SpatialGrid::new(std::iter::empty(), 2_000_000);
        let grids = collect_drc_grids(&cu_layers, &pad_grid, &[], &[]);
        let inputs = DrcInputs {
            cu_layers: &cu_layers,
            tracks: &[],
            pads: &[],
            vias: &[],
            pours: &pours,
            grids: &grids,
            max_clr: compute_max_clearance(&board, &pours),
        };
        let mut out = Vec::new();
        check_pour_clearances(&board, &inputs, &mut out);
        assert_eq!(out.len(), 1);
        assert_eq!(out[0].required_nm, 500_000);
        assert_eq!(out[0].measured_nm, 300_000);
    }
}
