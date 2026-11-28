use std::collections::{BTreeMap, BTreeSet, HashSet};
use std::f64::consts::PI;
use std::time::Instant;

use viahop::analyze_fanout;

use crate::drc_constraints::count_rule_vias;
use crate::drc_layout::keepout_matches_layer;
use crate::drc_scene::{
    BoardEdgeIndex, KeepoutItem, PadItem, collect_keepouts, collect_pads, compute_max_clearance,
    pad_min_dim,
};
use crate::geom::{
    EPS_NM, GridQueryScratch, SpatialGrid, closest_pt_on_segment, courtyard_world_polygon,
    local_to_board, pad_copper_layers, pad_query_bbox, point_in_polygon, point_to_segment_dist,
};
use crate::ir::{
    BBoxNm, BoardIr, LayerKind, Nm, PadDef, PadKind, PadShape, Placement, Pt, Track, Via,
    same_named_net,
};
use crate::place::{PowerViaOptions, PowerViaReport, SolvedViaDrop, UnresolvedPowerPad};
use crate::poly_index::{IndexedPour, indexed_shape_edge_clearance};
use crate::route::to_viahop_board;
use crate::shapes::{ResolvedShape, transform_pad_local_pt};
use crate::zone_scene::match_pour_specs;
use crate::zones::fill_zones;

const PLANE_DROP_BATCH_SIZE: usize = 16;

struct SmdTarget {
    pl_idx: usize,
    pad_idx: usize,
    pad_def_idx: usize,
    label: String,
    net: String,
    pad_layer: String,
    center: Pt,
    comp_center: Pt,
    min_dim_nm: Nm,
}

struct IndexedFilledPour {
    net: String,
    layer: String,
    clearance_nm: Option<Nm>,
    islands: Vec<IndexedPour>,
}

struct PlaneDropState {
    drops: Vec<SolvedViaDrop>,
    unresolved: Vec<UnresolvedPowerPad>,
    plane_vias_added: usize,
    stubs_added: usize,
    pofv_count: usize,
}

struct BatchSnapshot {
    vias_len: usize,
    tracks_len: usize,
    drops_len: usize,
    unresolved_len: usize,
    plane_vias_added: usize,
    stubs_added: usize,
    pofv_count: usize,
}

fn via_span_layers(cu_layers: &[String]) -> (String, String) {
    if cu_layers.len() >= 2 {
        (cu_layers[0].clone(), cu_layers[cu_layers.len() - 1].clone())
    } else {
        ("F.Cu".into(), "B.Cu".into())
    }
}

fn is_plane_or_inner_layer(board: &BoardIr, layer: &str) -> bool {
    if let Some(ld) = board.layers.iter().find(|l| l.name == layer) {
        return ld.kind == LayerKind::Plane || ld.side.is_none();
    }
    !matches!(layer, "F.Cu" | "B.Cu")
}

fn identify_plane_nets(board: &BoardIr, cfg: &PowerViaOptions) -> BTreeSet<String> {
    let mut nets: BTreeSet<String> = cfg
        .plane_nets
        .iter()
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
        .collect();
    let cu_layers = board.copper_layer_names();
    let mut pad_layers_by_net: BTreeMap<String, BTreeSet<String>> = BTreeMap::new();
    for pl in &board.placements {
        if let Some(fp) = board.footprints.get(&pl.footprint) {
            for pad in &fp.pads {
                if pad.kind == PadKind::Smd
                    && let Some(net) = pl.pin_nets.get(&pad.name)
                    && !net.is_empty()
                {
                    let layers = pad_copper_layers(pad, pl.side, &cu_layers);
                    pad_layers_by_net
                        .entry(net.clone())
                        .or_default()
                        .extend(layers);
                }
            }
        }
    }
    for pour in &board.pours {
        if pour.net.is_empty() {
            continue;
        }
        if is_plane_or_inner_layer(board, &pour.layer)
            || pad_layers_by_net
                .get(&pour.net)
                .is_some_and(|ls| ls.iter().any(|l| l != &pour.layer))
        {
            nets.insert(pour.net.clone());
        }
    }
    nets
}

fn via_antipad_radius(
    board: &BoardIr,
    pour_net: &str,
    pour_clr_ov: Option<Nm>,
    pour_layer: &str,
    via: &Via,
) -> Nm {
    let clr = board.pour_pair_clearance(pour_net, pour_clr_ov, &via.net);
    let hole_clr = clr.max(board.rules.effective_hole_clearance_nm());
    let mut radius = (via.diameter / 2 + clr).max(via.drill / 2 + hole_clr);
    if matches!(pour_layer, "F.Cu" | "B.Cu")
        && let Some(m2c) = board.rules.mask_to_copper_nm.filter(|&v| v >= 0)
        && !via.tented
    {
        let mask_dia = via.diameter + 2 * board.rules.mask_expansion_nm;
        if mask_dia > 0 {
            radius = radius.max(mask_dia / 2 + m2c);
        }
    }
    ((radius as f64) / (PI / 32.0).cos()).ceil() as Nm
}

fn stub_cleared_radius(
    board: &BoardIr,
    pour_net: &str,
    pour_clr_ov: Option<Nm>,
    stub: &Track,
) -> Nm {
    let clr = board.pour_pair_clearance(pour_net, pour_clr_ov, &stub.net);
    (((stub.width / 2 + clr) as f64) / (PI / 32.0).cos()).ceil() as Nm
}

fn build_indexed_pours(probe: &BoardIr) -> Vec<IndexedFilledPour> {
    let matched = match_pour_specs(probe, &probe.filled_pours);
    probe
        .filled_pours
        .iter()
        .zip(matched)
        .map(|(fp, spec)| IndexedFilledPour {
            net: fp.net.clone(),
            layer: fp.layer.clone(),
            clearance_nm: spec.and_then(|s| s.clearance_nm),
            islands: fp.polys.iter().map(IndexedPour::new).collect(),
        })
        .collect()
}

fn make_probe_board(board: &BoardIr) -> BoardIr {
    let mut probe = board.clone();
    for p in &mut probe.pours {
        p.remove_islands = false;
    }
    probe.filled_pours.clear();
    probe
}

fn local_pad_bbox(pad: &PadDef) -> BBoxNm {
    match &pad.shape {
        PadShape::Circle { diameter } => BBoxNm::from_pt(Pt::new(0, 0), diameter / 2),
        PadShape::Rect { width, height }
        | PadShape::RoundRect { width, height, .. }
        | PadShape::Oval { width, height } => {
            BBoxNm::new(-width / 2, -height / 2, width / 2, height / 2)
        }
        PadShape::Poly { verts } => BBoxNm::from_pts(verts),
    }
}

fn thermal_grid_world_pts(
    pad: &PadDef,
    pl: &Placement,
    target_net: &str,
    via_dia: Nm,
    via_drill: Nm,
    board: &BoardIr,
    cfg: &PowerViaOptions,
) -> Vec<Pt> {
    let lbb = local_pad_bbox(pad);
    let mut inset = via_dia / 2 + 50_000;
    let mut ux = lbb.width() - 2 * inset;
    let mut uy = lbb.height() - 2 * inset;
    if ux < 0 || uy < 0 {
        inset = via_drill / 2 + board.rules.min_annular_ring_nm;
        ux = lbb.width() - 2 * inset;
        uy = lbb.height() - 2 * inset;
    }
    if ux < 0 || uy < 0 {
        return vec![local_to_board(
            pad.pos, pl.pos.x, pl.pos.y, pl.rot_deg, pl.side,
        )];
    }
    let min_pitch = cfg
        .thermal_pitch_nm
        .max(via_drill + board.rules.hole_to_hole_nm)
        .max(via_dia + 100_000);
    // Prefer spreading thermal vias enough to preserve unbroken webs on opposite-net planes.
    let plane_web_pitch = board
        .pours
        .iter()
        .filter(|p| !same_named_net(&p.net, target_net))
        .map(|p| {
            let clr = board.pour_pair_clearance(&p.net, p.clearance_nm, target_net);
            let hole_clr = clr.max(board.rules.effective_hole_clearance_nm());
            let antipad_r = (via_dia / 2 + clr).max(via_drill / 2 + hole_clr);
            2 * antipad_r + p.min_width_nm * 2 + 40_000
        })
        .max()
        .unwrap_or(0);
    let max_spread = (min_pitch + 200_000).max(plane_web_pitch);
    let nx = (ux / min_pitch) as usize + 1;
    let ny = (uy / min_pitch) as usize + 1;
    let c = lbb.center();
    let sx = if nx > 1 {
        (ux / (nx as Nm - 1)).min(max_spread)
    } else {
        0
    };
    let sy = if ny > 1 {
        (uy / (ny as Nm - 1)).min(max_spread)
    } else {
        0
    };
    let x0 = c.x - sx * (nx as Nm - 1) / 2;
    let y0 = c.y - sy * (ny as Nm - 1) / 2;

    let mut pts = Vec::with_capacity(nx * ny);
    for iy in 0..ny {
        for ix in 0..nx {
            let local_pt = Pt::new(x0 + (ix as Nm) * sx, y0 + (iy as Nm) * sy);
            pts.push(transform_pad_local_pt(local_pt, pad, pl));
        }
    }
    pts
}

fn radial_candidates(pad: &PadItem, comp_center: Pt, via_dia: Nm, max_stub_len: Nm) -> Vec<Pt> {
    let out_x = (pad.center.x - comp_center.x) as f64;
    let out_y = (pad.center.y - comp_center.y) as f64;
    let out_len = out_x.hypot(out_y);
    let (ox, oy) = if out_len > 1.0 {
        (out_x / out_len, out_y / out_len)
    } else {
        (1.0, 0.0)
    };

    let mut angles_deg: Vec<f64> = vec![
        0.0, 90.0, 180.0, 270.0, 45.0, 135.0, 225.0, 315.0, 22.5, 67.5, 112.5, 157.5, 202.5, 247.5,
        292.5, 337.5,
    ];
    angles_deg.sort_by(|a, b| {
        let ra = a.to_radians();
        let rb = b.to_radians();
        let da = ra.cos() * ox + ra.sin() * oy;
        let db = rb.cos() * ox + rb.sin() * oy;
        let card_a = (a % 90.0).abs() < 1e-6;
        let card_b = (b % 90.0).abs() < 1e-6;
        card_b
            .cmp(&card_a)
            .then_with(|| db.total_cmp(&da))
            .then_with(|| a.total_cmp(b))
    });

    let half_max = pad.bbox.width().max(pad.bbox.height()) / 2;
    let min_r = (pad.min_dim_nm / 2 + via_dia / 2 + 40_000).max(via_dia / 2 + 80_000);
    let max_r = max_stub_len.max(half_max + via_dia / 2 + 100_000);
    let mut cands = Vec::new();
    let mut r = min_r;
    while r <= max_r {
        for &deg in &angles_deg {
            let rad = deg.to_radians();
            cands.push(Pt::new(
                pad.center.x + (r as f64 * rad.cos()).round() as Nm,
                pad.center.y + (r as f64 * rad.sin()).round() as Nm,
            ));
        }
        r += 150_000;
    }
    cands
}

fn track_bbox(t: &Track) -> BBoxNm {
    BBoxNm::from_pts(&[t.a, t.b]).expand(t.width / 2)
}

fn count_sealed_bga_pads(board: &BoardIr) -> usize {
    let Ok(vb) = to_viahop_board(board, &HashSet::new(), None) else {
        return 0;
    };
    analyze_fanout(&vb)
        .bga_clusters
        .iter()
        .map(|c| c.sealed_pad_ids.len())
        .sum()
}

struct PowerViaSolver<'a> {
    board: &'a mut BoardIr,
    probe: BoardIr,
    indexed_pours: Vec<IndexedFilledPour>,
    baseline_chokes: usize,
    baseline_sealed: usize,
    pads: Vec<PadItem>,
    pad_bboxes: Vec<BBoxNm>,
    pad_grid: SpatialGrid,
    is_dense_pad: Vec<bool>,
    keepouts: Vec<KeepoutItem>,
    keepout_grid: SpatialGrid,
    edge_idx: Option<BoardEdgeIndex>,
    via_grid: SpatialGrid,
    via_bboxes: Vec<BBoxNm>,
    track_grid: SpatialGrid,
    track_bboxes: Vec<BBoxNm>,
    max_clr: Nm,
    dense_bga_zones: Vec<BBoxNm>,
    via_layers: (String, String),
    cfg: &'a PowerViaOptions,
    scratch: GridQueryScratch,
    cands: Vec<usize>,
}

impl<'a> PowerViaSolver<'a> {
    fn pad_already_has_via(&mut self, pad_idx: usize, max_stub_len: Nm) -> bool {
        let pad = &self.pads[pad_idx];
        let layer = pad.layers.first().map(String::as_str).unwrap_or("F.Cu");
        let v_reach = max_stub_len + 10_000 + self.max_clr;
        let vq = pad.bbox.expand(v_reach);
        self.via_grid
            .query_with(vq, &mut self.scratch, &mut self.cands);
        let mut near_vias = Vec::new();
        for &vi in &self.cands {
            let v = &self.board.vias[vi];
            if same_named_net(&pad.net, &v.net) {
                if pad.shape.dist_to_circle(v.pos, v.diameter / 2) <= 10_000.0 {
                    return true;
                }
                if v.pos.dist(pad.center) <= max_stub_len as f64 + 10_000.0 {
                    near_vias.push(vi);
                }
            }
        }
        if near_vias.is_empty() {
            return false;
        }
        let tq = pad.bbox.expand(10_000);
        self.track_grid
            .query_with(tq, &mut self.scratch, &mut self.cands);
        for &ti in &self.cands {
            let t = &self.board.tracks[ti];
            if t.layer != layer || !same_named_net(&t.net, &pad.net) {
                continue;
            }
            if pad
                .shape
                .dist_to_point(closest_pt_on_segment(pad.center, t.a, t.b))
                > 10_000.0
            {
                continue;
            }
            for &vi in &near_vias {
                let v = &self.board.vias[vi];
                if t.a == v.pos
                    || t.b == v.pos
                    || point_to_segment_dist(v.pos, t.a, t.b) <= v.diameter as f64 / 2.0
                {
                    return true;
                }
            }
        }
        false
    }

    fn cleared_by_pending_obstacles(
        &self,
        pour: &IndexedFilledPour,
        pos: Pt,
        margin_nm: Nm,
        batch_via_start: usize,
        batch_track_start: usize,
    ) -> bool {
        for v in &self.board.vias[batch_via_start..] {
            if !same_named_net(&pour.net, &v.net)
                && v.touches_layer(&pour.layer, &self.board.layers)
            {
                let r_anti =
                    via_antipad_radius(self.board, &pour.net, pour.clearance_nm, &pour.layer, v);
                if pos.dist(v.pos) < (r_anti + margin_nm) as f64 - EPS_NM {
                    return false;
                }
            }
        }
        for st in &self.board.tracks[batch_track_start..] {
            if st.layer == pour.layer && !same_named_net(&pour.net, &st.net) {
                let r_stub = stub_cleared_radius(self.board, &pour.net, pour.clearance_nm, st);
                if point_to_segment_dist(pos, st.a, st.b) < (r_stub + margin_nm) as f64 - EPS_NM {
                    return false;
                }
            }
        }
        true
    }

    fn pad_on_same_layer_plane(
        &self,
        net: &str,
        pad_layer: &str,
        pos: Pt,
        batch_via_start: usize,
        batch_track_start: usize,
    ) -> bool {
        self.indexed_pours.iter().any(|pour| {
            pour.net == net
                && pour.layer == pad_layer
                && pour.islands.iter().any(|ip| ip.contains_pt(pos))
                && self.cleared_by_pending_obstacles(
                    pour,
                    pos,
                    0,
                    batch_via_start,
                    batch_track_start,
                )
        })
    }

    fn point_inside_target_plane(
        &self,
        net: &str,
        pad_layer: &str,
        pos: Pt,
        via_dia: Nm,
        batch_via_start: usize,
        batch_track_start: usize,
    ) -> bool {
        let mut has_target_pour = false;
        let req_dist_nm = via_dia / 2 + self.cfg.plane_edge_margin_nm;
        let req_dist = req_dist_nm as f64 - EPS_NM;
        for pour in &self.indexed_pours {
            if pour.net != net || pour.layer == pad_layer {
                continue;
            }
            has_target_pour = true;
            if pour.islands.iter().any(|ip| {
                ip.contains_pt(pos) && ip.min_dist_to_boundary_bounded(pos, req_dist_nm) >= req_dist
            }) && self.cleared_by_pending_obstacles(
                pour,
                pos,
                req_dist_nm,
                batch_via_start,
                batch_track_start,
            ) {
                return true;
            }
        }
        if !has_target_pour {
            // Only allow free placement if the board defines no pours at all for this explicit plane net.
            return !self.board.pours.iter().any(|p| p.net == net);
        }
        false
    }

    fn via_clears_board_and_objects(
        &mut self,
        net: &str,
        pos: Pt,
        dia: Nm,
        drill: Nm,
        ignore_pad_idx: Option<usize>,
    ) -> bool {
        if let Some(ref edge) = self.edge_idx {
            let vs = ResolvedShape::Circle {
                center: pos,
                radius: dia / 2,
            };
            let (d, off) = indexed_shape_edge_clearance(
                &vs,
                &edge.outline,
                &edge.cutouts,
                self.board.rules.edge_clearance_nm,
            );
            if off || d < self.board.rules.edge_clearance_nm as f64 - EPS_NM {
                return false;
            }
        }

        let hole_clr = self.board.rules.effective_hole_clearance_nm();
        let tmp_via = Via {
            net: net.to_string(),
            pos,
            diameter: dia,
            drill,
            layers: self.via_layers.clone(),
            locked: true,
            tented: true,
        };
        let r_max = (dia / 2).max(drill / 2);
        let via_bb = BBoxNm::from_pt(pos, r_max);
        let q_bb = via_bb.expand(self.max_clr);

        self.keepout_grid
            .query_with(via_bb, &mut self.scratch, &mut self.cands);
        for &ki in &self.cands {
            let ko = &self.keepouts[ki];
            if ko.kind.blocks_vias()
                && via_bb.intersects(ko.bbox)
                && (ko.layers.is_empty()
                    || ko
                        .layers
                        .iter()
                        .any(|l| l == "*.Cu" || tmp_via.touches_layer(l, &self.board.layers)))
                && (point_in_polygon(pos, &ko.poly)
                    || ko.shape.dist_to_circle(pos, dia / 2) < -EPS_NM)
            {
                return false;
            }
        }

        self.pad_grid
            .query_with(q_bb, &mut self.scratch, &mut self.cands);
        for &pi in &self.cands {
            if !q_bb.intersects(self.pad_bboxes[pi]) {
                continue;
            }
            let pad = &self.pads[pi];
            if let Some(d_pad) = pad.drill_nm
                && d_pad > 0
                && pos.dist(pad.center) - ((drill / 2 + d_pad / 2) as f64)
                    < self.board.rules.hole_to_hole_nm as f64 - EPS_NM
            {
                return false;
            }
            if pad.kind == PadKind::Npth {
                if let Some(d_pad) = pad.drill_nm
                    && d_pad > 0
                {
                    let req = self.board.pair_clearance(net, "", None).max(hole_clr);
                    if pos.dist(pad.center) - ((dia / 2 + d_pad / 2) as f64) < req as f64 - EPS_NM {
                        return false;
                    }
                }
                continue;
            }
            if Some(pi) == ignore_pad_idx || same_named_net(net, &pad.net) {
                continue;
            }
            let shares_layer = pad
                .layers
                .iter()
                .any(|l| tmp_via.touches_layer(l, &self.board.layers));
            if shares_layer {
                let rule_ov = (pad.kind == PadKind::Smd)
                    .then_some(self.board.rules.via_to_smd_nm)
                    .flatten();
                let req = self.board.pair_clearance(net, &pad.net, rule_ov);
                if pad.shape.dist_to_circle(pos, dia / 2) < req as f64 - EPS_NM {
                    return false;
                }
                let drill_req = self.board.pair_clearance(net, &pad.net, None).max(hole_clr);
                if pad.shape.dist_to_circle(pos, drill / 2) < drill_req as f64 - EPS_NM {
                    return false;
                }
            }
            if let Some(d_pad) = pad.drill_nm
                && d_pad > 0
            {
                let req = self.board.pair_clearance(net, &pad.net, None).max(hole_clr);
                if pos.dist(pad.center) - ((dia / 2 + d_pad / 2) as f64) < req as f64 - EPS_NM {
                    return false;
                }
            }
        }

        self.via_grid
            .query_with(q_bb, &mut self.scratch, &mut self.cands);
        for &vi in &self.cands {
            if !q_bb.intersects(self.via_bboxes[vi]) {
                continue;
            }
            let other = &self.board.vias[vi];
            if pos.dist(other.pos) - ((drill / 2 + other.drill / 2) as f64)
                < self.board.rules.hole_to_hole_nm as f64 - EPS_NM
            {
                return false;
            }
            if !same_named_net(net, &other.net) {
                let req =
                    self.board
                        .pair_clearance(net, &other.net, self.board.rules.via_to_via_nm);
                if pos.dist(other.pos) - (dia + other.diameter) as f64 / 2.0 < req as f64 - EPS_NM {
                    return false;
                }
            }
        }

        self.track_grid
            .query_with(q_bb, &mut self.scratch, &mut self.cands);
        for &ti in &self.cands {
            if !q_bb.intersects(self.track_bboxes[ti]) {
                continue;
            }
            let t = &self.board.tracks[ti];
            if same_named_net(net, &t.net) || !tmp_via.touches_layer(&t.layer, &self.board.layers) {
                continue;
            }
            let req = self
                .board
                .pair_clearance(net, &t.net, self.board.rules.via_to_track_nm);
            let dist = point_to_segment_dist(pos, t.a, t.b) - (dia / 2 + t.width / 2) as f64;
            if dist < req as f64 - EPS_NM {
                return false;
            }
        }

        true
    }

    fn stub_clears_board_and_objects(
        &mut self,
        net: &str,
        layer: &str,
        a: Pt,
        b: Pt,
        width: Nm,
    ) -> bool {
        let seg = ResolvedShape::Capsule {
            a,
            b,
            radius: width / 2,
        };
        let seg_bb = seg.bbox();
        if let Some(ref edge) = self.edge_idx {
            let (d, off) = indexed_shape_edge_clearance(
                &seg,
                &edge.outline,
                &edge.cutouts,
                self.board.rules.edge_clearance_nm,
            );
            if off || d < self.board.rules.edge_clearance_nm as f64 - EPS_NM {
                return false;
            }
        }
        let hole_clr = self.board.rules.effective_hole_clearance_nm();
        let q_bb = seg_bb.expand(self.max_clr);

        self.keepout_grid
            .query_with(seg_bb, &mut self.scratch, &mut self.cands);
        for &ki in &self.cands {
            let ko = &self.keepouts[ki];
            if ko.kind.blocks_tracks()
                && seg_bb.intersects(ko.bbox)
                && keepout_matches_layer(&ko.layers, layer)
                && (point_in_polygon(a, &ko.poly)
                    || point_in_polygon(b, &ko.poly)
                    || seg.dist_to_shape(&ko.shape) < -EPS_NM)
            {
                return false;
            }
        }

        self.pad_grid
            .query_with(q_bb, &mut self.scratch, &mut self.cands);
        for &pi in &self.cands {
            if !q_bb.intersects(self.pad_bboxes[pi]) {
                continue;
            }
            let pad = &self.pads[pi];
            if pad.kind == PadKind::Npth {
                if let Some(d_pad) = pad.drill_nm
                    && d_pad > 0
                {
                    let req = self.board.pair_clearance(net, "", None).max(hole_clr);
                    if seg.dist_to_circle(pad.center, d_pad / 2) < req as f64 - EPS_NM {
                        return false;
                    }
                }
                continue;
            }
            if same_named_net(net, &pad.net) {
                continue;
            }
            if let Some(d_pad) = pad.drill_nm
                && d_pad > 0
            {
                let req = self.board.pair_clearance(net, &pad.net, None).max(hole_clr);
                if seg.dist_to_circle(pad.center, d_pad / 2) < req as f64 - EPS_NM {
                    return false;
                }
            }
            if pad.layers.iter().any(|l| l == layer) {
                let req = self.board.pair_clearance(net, &pad.net, None);
                if pad.shape.dist_to_capsule(a, b, width / 2) < req as f64 - EPS_NM {
                    return false;
                }
            }
        }

        self.via_grid
            .query_with(q_bb, &mut self.scratch, &mut self.cands);
        for &vi in &self.cands {
            if !q_bb.intersects(self.via_bboxes[vi]) {
                continue;
            }
            let v = &self.board.vias[vi];
            if same_named_net(net, &v.net) || !v.touches_layer(layer, &self.board.layers) {
                continue;
            }
            let req = self
                .board
                .pair_clearance(net, &v.net, self.board.rules.via_to_track_nm);
            if seg.dist_to_circle(v.pos, v.diameter / 2) < req as f64 - EPS_NM {
                return false;
            }
        }

        self.track_grid
            .query_with(q_bb, &mut self.scratch, &mut self.cands);
        for &ti in &self.cands {
            if !q_bb.intersects(self.track_bboxes[ti]) {
                continue;
            }
            let t = &self.board.tracks[ti];
            if t.layer != layer || same_named_net(net, &t.net) {
                continue;
            }
            let req = self.board.pair_clearance(net, &t.net, None);
            let ts = ResolvedShape::Capsule {
                a: t.a,
                b: t.b,
                radius: t.width / 2,
            };
            if seg.dist_to_shape(&ts) < req as f64 - EPS_NM {
                return false;
            }
        }
        true
    }

    fn local_pad_has_escape(
        &self,
        sig_pad: &PadItem,
        nearby_pads: &[(usize, Nm)],
        extra_via: Option<(Pt, Nm)>,
        extra_stub: Option<(Pt, Pt, Nm)>,
    ) -> bool {
        let w = self.board.net_track_width(&sig_pad.net);
        let clr = self.board.pair_clearance(&sig_pad.net, "", None);
        let reach = 1_500_000.0;
        for k in 0..8 {
            let ang = (k as f64) * std::f64::consts::FRAC_PI_4;
            let end = Pt::new(
                sig_pad.center.x + (reach * ang.cos()).round() as Nm,
                sig_pad.center.y + (reach * ang.sin()).round() as Nm,
            );
            let ray = ResolvedShape::Capsule {
                a: sig_pad.center,
                b: end,
                radius: w / 2,
            };
            let ray_bb = ray.bbox();
            let mut blocked = false;
            for &(other_idx, req) in nearby_pads {
                let other = &self.pads[other_idx];
                if ray_bb.expand(req).intersects(other.bbox)
                    && other.shape.dist_to_capsule(sig_pad.center, end, w / 2) < req as f64 - EPS_NM
                {
                    blocked = true;
                    break;
                }
            }
            if !blocked
                && let Some((vp, vd)) = extra_via
                && ray.dist_to_circle(vp, vd / 2) < clr as f64 - EPS_NM
            {
                blocked = true;
            }
            if !blocked
                && let Some((sa, sb, sw)) = extra_stub
                && ray.dist_to_capsule(sa, sb, sw / 2) < clr as f64 - EPS_NM
            {
                blocked = true;
            }
            if !blocked {
                return true;
            }
        }
        false
    }

    fn seals_adjacent_dense_pad(
        &mut self,
        source_net: &str,
        layer: &str,
        via_pos: Pt,
        via_dia: Nm,
        stub: Option<(Pt, Pt, Nm)>,
    ) -> bool {
        let q = BBoxNm::from_pt(via_pos, 1_500_000);
        self.pad_grid
            .query_with(q, &mut self.scratch, &mut self.cands);
        let dense_cands: Vec<usize> = self
            .cands
            .iter()
            .copied()
            .filter(|&pi| {
                if !self.is_dense_pad[pi] {
                    return false;
                }
                let p = &self.pads[pi];
                p.kind == PadKind::Smd
                    && !p.net.is_empty()
                    && !same_named_net(source_net, &p.net)
                    && p.layers.iter().any(|l| l == layer)
                    && p.center.dist(via_pos) <= 1_500_000.0
            })
            .collect();
        for pi in dense_cands {
            let p = &self.pads[pi];
            let w = self.board.net_track_width(&p.net);
            let nq = BBoxNm::from_pt(p.center, 1_500_000 + w / 2 + self.max_clr);
            self.pad_grid
                .query_with(nq, &mut self.scratch, &mut self.cands);
            let nearby_pads: Vec<(usize, Nm)> = self
                .cands
                .iter()
                .copied()
                .filter(|&oi| {
                    let other = &self.pads[oi];
                    oi != pi
                        && !same_named_net(&p.net, &other.net)
                        && other.layers.iter().any(|l| l == layer)
                })
                .map(|oi| {
                    (
                        oi,
                        self.board.pair_clearance(&p.net, &self.pads[oi].net, None),
                    )
                })
                .collect();
            let before = self.local_pad_has_escape(p, &nearby_pads, None, None);
            if before && !self.local_pad_has_escape(p, &nearby_pads, Some((via_pos, via_dia)), stub)
            {
                return true;
            }
        }
        false
    }

    fn rebuild_via_grid(&mut self) {
        self.via_grid = SpatialGrid::new_dynamic(2_000_000);
        for (vi, &bb) in self.via_bboxes.iter().enumerate() {
            self.via_grid.insert(vi, bb);
        }
    }

    fn rebuild_track_grid(&mut self) {
        self.track_grid = SpatialGrid::new_dynamic(2_000_000);
        for (ti, &bb) in self.track_bboxes.iter().enumerate() {
            self.track_grid.insert(ti, bb);
        }
    }

    fn verify_and_commit_probe(
        &mut self,
        prev_vias_len: usize,
        prev_tracks_len: usize,
        check_seal: bool,
    ) -> bool {
        let touches_bga = check_seal
            && !self.dense_bga_zones.is_empty()
            && self.via_bboxes[prev_vias_len..]
                .iter()
                .chain(&self.track_bboxes[prev_tracks_len..])
                .any(|bb| self.dense_bga_zones.iter().any(|z| z.intersects(*bb)));
        if touches_bga && count_sealed_bga_pads(self.board) > self.baseline_sealed {
            return false;
        }
        if !self.board.pours.is_empty() {
            let mut next_probe = self.probe.clone();
            next_probe
                .vias
                .extend(self.board.vias[prev_vias_len..].iter().cloned());
            next_probe
                .tracks
                .extend(self.board.tracks[prev_tracks_len..].iter().cloned());
            let zone_rep = fill_zones(&mut next_probe);
            if zone_rep.chokes.len() > self.baseline_chokes {
                return false;
            }
            self.indexed_pours = build_indexed_pours(&next_probe);
            next_probe.filled_pours.clear();
            self.probe = next_probe;
        }
        true
    }

    fn try_commit_via(&mut self, via: Via, stub: Option<Track>, check_seal: bool) -> bool {
        let prev_vias = self.board.vias.len();
        let prev_tracks = self.board.tracks.len();
        let vbb = BBoxNm::from_pt(via.pos, via.diameter / 2);
        self.board.vias.push(via);
        self.via_bboxes.push(vbb);
        if let Some(st) = stub {
            let tbb = track_bbox(&st);
            self.board.tracks.push(st);
            self.track_bboxes.push(tbb);
        }
        if !self.verify_and_commit_probe(prev_vias, prev_tracks, check_seal) {
            self.board.vias.truncate(prev_vias);
            self.via_bboxes.truncate(prev_vias);
            self.board.tracks.truncate(prev_tracks);
            self.track_bboxes.truncate(prev_tracks);
            return false;
        }
        self.via_grid.insert(prev_vias, vbb);
        if self.board.tracks.len() > prev_tracks {
            self.track_grid
                .insert(prev_tracks, self.track_bboxes[prev_tracks]);
        }
        true
    }

    fn pofv_candidate_ok(
        &mut self,
        target: &SmdTarget,
        via_dia: Nm,
        via_drill: Nm,
        batch_via_start: usize,
        batch_track_start: usize,
    ) -> bool {
        self.point_inside_target_plane(
            &target.net,
            &target.pad_layer,
            target.center,
            via_dia,
            batch_via_start,
            batch_track_start,
        ) && self.via_clears_board_and_objects(
            &target.net,
            target.center,
            via_dia,
            via_drill,
            Some(target.pad_idx),
        ) && !self.seals_adjacent_dense_pad(
            &target.net,
            &target.pad_layer,
            target.center,
            via_dia,
            None,
        )
    }

    fn radial_candidate_ok(
        &mut self,
        target: &SmdTarget,
        cand: Pt,
        dims: (Nm, Nm, Nm),
        batch_start: (usize, usize),
    ) -> bool {
        let (via_dia, via_drill, stub_w) = dims;
        let (batch_via_start, batch_track_start) = batch_start;
        self.pads[target.pad_idx]
            .shape
            .dist_to_circle(cand, via_dia / 2)
            >= 10_000.0
            && self.point_inside_target_plane(
                &target.net,
                &target.pad_layer,
                cand,
                via_dia,
                batch_via_start,
                batch_track_start,
            )
            && self.via_clears_board_and_objects(
                &target.net,
                cand,
                via_dia,
                via_drill,
                Some(target.pad_idx),
            )
            && self.stub_clears_board_and_objects(
                &target.net,
                &target.pad_layer,
                target.center,
                cand,
                stub_w,
            )
            && !self.seals_adjacent_dense_pad(
                &target.net,
                &target.pad_layer,
                cand,
                via_dia,
                Some((target.center, cand, stub_w)),
            )
    }

    fn make_via(&self, net: &str, pos: Pt, diameter: Nm, drill: Nm) -> Via {
        Via {
            net: net.to_string(),
            pos,
            diameter,
            drill,
            layers: self.via_layers.clone(),
            locked: true,
            tented: true,
        }
    }

    fn make_stub(net: &str, layer: &str, a: Pt, b: Pt, width: Nm) -> Track {
        Track {
            net: net.to_string(),
            layer: layer.to_string(),
            a,
            b,
            width,
            locked: true,
        }
    }

    fn try_commit_radial_via(
        &mut self,
        target: &SmdTarget,
        cand: Pt,
        via_dia: Nm,
        via_drill: Nm,
        stub_w: Nm,
    ) -> Option<SolvedViaDrop> {
        let via_start = self.board.vias.len();
        let track_start = self.board.tracks.len();
        if !self.radial_candidate_ok(
            target,
            cand,
            (via_dia, via_drill, stub_w),
            (via_start, track_start),
        ) {
            return None;
        }
        let via = self.make_via(&target.net, cand, via_dia, via_drill);
        let stub = Self::make_stub(&target.net, &target.pad_layer, target.center, cand, stub_w);
        if !self.try_commit_via(via, Some(stub), true) {
            return None;
        }
        Some(SolvedViaDrop {
            pad_label: target.label.clone(),
            net: target.net.clone(),
            pos: cand,
            is_thermal: false,
            is_pofv: false,
            stub_len_nm: target.center.dist(cand).round() as Nm,
        })
    }

    fn place_single_plane_drop(
        &mut self,
        target: &SmdTarget,
        pofv_enabled: bool,
        state: &mut PlaneDropState,
    ) {
        let via_start = self.board.vias.len();
        let track_start = self.board.tracks.len();
        if self.pad_on_same_layer_plane(
            &target.net,
            &target.pad_layer,
            target.center,
            via_start,
            track_start,
        ) || self.pad_already_has_via(target.pad_idx, self.cfg.max_stub_len_nm)
        {
            return;
        }

        let (via_dia, via_drill) = self.board.net_via_dims(&target.net);
        let stub_w = self
            .cfg
            .stub_width_nm
            .unwrap_or_else(|| self.board.net_track_width(&target.net));

        if pofv_enabled
            && self.pofv_candidate_ok(target, via_dia, via_drill, via_start, track_start)
        {
            let via = self.make_via(&target.net, target.center, via_dia, via_drill);
            if self.try_commit_via(via, None, true) {
                state.plane_vias_added += 1;
                state.pofv_count += 1;
                state.drops.push(SolvedViaDrop {
                    pad_label: target.label.clone(),
                    net: target.net.clone(),
                    pos: target.center,
                    is_thermal: false,
                    is_pofv: true,
                    stub_len_nm: 0,
                });
                return;
            }
        }

        let cands = radial_candidates(
            &self.pads[target.pad_idx],
            target.comp_center,
            via_dia,
            self.cfg.max_stub_len_nm,
        );
        for cand in cands {
            if let Some(drop) = self.try_commit_radial_via(target, cand, via_dia, via_drill, stub_w)
            {
                state.plane_vias_added += 1;
                state.stubs_added += 1;
                state.drops.push(drop);
                return;
            }
        }

        state.unresolved.push(UnresolvedPowerPad {
            pad_label: target.label.clone(),
            net: target.net.clone(),
            reason: "no legal plane via candidate cleared obstacles and chokes".into(),
        });
    }

    fn push_optimistic_drop(
        &mut self,
        via: Via,
        stub: Option<Track>,
        drop: SolvedViaDrop,
        state: &mut PlaneDropState,
    ) {
        let vi = self.board.vias.len();
        let vbb = BBoxNm::from_pt(via.pos, via.diameter / 2);
        self.board.vias.push(via);
        self.via_bboxes.push(vbb);
        self.via_grid.insert(vi, vbb);
        state.plane_vias_added += 1;
        if let Some(st) = stub {
            let ti = self.board.tracks.len();
            let tbb = track_bbox(&st);
            self.board.tracks.push(st);
            self.track_bboxes.push(tbb);
            self.track_grid.insert(ti, tbb);
            state.stubs_added += 1;
        }
        if drop.is_pofv {
            state.pofv_count += 1;
        }
        state.drops.push(drop);
    }

    fn solve_plane_drop_batch(
        &mut self,
        batch: &[SmdTarget],
        pofv_enabled: bool,
        state: &mut PlaneDropState,
    ) {
        if batch.is_empty() {
            return;
        }
        if batch.len() == 1 {
            self.place_single_plane_drop(&batch[0], pofv_enabled, state);
            return;
        }

        let snap = BatchSnapshot {
            vias_len: self.board.vias.len(),
            tracks_len: self.board.tracks.len(),
            drops_len: state.drops.len(),
            unresolved_len: state.unresolved.len(),
            plane_vias_added: state.plane_vias_added,
            stubs_added: state.stubs_added,
            pofv_count: state.pofv_count,
        };

        for target in batch {
            if self.pad_on_same_layer_plane(
                &target.net,
                &target.pad_layer,
                target.center,
                snap.vias_len,
                snap.tracks_len,
            ) || self.pad_already_has_via(target.pad_idx, self.cfg.max_stub_len_nm)
            {
                continue;
            }
            let (via_dia, via_drill) = self.board.net_via_dims(&target.net);
            let stub_w = self
                .cfg
                .stub_width_nm
                .unwrap_or_else(|| self.board.net_track_width(&target.net));

            if pofv_enabled
                && self.pofv_candidate_ok(
                    target,
                    via_dia,
                    via_drill,
                    snap.vias_len,
                    snap.tracks_len,
                )
            {
                let via = self.make_via(&target.net, target.center, via_dia, via_drill);
                let drop = SolvedViaDrop {
                    pad_label: target.label.clone(),
                    net: target.net.clone(),
                    pos: target.center,
                    is_thermal: false,
                    is_pofv: true,
                    stub_len_nm: 0,
                };
                self.push_optimistic_drop(via, None, drop, state);
                continue;
            }

            let cands = radial_candidates(
                &self.pads[target.pad_idx],
                target.comp_center,
                via_dia,
                self.cfg.max_stub_len_nm,
            );
            let mut found = false;
            for cand in cands {
                if self.radial_candidate_ok(
                    target,
                    cand,
                    (via_dia, via_drill, stub_w),
                    (snap.vias_len, snap.tracks_len),
                ) {
                    let via = self.make_via(&target.net, cand, via_dia, via_drill);
                    let stub = Self::make_stub(
                        &target.net,
                        &target.pad_layer,
                        target.center,
                        cand,
                        stub_w,
                    );
                    let drop = SolvedViaDrop {
                        pad_label: target.label.clone(),
                        net: target.net.clone(),
                        pos: cand,
                        is_thermal: false,
                        is_pofv: false,
                        stub_len_nm: target.center.dist(cand).round() as Nm,
                    };
                    self.push_optimistic_drop(via, Some(stub), drop, state);
                    found = true;
                    break;
                }
            }
            if !found {
                state.unresolved.push(UnresolvedPowerPad {
                    pad_label: target.label.clone(),
                    net: target.net.clone(),
                    reason: "no legal plane via candidate cleared obstacles and chokes".into(),
                });
            }
        }

        if self.board.vias.len() == snap.vias_len {
            return;
        }
        if self.verify_and_commit_probe(snap.vias_len, snap.tracks_len, true) {
            return;
        }

        self.board.vias.truncate(snap.vias_len);
        self.board.tracks.truncate(snap.tracks_len);
        self.via_bboxes.truncate(snap.vias_len);
        self.track_bboxes.truncate(snap.tracks_len);
        self.rebuild_via_grid();
        self.rebuild_track_grid();
        state.drops.truncate(snap.drops_len);
        state.unresolved.truncate(snap.unresolved_len);
        state.plane_vias_added = snap.plane_vias_added;
        state.stubs_added = snap.stubs_added;
        state.pofv_count = snap.pofv_count;

        let mid = batch.len() / 2;
        self.solve_plane_drop_batch(&batch[..mid], pofv_enabled, state);
        self.solve_plane_drop_batch(&batch[mid..], pofv_enabled, state);
    }

    fn place_thermal_pad(
        &mut self,
        target: &SmdTarget,
        drops: &mut Vec<SolvedViaDrop>,
        unresolved: &mut Vec<UnresolvedPowerPad>,
        thermal_vias_added: &mut usize,
    ) {
        let (def_dia, def_drill) = self.board.net_via_dims(&target.net);
        let via_drill = self.cfg.thermal_via_drill_nm.unwrap_or(def_drill);
        let via_dia = self
            .cfg
            .thermal_via_dia_nm
            .unwrap_or(def_dia)
            .max(via_drill + 2 * self.board.rules.min_annular_ring_nm);
        let pl = &self.board.placements[target.pl_idx];
        let pad_def = &self.board.footprints[&pl.footprint].pads[target.pad_def_idx];
        let grid_pts = thermal_grid_world_pts(
            pad_def,
            pl,
            &target.net,
            via_dia,
            via_drill,
            self.board,
            self.cfg,
        );

        let prev_vias = self.board.vias.len();
        let prev_tracks = self.board.tracks.len();
        let prev_drops = drops.len();
        let prev_thermal = *thermal_vias_added;

        let mut candidate_pts = Vec::new();
        for pt in grid_pts {
            if self.pads[target.pad_idx]
                .shape
                .dist_to_circle(pt, via_dia / 2)
                > EPS_NM
                || !self.point_inside_target_plane(
                    &target.net,
                    &target.pad_layer,
                    pt,
                    via_dia,
                    prev_vias,
                    prev_tracks,
                )
                || !self.via_clears_board_and_objects(
                    &target.net,
                    pt,
                    via_dia,
                    via_drill,
                    Some(target.pad_idx),
                )
            {
                continue;
            }
            let via = self.make_via(&target.net, pt, via_dia, via_drill);
            let vi = self.board.vias.len();
            let vbb = BBoxNm::from_pt(via.pos, via.diameter / 2);
            self.board.vias.push(via);
            self.via_bboxes.push(vbb);
            self.via_grid.insert(vi, vbb);
            *thermal_vias_added += 1;
            drops.push(SolvedViaDrop {
                pad_label: target.label.clone(),
                net: target.net.clone(),
                pos: pt,
                is_thermal: true,
                is_pofv: false,
                stub_len_nm: 0,
            });
            candidate_pts.push(pt);
        }

        if self.board.vias.len() > prev_vias
            && !self.verify_and_commit_probe(prev_vias, prev_tracks, false)
        {
            self.board.vias.truncate(prev_vias);
            self.via_bboxes.truncate(prev_vias);
            self.rebuild_via_grid();
            drops.truncate(prev_drops);
            *thermal_vias_added = prev_thermal;

            for pt in candidate_pts {
                if !self.via_clears_board_and_objects(
                    &target.net,
                    pt,
                    via_dia,
                    via_drill,
                    Some(target.pad_idx),
                ) {
                    continue;
                }
                let via = self.make_via(&target.net, pt, via_dia, via_drill);
                if self.try_commit_via(via, None, false) {
                    *thermal_vias_added += 1;
                    drops.push(SolvedViaDrop {
                        pad_label: target.label.clone(),
                        net: target.net.clone(),
                        pos: pt,
                        is_thermal: true,
                        is_pofv: false,
                        stub_len_nm: 0,
                    });
                }
            }
        }

        let added_here = *thermal_vias_added - prev_thermal;
        if added_here == 0
            && !self.board.vias.iter().any(|v| {
                same_named_net(&v.net, &target.net)
                    && self.pads[target.pad_idx].shape.dist_to_point(v.pos) == 0.0
            })
        {
            unresolved.push(UnresolvedPowerPad {
                pad_label: target.label.clone(),
                net: target.net.clone(),
                reason: "no legal thermal via position inside exposed pad".into(),
            });
        }
    }
}

pub(crate) fn solve_power_vias(
    board: &mut BoardIr,
    cfg: &PowerViaOptions,
) -> Result<PowerViaReport, String> {
    let start = Instant::now();
    let had_filled_pours = !board.filled_pours.is_empty();
    let cu_layers = board.copper_layer_names();
    let via_layers = via_span_layers(&cu_layers);
    let plane_nets = identify_plane_nets(board, cfg);
    let pads = collect_pads(board, &cu_layers);
    let pad_bboxes: Vec<BBoxNm> = pads
        .iter()
        .map(|p| pad_query_bbox(p.bbox, p.center, p.drill_nm))
        .collect();
    let pad_grid = SpatialGrid::new(pad_bboxes.iter().copied(), 2_000_000);
    let keepouts = collect_keepouts(board);
    let keepout_grid = SpatialGrid::new(keepouts.iter().map(|k| k.bbox), 2_000_000);
    let edge_idx = BoardEdgeIndex::new(board);
    let max_clr = compute_max_clearance(board, &[]);

    let mut pads_by_pl: Vec<Vec<usize>> = vec![Vec::new(); board.placements.len()];
    for (pi, p) in pads.iter().enumerate() {
        if p.pl_idx < pads_by_pl.len() {
            pads_by_pl[p.pl_idx].push(pi);
        }
    }
    let mut is_dense_pad = vec![false; pads.len()];
    for pl_pads in &pads_by_pl {
        for &pi in pl_pads {
            let p_center = pads[pi].center;
            let count = pl_pads
                .iter()
                .filter(|&&qi| qi != pi && pads[qi].center.dist(p_center) <= 1_200_000.0)
                .count();
            if count >= 3 {
                is_dense_pad[pi] = true;
            }
        }
    }
    let max_via_dia = board
        .vias
        .iter()
        .map(|v| v.diameter)
        .chain(board.net_classes.values().map(|nc| nc.via_dia_nm))
        .chain(cfg.thermal_via_dia_nm)
        .max()
        .unwrap_or(board.rules.via_dia_nm)
        .max(board.rules.via_dia_nm);
    let bga_margin = 2_500_000.max(max_via_dia + max_clr + 2_000_000);
    let mut dense_bga_zones = Vec::new();
    for (pl_idx, pl) in board.placements.iter().enumerate() {
        if board
            .footprints
            .get(&pl.footprint)
            .is_some_and(|fp| fp.pads.len() >= 9)
        {
            let mut bb: Option<BBoxNm> = None;
            for &pi in &pads_by_pl[pl_idx] {
                let pb = pads[pi].bbox;
                bb = Some(bb.map_or(pb, |prev| prev.union(pb)));
            }
            if let Some(b) = bb {
                dense_bga_zones.push(b.expand(bga_margin));
            }
        }
    }
    let baseline_sealed = if !dense_bga_zones.is_empty() {
        count_sealed_bga_pads(board)
    } else {
        0
    };

    let mut probe = make_probe_board(board);
    let (baseline_chokes, indexed_pours) = if !probe.pours.is_empty() {
        let rep = fill_zones(&mut probe);
        let idx_pours = build_indexed_pours(&probe);
        probe.filled_pours.clear();
        (rep.chokes.len(), idx_pours)
    } else {
        (0, Vec::new())
    };

    let mut via_grid = SpatialGrid::new_dynamic(2_000_000);
    let mut via_bboxes = Vec::with_capacity(board.vias.len());
    for (vi, v) in board.vias.iter().enumerate() {
        let bb = BBoxNm::from_pt(v.pos, v.diameter / 2);
        via_bboxes.push(bb);
        via_grid.insert(vi, bb);
    }
    let mut track_grid = SpatialGrid::new_dynamic(2_000_000);
    let mut track_bboxes = Vec::with_capacity(board.tracks.len());
    for (ti, t) in board.tracks.iter().enumerate() {
        let bb = track_bbox(t);
        track_bboxes.push(bb);
        track_grid.insert(ti, bb);
    }

    let mut targets = Vec::new();
    let mut pad_cursor = 0usize;
    for (pl_idx, pl) in board.placements.iter().enumerate() {
        let Some(fp) = board.footprints.get(&pl.footprint) else {
            continue;
        };
        for (pad_def_idx, pad) in fp.pads.iter().enumerate() {
            let pad_item = &pads[pad_cursor];
            let pad_idx = pad_cursor;
            pad_cursor += 1;
            if pad.kind != PadKind::Smd || !plane_nets.contains(&pad_item.net) {
                continue;
            }
            let Some(pad_layer) = pad_item.layers.first().cloned() else {
                continue;
            };
            targets.push(SmdTarget {
                pl_idx,
                pad_idx,
                pad_def_idx,
                label: pad_item.label.clone(),
                net: pad_item.net.clone(),
                pad_layer,
                center: pad_item.center,
                comp_center: pl.pos,
                min_dim_nm: pad_min_dim(&pad.shape, &pad_item.shape),
            });
        }
    }
    targets.sort_by_key(|t| (t.min_dim_nm < cfg.thermal_pad_min_dim_nm, t.label.clone()));

    let mut solver = PowerViaSolver {
        board,
        probe,
        indexed_pours,
        baseline_chokes,
        baseline_sealed,
        pads,
        pad_bboxes,
        pad_grid,
        is_dense_pad,
        keepouts,
        keepout_grid,
        edge_idx,
        via_grid,
        via_bboxes,
        track_grid,
        track_bboxes,
        max_clr,
        dense_bga_zones,
        via_layers,
        cfg,
        scratch: GridQueryScratch::default(),
        cands: Vec::new(),
    };

    let pofv_enabled =
        cfg.allow_pofv.unwrap_or(solver.board.rules.via_at_smd) && solver.board.rules.via_at_smd;
    let plane_start = targets.partition_point(|t| t.min_dim_nm >= cfg.thermal_pad_min_dim_nm);

    let mut drops = Vec::new();
    let mut unresolved = Vec::new();
    let mut thermal_vias_added = 0usize;
    for target in &targets[..plane_start] {
        solver.place_thermal_pad(target, &mut drops, &mut unresolved, &mut thermal_vias_added);
    }

    let mut plane_state = PlaneDropState {
        drops,
        unresolved,
        plane_vias_added: 0,
        stubs_added: 0,
        pofv_count: 0,
    };
    for batch in targets[plane_start..].chunks(PLANE_DROP_BATCH_SIZE) {
        solver.solve_plane_drop_batch(batch, pofv_enabled, &mut plane_state);
    }

    let mut drops = plane_state.drops;
    let unresolved = plane_state.unresolved;
    let mut plane_vias_added = plane_state.plane_vias_added;
    let mut stubs_added = plane_state.stubs_added;
    let pofv_count = plane_state.pofv_count;

    for rule in solver.board.constraints.min_vias.clone() {
        let (mut current, _) = count_rule_vias(solver.board, &solver.pads, &rule);
        if current >= rule.min_vias {
            continue;
        }
        let cy_opt = rule.refdes.as_ref().and_then(|rd| {
            solver
                .board
                .placements
                .iter()
                .find(|p| &p.refdes == rd)
                .and_then(|pl| {
                    solver
                        .board
                        .footprints
                        .get(&pl.footprint)
                        .map(|fp| courtyard_world_polygon(fp, pl))
                })
        });
        let (via_dia, via_drill) = solver.board.net_via_dims(&rule.net);
        let stub_w = cfg
            .stub_width_nm
            .unwrap_or_else(|| solver.board.net_track_width(&rule.net));
        for target in &targets {
            if current >= rule.min_vias {
                break;
            }
            if target.net != rule.net {
                continue;
            }
            if let Some(ref rd) = rule.refdes
                && &solver.board.placements[target.pl_idx].refdes != rd
            {
                continue;
            }
            let cands = radial_candidates(
                &solver.pads[target.pad_idx],
                target.comp_center,
                via_dia,
                cfg.max_stub_len_nm,
            );
            for cand in cands {
                if current >= rule.min_vias {
                    break;
                }
                if let Some(ref cy) = cy_opt
                    && !point_in_polygon(cand, cy)
                    && cand.dist(target.center) > 500_000.0
                {
                    continue;
                }
                if let Some(drop) =
                    solver.try_commit_radial_via(target, cand, via_dia, via_drill, stub_w)
                {
                    plane_vias_added += 1;
                    stubs_added += 1;
                    current += 1;
                    drops.push(drop);
                }
            }
        }
    }

    let board = solver.board;
    let chokes_after = if had_filled_pours {
        fill_zones(board).chokes.len()
    } else if !board.pours.is_empty() {
        let mut check_board = board.clone();
        fill_zones(&mut check_board).chokes.len()
    } else {
        0
    };

    let ok = unresolved.is_empty();
    Ok(PowerViaReport {
        plane_nets: plane_nets.into_iter().collect(),
        thermal_vias_added,
        plane_vias_added,
        stubs_added,
        pofv_count,
        drops,
        unresolved,
        chokes_after,
        ok,
        elapsed_ms: start.elapsed().as_millis(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::drc::check_board_drc;
    use crate::ir::{BoardSpec, FootprintDef, Keepout, KeepoutKind, PourSpec, Side, mm};

    fn make_smd_board(
        name: &str,
        w_mm: f64,
        h_mm: f64,
        pads: &[(&str, f64, f64, &str)],
    ) -> BoardIr {
        let spec: BoardSpec = toml::from_str(&format!(
            r#"
name = "{name}"
size_nm = [{}, {}]
[[layers]]
name = "F.Cu"
kind = "signal"
side = "top"
[[layers]]
name = "B.Cu"
kind = "plane"
side = "bottom"
[rules]
clearance_nm = 150000
edge_clearance_nm = 300000
track_width_nm = 200000
via_dia_nm = 450000
via_drill_nm = 200000
"#,
            mm(w_mm),
            mm(h_mm)
        ))
        .unwrap();
        let mut board = BoardIr::from_spec(&spec).unwrap();
        board.footprints.insert(
            "PAD".into(),
            FootprintDef {
                name: "PAD".into(),
                pads: vec![PadDef {
                    name: "1".into(),
                    kind: PadKind::Smd,
                    shape: PadShape::Rect {
                        width: 500_000,
                        height: 500_000,
                    },
                    pos: Pt::new(0, 0),
                    rot_deg: 0.0,
                    drill_nm: None,
                    layers: vec!["F.Cu".into()],
                    mask_margin_nm: None,
                    paste_margin_nm: None,
                }],
                courtyard: Vec::new(),
                graphics: Vec::new(),
                keepouts: Vec::new(),
                models: Vec::new(),
                pin_types: Default::default(),
                kicad_raw: None,
            },
        );
        for &(rd, x_mm, y_mm, net) in pads {
            board.placements.push(Placement {
                refdes: rd.into(),
                footprint: "PAD".into(),
                pos: Pt::new(mm(x_mm), mm(y_mm)),
                side: Side::Top,
                locked: true,
                pin_nets: [("1".into(), net.into())].into_iter().collect(),
                ..Default::default()
            });
        }
        board
    }

    #[test]
    fn batched_optimistic_commit_bisect_fallback_avoids_pour_neck_choke() {
        let mut board = make_smd_board(
            "batch_bisect",
            26.0,
            14.0,
            &[
                ("U1", 4.0, 7.0, "VCC"),
                ("U2", 6.0, 7.0, "VCC"),
                // U3 sits centered in a 0.8mm neck: POFV splits it, triggering bisect + radial escape.
                ("U3", 12.5, 7.0, "VCC"),
                ("U4", 20.0, 7.0, "VCC"),
            ],
        );
        board.rules.via_at_smd = true;
        for (y0, y1) in [(0, 6_600_000), (7_400_000, 14_000_000)] {
            board.keepouts.push(Keepout {
                kind: KeepoutKind::Copper,
                layers: vec!["B.Cu".into()],
                poly: vec![
                    Pt::new(mm(12.0), y0),
                    Pt::new(mm(13.0), y0),
                    Pt::new(mm(13.0), y1),
                    Pt::new(mm(12.0), y1),
                ],
            });
        }
        board.pours.push(PourSpec {
            name: "GND_B".into(),
            net: "GND".into(),
            layer: "B.Cu".into(),
            outline: board.outline.clone(),
            min_width_nm: 200_000,
            remove_islands: false,
            ..Default::default()
        });

        let cfg = PowerViaOptions {
            plane_nets: vec!["VCC".into()],
            allow_pofv: Some(true),
            max_stub_len_nm: 1_500_000,
            ..Default::default()
        };
        let rep = solve_power_vias(&mut board, &cfg).unwrap();
        assert_eq!(rep.chokes_after, 0);
        // Safe pads U1, U2, U4 commit via POFV; U3 rejects POFV at (12.5, 7.0) and places outside neck.
        assert_eq!(rep.plane_vias_added, 4);
        assert_eq!(rep.pofv_count, 3);
        assert!(
            board
                .vias
                .iter()
                .all(|v| v.pos != Pt::new(mm(12.5), mm(7.0))),
            "U3 POFV via should be rejected in favor of non-choking radial candidate: {:?}",
            board.vias
        );
    }

    #[test]
    fn npth_oversized_drill_zero_pad_size_prevents_power_via_overlap() {
        let mut board = make_smd_board("npth_drill", 20.0, 20.0, &[("C1", 12.1, 10.0, "GND")]);
        board.footprints.insert(
            "NPTH_HOLE".into(),
            FootprintDef {
                name: "NPTH_HOLE".into(),
                pads: vec![PadDef {
                    name: "1".into(),
                    kind: PadKind::Npth,
                    shape: PadShape::Circle { diameter: 0 },
                    pos: Pt::new(0, 0),
                    rot_deg: 0.0,
                    drill_nm: Some(mm(3.2)),
                    layers: Vec::new(),
                    mask_margin_nm: None,
                    paste_margin_nm: None,
                }],
                courtyard: Vec::new(),
                graphics: Vec::new(),
                keepouts: Vec::new(),
                models: Vec::new(),
                pin_types: Default::default(),
                kicad_raw: None,
            },
        );
        board.placements.push(Placement {
            refdes: "H1".into(),
            footprint: "NPTH_HOLE".into(),
            pos: Pt::new(mm(10.0), mm(10.0)),
            side: Side::Top,
            locked: true,
            ..Default::default()
        });
        board.pours.push(PourSpec {
            name: "GND_B".into(),
            net: "GND".into(),
            layer: "B.Cu".into(),
            outline: board.outline.clone(),
            min_width_nm: 200_000,
            ..Default::default()
        });

        let rep = solve_power_vias(
            &mut board,
            &PowerViaOptions {
                allow_pofv: Some(false),
                ..Default::default()
            },
        )
        .unwrap();
        assert_eq!(rep.plane_vias_added, 1);
        let hole_center = Pt::new(mm(10.0), mm(10.0));
        let min_edge_dist =
            board.vias[0].pos.dist(hole_center) - (mm(3.2) / 2 + board.vias[0].diameter / 2) as f64;
        assert!(
            min_edge_dist >= board.rules.effective_hole_clearance_nm() as f64 - EPS_NM,
            "via overlapped oversized NPTH drill: edge dist {min_edge_dist}"
        );
        fill_zones(&mut board);
        let drc = check_board_drc(&board);
        assert!(
            drc.violations
                .iter()
                .all(|v| v.kind != crate::drc::ViolationKind::DrillToCopper
                    && v.kind != crate::drc::ViolationKind::HoleToHole),
            "unexpected drill DRC violations: {:?}",
            drc.violations
        );
    }

    #[test]
    fn same_net_terminal_transition_narrow_neck_caught_by_probe_fill_zones() {
        let mut board = make_smd_board("same_net_neck", 16.0, 14.0, &[]);
        board.vias.push(Via {
            net: "GND".into(),
            pos: Pt::new(mm(4.0), mm(7.0)),
            diameter: 450_000,
            drill: 200_000,
            layers: ("F.Cu".into(), "B.Cu".into()),
            locked: true,
            tented: true,
        });
        // Narrow 0.3mm peninsula on B.Cu from x=8..14mm, y=6.85..7.15mm (< 0.4mm required neck width).
        for (y0, y1) in [(0, 6_850_000), (7_150_000, 14_000_000)] {
            board.keepouts.push(Keepout {
                kind: KeepoutKind::Copper,
                layers: vec!["B.Cu".into()],
                poly: vec![
                    Pt::new(mm(8.0), y0),
                    Pt::new(mm(16.0), y0),
                    Pt::new(mm(16.0), y1),
                    Pt::new(mm(8.0), y1),
                ],
            });
        }
        board.keepouts.push(Keepout {
            kind: KeepoutKind::Copper,
            layers: vec!["B.Cu".into()],
            poly: vec![
                Pt::new(mm(14.0), 6_850_000),
                Pt::new(mm(16.0), 6_850_000),
                Pt::new(mm(16.0), 7_150_000),
                Pt::new(mm(14.0), 7_150_000),
            ],
        });
        board.pours.push(PourSpec {
            name: "GND_B".into(),
            net: "GND".into(),
            layer: "B.Cu".into(),
            outline: board.outline.clone(),
            min_width_nm: 200_000,
            remove_islands: false,
            ..Default::default()
        });

        let cu_layers = board.copper_layer_names();
        let pads = collect_pads(&board, &cu_layers);
        let pad_bboxes: Vec<BBoxNm> = pads
            .iter()
            .map(|p| pad_query_bbox(p.bbox, p.center, p.drill_nm))
            .collect();
        let mut probe = make_probe_board(&board);
        let base_rep = fill_zones(&mut probe);
        // With only 1 GND terminal (< 2), the 0.3mm peninsula triggers no NarrowNeck choke yet.
        assert_eq!(base_rep.chokes.len(), 0);
        let indexed_pours = build_indexed_pours(&probe);
        probe.filled_pours.clear();

        let cfg = PowerViaOptions::default();
        let mut via_grid = SpatialGrid::new_dynamic(2_000_000);
        let via_bboxes = vec![BBoxNm::from_pt(
            board.vias[0].pos,
            board.vias[0].diameter / 2,
        )];
        via_grid.insert(0, via_bboxes[0]);
        let mut solver = PowerViaSolver {
            board: &mut board,
            probe,
            indexed_pours,
            baseline_chokes: base_rep.chokes.len(),
            baseline_sealed: 0,
            pads,
            pad_grid: SpatialGrid::new(pad_bboxes.iter().copied(), 2_000_000),
            pad_bboxes,
            is_dense_pad: Vec::new(),
            keepouts: Vec::new(),
            keepout_grid: SpatialGrid::new(std::iter::empty(), 2_000_000),
            edge_idx: None,
            via_grid,
            via_bboxes,
            track_grid: SpatialGrid::new_dynamic(2_000_000),
            track_bboxes: Vec::new(),
            max_clr: 150_000,
            dense_bga_zones: Vec::new(),
            via_layers: ("F.Cu".into(), "B.Cu".into()),
            cfg: &cfg,
            scratch: GridQueryScratch::default(),
            cands: Vec::new(),
        };

        // Placing a second GND via on the peninsula tip (< 2 -> >= 2 terminals) triggers NarrowNeck.
        let tip_via = Via {
            net: "GND".into(),
            pos: Pt::new(mm(13.0), mm(7.0)),
            diameter: 450_000,
            drill: 200_000,
            layers: ("F.Cu".into(), "B.Cu".into()),
            locked: true,
            tented: true,
        };
        let mut check_board = solver.board.clone();
        check_board.vias.push(tip_via.clone());
        assert!(fill_zones(&mut check_board).chokes.len() > base_rep.chokes.len());
        assert!(!solver.try_commit_via(tip_via, None, false));
        assert_eq!(solver.board.vias.len(), 1);
    }
}
