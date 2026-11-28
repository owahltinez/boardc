use crate::drc::{Severity, Violation, ViolationKind};
use crate::drc_scene::{BoardEdgeIndex, DrcInputs, KeepoutItem};
use crate::geom::{
    EPS_NM, GridQueryScratch, SpatialGrid, courtyard_world_polygon, point_in_polygon,
    polygon_to_polygon_dist, polygon_violates_clearance, polys_overlap_area,
    pour_poly_overlaps_poly,
};
use crate::ir::{BBoxNm, BoardIr, Nm, PadKind, Pt, Side};
use crate::poly_index::indexed_shape_edge_clearance;
use crate::shapes::ResolvedShape;

pub(crate) fn keepout_matches_layer(layers: &[String], layer: &str) -> bool {
    layers.is_empty() || layers.iter().any(|l| l == layer || l == "*.Cu")
}

pub(crate) fn keepout_matches_side(layers: &[String], side: Side) -> bool {
    layers.is_empty() || layers.iter().any(|l| l == "*.Cu" || l == side.cu_layer())
}

fn push_keepout(
    out: &mut Vec<Violation>,
    layer: &str,
    pos_nm: Pt,
    item_a: String,
    ko_label: &str,
    net_a: &str,
) {
    let message = format!("{item_a} violates {ko_label}");
    out.push(Violation {
        kind: ViolationKind::Keepout,
        severity: Severity::Error,
        layer: layer.to_string(),
        pos_nm,
        measured_nm: 0,
        required_nm: 1,
        item_a,
        item_b: Some(ko_label.to_string()),
        net_a: net_a.to_string(),
        net_b: String::new(),
        message,
    });
}

pub(crate) fn check_keepouts(
    board: &BoardIr,
    inputs: &DrcInputs<'_>,
    keepouts: &[KeepoutItem],
    out: &mut Vec<Violation>,
) {
    if keepouts.is_empty() {
        return;
    }
    let (tracks, pads, vias, pours, grids) = (
        inputs.tracks,
        inputs.pads,
        inputs.vias,
        inputs.pours,
        inputs.grids,
    );
    let mut scratch = GridQueryScratch::default();
    let mut candidates = Vec::new();
    for ko in keepouts {
        if ko.kind.blocks_tracks() {
            grids
                .track_grid
                .query_with(ko.bbox, &mut scratch, &mut candidates);
            for &ti in &candidates {
                let tr = &tracks[ti];
                if !keepout_matches_layer(&ko.layers, tr.layer) || !tr.bbox.intersects(ko.bbox) {
                    continue;
                }
                if point_in_polygon(tr.a, &ko.poly)
                    || point_in_polygon(tr.b, &ko.poly)
                    || tr.shape.dist_to_shape(&ko.shape) < -EPS_NM
                {
                    push_keepout(
                        out,
                        tr.layer,
                        tr.shape.center(),
                        tr.label.clone(),
                        &ko.label,
                        tr.net,
                    );
                }
            }
        }
        if ko.kind.blocks_vias() {
            grids
                .via_grid
                .query_with(ko.bbox, &mut scratch, &mut candidates);
            for &vi in &candidates {
                let via = &vias[vi];
                let raw_via = &board.vias[vi];
                let ok_layer = ko.layers.is_empty()
                    || ko
                        .layers
                        .iter()
                        .any(|l| l == "*.Cu" || raw_via.touches_layer(l, &board.layers));
                if ok_layer
                    && via.bbox.intersects(ko.bbox)
                    && (point_in_polygon(via.pos, &ko.poly)
                        || ko.shape.dist_to_circle(via.pos, via.dia / 2) < -EPS_NM)
                {
                    push_keepout(
                        out,
                        &via.layers.0,
                        via.pos,
                        via.label.clone(),
                        &ko.label,
                        &via.net,
                    );
                }
            }
        }
        if ko.kind.blocks_pads() {
            grids
                .pad_grid
                .query_with(ko.bbox, &mut scratch, &mut candidates);
            for &pi in &candidates {
                let pad = &pads[pi];
                if pad.kind == PadKind::Npth || !pad.bbox.intersects(ko.bbox) {
                    continue;
                }
                let Some(layer) = pad
                    .layers
                    .iter()
                    .find(|l| keepout_matches_layer(&ko.layers, l))
                else {
                    continue;
                };
                let overlaps = match &pad.shape {
                    ResolvedShape::Poly { verts, .. } => polys_overlap_area(verts, &ko.poly) > 0.0,
                    _ => {
                        point_in_polygon(pad.center, &ko.poly)
                            || pad.shape.dist_to_shape(&ko.shape) < -EPS_NM
                    }
                };
                if overlaps {
                    push_keepout(
                        out,
                        layer,
                        pad.center,
                        pad.label.clone(),
                        &ko.label,
                        &pad.net,
                    );
                }
            }
        }
        if ko.kind.blocks_copper() {
            for pour in pours {
                if keepout_matches_layer(&ko.layers, pour.layer)
                    && pour.bbox.intersects(ko.bbox)
                    && pour.indexed.intersects_poly(&ko.poly, ko.bbox)
                    && pour_poly_overlaps_poly(pour.poly, &ko.poly)
                {
                    push_keepout(
                        out,
                        pour.layer,
                        ko.shape.center(),
                        pour.label.clone(),
                        &ko.label,
                        pour.net,
                    );
                }
            }
        }
    }
}

pub(crate) fn check_courtyards_and_component_edges(
    board: &BoardIr,
    keepouts: &[KeepoutItem],
    edge_idx: Option<&BoardEdgeIndex>,
    out: &mut Vec<Violation>,
) {
    let mut courtyards: Vec<(usize, Vec<Pt>, BBoxNm)> = Vec::new();
    for (idx, pl) in board.placements.iter().enumerate() {
        let Some(fp) = board.footprints.get(&pl.footprint) else {
            continue;
        };
        if fp.courtyard.is_empty() && fp.pads.is_empty() {
            continue;
        }
        let cy = courtyard_world_polygon(fp, pl);
        if cy.len() >= 3 {
            let bbox = BBoxNm::from_pts(&cy);
            courtyards.push((idx, cy, bbox));
        }
    }

    let cy_clr = board.rules.courtyard_clearance_nm;
    let cy_grid = SpatialGrid::new(courtyards.iter().map(|c| c.2), 2_000_000);
    let ko_grid = (!keepouts.is_empty())
        .then(|| SpatialGrid::new(keepouts.iter().map(|k| k.bbox), 2_000_000));
    let mut scratch = GridQueryScratch::default();
    let mut candidates = Vec::new();

    for i in 0..courtyards.len() {
        let (ia, ref cya, bba) = courtyards[i];
        let pla = &board.placements[ia];
        let bba_exp = bba.expand(cy_clr);
        cy_grid.query_with(bba_exp, &mut scratch, &mut candidates);
        for &j in &candidates {
            if j <= i {
                continue;
            }
            let (ib, ref cyb, bbb) = courtyards[j];
            let plb = &board.placements[ib];
            if pla.side != plb.side || !bba_exp.intersects(bbb) {
                continue;
            }
            if polygon_violates_clearance(cya, cyb, cy_clr) {
                let meas = polygon_to_polygon_dist(cya, cyb).max(0.0).round() as Nm;
                out.push(Violation {
                    kind: ViolationKind::CourtyardOverlap,
                    severity: Severity::Error,
                    layer: pla.side.cu_layer().to_string(),
                    pos_nm: pla.pos.midpoint(plb.pos),
                    measured_nm: meas,
                    required_nm: cy_clr,
                    item_a: pla.refdes.clone(),
                    item_b: Some(plb.refdes.clone()),
                    net_a: String::new(),
                    net_b: String::new(),
                    message: format!(
                        "courtyard {} overlaps/too close to {} ({meas} nm < {cy_clr} nm)",
                        pla.refdes, plb.refdes
                    ),
                });
            }
        }
        if let Some(ref kg) = ko_grid {
            kg.query_with(bba, &mut scratch, &mut candidates);
            for &ki in &candidates {
                let ko = &keepouts[ki];
                if ko.kind.blocks_placement()
                    && keepout_matches_side(&ko.layers, pla.side)
                    && bba.intersects(ko.bbox)
                    && !ko
                        .label
                        .strip_prefix(&pla.refdes)
                        .is_some_and(|rest| rest.starts_with('.'))
                    && polys_overlap_area(cya, &ko.poly) > 0.0
                {
                    out.push(Violation {
                        kind: ViolationKind::Keepout,
                        severity: Severity::Error,
                        layer: pla.side.cu_layer().to_string(),
                        pos_nm: pla.pos,
                        measured_nm: 0,
                        required_nm: 1,
                        item_a: pla.refdes.clone(),
                        item_b: Some(ko.label.clone()),
                        net_a: String::new(),
                        net_b: String::new(),
                        message: format!("placement {} violates {}", pla.refdes, ko.label),
                    });
                }
            }
        }
        if let Some(edge) = edge_idx {
            let violation_meas = if !pla.allow_edge_overhang {
                let cy_shape = ResolvedShape::polygon(cya.clone());
                let (dist, off_board) =
                    indexed_shape_edge_clearance(&cy_shape, &edge.outline, &edge.cutouts, cy_clr);
                if off_board {
                    Some(0)
                } else if cy_clr > 0 && dist < cy_clr as f64 - EPS_NM {
                    Some(dist.max(0.0).round() as Nm)
                } else {
                    None
                }
            } else if polys_overlap_area(cya, &board.outline) <= 0.0 {
                Some(0)
            } else {
                board
                    .cutouts
                    .iter()
                    .filter(|c| c.len() >= 3 && polygon_violates_clearance(cya, c, cy_clr))
                    .map(|c| polygon_to_polygon_dist(cya, c).max(0.0).round() as Nm)
                    .min()
            };
            if let Some(meas) = violation_meas {
                out.push(Violation {
                    kind: ViolationKind::ComponentToEdge,
                    severity: Severity::Error,
                    layer: pla.side.cu_layer().to_string(),
                    pos_nm: pla.pos,
                    measured_nm: meas,
                    required_nm: cy_clr,
                    item_a: pla.refdes.clone(),
                    item_b: Some("board_edge".into()),
                    net_a: String::new(),
                    net_b: String::new(),
                    message: format!(
                        "{} component-to-edge clearance {meas} nm < {cy_clr} nm",
                        pla.refdes
                    ),
                });
            }
        }
    }
}
