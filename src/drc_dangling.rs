use std::collections::VecDeque;

use crate::drc::{Severity, Violation, ViolationKind};
use crate::drc_scene::{PadItem, PourItem, TrackItem, ViaItem};
use crate::geom::{SpatialGrid, point_to_segment_dist, segment_to_segment_dist};
use crate::ir::{BBoxNm, BoardIr, Nm, PadKind, Pt, same_named_net};
use crate::shapes::ResolvedShape;

const TOUCH_TOL_NM: f64 = 1.0;
const PRUNE_COPPER_TOL_NM: f64 = 2_000.0;
const PRUNE_PAD_NM: Nm = 2_002;

pub(crate) struct CopperContext<'a> {
    cu_layers: &'a [String],
    pads: &'a [PadItem],
    tht_barrels: Vec<Option<ResolvedShape>>,
    pad_grid: SpatialGrid,
    vias: &'a [ViaItem],
    via_spanned_idx: Vec<Vec<usize>>,
    via_grid: SpatialGrid,
    pours: &'a [PourItem<'a>],
    tracks: &'a [TrackItem<'a>],
    track_grid: SpatialGrid,
    candidates: Vec<usize>,
    pub(crate) alive_tracks: Vec<bool>,
    pub(crate) alive_vias: Vec<bool>,
}

impl<'a> CopperContext<'a> {
    pub(crate) fn new(
        board: &BoardIr,
        cu_layers: &'a [String],
        pads: &'a [PadItem],
        vias: &'a [ViaItem],
        pours: &'a [PourItem<'a>],
        tracks: &'a [TrackItem<'a>],
    ) -> Self {
        let mut tht_barrels = Vec::with_capacity(pads.len());
        let pad_grid = SpatialGrid::from_bboxes(
            pads.iter().map(|pad| {
                if pad.kind == PadKind::Tht
                    && let Some(drill) = pad.drill_nm
                    && drill > 0
                    && pad.layers.len() < cu_layers.len()
                {
                    let barrel = ResolvedShape::Circle {
                        center: pad.center,
                        radius: drill / 2,
                    };
                    let bb = pad.bbox.union(barrel.bbox());
                    tht_barrels.push(Some(barrel));
                    bb
                } else {
                    tht_barrels.push(None);
                    pad.bbox
                }
            }),
            2_000_000,
        );
        let via_spanned_idx = board
            .vias
            .iter()
            .map(|v| {
                v.spanned_layers(cu_layers, &board.layers)
                    .into_iter()
                    .filter_map(|l| cu_layers.iter().position(|c| c == &l))
                    .collect()
            })
            .collect();
        Self {
            cu_layers,
            pads,
            tht_barrels,
            pad_grid,
            vias,
            via_spanned_idx,
            via_grid: SpatialGrid::from_bboxes(vias.iter().map(|v| v.bbox), 2_000_000),
            pours,
            tracks,
            track_grid: SpatialGrid::from_bboxes(tracks.iter().map(|t| t.bbox), 2_000_000),
            candidates: Vec::new(),
            alive_tracks: vec![true; tracks.len()],
            alive_vias: vec![true; vias.len()],
        }
    }

    fn pad_shape_on_layer(&self, pad_idx: usize, layer: &str) -> Option<&ResolvedShape> {
        let pad = &self.pads[pad_idx];
        if pad.kind == PadKind::Npth {
            return None;
        }
        if pad.layers.iter().any(|l| l == layer) {
            return Some(&pad.shape);
        }
        if self.cu_layers.iter().any(|l| l == layer) {
            return self.tht_barrels[pad_idx].as_ref();
        }
        None
    }

    fn via_touches_layer(&self, via_idx: usize, layer: &str) -> bool {
        self.via_spanned_idx[via_idx]
            .iter()
            .any(|&li| self.cu_layers[li] == layer)
    }

    pub(crate) fn is_track_endpoint_anchored(&mut self, track_idx: usize, p: Pt) -> bool {
        let tr = &self.tracks[track_idx];
        if tr.net.is_empty() {
            return false;
        }
        let q = BBoxNm::from_pt(p, 2);
        let mut cands = std::mem::take(&mut self.candidates);

        self.pad_grid.query(q, &mut cands);
        let hit_pad = cands.iter().any(|&pi| {
            same_named_net(&self.pads[pi].net, tr.net)
                && self
                    .pad_shape_on_layer(pi, tr.layer)
                    .is_some_and(|s| s.dist_to_point(p) <= TOUCH_TOL_NM)
        });

        let hit_via = !hit_pad && {
            self.via_grid.query(q, &mut cands);
            cands.iter().any(|&vi| {
                self.alive_vias[vi]
                    && same_named_net(&self.vias[vi].net, tr.net)
                    && self.via_touches_layer(vi, tr.layer)
                    && self.vias[vi].shape.dist_to_point(p) <= TOUCH_TOL_NM
            })
        };

        let hit_pour = !hit_pad && !hit_via && {
            let pt_shape = ResolvedShape::Circle {
                center: p,
                radius: 0,
            };
            self.pours.iter().any(|pour| {
                pour.layer == tr.layer
                    && same_named_net(pour.net, tr.net)
                    && pour.indexed.dist_to_shape_bounded(&pt_shape, 2) <= TOUCH_TOL_NM
            })
        };

        let hit_track = !hit_pad && !hit_via && !hit_pour && {
            self.track_grid.query(q, &mut cands);
            cands.iter().any(|&oi| {
                oi != track_idx
                    && self.alive_tracks[oi]
                    && self.tracks[oi].layer == tr.layer
                    && same_named_net(self.tracks[oi].net, tr.net)
                    && point_to_segment_dist(p, self.tracks[oi].a, self.tracks[oi].b)
                        <= TOUCH_TOL_NM
            })
        };

        self.candidates = cands;
        hit_pad || hit_via || hit_pour || hit_track
    }

    pub(crate) fn track_has_external_touch(
        &mut self,
        track_idx: usize,
        anchor: Option<Pt>,
    ) -> bool {
        let tr = &self.tracks[track_idx];
        let tol = PRUNE_COPPER_TOL_NM;
        let q = tr.bbox.expand(PRUNE_PAD_NM);
        let min_hits = if anchor.is_some() { 1 } else { 2 };
        let mut hits = 0usize;
        let mut cands = std::mem::take(&mut self.candidates);
        let anc_pt = anchor.map(|center| ResolvedShape::Circle { center, radius: 0 });

        let mut hit = false;
        self.track_grid.query(q, &mut cands);
        for &oi in &cands {
            if oi == track_idx || !self.alive_tracks[oi] {
                continue;
            }
            let other = &self.tracks[oi];
            let copper_touch = tr.shape.dist_to_shape(&other.shape) <= tol
                && anchor.is_none_or(|anc| other.shape.dist_to_point(anc) > tol);
            let centerline_touch = segment_to_segment_dist(tr.a, tr.b, other.a, other.b) <= tol
                && anchor.is_none_or(|anc| point_to_segment_dist(anc, other.a, other.b) > tol);
            if other.layer == tr.layer
                && same_named_net(other.net, tr.net)
                && (copper_touch || centerline_touch)
            {
                hits += 1;
                if hits >= min_hits {
                    hit = true;
                    break;
                }
            }
        }

        if !hit {
            self.pad_grid.query(q, &mut cands);
            for &pi in &cands {
                if same_named_net(&self.pads[pi].net, tr.net)
                    && self.pad_shape_on_layer(pi, tr.layer).is_some_and(|shape| {
                        shape.dist_to_shape(&tr.shape) <= tol
                            && anchor.is_none_or(|anc| shape.dist_to_point(anc) > tol)
                    })
                {
                    hits += 1;
                    if hits >= min_hits {
                        hit = true;
                        break;
                    }
                }
            }
        }

        if !hit {
            self.via_grid.query(q, &mut cands);
            for &vi in &cands {
                if !self.alive_vias[vi] {
                    continue;
                }
                let via = &self.vias[vi];
                if same_named_net(&via.net, tr.net)
                    && self.via_touches_layer(vi, tr.layer)
                    && via.shape.dist_to_shape(&tr.shape) <= tol
                    && anchor.is_none_or(|anc| via.shape.dist_to_point(anc) > tol)
                {
                    hits += 1;
                    if hits >= min_hits {
                        hit = true;
                        break;
                    }
                }
            }
        }

        if !hit {
            for pour in self.pours {
                if pour.layer == tr.layer
                    && same_named_net(pour.net, tr.net)
                    && pour.indexed.dist_to_shape_bounded(&tr.shape, PRUNE_PAD_NM) <= tol
                    && anc_pt
                        .as_ref()
                        .is_none_or(|ps| pour.indexed.dist_to_shape_bounded(ps, PRUNE_PAD_NM) > tol)
                {
                    hits += 1;
                    if hits >= min_hits {
                        hit = true;
                        break;
                    }
                }
            }
        }

        self.candidates = cands;
        hit
    }

    fn via_direct_planar_layers(&mut self, via_idx: usize) -> Vec<usize> {
        let via = &self.vias[via_idx];
        if via.net.is_empty() {
            return Vec::new();
        }
        let spanned = &self.via_spanned_idx[via_idx];
        let q = via.bbox.expand(2);
        let mut found = Vec::new();
        let mut cands = std::mem::take(&mut self.candidates);

        self.pad_grid.query(q, &mut cands);
        for &pi in &cands {
            if !same_named_net(&self.pads[pi].net, &via.net) || self.pads[pi].kind == PadKind::Npth
            {
                continue;
            }
            for &li in spanned {
                if found.contains(&li) {
                    continue;
                }
                if self
                    .pad_shape_on_layer(pi, &self.cu_layers[li])
                    .is_some_and(|s| s.dist_to_shape(&via.shape) <= TOUCH_TOL_NM)
                {
                    found.push(li);
                    if found.len() >= 2 {
                        break;
                    }
                }
            }
            if found.len() >= 2 {
                break;
            }
        }

        if found.len() < 2 {
            self.track_grid.query(q, &mut cands);
            for &ti in &cands {
                if !self.alive_tracks[ti] {
                    continue;
                }
                let tr = &self.tracks[ti];
                if !same_named_net(tr.net, &via.net) {
                    continue;
                }
                if let Some(&li) = spanned.iter().find(|&&li| self.cu_layers[li] == tr.layer)
                    && !found.contains(&li)
                    && tr.shape.dist_to_shape(&via.shape) <= TOUCH_TOL_NM
                {
                    found.push(li);
                    if found.len() >= 2 {
                        break;
                    }
                }
            }
        }

        if found.len() < 2 {
            for pour in self.pours {
                if !same_named_net(pour.net, &via.net) {
                    continue;
                }
                if let Some(&li) = spanned.iter().find(|&&li| self.cu_layers[li] == pour.layer)
                    && !found.contains(&li)
                    && pour.indexed.dist_to_shape_bounded(&via.shape, 2) <= TOUCH_TOL_NM
                {
                    found.push(li);
                    if found.len() >= 2 {
                        break;
                    }
                }
            }
        }

        self.candidates = cands;
        found
    }

    pub(crate) fn compute_via_connected_counts(&mut self) -> Vec<usize> {
        let n = self.vias.len();
        let direct: Vec<Vec<usize>> = (0..n)
            .map(|vi| {
                if self.alive_vias[vi] {
                    self.via_direct_planar_layers(vi)
                } else {
                    Vec::new()
                }
            })
            .collect();

        let mut counts: Vec<usize> = direct.iter().map(Vec::len).collect();
        let needs_graph =
            (0..n).any(|vi| self.alive_vias[vi] && !self.vias[vi].net.is_empty() && counts[vi] < 2);
        if !needs_graph {
            return counts;
        }

        let mut via_adj = vec![Vec::new(); n];
        let mut cands = std::mem::take(&mut self.candidates);
        for vi in 0..n {
            if !self.alive_vias[vi] || self.vias[vi].net.is_empty() {
                continue;
            }
            self.via_grid
                .query(self.vias[vi].bbox.expand(2), &mut cands);
            for &vj in &cands {
                if vj <= vi || !self.alive_vias[vj] {
                    continue;
                }
                if same_named_net(&self.vias[vj].net, &self.vias[vi].net)
                    && self.via_spanned_idx[vi]
                        .iter()
                        .any(|l| self.via_spanned_idx[vj].contains(l))
                    && self.vias[vi].shape.dist_to_shape(&self.vias[vj].shape) <= TOUCH_TOL_NM
                {
                    via_adj[vi].push(vj);
                    via_adj[vj].push(vi);
                }
            }
        }
        self.candidates = cands;

        let mut visited = vec![0u32; n];
        let mut stamp = 0u32;
        let mut queue = VecDeque::new();
        for vi in 0..n {
            if !self.alive_vias[vi]
                || self.vias[vi].net.is_empty()
                || counts[vi] >= 2
                || via_adj[vi].is_empty()
            {
                continue;
            }
            stamp += 1;
            visited[vi] = stamp;
            if direct[vi].len() == 1 {
                let l1 = direct[vi][0];
                queue.clear();
                for &vj in &via_adj[vi] {
                    if self.via_spanned_idx[vi]
                        .iter()
                        .any(|&l| l != l1 && self.via_spanned_idx[vj].contains(&l))
                    {
                        visited[vj] = stamp;
                        queue.push_back(vj);
                    }
                }
                while let Some(u) = queue.pop_front() {
                    if direct[u].iter().any(|&l| l != l1) {
                        counts[vi] = 2;
                        break;
                    }
                    for &w in &via_adj[u] {
                        if visited[w] != stamp {
                            visited[w] = stamp;
                            queue.push_back(w);
                        }
                    }
                }
            } else {
                // With 0 direct planar layers, vi is active iff it bridges >= 2 planar-backed via components reaching >= 2 distinct layers.
                let mut nonempty_comps = 0usize;
                let mut union_layers: Vec<usize> = Vec::new();
                for &vj in &via_adj[vi] {
                    if visited[vj] == stamp {
                        continue;
                    }
                    visited[vj] = stamp;
                    queue.clear();
                    queue.push_back(vj);
                    let mut has_planar = false;
                    while let Some(u) = queue.pop_front() {
                        for &l in &direct[u] {
                            has_planar = true;
                            if !union_layers.contains(&l) {
                                union_layers.push(l);
                            }
                        }
                        for &w in &via_adj[u] {
                            if visited[w] != stamp {
                                visited[w] = stamp;
                                queue.push_back(w);
                            }
                        }
                    }
                    if has_planar {
                        nonempty_comps += 1;
                        if nonempty_comps >= 2 && union_layers.len() >= 2 {
                            counts[vi] = 2;
                            break;
                        }
                    }
                }
            }
        }
        counts
    }
}

/// Checks for dangling tracks (unconnected endpoints) and dangling vias (< 2 connected copper layers).
pub(crate) fn check_dangling(
    board: &BoardIr,
    cu_layers: &[String],
    tracks: &[TrackItem<'_>],
    pads: &[PadItem],
    vias: &[ViaItem],
    pours: &[PourItem<'_>],
) -> Vec<Violation> {
    let mut ctx = CopperContext::new(board, cu_layers, pads, vias, pours, tracks);
    let mut out = Vec::new();

    for (ti, tr) in tracks.iter().enumerate() {
        let a_conn = ctx.is_track_endpoint_anchored(ti, tr.a);
        let b_conn = a_conn && ctx.is_track_endpoint_anchored(ti, tr.b);
        if !b_conn {
            let free_pt = if !a_conn { tr.a } else { tr.b };
            let net_disp = if tr.net.is_empty() {
                "<no net>"
            } else {
                tr.net
            };
            out.push(Violation {
                kind: ViolationKind::TrackDangling,
                severity: Severity::Error,
                layer: tr.layer.to_string(),
                pos_nm: free_pt,
                measured_nm: tr.a.dist(tr.b).round() as Nm,
                required_nm: 0,
                item_a: tr.label.clone(),
                item_b: None,
                net_a: tr.net.to_string(),
                net_b: String::new(),
                message: format!(
                    "{} (net {net_disp}) has unconnected endpoint on {}",
                    tr.label, tr.layer
                ),
            });
        }
    }

    let via_counts = ctx.compute_via_connected_counts();
    for (vi, (via, &connected_layers)) in vias.iter().zip(&via_counts).enumerate() {
        if connected_layers < 2 {
            let net_disp = if via.net.is_empty() {
                "<no net>"
            } else {
                &via.net
            };
            let layer = ctx.via_spanned_idx[vi]
                .first()
                .map(|&li| cu_layers[li].clone())
                .unwrap_or_else(|| via.layers.0.clone());
            out.push(Violation {
                kind: ViolationKind::ViaDangling,
                severity: Severity::Error,
                layer,
                pos_nm: via.pos,
                measured_nm: connected_layers as Nm,
                required_nm: 2,
                item_a: via.label.clone(),
                item_b: None,
                net_a: via.net.clone(),
                net_b: String::new(),
                message: format!(
                    "{} (net {net_disp}) connects only {connected_layers} copper layer(s) (< 2)",
                    via.label
                ),
            });
        }
    }
    out
}
