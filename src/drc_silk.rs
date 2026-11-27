use std::collections::BTreeMap;

use crate::drc::{Severity, Violation, ViolationKind};
use crate::drc_mask::{MaskOpening, MaskOwner, collect_mask_openings, shape_violates_mask};
use crate::drc_scene::{ClearanceHit, PadItem, ViaItem, push_clearance_violation};
use crate::fab_silk::{SilkElement, for_each_silk_element, push_graphic_mask_shapes};
use crate::geom::{EPS_NM, SpatialGrid, closest_pt_on_segment, segment_pair_midpoint};
use crate::ir::{BBoxNm, BoardIr, Nm, Pt, Side};
use crate::poly_index::{IndexedRing, indexed_shape_edge_clearance};
use crate::shapes::ResolvedShape;

struct SilkItem {
    label: String,
    shapes: Vec<ResolvedShape>,
}

fn push_silk_item(items: &mut Vec<SilkItem>, label: String, shapes: Vec<ResolvedShape>) {
    if !shapes.is_empty() {
        items.push(SilkItem { label, shapes });
    }
}

fn collect_silk_items(board: &BoardIr, side: Side) -> Vec<SilkItem> {
    let mut items = Vec::new();
    for_each_silk_element(board, side, board.fab, |elem| match elem {
        SilkElement::Graphic { pl, idx, graphic } => {
            let mut shapes = Vec::new();
            push_graphic_mask_shapes(graphic, pl, &mut shapes);
            let label = match pl {
                Some(pl) => format!("{}.silk#{idx}", pl.refdes),
                None => format!("silk#{idx}"),
            };
            push_silk_item(&mut items, label, shapes);
        }
        SilkElement::Pin1Dot {
            refdes,
            center,
            radius,
        } => {
            push_silk_item(
                &mut items,
                format!("{refdes}.pin1_dot"),
                vec![ResolvedShape::Circle { center, radius }],
            );
        }
        SilkElement::Refdes {
            refdes,
            segs,
            stroke_nm,
        } => {
            let ref_r = stroke_nm.max(10_000) / 2;
            let shapes = segs
                .into_iter()
                .map(|(a, b)| ResolvedShape::Capsule {
                    a,
                    b,
                    radius: ref_r,
                })
                .collect();
            push_silk_item(&mut items, format!("{refdes}.refdes"), shapes);
        }
    });
    items
}

fn check_silk_to_mask(
    silk_items: &[SilkItem],
    openings: &[MaskOpening<'_>],
    silk_layer: &str,
    req: Nm,
    out: &mut Vec<Violation>,
) {
    if openings.is_empty() {
        return;
    }
    let mask_bboxes: Vec<BBoxNm> = openings.iter().map(|o| o.bbox).collect();
    let mut mask_grid = SpatialGrid::new(&mask_bboxes, 2_000_000);
    let mut candidates = Vec::new();

    for item in silk_items {
        let mut best_by_mask: BTreeMap<MaskOwner, (usize, f64, Pt)> = BTreeMap::new();
        for sh in &item.shapes {
            let q = sh.bbox().expand(req);
            mask_grid.query(q, &mut candidates);
            for &mi in &candidates {
                let op = &openings[mi];
                if !op.bbox.intersects(q) {
                    continue;
                }
                if let Some(dist) = shape_violates_mask(sh, &op.shape, req) {
                    let pos_nm = match (sh, &op.shape) {
                        (
                            ResolvedShape::Capsule { a, b, .. },
                            ResolvedShape::Capsule { a: oa, b: ob, .. },
                        ) => segment_pair_midpoint(*a, *b, *oa, *ob),
                        (ResolvedShape::Capsule { a, b, .. }, _) => {
                            closest_pt_on_segment(op.center, *a, *b).midpoint(op.center)
                        }
                        (_, ResolvedShape::Capsule { a: oa, b: ob, .. }) => {
                            closest_pt_on_segment(sh.center(), *oa, *ob).midpoint(sh.center())
                        }
                        _ => sh.center().midpoint(op.center),
                    };
                    best_by_mask
                        .entry(op.owner)
                        .and_modify(|e| {
                            if dist < e.1 {
                                *e = (mi, dist, pos_nm);
                            }
                        })
                        .or_insert((mi, dist, pos_nm));
                }
            }
        }
        for (mi, dist, pos_nm) in best_by_mask.into_values() {
            let op = &openings[mi];
            push_clearance_violation(
                out,
                ClearanceHit {
                    kind: ViolationKind::SilkOverCopper,
                    layer: silk_layer,
                    pos_nm,
                    dist,
                    required_nm: req,
                    item_a: &item.label,
                    item_b: &op.label,
                    net_a: "",
                    net_b: op.net,
                    prefix: "silk ",
                },
            );
        }
    }
}

fn check_silk_to_edge(
    silk_items: &[SilkItem],
    outline: &IndexedRing,
    cutouts: &[IndexedRing],
    silk_layer: &str,
    req: Nm,
    out: &mut Vec<Violation>,
) {
    for item in silk_items {
        let mut worst: Option<(bool, f64, Pt)> = None;
        for sh in &item.shapes {
            let (dist, off) = indexed_shape_edge_clearance(sh, outline, cutouts, req);
            if (off || dist < req as f64 - EPS_NM)
                && worst.is_none_or(|(wo, wd, _)| (off && !wo) || (off == wo && dist < wd))
            {
                worst = Some((off, dist, sh.center()));
            }
        }
        if let Some((off, dist, pos_nm)) = worst {
            let meas = if off { 0 } else { dist.max(0.0).round() as Nm };
            let message = if off && req == 0 {
                format!("{} silk extends off board/cutout edge", item.label)
            } else {
                format!(
                    "{label} silk edge clearance {meas} nm < {req} nm",
                    label = item.label
                )
            };
            out.push(Violation {
                kind: ViolationKind::SilkEdgeClearance,
                severity: Severity::Error,
                layer: silk_layer.to_string(),
                pos_nm,
                measured_nm: meas,
                required_nm: req,
                item_a: item.label.clone(),
                item_b: Some("board_edge".into()),
                net_a: String::new(),
                net_b: String::new(),
                message,
            });
        }
    }
}

pub(crate) fn check_silkscreen(
    board: &BoardIr,
    cu_layers: &[String],
    pads: &[PadItem],
    vias: &[ViaItem],
    out: &mut Vec<Violation>,
) {
    let mask_req = board.rules.silk_to_mask_nm.filter(|&v| v >= 0);
    let edge_rings = board
        .rules
        .silk_to_edge_nm
        .filter(|&v| v >= 0 && board.outline.len() >= 3)
        .map(|req| {
            let outline = IndexedRing::new(&board.outline);
            let cutouts: Vec<IndexedRing> = board
                .cutouts
                .iter()
                .filter(|c| c.len() >= 3)
                .map(|c| IndexedRing::new(c))
                .collect();
            (req, outline, cutouts)
        });
    if mask_req.is_none() && edge_rings.is_none() {
        return;
    }

    for side in [Side::Top, Side::Bottom] {
        let silk_items = collect_silk_items(board, side);
        if silk_items.is_empty() {
            continue;
        }
        let silk_layer = match side {
            Side::Top => "F.SilkS",
            Side::Bottom => "B.SilkS",
        };
        if let Some(req) = mask_req {
            let openings = collect_mask_openings(board, cu_layers, pads, vias, side, true);
            check_silk_to_mask(&silk_items, &openings, silk_layer, req, out);
        }
        if let Some((req, ref outline, ref cutouts)) = edge_rings {
            check_silk_to_edge(&silk_items, outline, cutouts, silk_layer, req, out);
        }
    }
}
