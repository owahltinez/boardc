use std::borrow::Cow;

use crate::drc::{Violation, ViolationKind};
use crate::drc_scene::{
    ClearanceHit, PadItem, PourItem, TrackItem, ViaItem, push_clearance_violation,
};
use crate::fab_gerber::pad_world_shape_with_margin;
use crate::fab_silk::push_graphic_mask_shapes;
use crate::geom::{
    EPS_NM, SpatialGrid, closest_pt_on_segment, pad_world_layers, polys_overlap_area,
    remap_layer_for_side,
};
use crate::ir::{BBoxNm, BoardIr, Graphic, Nm, PadKind, Placement, Pt, Side, same_named_net};
use crate::shapes::ResolvedShape;

#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub(crate) enum MaskOwner {
    Pad {
        pad_idx: usize,
        pl_idx: usize,
        mask_only: bool,
    },
    Via(usize),
    Graphic(usize),
}

pub(crate) struct MaskOpening<'a> {
    pub(crate) label: Cow<'a, str>,
    pub(crate) net: &'a str,
    pub(crate) center: Pt,
    pub(crate) shape: ResolvedShape,
    pub(crate) bbox: BBoxNm,
    pub(crate) owner: MaskOwner,
}

enum TargetKind {
    Track { a: Pt, b: Pt },
    Via(usize),
    Pad { pad_idx: usize, pl_idx: usize },
}

struct CopperTarget<'a> {
    label: &'a str,
    net: &'a str,
    shape: &'a ResolvedShape,
    bbox: BBoxNm,
    center: Pt,
    kind: TargetKind,
}

pub(crate) fn collect_mask_openings<'a>(
    board: &'a BoardIr,
    cu_layers: &[String],
    pads: &'a [PadItem],
    vias: &'a [ViaItem],
    side: Side,
    include_graphics: bool,
) -> Vec<MaskOpening<'a>> {
    let mask_name = match side {
        Side::Top => "F.Mask",
        Side::Bottom => "B.Mask",
    };
    let cu_name = side.cu_layer();
    let mut openings = Vec::new();
    let mut pad_cursor = 0usize;
    for (pl_idx, pl) in board.placements.iter().enumerate() {
        let Some(fp) = board.footprints.get(&pl.footprint) else {
            continue;
        };
        for pad in &fp.pads {
            let pad_idx = pad_cursor;
            pad_cursor += 1;
            if !pad_world_layers(pad, pl.side, cu_layers)
                .iter()
                .any(|l| l == mask_name)
            {
                continue;
            }
            let margin = pad.mask_margin_nm.unwrap_or(board.rules.mask_expansion_nm);
            if let Some(shape) = pad_world_shape_with_margin(pad, pl, margin) {
                let item = &pads[pad_idx];
                let net = if pad.kind == PadKind::Npth {
                    ""
                } else {
                    item.net.as_str()
                };
                openings.push(MaskOpening {
                    label: Cow::Borrowed(&item.label),
                    net,
                    center: item.center,
                    bbox: shape.bbox(),
                    shape,
                    owner: MaskOwner::Pad {
                        pad_idx,
                        pl_idx,
                        mask_only: pad.kind != PadKind::Npth && item.layers.is_empty(),
                    },
                });
            }
        }
    }
    for (vi, (via, item)) in board.vias.iter().zip(vias).enumerate() {
        if !via.tented && via.touches_layer(cu_name, &board.layers) {
            let dia = via.diameter + 2 * board.rules.mask_expansion_nm;
            if dia > 0 {
                let shape = ResolvedShape::Circle {
                    center: via.pos,
                    radius: dia / 2,
                };
                openings.push(MaskOpening {
                    label: Cow::Borrowed(&item.label),
                    net: &item.net,
                    center: via.pos,
                    bbox: shape.bbox(),
                    shape,
                    owner: MaskOwner::Via(vi),
                });
            }
        }
    }
    if include_graphics {
        let mut graphic_id = 0usize;
        let mut shapes = Vec::new();
        let mut push_graphic = |label: String, g: &Graphic, pl: Option<&Placement>| {
            shapes.clear();
            push_graphic_mask_shapes(g, pl, &mut shapes);
            if shapes.is_empty() {
                return;
            }
            let gid = graphic_id;
            graphic_id += 1;
            for sh in shapes.drain(..) {
                openings.push(MaskOpening {
                    label: Cow::Owned(label.clone()),
                    net: "",
                    center: sh.center(),
                    bbox: sh.bbox(),
                    shape: sh,
                    owner: MaskOwner::Graphic(gid),
                });
            }
        };
        for pl in &board.placements {
            let Some(fp) = board.footprints.get(&pl.footprint) else {
                continue;
            };
            for (gi, g) in fp.graphics.iter().enumerate() {
                if remap_layer_for_side(g.layer(), pl.side) == mask_name {
                    push_graphic(format!("{}.mask#{gi}", pl.refdes), g, Some(pl));
                }
            }
        }
        for (gi, g) in board.graphics.iter().enumerate() {
            if g.layer() == mask_name {
                push_graphic(format!("mask#{gi}"), g, None);
            }
        }
    }
    openings
}

pub(crate) fn shape_violates_mask(a: &ResolvedShape, b: &ResolvedShape, req: Nm) -> Option<f64> {
    let dist = a.dist_to_shape(b);
    if dist < req as f64 - EPS_NM {
        return Some(dist);
    }
    // Polygon distance bottoms out at 0.0, so req == 0 needs an explicit area overlap check.
    if req == 0
        && dist <= EPS_NM
        && let (ResolvedShape::Poly { verts: va, .. }, ResolvedShape::Poly { verts: vb, .. }) =
            (a, b)
        && polys_overlap_area(va, vb) > 0.0
    {
        return Some(0.0);
    }
    None
}

fn pour_violates_mask(pour: &PourItem<'_>, shape: &ResolvedShape, req: Nm) -> Option<f64> {
    if req > 0 {
        let dist = pour.indexed.dist_to_shape_bounded(shape, req);
        return (dist < req as f64 - EPS_NM).then_some(dist);
    }
    pour.indexed.overlaps_shape(pour.poly, shape).then_some(0.0)
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
    let Some(req) = board.rules.mask_to_copper_nm.filter(|&v| v >= 0) else {
        return;
    };
    let mut candidates = Vec::new();

    for side in [Side::Top, Side::Bottom] {
        let cu_layer = side.cu_layer();
        if !cu_layers.iter().any(|l| l == cu_layer) {
            continue;
        }
        let openings = collect_mask_openings(board, cu_layers, pads, vias, side, false);
        if openings.is_empty() {
            continue;
        }

        let mut targets: Vec<CopperTarget<'_>> = Vec::new();
        for tr in tracks.iter().filter(|t| t.layer == cu_layer) {
            targets.push(CopperTarget {
                label: &tr.label,
                net: tr.net,
                shape: &tr.shape,
                bbox: tr.bbox,
                center: tr.shape.center(),
                kind: TargetKind::Track { a: tr.a, b: tr.b },
            });
        }
        for (vi, via) in vias.iter().enumerate() {
            if board.vias[vi].touches_layer(cu_layer, &board.layers) {
                targets.push(CopperTarget {
                    label: &via.label,
                    net: &via.net,
                    shape: &via.shape,
                    bbox: via.bbox,
                    center: via.pos,
                    kind: TargetKind::Via(vi),
                });
            }
        }
        for (pi, pad) in pads.iter().enumerate() {
            if pad.kind != PadKind::Npth && pad.layers.iter().any(|l| l == cu_layer) {
                targets.push(CopperTarget {
                    label: &pad.label,
                    net: &pad.net,
                    shape: &pad.shape,
                    bbox: pad.bbox,
                    center: pad.center,
                    kind: TargetKind::Pad {
                        pad_idx: pi,
                        pl_idx: pad.pl_idx,
                    },
                });
            }
        }
        let target_bboxes: Vec<BBoxNm> = targets.iter().map(|t| t.bbox).collect();
        let mut target_grid = SpatialGrid::new(&target_bboxes, 2_000_000);
        let side_pours: Vec<&PourItem<'_>> = pours.iter().filter(|p| p.layer == cu_layer).collect();

        for op in &openings {
            let q = op.bbox.expand(req);
            target_grid.query(q, &mut candidates);
            for &ti in &candidates {
                let target = &targets[ti];
                if !target.bbox.intersects(q) || same_named_net(op.net, target.net) {
                    continue;
                }
                match (&op.owner, &target.kind) {
                    (MaskOwner::Via(ovi), TargetKind::Via(tvi)) if ovi == tvi => continue,
                    (
                        MaskOwner::Pad {
                            pad_idx: opi,
                            pl_idx: opl,
                            mask_only,
                        },
                        TargetKind::Pad {
                            pad_idx: tpi,
                            pl_idx: tpl,
                        },
                    ) if opi == tpi
                        || (opl == tpl
                            && op.net.is_empty()
                            && (*mask_only
                                || (pads[*opi].kind != PadKind::Npth
                                    && target.net.is_empty()))) =>
                    {
                        continue;
                    }
                    _ => {}
                }
                if let Some(dist) = shape_violates_mask(&op.shape, target.shape, req) {
                    let pos_nm = match target.kind {
                        TargetKind::Track { a, b } => {
                            closest_pt_on_segment(op.center, a, b).midpoint(op.center)
                        }
                        TargetKind::Via(_) | TargetKind::Pad { .. } => {
                            op.center.midpoint(target.center)
                        }
                    };
                    push_clearance_violation(
                        out,
                        ClearanceHit {
                            kind: ViolationKind::SolderMaskToCopper,
                            layer: cu_layer,
                            pos_nm,
                            dist,
                            required_nm: req,
                            item_a: &op.label,
                            item_b: target.label,
                            net_a: op.net,
                            net_b: target.net,
                            prefix: "mask ",
                        },
                    );
                }
            }

            for pour in &side_pours {
                if same_named_net(op.net, pour.net) || !pour.bbox.intersects(q) {
                    continue;
                }
                if let Some(dist) = pour_violates_mask(pour, &op.shape, req) {
                    push_clearance_violation(
                        out,
                        ClearanceHit {
                            kind: ViolationKind::SolderMaskToCopper,
                            layer: cu_layer,
                            pos_nm: op.center,
                            dist,
                            required_nm: req,
                            item_a: &op.label,
                            item_b: &pour.label,
                            net_a: op.net,
                            net_b: pour.net,
                            prefix: "mask ",
                        },
                    );
                }
            }
        }
    }
}
