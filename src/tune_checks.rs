use crate::drc_layout::keepout_matches_layer;
use crate::drc_scene::{
    DrillItem, KeepoutItem, PadItem, PourItem, ViaItem, collect_drills, collect_keepouts,
    collect_pads, collect_pours, collect_vias, compute_max_clearance,
};
use crate::geom::{SpatialGrid, point_in_polygon, point_to_segment_dist};
use crate::ir::{BBoxNm, BoardIr, Nm, PadKind, Pt, Track, same_named_net};
use crate::poly_index::{IndexedRing, indexed_shape_edge_clearance};
use crate::shapes::ResolvedShape;

const TOUCH_TOL_NM: f64 = 1.0;
const REBUILD_GRID_THRESHOLD: usize = 64;

pub(crate) struct TuneTrack {
    pub(crate) net: String,
    pub(crate) layer: String,
    pub(crate) a: Pt,
    pub(crate) b: Pt,
    pub(crate) width: Nm,
    pub(crate) locked: bool,
    pub(crate) shape: ResolvedShape,
    pub(crate) bbox: BBoxNm,
    pub(crate) active: bool,
}

impl TuneTrack {
    fn from_track(t: &Track) -> Self {
        let shape = ResolvedShape::Capsule {
            a: t.a,
            b: t.b,
            radius: t.width / 2,
        };
        let bbox = shape.bbox();
        Self {
            net: t.net.clone(),
            layer: t.layer.clone(),
            a: t.a,
            b: t.b,
            width: t.width,
            locked: t.locked,
            shape,
            bbox,
            active: true,
        }
    }

    fn to_track(&self) -> Track {
        Track {
            net: self.net.clone(),
            layer: self.layer.clone(),
            a: self.a,
            b: self.b,
            width: self.width,
            locked: self.locked,
        }
    }
}

pub(crate) struct SpanContext<'a> {
    pub(crate) orig_idx: usize,
    pub(crate) net: &'a str,
    pub(crate) layer: &'a str,
    pub(crate) width: Nm,
    pub(crate) span_a: Pt,
    pub(crate) span_b: Pt,
    pub(crate) step: f64,
}

pub(crate) struct TuneScene<'a> {
    pub(crate) board: &'a BoardIr,
    pub(crate) outline_idx: Option<IndexedRing>,
    pub(crate) cutout_idxs: Vec<IndexedRing>,
    pub(crate) pads: Vec<PadItem>,
    pad_grid: SpatialGrid,
    pub(crate) vias: Vec<ViaItem>,
    via_grid: SpatialGrid,
    drills: Vec<DrillItem>,
    drill_grid: SpatialGrid,
    keepouts: Vec<KeepoutItem>,
    keepout_grid: SpatialGrid,
    pours: Vec<PourItem<'a>>,
    pub(crate) tracks: Vec<TuneTrack>,
    orig_len: usize,
    replacements: Vec<Option<Vec<usize>>>,
    track_grid: SpatialGrid,
    pending_tracks: Vec<usize>,
    max_clr: Nm,
    edge_clr: Nm,
    hole_clr: Nm,
    candidates: Vec<usize>,
}

impl<'a> TuneScene<'a> {
    pub(crate) fn new(board: &'a BoardIr) -> Self {
        let cu_layers = board.copper_layer_names();
        let outline_idx = (board.outline.len() >= 3).then(|| IndexedRing::new(&board.outline));
        let cutout_idxs: Vec<IndexedRing> = board
            .cutouts
            .iter()
            .filter(|c| c.len() >= 3)
            .map(|c| IndexedRing::new(c))
            .collect();
        let pads = collect_pads(board, &cu_layers);
        let vias = collect_vias(board);
        let drills = collect_drills(board, &pads, &vias, &cu_layers);
        let pours = collect_pours(board);
        let keepouts = collect_keepouts(board);
        let max_clr = compute_max_clearance(board, &pours);

        let pad_bboxes: Vec<BBoxNm> = pads.iter().map(|p| p.bbox).collect();
        let via_bboxes: Vec<BBoxNm> = vias.iter().map(|v| v.bbox).collect();
        let drill_bboxes: Vec<BBoxNm> = drills.iter().map(|d| d.bbox).collect();
        let ko_bboxes: Vec<BBoxNm> = keepouts.iter().map(|k| k.bbox).collect();

        let tracks: Vec<TuneTrack> = board.tracks.iter().map(TuneTrack::from_track).collect();
        let orig_len = tracks.len();
        let replacements = vec![None; orig_len];
        let track_bboxes: Vec<BBoxNm> = tracks.iter().map(|t| t.bbox).collect();

        Self {
            board,
            outline_idx,
            cutout_idxs,
            pads,
            pad_grid: SpatialGrid::new(&pad_bboxes, 2_000_000),
            vias,
            via_grid: SpatialGrid::new(&via_bboxes, 2_000_000),
            drills,
            drill_grid: SpatialGrid::new(&drill_bboxes, 2_000_000),
            keepouts,
            keepout_grid: SpatialGrid::new(&ko_bboxes, 2_000_000),
            pours,
            tracks,
            orig_len,
            replacements,
            track_grid: SpatialGrid::new(&track_bboxes, 2_000_000),
            pending_tracks: Vec::new(),
            max_clr,
            edge_clr: board.rules.edge_clearance_nm,
            hole_clr: board.rules.effective_hole_clearance_nm(),
            candidates: Vec::new(),
        }
    }

    pub(crate) fn board_max_extent(&self) -> f64 {
        let bb = BBoxNm::from_pts(&self.board.outline);
        (bb.width().max(bb.height()) as f64).max(10_000_000.0)
    }

    /// Computes required lead-in/lead-out distance at `ep` to step past same-net pads/vias there.
    pub(crate) fn endpoint_pad_clear_lead(
        &self,
        net: &str,
        layer: &str,
        ep: Pt,
        width: Nm,
        same_net_clr: Nm,
        base_lead: f64,
    ) -> f64 {
        let mut lead = base_lead;
        let pad_margin = width as f64 * 0.5 + same_net_clr as f64;
        for pad in &self.pads {
            if pad.kind == PadKind::Npth
                || !same_named_net(net, &pad.net)
                || !pad.layers.iter().any(|l| l == layer)
            {
                continue;
            }
            if pad.shape.dist_to_point(ep) <= TOUCH_TOL_NM {
                let half_diag = ((pad.bbox.width() as f64).hypot(pad.bbox.height() as f64)) * 0.5;
                lead = lead.max(ep.dist(pad.center) + half_diag + pad_margin);
            }
        }
        for (vi, via) in self.vias.iter().enumerate() {
            if !same_named_net(net, &via.net)
                || !self.board.vias[vi].touches_layer(layer, &self.board.layers)
            {
                continue;
            }
            if via.pos.dist(ep) <= (via.dia / 2) as f64 + TOUCH_TOL_NM {
                lead = lead.max(ep.dist(via.pos) + (via.dia / 2) as f64 + pad_margin);
            }
        }
        lead
    }

    /// Finds sorted interior T-junction offsets along segment `(a, b)` for `net` on `layer`.
    pub(crate) fn interior_junctions(
        &mut self,
        orig_idx: usize,
        net: &str,
        layer: &str,
        a: Pt,
        b: Pt,
        width: Nm,
    ) -> Vec<f64> {
        let len = a.dist(b);
        if len <= TOUCH_TOL_NM {
            return Vec::new();
        }
        let ux = (b.x - a.x) as f64 / len;
        let uy = (b.y - a.y) as f64 / len;
        let tol = (width as f64 * 0.5).max(TOUCH_TOL_NM);
        let q = BBoxNm::from_pts(&[a, b]).expand(width);
        let mut splits = Vec::new();
        let mut push_pt = |p: Pt| {
            if p.dist(a) <= TOUCH_TOL_NM || p.dist(b) <= TOUCH_TOL_NM {
                return;
            }
            if point_to_segment_dist(p, a, b) <= tol {
                let s = (p.x - a.x) as f64 * ux + (p.y - a.y) as f64 * uy;
                if s > TOUCH_TOL_NM && s < len - TOUCH_TOL_NM {
                    splits.push(s);
                }
            }
        };

        self.track_grid.query(q, &mut self.candidates);
        for &ti in &self.candidates {
            if ti == orig_idx || !self.tracks[ti].active {
                continue;
            }
            let t = &self.tracks[ti];
            if t.layer == layer && same_named_net(net, &t.net) {
                push_pt(t.a);
                push_pt(t.b);
            }
        }
        for &ti in &self.pending_tracks {
            if ti == orig_idx || !self.tracks[ti].active {
                continue;
            }
            let t = &self.tracks[ti];
            if t.layer == layer && same_named_net(net, &t.net) && t.bbox.intersects(q) {
                push_pt(t.a);
                push_pt(t.b);
            }
        }
        self.via_grid.query(q, &mut self.candidates);
        for &vi in &self.candidates {
            let v = &self.vias[vi];
            if same_named_net(net, &v.net)
                && self.board.vias[vi].touches_layer(layer, &self.board.layers)
            {
                push_pt(v.pos);
            }
        }
        self.pad_grid.query(q, &mut self.candidates);
        for &pi in &self.candidates {
            let p = &self.pads[pi];
            if p.kind != PadKind::Npth
                && same_named_net(net, &p.net)
                && p.layers.iter().any(|l| l == layer)
            {
                push_pt(p.center);
            }
        }

        splits.sort_by(f64::total_cmp);
        splits.dedup_by(|x, y| (*x - *y).abs() <= 10.0);
        splits
    }

    pub(crate) fn commit_replacement(
        &mut self,
        orig_idx: usize,
        new_tracks: Vec<Track>,
    ) -> Vec<usize> {
        self.tracks[orig_idx].active = false;
        let mut new_indices = Vec::with_capacity(new_tracks.len());
        for t in new_tracks {
            let idx = self.tracks.len();
            self.tracks.push(TuneTrack::from_track(&t));
            self.replacements.push(None);
            self.pending_tracks.push(idx);
            new_indices.push(idx);
        }
        self.replacements[orig_idx] = Some(new_indices.clone());
        if self.pending_tracks.len() >= REBUILD_GRID_THRESHOLD {
            let bboxes: Vec<BBoxNm> = self.tracks.iter().map(|t| t.bbox).collect();
            self.track_grid = SpatialGrid::new(&bboxes, 2_000_000);
            self.pending_tracks.clear();
        }
        new_indices
    }

    pub(crate) fn into_tracks(self) -> Vec<Track> {
        fn collect_rec(
            idx: usize,
            tracks: &[TuneTrack],
            replacements: &[Option<Vec<usize>>],
            out: &mut Vec<Track>,
        ) {
            if tracks[idx].active {
                out.push(tracks[idx].to_track());
            } else if let Some(children) = &replacements[idx] {
                for &c in children {
                    collect_rec(c, tracks, replacements, out);
                }
            }
        }
        let mut out = Vec::with_capacity(self.tracks.len());
        for i in 0..self.orig_len {
            collect_rec(i, &self.tracks, &self.replacements, &mut out);
        }
        out
    }

    /// Strict continuous DRC clearance gate for candidate segments `(a, b, is_excursion)`.
    pub(crate) fn segments_are_clean(
        &mut self,
        ctx: &SpanContext<'_>,
        segs: &[(Pt, Pt, bool)],
    ) -> bool {
        let same_net_clr = self.board.net_clearance(ctx.net).max(1);
        let radius = ctx.width / 2;
        let query_pad = self.max_clr.max(same_net_clr) + radius;

        // Guard against self-intersection among newly created meander segments.
        let shapes: Vec<ResolvedShape> = segs
            .iter()
            .map(|&(a, b, _)| ResolvedShape::Capsule { a, b, radius })
            .collect();
        for i in 0..shapes.len() {
            for j in (i + 2)..shapes.len() {
                if shapes[i].dist_to_shape(&shapes[j]) < 0.0 {
                    return false;
                }
            }
        }

        for (seg, shape) in segs.iter().zip(&shapes) {
            if !self.single_segment_clean(ctx, same_net_clr, query_pad, *seg, shape) {
                return false;
            }
        }
        true
    }

    fn single_segment_clean(
        &mut self,
        ctx: &SpanContext<'_>,
        same_net_clr: Nm,
        query_pad: Nm,
        (sa, sb, is_excursion): (Pt, Pt, bool),
        shape: &ResolvedShape,
    ) -> bool {
        let (orig_idx, net, layer, span_a, span_b) =
            (ctx.orig_idx, ctx.net, ctx.layer, ctx.span_a, ctx.span_b);
        let radius = ctx.width / 2;
        let seg_bbox = shape.bbox();
        let q_bbox = seg_bbox.expand(query_pad);

        // 1. Board outline and cutouts.
        if let Some(ref outline_idx) = self.outline_idx {
            let (dist, off) =
                indexed_shape_edge_clearance(shape, outline_idx, &self.cutout_idxs, self.edge_clr);
            if off || dist < self.edge_clr as f64 {
                return false;
            }
        }

        // 2. Track-blocking keepouts on layer.
        self.keepout_grid.query(seg_bbox, &mut self.candidates);
        for &ki in &self.candidates {
            let ko = &self.keepouts[ki];
            if ko.kind.blocks_tracks()
                && keepout_matches_layer(&ko.layers, layer)
                && (point_in_polygon(sa, &ko.poly)
                    || point_in_polygon(sb, &ko.poly)
                    || shape.dist_to_shape(&ko.shape) < 0.0)
            {
                return false;
            }
        }

        // 3. Other tracks (other-net pair_clearance; same-net excursion clearance).
        self.track_grid.query(q_bbox, &mut self.candidates);
        for &ti in &self.candidates {
            if !self.track_clear_against(
                ti,
                orig_idx,
                net,
                layer,
                same_net_clr,
                is_excursion,
                shape,
            ) {
                return false;
            }
        }
        for &ti in &self.pending_tracks {
            if self.tracks[ti].bbox.intersects(q_bbox)
                && !self.track_clear_against(
                    ti,
                    orig_idx,
                    net,
                    layer,
                    same_net_clr,
                    is_excursion,
                    shape,
                )
            {
                return false;
            }
        }

        // 4. Non-NPTH pads on layer.
        self.pad_grid.query(q_bbox, &mut self.candidates);
        for &pi in &self.candidates {
            let pad = &self.pads[pi];
            if pad.kind == PadKind::Npth || !pad.layers.iter().any(|l| l == layer) {
                continue;
            }
            if same_named_net(net, &pad.net) {
                if is_excursion
                    && pad.shape.dist_to_point(span_a) > TOUCH_TOL_NM
                    && pad.shape.dist_to_point(span_b) > TOUCH_TOL_NM
                    && pad.shape.dist_to_capsule(sa, sb, radius) < same_net_clr as f64
                {
                    return false;
                }
            } else if pad.shape.dist_to_capsule(sa, sb, radius)
                < self.board.pair_clearance(net, &pad.net, None) as f64
            {
                return false;
            }
        }

        // 5. Vias touching layer.
        self.via_grid.query(q_bbox, &mut self.candidates);
        for &vi in &self.candidates {
            let via = &self.vias[vi];
            if !self.board.vias[vi].touches_layer(layer, &self.board.layers) {
                continue;
            }
            if same_named_net(net, &via.net) {
                if is_excursion
                    && via.pos.dist(span_a) > (via.dia / 2) as f64 + TOUCH_TOL_NM
                    && via.pos.dist(span_b) > (via.dia / 2) as f64 + TOUCH_TOL_NM
                    && shape.dist_to_circle(via.pos, via.dia / 2) < same_net_clr as f64
                {
                    return false;
                }
            } else {
                let req =
                    self.board
                        .pair_clearance(net, &via.net, self.board.rules.via_to_track_nm);
                if shape.dist_to_circle(via.pos, via.dia / 2) < req as f64 {
                    return false;
                }
            }
        }

        // 6. Drills touching layer (NPTH + other-net THT/via drills).
        self.drill_grid.query(q_bbox, &mut self.candidates);
        for &di in &self.candidates {
            let d = &self.drills[di];
            if (!d.is_npth && same_named_net(&d.net, net)) || !d.layers.iter().any(|l| l == layer) {
                continue;
            }
            let d_net = if d.is_npth { "" } else { d.net.as_str() };
            let req = self
                .board
                .pair_clearance(d_net, net, None)
                .max(self.hole_clr);
            let dist = point_to_segment_dist(d.pos, sa, sb) - d.radius as f64 - radius as f64;
            if dist < req as f64 {
                return false;
            }
        }

        // 7. Existing filled pours on layer.
        for pour in &self.pours {
            if pour.layer != layer
                || same_named_net(pour.net, net)
                || !seg_bbox.expand(self.max_clr).intersects(pour.bbox)
            {
                continue;
            }
            let req = self
                .board
                .pour_pair_clearance(pour.net, pour.clearance_override, net);
            if pour.indexed.dist_to_shape_bounded(shape, req) < req as f64 {
                return false;
            }
        }
        true
    }

    #[allow(clippy::too_many_arguments)]
    fn track_clear_against(
        &self,
        ti: usize,
        orig_idx: usize,
        net: &str,
        layer: &str,
        same_net_clr: Nm,
        is_excursion: bool,
        shape: &ResolvedShape,
    ) -> bool {
        if ti == orig_idx || !self.tracks[ti].active {
            return true;
        }
        let other = &self.tracks[ti];
        if other.layer != layer {
            return true;
        }
        if same_named_net(net, &other.net) {
            if !is_excursion {
                return true;
            }
            return shape.dist_to_shape(&other.shape) >= same_net_clr as f64;
        }
        let req = self.board.pair_clearance(net, &other.net, None);
        shape.dist_to_shape(&other.shape) >= req as f64
    }
}
