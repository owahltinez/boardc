use crate::drc::{Severity, Violation, ViolationKind};
use crate::drc_scene::{
    BoardEdgeIndex, ClearanceHit, DrcInputs, DrillItem, PadItem, PourItem, TrackItem, ViaItem,
    push_clearance_violation,
};
use crate::geom::{EPS_NM, GridQueryScratch, SpatialGrid, closest_pt_on_segment};
use crate::ir::{BoardIr, Nm, PadKind, Pt, same_named_net};
use crate::poly_index::indexed_shape_edge_clearance;
use crate::shapes::ResolvedShape;

pub(crate) fn check_drill_rules(
    board: &BoardIr,
    inputs: &DrcInputs<'_>,
    drills: &[DrillItem],
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
    let base_clr = board.rules.clearance_nm;
    let use_mask = cu_layers.len() <= 64;
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

    let drill_grid = SpatialGrid::new(drills.iter().map(|d| d.bbox), 2_000_000);
    let mut scratch = GridQueryScratch::default();
    let mut candidates = Vec::new();
    for i in 0..drills.len() {
        let da = &drills[i];
        let da_exp = da.bbox.expand(board.rules.hole_to_hole_nm);
        drill_grid.query_with(da_exp, &mut scratch, &mut candidates);
        for &j in &candidates {
            if j <= i {
                continue;
            }
            let db = &drills[j];
            if !db.bbox.intersects(da_exp)
                || (use_mask && (da.layer_mask & db.layer_mask) == 0)
                || (!use_mask && !da.layers.iter().any(|l| db.layers.contains(l)))
            {
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

    let hole_clr = board.rules.effective_hole_clearance_nm();
    for d in drills {
        let d_shape = ResolvedShape::Circle {
            center: d.pos,
            radius: d.radius,
        };
        let d_exp = d.bbox.expand(max_clr);

        grids
            .track_grid
            .query_with(d_exp, &mut scratch, &mut candidates);
        for &ti in &candidates {
            let t = &tracks[ti];
            if (!d.is_npth && same_named_net(&d.net, t.net))
                || !t.bbox.intersects(d_exp)
                || (use_mask && (d.layer_mask & t.layer_mask) == 0)
                || (!use_mask && !d.layers.iter().any(|l| l == t.layer))
            {
                continue;
            }
            let req = d.clr.pair(t.clr, base_clr, None).max(hole_clr);
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

        grids
            .pad_grid
            .query_with(d_exp, &mut scratch, &mut candidates);
        for &pi in &candidates {
            let pad = &pads[pi];
            if pad.kind == PadKind::Npth
                || pad.layers.is_empty()
                || d.pad_idx == Some(pi)
                || (!d.is_npth && same_named_net(&d.net, &pad.net))
                || !pad.bbox.intersects(d_exp)
                || (use_mask && (d.layer_mask & pad.layer_mask) == 0)
            {
                continue;
            }
            let layer = if use_mask && pad.layers.len() == 1 {
                &pad.layers[0]
            } else {
                let Some(l) = d.layers.iter().find(|l| pad.layers.contains(l)) else {
                    continue;
                };
                l
            };
            let req = d.clr.pair(pad.clr, base_clr, None).max(hole_clr);
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

        grids
            .via_grid
            .query_with(d_exp, &mut scratch, &mut candidates);
        for &vi in &candidates {
            let via = &vias[vi];
            if d.via_idx == Some(vi)
                || (!d.is_npth && same_named_net(&d.net, &via.net))
                || !via.bbox.intersects(d_exp)
                || (use_mask && (d.layer_mask & via.layer_mask) == 0)
            {
                continue;
            }
            let layer: &str = if use_mask {
                let shared = d.layer_mask & via.layer_mask;
                &cu_layers[shared.trailing_zeros() as usize]
            } else {
                let Some(l) = d
                    .layers
                    .iter()
                    .find(|l| board.vias[vi].touches_layer(l, &board.layers))
                else {
                    continue;
                };
                l
            };
            let req = d.clr.pair(via.clr, base_clr, None).max(hole_clr);
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
                || !pour.bbox.intersects(d_exp)
                || (use_mask && (d.layer_mask & pour.layer_mask) == 0)
                || (!use_mask && !d.layers.iter().any(|l| l == pour.layer))
            {
                continue;
            }
            let req = pour
                .clr
                .pour_pair(pour.clearance_override, d.clr, base_clr)
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
    edge_idx: Option<&BoardEdgeIndex>,
    out: &mut Vec<Violation>,
) {
    let Some(edge) = edge_idx else {
        return;
    };
    let req = board.rules.edge_clearance_nm;
    let push_edge = |dist: f64,
                     off_board: bool,
                     pos: Pt,
                     layer: &str,
                     label: &str,
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
                item_a: label.to_string(),
                item_b: Some("board_edge".into()),
                net_a: net.to_string(),
                net_b: String::new(),
                message: msg,
            });
        }
    };

    for tr in tracks {
        let (dist, off) =
            indexed_shape_edge_clearance(&tr.shape, &edge.outline, &edge.cutouts, req);
        push_edge(
            dist,
            off,
            tr.shape.center(),
            tr.layer,
            &tr.label,
            tr.net,
            out,
        );
    }
    for via in vias {
        let (dist, off) =
            indexed_shape_edge_clearance(&via.shape, &edge.outline, &edge.cutouts, req);
        push_edge(dist, off, via.pos, &via.layers.0, &via.label, &via.net, out);
    }
    for pad in pads {
        let mut worst: Option<(f64, bool, &str)> = None;
        if pad.kind != PadKind::Npth
            && let Some(layer) = pad.layers.first()
        {
            let (dist, off) =
                indexed_shape_edge_clearance(&pad.shape, &edge.outline, &edge.cutouts, req);
            if off || dist < req as f64 - EPS_NM {
                worst = Some((dist, off, layer.as_str()));
            }
        }
        if let Some(d) = pad.drill_nm
            && d > 0
            && (pad.kind == PadKind::Npth || pad.layers.is_empty() || d > pad.min_dim_nm)
        {
            let drill_shape = ResolvedShape::Circle {
                center: pad.center,
                radius: d / 2,
            };
            let (dist, off) =
                indexed_shape_edge_clearance(&drill_shape, &edge.outline, &edge.cutouts, req);
            if (off || dist < req as f64 - EPS_NM)
                && worst.is_none_or(|(wd, woff, _)| !woff && (off || dist < wd))
            {
                let fallback = if pad.kind == PadKind::Npth {
                    "NPTH"
                } else {
                    "*.Cu"
                };
                let layer = pad.layers.first().map(String::as_str).unwrap_or(fallback);
                worst = Some((dist, off, layer));
            }
        }
        if let Some((dist, off, layer)) = worst {
            push_edge(dist, off, pad.center, layer, &pad.label, &pad.net, out);
        }
    }
    for pour in pours {
        let Some(&ref_pt) = pour.indexed.outer.pts.first() else {
            continue;
        };
        let (dist, off) = pour
            .indexed
            .edge_clearance(pour.poly, &edge.outline, &edge.cutouts, req);
        push_edge(dist, off, ref_pt, pour.layer, &pour.label, pour.net, out);
    }
}
