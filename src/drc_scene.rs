use crate::drc::{Severity, Violation, ViolationKind};
use crate::geom::{
    SpatialGrid, local_to_board, pad_copper_layers, pad_to_world_shape, remap_layer_for_side,
};
use crate::ir::{BBoxNm, BoardIr, KeepoutKind, Nm, PadKind, PadShape, PourPoly, Pt};
use crate::poly_index::{IndexedPour, IndexedRing};
use crate::shapes::ResolvedShape;
use crate::zone_scene::match_pour_specs;

#[inline]
pub(crate) fn layer_mask_for_name(layer: &str, cu_layers: &[String]) -> (Option<usize>, u64) {
    let idx = cu_layers.iter().position(|l| l == layer);
    let mask = idx.filter(|&i| i < 64).map_or(0, |i| 1u64 << i);
    (idx, mask)
}

#[inline]
pub(crate) fn layer_mask_for_list(layers: &[String], cu_layers: &[String]) -> u64 {
    let mut mask = 0u64;
    for l in layers {
        if let Some(i) = cu_layers.iter().position(|c| c == l)
            && i < 64
        {
            mask |= 1u64 << i;
        }
    }
    mask
}

#[inline]
pub(crate) fn all_cu_mask(cu_layers: &[String]) -> u64 {
    if cu_layers.len() >= 64 {
        u64::MAX
    } else {
        (1u64 << cu_layers.len()) - 1
    }
}

#[derive(Clone, Copy, Default)]
pub(crate) struct NetClearance {
    explicit: Option<Nm>,
    non_default: Option<Nm>,
}

impl NetClearance {
    pub(crate) fn of(board: &BoardIr, net: &str) -> Self {
        let Some(nd) = board.nets.get(net) else {
            return Self::default();
        };
        let explicit = board.net_classes.get(&nd.class).map(|nc| nc.clearance_nm);
        let non_default = (nd.class != "Default").then_some(explicit).flatten();
        Self {
            explicit,
            non_default,
        }
    }

    #[inline]
    pub(crate) fn pair(self, other: Self, base_clr: Nm, rule_override: Option<Nm>) -> Nm {
        if let Some(ov) = rule_override {
            ov.max(self.non_default.unwrap_or(ov))
                .max(other.non_default.unwrap_or(ov))
        } else {
            base_clr
                .max(self.explicit.unwrap_or(base_clr))
                .max(other.explicit.unwrap_or(base_clr))
        }
    }

    #[inline]
    pub(crate) fn pour_pair(self, pour_clr_override: Option<Nm>, other: Self, base_clr: Nm) -> Nm {
        let base = pour_clr_override.unwrap_or_else(|| self.explicit.unwrap_or(base_clr));
        if pour_clr_override.is_some() {
            other.non_default.map_or(base, |c| base.max(c))
        } else {
            base.max(other.explicit.unwrap_or(base_clr))
        }
    }
}

pub(crate) struct PadItem {
    pub label: String,
    pub pl_idx: usize,
    pub net: String,
    pub kind: PadKind,
    pub center: Pt,
    pub shape: ResolvedShape,
    pub bbox: BBoxNm,
    pub layers: Vec<String>,
    pub layer_mask: u64,
    pub drill_nm: Option<Nm>,
    pub min_dim_nm: Nm,
    pub clr: NetClearance,
}

pub(crate) struct TrackItem<'a> {
    pub label: String,
    pub net: &'a str,
    pub layer: &'a str,
    pub layer_idx: Option<usize>,
    pub layer_mask: u64,
    pub a: Pt,
    pub b: Pt,
    pub width: Nm,
    pub shape: ResolvedShape,
    pub bbox: BBoxNm,
    pub clr: NetClearance,
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
    pub layer_mask: u64,
    pub clr: NetClearance,
}

pub(crate) struct DrillItem {
    pub label: String,
    pub net: String,
    pub pos: Pt,
    pub radius: Nm,
    pub is_npth: bool,
    pub layers: Vec<String>,
    pub layer_mask: u64,
    pub bbox: BBoxNm,
    pub pad_idx: Option<usize>,
    pub via_idx: Option<usize>,
    pub clr: NetClearance,
}

pub(crate) struct PourItem<'a> {
    pub label: String,
    pub net: &'a str,
    pub layer: &'a str,
    pub layer_idx: Option<usize>,
    pub layer_mask: u64,
    pub poly: &'a PourPoly,
    pub bbox: BBoxNm,
    pub indexed: IndexedPour,
    pub clearance_override: Option<Nm>,
    pub clr: NetClearance,
}

pub(crate) struct KeepoutItem {
    pub label: String,
    pub kind: KeepoutKind,
    pub layers: Vec<String>,
    pub poly: Vec<Pt>,
    pub shape: ResolvedShape,
    pub bbox: BBoxNm,
}

pub(crate) struct DrcGrids {
    pub pad_grid: SpatialGrid,
    pub via_grid: SpatialGrid,
    pub track_grid: SpatialGrid,
    pub tracks_by_layer: Vec<(Vec<usize>, SpatialGrid)>,
}

pub(crate) struct DrcInputs<'a> {
    pub cu_layers: &'a [String],
    pub tracks: &'a [TrackItem<'a>],
    pub pads: &'a [PadItem],
    pub vias: &'a [ViaItem],
    pub pours: &'a [PourItem<'a>],
    pub grids: &'a DrcGrids,
    pub max_clr: Nm,
}

pub(crate) struct BoardEdgeIndex {
    pub outline: IndexedRing,
    pub cutouts: Vec<IndexedRing>,
}

impl BoardEdgeIndex {
    pub(crate) fn new(board: &BoardIr) -> Option<Self> {
        if board.outline.len() < 3 {
            return None;
        }
        Some(Self {
            outline: IndexedRing::new(&board.outline),
            cutouts: board
                .cutouts
                .iter()
                .filter(|c| c.len() >= 3)
                .map(|c| IndexedRing::new(c))
                .collect(),
        })
    }
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
            let layer_mask = layer_mask_for_list(&layers, cu_layers);
            let min_dim_nm = pad_min_dim(&pad.shape, &shape);
            let clr = NetClearance::of(board, &net);
            pads.push(PadItem {
                label: format!("{}.{}", pl.refdes, pad.name),
                pl_idx,
                net,
                kind: pad.kind,
                center,
                shape,
                bbox,
                layers,
                layer_mask,
                drill_nm: pad.drill_nm,
                min_dim_nm,
                clr,
            });
        }
    }
    pads
}

pub(crate) fn collect_tracks<'a>(board: &'a BoardIr, cu_layers: &[String]) -> Vec<TrackItem<'a>> {
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
            let (layer_idx, layer_mask) = layer_mask_for_name(&t.layer, cu_layers);
            TrackItem {
                label: format!("track#{idx}"),
                net: &t.net,
                layer: &t.layer,
                layer_idx,
                layer_mask,
                a: t.a,
                b: t.b,
                width: t.width,
                bbox: shape.bbox(),
                shape,
                clr: NetClearance::of(board, &t.net),
            }
        })
        .collect()
}

pub(crate) fn collect_vias(board: &BoardIr, cu_layers: &[String]) -> Vec<ViaItem> {
    board
        .vias
        .iter()
        .enumerate()
        .map(|(i, v)| {
            let shape = ResolvedShape::Circle {
                center: v.pos,
                radius: v.diameter / 2,
            };
            let spanned = v.spanned_layers(cu_layers, &board.layers);
            let layer_mask = layer_mask_for_list(&spanned, cu_layers);
            ViaItem {
                label: format!("via#{i}"),
                net: v.net.clone(),
                pos: v.pos,
                dia: v.diameter,
                drill: v.drill,
                bbox: shape.bbox(),
                shape,
                layers: v.layers.clone(),
                layer_mask,
                clr: NetClearance::of(board, &v.net),
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
    let full_cu_mask = all_cu_mask(cu_layers);
    for (i, pad) in pads.iter().enumerate() {
        if let Some(d) = pad.drill_nm
            && d > 0
        {
            let r = d / 2;
            let is_npth = pad.kind == PadKind::Npth;
            drills.push(DrillItem {
                label: pad.label.clone(),
                net: pad.net.clone(),
                pos: pad.center,
                radius: r,
                is_npth,
                layers: cu_layers.to_vec(),
                layer_mask: full_cu_mask,
                bbox: BBoxNm::from_pt(pad.center, r),
                pad_idx: Some(i),
                via_idx: None,
                clr: if is_npth {
                    NetClearance::default()
                } else {
                    pad.clr
                },
            });
        }
    }
    for (i, via) in vias.iter().enumerate() {
        if via.drill > 0 {
            let r = via.drill / 2;
            let raw_via = &board.vias[i];
            let layers = raw_via.spanned_layers(cu_layers, &board.layers);
            drills.push(DrillItem {
                label: via.label.clone(),
                net: via.net.clone(),
                pos: via.pos,
                radius: r,
                is_npth: false,
                layer_mask: via.layer_mask,
                layers,
                bbox: BBoxNm::from_pt(via.pos, r),
                pad_idx: None,
                via_idx: Some(i),
                clr: via.clr,
            });
        }
    }
    drills
}

pub(crate) fn collect_pours(board: &BoardIr) -> Vec<PourItem<'_>> {
    let cu_layers = board.copper_layer_names();
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
        board: &'a BoardIr,
        cu_layers: &[String],
        (id, net, layer, poly, clr): (usize, &'a str, &'a str, &'a PourPoly, Option<Nm>),
    ) -> PourItem<'a> {
        let indexed = IndexedPour::new(poly);
        let bbox = indexed.outer.bbox;
        let (layer_idx, layer_mask) = layer_mask_for_name(layer, cu_layers);
        PourItem {
            label: format!("pour#{id}"),
            net,
            layer,
            layer_idx,
            layer_mask,
            poly,
            bbox,
            indexed,
            clearance_override: clr,
            clr: NetClearance::of(board, net),
        }
    }
    if raw.len() <= 1 {
        return raw
            .into_iter()
            .map(|item| build_pour_item(board, &cu_layers, item))
            .collect();
    }
    std::thread::scope(|s| {
        let cu_ref = &cu_layers;
        let handles: Vec<_> = raw
            .into_iter()
            .map(|item| s.spawn(move || build_pour_item(board, cu_ref, item)))
            .collect();
        handles
            .into_iter()
            .map(|h| h.join().expect("pour index thread panicked"))
            .collect()
    })
}

pub(crate) fn collect_drc_grids(
    cu_layers: &[String],
    pads: &[PadItem],
    vias: &[ViaItem],
    tracks: &[TrackItem<'_>],
) -> DrcGrids {
    let pad_grid = SpatialGrid::new(pads.iter().map(|p| p.bbox), 2_000_000);
    let via_grid = SpatialGrid::new(vias.iter().map(|v| v.bbox), 2_000_000);
    let track_grid = SpatialGrid::new(tracks.iter().map(|t| t.bbox), 2_000_000);
    let mut layer_indices: Vec<Vec<usize>> = vec![Vec::new(); cu_layers.len()];
    for (ti, tr) in tracks.iter().enumerate() {
        if let Some(li) = tr.layer_idx {
            layer_indices[li].push(ti);
        }
    }
    let tracks_by_layer = layer_indices
        .into_iter()
        .map(|idxs| {
            let grid = SpatialGrid::new(idxs.iter().map(|&ti| tracks[ti].bbox), 2_000_000);
            (idxs, grid)
        })
        .collect();
    DrcGrids {
        pad_grid,
        via_grid,
        track_grid,
        tracks_by_layer,
    }
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
