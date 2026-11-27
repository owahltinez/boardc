use crate::drc::{Severity, Violation, ViolationKind};
use crate::drc_scene::{
    ClearanceHit, PadItem, PourItem, TrackItem, ViaItem, push_clearance_violation,
};
use crate::fab_gerber::pad_world_shape_with_margin;
use crate::geom::{
    EPS_NM, SpatialGrid, closest_pt_on_segment, local_to_board, pad_world_layers,
    polys_overlap_area, pour_poly_overlaps_poly,
};
use crate::ir::{BBoxNm, BoardIr, Nm, PadKind, Pt, Side, same_named_net};
use crate::shapes::ResolvedShape;

struct MaskOpening {
    label: String,
    net: String,
    center: Pt,
    shape: ResolvedShape,
    bbox: BBoxNm,
    pl_idx: Option<usize>,
    pad_idx: Option<usize>,
    via_idx: Option<usize>,
}

fn collect_mask_openings(board: &BoardIr, cu_layers: &[String], side: Side) -> Vec<MaskOpening> {
    let mask_name = match side {
        Side::Top => "F.Mask",
        Side::Bottom => "B.Mask",
    };
    let cu_name = side.cu_layer();
    let mut openings = Vec::new();
    for (pl_idx, pl) in board.placements.iter().enumerate() {
        let Some(fp) = board.footprints.get(&pl.footprint) else {
            continue;
        };
        for (pad_idx, pad) in fp.pads.iter().enumerate() {
            if !pad_world_layers(pad, pl.side, cu_layers)
                .iter()
                .any(|l| l == mask_name)
            {
                continue;
            }
            let margin = pad.mask_margin_nm.unwrap_or(board.rules.mask_expansion_nm);
            if let Some(shape) = pad_world_shape_with_margin(pad, pl, margin) {
                let center = local_to_board(pad.pos, pl.pos.x, pl.pos.y, pl.rot_deg, pl.side);
                let net = pl.pin_nets.get(&pad.name).cloned().unwrap_or_default();
                openings.push(MaskOpening {
                    label: format!("{}.{}", pl.refdes, pad.name),
                    net,
                    center,
                    bbox: shape.bbox(),
                    shape,
                    pl_idx: Some(pl_idx),
                    pad_idx: Some(pad_idx),
                    via_idx: None,
                });
            }
        }
    }
    for (via_idx, via) in board.vias.iter().enumerate() {
        if !via.tented && via.touches_layer(cu_name, &board.layers) {
            let dia = via.diameter + 2 * board.rules.mask_expansion_nm;
            if dia > 0 {
                let shape = ResolvedShape::Circle {
                    center: via.pos,
                    radius: dia / 2,
                };
                openings.push(MaskOpening {
                    label: format!("via#{via_idx}"),
                    net: via.net.clone(),
                    center: via.pos,
                    bbox: shape.bbox(),
                    shape,
                    pl_idx: None,
                    pad_idx: None,
                    via_idx: Some(via_idx),
                });
            }
        }
    }
    openings
}

fn shape_violates_mask(a: &ResolvedShape, b: &ResolvedShape, req: Nm) -> Option<f64> {
    let dist = a.dist_to_shape(b);
    if req > 0 {
        return (dist < req as f64 - EPS_NM).then_some(dist);
    }
    // At req == 0, flag only strict intrusion; polygon distance bottoms out at 0.0.
    if dist < -EPS_NM {
        return Some(dist);
    }
    if dist <= EPS_NM
        && let (ResolvedShape::Poly { verts: va, .. }, ResolvedShape::Poly { verts: vb, .. }) =
            (a, b)
        && polys_overlap_area(va, vb) > 0.0
    {
        return Some(0.0);
    }
    None
}

fn pour_violates_mask(pour: &PourItem<'_>, shape: &ResolvedShape, req: Nm) -> Option<f64> {
    let dist = pour.indexed.dist_to_shape_bounded(shape, req);
    if req > 0 {
        return (dist < req as f64 - EPS_NM).then_some(dist);
    }
    if dist <= EPS_NM {
        let poly = shape.to_polygon(16);
        let p_bbox = BBoxNm::from_pts(&poly);
        if pour.indexed.intersects_poly(&poly, p_bbox) && pour_poly_overlaps_poly(pour.poly, &poly)
        {
            return Some(0.0);
        }
    }
    None
}

#[allow(clippy::too_many_arguments)]
fn emit_mask_violation(
    out: &mut Vec<Violation>,
    layer: &str,
    pos_nm: Pt,
    dist: f64,
    required_nm: Nm,
    opening: &MaskOpening,
    item_b: &str,
    net_b: &str,
) {
    if required_nm == 0 {
        out.push(Violation {
            kind: ViolationKind::SolderMaskToCopper,
            severity: Severity::Error,
            layer: layer.to_string(),
            pos_nm,
            measured_nm: 0,
            required_nm: 0,
            item_a: opening.label.clone(),
            item_b: Some(item_b.to_string()),
            net_a: opening.net.clone(),
            net_b: net_b.to_string(),
            message: format!(
                "mask {} intrudes on {item_b} (0 nm clearance required)",
                opening.label
            ),
        });
    } else {
        push_clearance_violation(
            out,
            ClearanceHit {
                kind: ViolationKind::SolderMaskToCopper,
                layer,
                pos_nm,
                dist,
                required_nm,
                item_a: &opening.label,
                item_b,
                net_a: &opening.net,
                net_b,
                prefix: "mask ",
            },
        );
    }
}

pub(crate) fn check_mask_to_copper(
    board: &BoardIr,
    cu_layers: &[String],
    tracks: &[TrackItem<'_>],
    pads: &[PadItem],
    vias: &[ViaItem],
    pours: &[PourItem<'_>],
    out: &mut Vec<Violation>,
) {
    let Some(req) = board.rules.mask_to_copper_nm else {
        return;
    };
    if req < 0 {
        return;
    }
    let mut candidates = Vec::new();

    for side in [Side::Top, Side::Bottom] {
        let cu_layer = side.cu_layer();
        if !cu_layers.iter().any(|l| l == cu_layer) {
            continue;
        }
        let openings = collect_mask_openings(board, cu_layers, side);
        if openings.is_empty() {
            continue;
        }

        let side_tracks: Vec<&TrackItem<'_>> =
            tracks.iter().filter(|t| t.layer == cu_layer).collect();
        let side_vias: Vec<(usize, &ViaItem)> = vias
            .iter()
            .enumerate()
            .filter(|(vi, _)| board.vias[*vi].touches_layer(cu_layer, &board.layers))
            .collect();
        let side_pads: Vec<&PadItem> = pads
            .iter()
            .filter(|p| p.kind != PadKind::Npth && p.layers.iter().any(|l| l == cu_layer))
            .collect();
        let side_pours: Vec<&PourItem<'_>> = pours.iter().filter(|p| p.layer == cu_layer).collect();

        let track_bboxes: Vec<BBoxNm> = side_tracks.iter().map(|t| t.bbox).collect();
        let via_bboxes: Vec<BBoxNm> = side_vias.iter().map(|(_, v)| v.bbox).collect();
        let pad_bboxes: Vec<BBoxNm> = side_pads.iter().map(|p| p.bbox).collect();
        let pour_bboxes: Vec<BBoxNm> = side_pours.iter().map(|p| p.bbox).collect();

        let mut track_grid = SpatialGrid::new(&track_bboxes, 2_000_000);
        let mut via_grid = SpatialGrid::new(&via_bboxes, 2_000_000);
        let mut pad_grid = SpatialGrid::new(&pad_bboxes, 2_000_000);
        let mut pour_grid = SpatialGrid::new(&pour_bboxes, 2_000_000);

        for op in &openings {
            let q = op.bbox.expand(req);

            track_grid.query(q, &mut candidates);
            for &ti in &candidates {
                let tr = side_tracks[ti];
                if same_named_net(&op.net, tr.net) || !tr.bbox.intersects(q) {
                    continue;
                }
                if let Some(dist) = shape_violates_mask(&op.shape, &tr.shape, req) {
                    let pos = closest_pt_on_segment(op.center, tr.a, tr.b).midpoint(op.center);
                    emit_mask_violation(out, cu_layer, pos, dist, req, op, &tr.label, tr.net);
                }
            }

            via_grid.query(q, &mut candidates);
            for &idx in &candidates {
                let (vi, via) = side_vias[idx];
                if op.via_idx == Some(vi)
                    || same_named_net(&op.net, &via.net)
                    || !via.bbox.intersects(q)
                {
                    continue;
                }
                if let Some(dist) = shape_violates_mask(&op.shape, &via.shape, req) {
                    let pos = op.center.midpoint(via.pos);
                    emit_mask_violation(out, cu_layer, pos, dist, req, op, &via.label, &via.net);
                }
            }

            pad_grid.query(q, &mut candidates);
            for &pi in &candidates {
                let pad = side_pads[pi];
                if (op.pl_idx == Some(pad.pl_idx) && op.pad_idx == Some(pad.pad_idx))
                    || (op.pl_idx == Some(pad.pl_idx) && op.net.is_empty() && pad.net.is_empty())
                    || same_named_net(&op.net, &pad.net)
                    || !pad.bbox.intersects(q)
                {
                    continue;
                }
                if let Some(dist) = shape_violates_mask(&op.shape, &pad.shape, req) {
                    let pos = op.center.midpoint(pad.center);
                    emit_mask_violation(out, cu_layer, pos, dist, req, op, &pad.label, &pad.net);
                }
            }

            pour_grid.query(q, &mut candidates);
            for &pi in &candidates {
                let pour = side_pours[pi];
                if same_named_net(&op.net, pour.net) || !pour.bbox.intersects(q) {
                    continue;
                }
                if let Some(dist) = pour_violates_mask(pour, &op.shape, req) {
                    emit_mask_violation(
                        out,
                        cu_layer,
                        op.center,
                        dist,
                        req,
                        op,
                        &pour.label,
                        pour.net,
                    );
                }
            }
        }
    }
}
