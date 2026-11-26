use std::collections::{BTreeMap, BTreeSet, HashSet};
use std::time::Instant;

use viahop::run_preflight as viahop_run_preflight;

use crate::drc_constraints::count_rule_vias;
use crate::drc_layout::keepout_matches_layer;
use crate::drc_scene::{KeepoutItem, PadItem, collect_keepouts, collect_pads, pad_min_dim};
use crate::geom::{
    EPS_NM, closest_pt_on_segment, courtyard_world_polygon, local_to_board, pad_copper_layers,
    point_in_polygon, point_to_polygon_edge_dist, point_to_segment_dist, shape_edge_clearance,
};
use crate::ir::{
    BBoxNm, BoardIr, Nm, PadDef, PadKind, PadShape, Placement, PourPoly, Pt, Track, Via,
    same_named_net,
};
use crate::place::{PowerViaOptions, PowerViaReport, SolvedViaDrop, UnresolvedPowerPad};
use crate::route::to_viahop_board;
use crate::shapes::{ResolvedShape, transform_pad_local_pt};
use crate::zone_scene::point_in_pour_poly;
use crate::zones::fill_zones;

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

fn via_span_layers(cu_layers: &[String]) -> (String, String) {
    if cu_layers.len() >= 2 {
        (cu_layers[0].clone(), cu_layers[cu_layers.len() - 1].clone())
    } else {
        ("F.Cu".into(), "B.Cu".into())
    }
}

fn is_inner_layer(board: &BoardIr, layer: &str) -> bool {
    if let Some(ld) = board.layers.iter().find(|l| l.name == layer) {
        return ld.side.is_none();
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
        if is_inner_layer(board, &pour.layer)
            || pad_layers_by_net
                .get(&pour.net)
                .is_some_and(|ls| ls.iter().any(|l| l != &pour.layer))
        {
            nets.insert(pour.net.clone());
        }
    }
    nets
}

fn build_probe_board(board: &BoardIr) -> (BoardIr, usize) {
    let mut probe = board.clone();
    for p in &mut probe.pours {
        p.remove_islands = false;
    }
    let rep = fill_zones(&mut probe);
    (probe, rep.chokes.len())
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

fn pad_already_has_via(board: &BoardIr, pad: &PadItem, max_stub_len: Nm) -> bool {
    let layer = pad.layers.first().map(String::as_str).unwrap_or("F.Cu");
    for v in &board.vias {
        if !same_named_net(&pad.net, &v.net) {
            continue;
        }
        if pad.shape.dist_to_circle(v.pos, v.diameter / 2) <= 10_000.0 {
            return true;
        }
        if v.pos.dist(pad.center) <= max_stub_len as f64 + 10_000.0
            && board.tracks.iter().any(|t| {
                t.layer == layer
                    && same_named_net(&t.net, &pad.net)
                    && (t.a == v.pos
                        || t.b == v.pos
                        || point_to_segment_dist(v.pos, t.a, t.b) <= v.diameter as f64 / 2.0)
                    && pad
                        .shape
                        .dist_to_point(closest_pt_on_segment(pad.center, t.a, t.b))
                        <= 10_000.0
            })
        {
            return true;
        }
    }
    false
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

fn dist_to_pour_edge(p: Pt, poly: &PourPoly) -> f64 {
    let mut d = point_to_polygon_edge_dist(p, &poly.outer);
    for h in &poly.holes {
        d = d.min(point_to_polygon_edge_dist(p, h));
    }
    d
}

fn has_dense_bga(board: &BoardIr) -> bool {
    board.placements.iter().any(|pl| {
        board
            .footprints
            .get(&pl.footprint)
            .is_some_and(|fp| fp.pads.len() >= 9)
    })
}

fn count_sealed_bga_pads(board: &BoardIr) -> usize {
    let Ok(vb) = to_viahop_board(board, &HashSet::new(), None) else {
        return 0;
    };
    viahop_run_preflight(&vb, 1).sealed_pads.len()
}

struct PowerViaSolver<'a> {
    board: &'a mut BoardIr,
    probe: BoardIr,
    pads: Vec<PadItem>,
    keepouts: Vec<KeepoutItem>,
    via_layers: (String, String),
    cfg: &'a PowerViaOptions,
    baseline_chokes: usize,
    check_bga_seal: bool,
    baseline_sealed: usize,
}

impl<'a> PowerViaSolver<'a> {
    fn pad_on_same_layer_plane(&self, net: &str, pad_layer: &str, pos: Pt) -> bool {
        self.probe.filled_pours.iter().any(|fp| {
            fp.net == net
                && fp.layer == pad_layer
                && fp.polys.iter().any(|poly| point_in_pour_poly(pos, poly))
        })
    }

    fn point_inside_target_plane(&self, net: &str, pad_layer: &str, pos: Pt, via_dia: Nm) -> bool {
        let pours_for_net: Vec<_> = self
            .probe
            .filled_pours
            .iter()
            .filter(|fp| fp.net == net && fp.layer != pad_layer)
            .collect();
        if pours_for_net.is_empty() {
            // Only allow free placement if the board defines no pours at all for this explicit plane net.
            return !self.probe.pours.iter().any(|p| p.net == net);
        }
        let req_dist = (via_dia / 2 + self.cfg.plane_edge_margin_nm) as f64 - EPS_NM;
        for fp in pours_for_net {
            for poly in &fp.polys {
                if point_in_pour_poly(pos, poly) && dist_to_pour_edge(pos, poly) >= req_dist {
                    return true;
                }
            }
        }
        false
    }

    fn via_clears_board_and_objects(
        &self,
        net: &str,
        pos: Pt,
        dia: Nm,
        drill: Nm,
        ignore_pad_idx: Option<usize>,
    ) -> bool {
        if self.board.outline.len() >= 3 {
            let vs = ResolvedShape::Circle {
                center: pos,
                radius: dia / 2,
            };
            let (d, off) = shape_edge_clearance(&vs, &self.board.outline, &self.board.cutouts);
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

        for ko in &self.keepouts {
            if ko.kind.blocks_vias()
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

        for (pi, pad) in self.pads.iter().enumerate() {
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

        for other in &self.board.vias {
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

        for t in &self.board.tracks {
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
        &self,
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
        if self.board.outline.len() >= 3 {
            let (d, off) = shape_edge_clearance(&seg, &self.board.outline, &self.board.cutouts);
            if off || d < self.board.rules.edge_clearance_nm as f64 - EPS_NM {
                return false;
            }
        }
        let hole_clr = self.board.rules.effective_hole_clearance_nm();
        for ko in &self.keepouts {
            if ko.kind.blocks_tracks()
                && keepout_matches_layer(&ko.layers, layer)
                && (point_in_polygon(a, &ko.poly)
                    || point_in_polygon(b, &ko.poly)
                    || seg.dist_to_shape(&ko.shape) < -EPS_NM)
            {
                return false;
            }
        }
        for pad in &self.pads {
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
        for v in &self.board.vias {
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
        for t in &self.board.tracks {
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
        layer: &str,
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
            let mut blocked = false;
            for other in &self.pads {
                if same_named_net(&sig_pad.net, &other.net)
                    || other.label == sig_pad.label
                    || !other.layers.iter().any(|l| l == layer)
                {
                    continue;
                }
                let req = self.board.pair_clearance(&sig_pad.net, &other.net, None);
                if other.shape.dist_to_capsule(sig_pad.center, end, w / 2) < req as f64 - EPS_NM {
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
        &self,
        source_net: &str,
        layer: &str,
        via_pos: Pt,
        via_dia: Nm,
        stub: Option<(Pt, Pt, Nm)>,
    ) -> bool {
        for p in &self.pads {
            if p.kind != PadKind::Smd
                || p.net.is_empty()
                || same_named_net(source_net, &p.net)
                || !p.layers.iter().any(|l| l == layer)
                || p.center.dist(via_pos) > 1_500_000.0
            {
                continue;
            }
            let neighbor_count = self
                .pads
                .iter()
                .filter(|q| {
                    q.pl_idx == p.pl_idx
                        && q.label != p.label
                        && q.center.dist(p.center) <= 1_200_000.0
                })
                .count();
            if neighbor_count < 3 {
                continue;
            }
            let before = self.local_pad_has_escape(p, layer, None, None);
            if before && !self.local_pad_has_escape(p, layer, Some((via_pos, via_dia)), stub) {
                return true;
            }
        }
        false
    }

    fn try_commit_via(&mut self, via: Via, stub: Option<Track>, check_seal: bool) -> bool {
        if check_seal && self.check_bga_seal {
            let mut bga_test = self.board.clone();
            bga_test.vias.push(via.clone());
            if let Some(ref st) = stub {
                bga_test.tracks.push(st.clone());
            }
            if count_sealed_bga_pads(&bga_test) > self.baseline_sealed {
                return false;
            }
        }
        let mut next_probe = self.probe.clone();
        next_probe.vias.push(via.clone());
        if let Some(ref st) = stub {
            next_probe.tracks.push(st.clone());
        }
        if !next_probe.pours.is_empty()
            && fill_zones(&mut next_probe).chokes.len() > self.baseline_chokes
        {
            return false;
        }
        self.probe = next_probe;
        self.board.vias.push(via);
        if let Some(st) = stub {
            self.board.tracks.push(st);
        }
        true
    }

    fn try_commit_radial_via(
        &mut self,
        target: &SmdTarget,
        cand: Pt,
        via_dia: Nm,
        via_drill: Nm,
        stub_w: Nm,
    ) -> Option<SolvedViaDrop> {
        if self.pads[target.pad_idx]
            .shape
            .dist_to_circle(cand, via_dia / 2)
            < 10_000.0
        {
            return None;
        }
        if !self.point_inside_target_plane(&target.net, &target.pad_layer, cand, via_dia)
            || !self.via_clears_board_and_objects(
                &target.net,
                cand,
                via_dia,
                via_drill,
                Some(target.pad_idx),
            )
            || !self.stub_clears_board_and_objects(
                &target.net,
                &target.pad_layer,
                target.center,
                cand,
                stub_w,
            )
            || self.seals_adjacent_dense_pad(
                &target.net,
                &target.pad_layer,
                cand,
                via_dia,
                Some((target.center, cand, stub_w)),
            )
        {
            return None;
        }
        let via = Via {
            net: target.net.clone(),
            pos: cand,
            diameter: via_dia,
            drill: via_drill,
            layers: self.via_layers.clone(),
            locked: true,
            tented: true,
        };
        let stub = Track {
            net: target.net.clone(),
            layer: target.pad_layer.clone(),
            a: target.center,
            b: cand,
            width: stub_w,
            locked: true,
        };
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
    let keepouts = collect_keepouts(board);
    let (probe, baseline_chokes) = build_probe_board(board);
    let check_bga_seal = has_dense_bga(board);
    let baseline_sealed = if check_bga_seal {
        count_sealed_bga_pads(board)
    } else {
        0
    };

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
        pads,
        keepouts,
        via_layers,
        cfg,
        baseline_chokes,
        check_bga_seal,
        baseline_sealed,
    };

    let mut drops = Vec::new();
    let mut unresolved = Vec::new();
    let mut thermal_vias_added = 0usize;
    let mut plane_vias_added = 0usize;
    let mut stubs_added = 0usize;
    let mut pofv_count = 0usize;
    let pofv_enabled =
        cfg.allow_pofv.unwrap_or(solver.board.rules.via_at_smd) && solver.board.rules.via_at_smd;

    for target in &targets {
        if target.min_dim_nm >= cfg.thermal_pad_min_dim_nm {
            let (def_dia, def_drill) = solver.board.net_via_dims(&target.net);
            let via_drill = cfg.thermal_via_drill_nm.unwrap_or(def_drill);
            let via_dia = cfg
                .thermal_via_dia_nm
                .unwrap_or(def_dia)
                .max(via_drill + 2 * solver.board.rules.min_annular_ring_nm);
            let pl = &solver.board.placements[target.pl_idx];
            let pad_def = &solver.board.footprints[&pl.footprint].pads[target.pad_def_idx];
            let grid_pts = thermal_grid_world_pts(
                pad_def,
                pl,
                &target.net,
                via_dia,
                via_drill,
                solver.board,
                cfg,
            );

            let mut added_here = 0usize;
            for pt in grid_pts {
                if solver.pads[target.pad_idx]
                    .shape
                    .dist_to_circle(pt, via_dia / 2)
                    > EPS_NM
                    || !solver.point_inside_target_plane(
                        &target.net,
                        &target.pad_layer,
                        pt,
                        via_dia,
                    )
                    || !solver.via_clears_board_and_objects(
                        &target.net,
                        pt,
                        via_dia,
                        via_drill,
                        Some(target.pad_idx),
                    )
                {
                    continue;
                }
                let via = Via {
                    net: target.net.clone(),
                    pos: pt,
                    diameter: via_dia,
                    drill: via_drill,
                    layers: solver.via_layers.clone(),
                    locked: true,
                    tented: true,
                };
                if solver.try_commit_via(via, None, false) {
                    thermal_vias_added += 1;
                    added_here += 1;
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
            if added_here == 0
                && !solver.board.vias.iter().any(|v| {
                    same_named_net(&v.net, &target.net)
                        && solver.pads[target.pad_idx].shape.dist_to_point(v.pos) == 0.0
                })
            {
                unresolved.push(UnresolvedPowerPad {
                    pad_label: target.label.clone(),
                    net: target.net.clone(),
                    reason: "no legal thermal via position inside exposed pad".into(),
                });
            }
            continue;
        }

        if solver.pad_on_same_layer_plane(&target.net, &target.pad_layer, target.center)
            || pad_already_has_via(
                solver.board,
                &solver.pads[target.pad_idx],
                cfg.max_stub_len_nm,
            )
        {
            continue;
        }

        let (via_dia, via_drill) = solver.board.net_via_dims(&target.net);
        let stub_w = cfg
            .stub_width_nm
            .unwrap_or_else(|| solver.board.net_track_width(&target.net));
        let mut placed_for_pad = false;

        if pofv_enabled
            && solver.point_inside_target_plane(
                &target.net,
                &target.pad_layer,
                target.center,
                via_dia,
            )
            && solver.via_clears_board_and_objects(
                &target.net,
                target.center,
                via_dia,
                via_drill,
                Some(target.pad_idx),
            )
            && !solver.seals_adjacent_dense_pad(
                &target.net,
                &target.pad_layer,
                target.center,
                via_dia,
                None,
            )
        {
            let via = Via {
                net: target.net.clone(),
                pos: target.center,
                diameter: via_dia,
                drill: via_drill,
                layers: solver.via_layers.clone(),
                locked: true,
                tented: true,
            };
            if solver.try_commit_via(via, None, true) {
                plane_vias_added += 1;
                pofv_count += 1;
                placed_for_pad = true;
                drops.push(SolvedViaDrop {
                    pad_label: target.label.clone(),
                    net: target.net.clone(),
                    pos: target.center,
                    is_thermal: false,
                    is_pofv: true,
                    stub_len_nm: 0,
                });
            }
        }

        if !placed_for_pad {
            let cands = radial_candidates(
                &solver.pads[target.pad_idx],
                target.comp_center,
                via_dia,
                cfg.max_stub_len_nm,
            );
            for cand in cands {
                if let Some(drop) =
                    solver.try_commit_radial_via(target, cand, via_dia, via_drill, stub_w)
                {
                    plane_vias_added += 1;
                    stubs_added += 1;
                    placed_for_pad = true;
                    drops.push(drop);
                    break;
                }
            }
        }

        if !placed_for_pad {
            unresolved.push(UnresolvedPowerPad {
                pad_label: target.label.clone(),
                net: target.net.clone(),
                reason: "no legal plane via candidate cleared obstacles and chokes".into(),
            });
        }
    }

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
