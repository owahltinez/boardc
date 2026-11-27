use crate::drc::{Severity, Violation, ViolationKind};
use crate::geom::{local_to_board, pad_copper_layers, pad_to_world_shape, remap_layer_for_side};
use crate::ir::{BBoxNm, BoardIr, KeepoutKind, Nm, PadKind, PadShape, PourPoly, Pt};
use crate::poly_index::IndexedPour;
use crate::shapes::ResolvedShape;
use crate::zone_scene::match_pour_specs;

pub(crate) struct PadItem {
    pub label: String,
    pub pl_idx: usize,
    pub net: String,
    pub kind: PadKind,
    pub center: Pt,
    pub shape: ResolvedShape,
    pub bbox: BBoxNm,
    pub layers: Vec<String>,
    pub drill_nm: Option<Nm>,
    pub min_dim_nm: Nm,
}

pub(crate) struct TrackItem<'a> {
    pub label: String,
    pub net: &'a str,
    pub layer: &'a str,
    pub a: Pt,
    pub b: Pt,
    pub width: Nm,
    pub shape: ResolvedShape,
    pub bbox: BBoxNm,
}

pub(crate) struct ViaItem {
    pub label: String,
    pub net: String,
    pub pos: Pt,
    pub dia: Nm,
    pub drill: Nm,
    pub shape: ResolvedShape,
    pub bbox: BBoxNm,
    pub layers: (String, String),
}

pub(crate) struct DrillItem {
    pub label: String,
    pub net: String,
    pub pos: Pt,
    pub radius: Nm,
    pub is_npth: bool,
    pub layers: Vec<String>,
    pub bbox: BBoxNm,
    pub pad_idx: Option<usize>,
    pub via_idx: Option<usize>,
}

pub(crate) struct PourItem<'a> {
    pub label: String,
    pub net: &'a str,
    pub layer: &'a str,
    pub poly: &'a PourPoly,
    pub bbox: BBoxNm,
    pub indexed: IndexedPour,
    pub clearance_override: Option<Nm>,
}

pub(crate) struct KeepoutItem {
    pub label: String,
    pub kind: KeepoutKind,
    pub layers: Vec<String>,
    pub poly: Vec<Pt>,
    pub shape: ResolvedShape,
    pub bbox: BBoxNm,
}

pub(crate) fn pad_min_dim(shape: &PadShape, resolved: &ResolvedShape) -> Nm {
    match shape {
        PadShape::Circle { diameter } => *diameter,
        PadShape::Rect { width, height }
        | PadShape::RoundRect { width, height, .. }
        | PadShape::Oval { width, height } => (*width).min(*height),
        PadShape::Poly { .. } => {
            let b = resolved.bbox();
            b.width().min(b.height())
        }
    }
}

pub(crate) struct ClearanceHit<'a> {
    pub kind: ViolationKind,
    pub layer: &'a str,
    pub pos_nm: Pt,
    pub dist: f64,
    pub required_nm: Nm,
    pub item_a: &'a str,
    pub item_b: &'a str,
    pub net_a: &'a str,
    pub net_b: &'a str,
    pub prefix: &'a str,
}

pub(crate) fn push_clearance_violation(out: &mut Vec<Violation>, hit: ClearanceHit<'_>) {
    let meas = hit.dist.max(0.0).round() as Nm;
    let message = if hit.required_nm == 0 {
        format!(
            "{}{} intrudes on {} (0 nm clearance required)",
            hit.prefix, hit.item_a, hit.item_b
        )
    } else {
        format!(
            "{}{} to {} clearance {meas} nm < {} nm",
            hit.prefix, hit.item_a, hit.item_b, hit.required_nm
        )
    };
    out.push(Violation {
        kind: hit.kind,
        severity: Severity::Error,
        layer: hit.layer.to_string(),
        pos_nm: hit.pos_nm,
        measured_nm: meas,
        required_nm: hit.required_nm,
        item_a: hit.item_a.to_string(),
        item_b: Some(hit.item_b.to_string()),
        net_a: hit.net_a.to_string(),
        net_b: hit.net_b.to_string(),
        message,
    });
}

pub(crate) fn collect_pads(board: &BoardIr, cu_layers: &[String]) -> Vec<PadItem> {
    let mut pads = Vec::new();
    for (pl_idx, pl) in board.placements.iter().enumerate() {
        let Some(fp) = board.footprints.get(&pl.footprint) else {
            continue;
        };
        for pad in &fp.pads {
            let net = pl.pin_nets.get(&pad.name).cloned().unwrap_or_default();
            let center = local_to_board(pad.pos, pl.pos.x, pl.pos.y, pl.rot_deg, pl.side);
            let shape = pad_to_world_shape(pad, pl, 16);
            let bbox = shape.bbox();
            let layers = pad_copper_layers(pad, pl.side, cu_layers);
            let min_dim_nm = pad_min_dim(&pad.shape, &shape);
            pads.push(PadItem {
                label: format!("{}.{}", pl.refdes, pad.name),
                pl_idx,
                net,
                kind: pad.kind,
                center,
                shape,
                bbox,
                layers,
                drill_nm: pad.drill_nm,
                min_dim_nm,
            });
        }
    }
    pads
}

pub(crate) fn collect_tracks(board: &BoardIr) -> Vec<TrackItem<'_>> {
    board
        .tracks
        .iter()
        .enumerate()
        .map(|(idx, t)| {
            let shape = ResolvedShape::Capsule {
                a: t.a,
                b: t.b,
                radius: t.width / 2,
            };
            TrackItem {
                label: format!("track#{idx}"),
                net: &t.net,
                layer: &t.layer,
                a: t.a,
                b: t.b,
                width: t.width,
                bbox: shape.bbox(),
                shape,
            }
        })
        .collect()
}

pub(crate) fn collect_vias(board: &BoardIr) -> Vec<ViaItem> {
    board
        .vias
        .iter()
        .enumerate()
        .map(|(i, v)| {
            let shape = ResolvedShape::Circle {
                center: v.pos,
                radius: v.diameter / 2,
            };
            ViaItem {
                label: format!("via#{i}"),
                net: v.net.clone(),
                pos: v.pos,
                dia: v.diameter,
                drill: v.drill,
                bbox: shape.bbox(),
                shape,
                layers: v.layers.clone(),
            }
        })
        .collect()
}

pub(crate) fn collect_drills(
    board: &BoardIr,
    pads: &[PadItem],
    vias: &[ViaItem],
    cu_layers: &[String],
) -> Vec<DrillItem> {
    let mut drills = Vec::new();
    for (i, pad) in pads.iter().enumerate() {
        if let Some(d) = pad.drill_nm
            && d > 0
        {
            let r = d / 2;
            drills.push(DrillItem {
                label: pad.label.clone(),
                net: pad.net.clone(),
                pos: pad.center,
                radius: r,
                is_npth: pad.kind == PadKind::Npth,
                layers: cu_layers.to_vec(),
                bbox: BBoxNm::from_pt(pad.center, r),
                pad_idx: Some(i),
                via_idx: None,
            });
        }
    }
    for (i, via) in vias.iter().enumerate() {
        if via.drill > 0 {
            let r = via.drill / 2;
            let raw_via = &board.vias[i];
            drills.push(DrillItem {
                label: via.label.clone(),
                net: via.net.clone(),
                pos: via.pos,
                radius: r,
                is_npth: false,
                layers: raw_via.spanned_layers(cu_layers, &board.layers),
                bbox: BBoxNm::from_pt(via.pos, r),
                pad_idx: None,
                via_idx: Some(i),
            });
        }
    }
    drills
}

pub(crate) fn collect_pours(board: &BoardIr) -> Vec<PourItem<'_>> {
    let matched = match_pour_specs(board, &board.filled_pours);
    let mut raw: Vec<(usize, &str, &str, &PourPoly, Option<Nm>)> = Vec::new();
    let mut poly_id = 0usize;
    for (fp, spec) in board.filled_pours.iter().zip(matched) {
        let spec_clr = spec.and_then(|s| s.clearance_nm);
        for poly in &fp.polys {
            if poly.outer.len() < 3 {
                continue;
            }
            raw.push((poly_id, &fp.net, &fp.layer, poly, spec_clr));
            poly_id += 1;
        }
    }
    fn build_pour_item<'a>(
        (id, net, layer, poly, clr): (usize, &'a str, &'a str, &'a PourPoly, Option<Nm>),
    ) -> PourItem<'a> {
        let indexed = IndexedPour::new(poly);
        let bbox = indexed.outer.bbox;
        PourItem {
            label: format!("pour#{id}"),
            net,
            layer,
            poly,
            bbox,
            indexed,
            clearance_override: clr,
        }
    }
    if raw.len() <= 1 {
        return raw.into_iter().map(build_pour_item).collect();
    }
    std::thread::scope(|s| {
        let handles: Vec<_> = raw
            .into_iter()
            .map(|item| s.spawn(move || build_pour_item(item)))
            .collect();
        handles
            .into_iter()
            .map(|h| h.join().expect("pour index thread panicked"))
            .collect()
    })
}

pub(crate) fn collect_keepouts(board: &BoardIr) -> Vec<KeepoutItem> {
    let mut out = Vec::new();
    for (i, ko) in board.keepouts.iter().enumerate() {
        if ko.poly.len() < 3 {
            continue;
        }
        let shape = ResolvedShape::polygon(ko.poly.clone());
        out.push(KeepoutItem {
            label: format!("keepout#{i}"),
            kind: ko.kind,
            layers: ko.layers.clone(),
            poly: ko.poly.clone(),
            bbox: shape.bbox(),
            shape,
        });
    }
    for pl in &board.placements {
        let Some(fp) = board.footprints.get(&pl.footprint) else {
            continue;
        };
        for (i, ko) in fp.keepouts.iter().enumerate() {
            if ko.poly.len() < 3 {
                continue;
            }
            let world: Vec<Pt> = ko
                .poly
                .iter()
                .map(|&p| local_to_board(p, pl.pos.x, pl.pos.y, pl.rot_deg, pl.side))
                .collect();
            let shape = ResolvedShape::polygon(world.clone());
            out.push(KeepoutItem {
                label: format!("{}.keepout#{i}", pl.refdes),
                kind: ko.kind,
                layers: ko
                    .layers
                    .iter()
                    .map(|l| remap_layer_for_side(l, pl.side))
                    .collect(),
                poly: world,
                bbox: shape.bbox(),
                shape,
            });
        }
    }
    out
}

pub(crate) fn compute_max_clearance(board: &BoardIr, pours: &[PourItem<'_>]) -> Nm {
    let mut m = board
        .rules
        .clearance_nm
        .max(board.rules.hole_to_hole_nm)
        .max(board.rules.effective_hole_clearance_nm())
        .max(board.rules.smd_to_smd_nm.unwrap_or(0))
        .max(board.rules.via_to_smd_nm.unwrap_or(0))
        .max(board.rules.via_to_via_nm.unwrap_or(0))
        .max(board.rules.via_to_track_nm.unwrap_or(0));
    for nc in board.net_classes.values() {
        m = m.max(nc.clearance_nm);
    }
    for p in pours {
        if let Some(c) = p.clearance_override {
            m = m.max(c);
        }
    }
    m
}
