use crate::drc_layout::keepout_matches_layer;
use crate::drc_mask::{collect_mask_openings, shape_violates_mask};
use crate::drc_scene::{
    DrillItem, KeepoutItem, PadItem, PourItem, ViaItem, collect_drills, collect_keepouts,
    collect_pads, collect_pours, collect_vias, compute_max_clearance,
};
use crate::geom::{
    EPS_NM, SpatialGrid, point_in_polygon, point_to_segment_dist, segment_to_segment_dist,
};
use crate::ir::{BBoxNm, BoardIr, Nm, PadKind, Pt, Side, Track, same_named_net};
use crate::poly_index::{IndexedRing, indexed_shape_edge_clearance};
use crate::shapes::ResolvedShape;
use crate::tune_geom::{
    TOUCH_TOL_NM, polyline_self_clear, project_shape_interval, push_circle_contact, push_contact,
    track_contact_interval,
};

pub(crate) struct SpanContext<'a> {
    pub(crate) orig_idx: usize,
    pub(crate) net: &'a str,
    pub(crate) layer: &'a str,
    pub(crate) width: Nm,
    pub(crate) span_a: Pt,
    pub(crate) span_b: Pt,
    pub(crate) step: f64,
}

struct MaskLayerIndex {
    items: Vec<(String, ResolvedShape, BBoxNm)>,
    grid: SpatialGrid,
}

fn build_mask_layer_index(
    board: &BoardIr,
    cu_layers: &[String],
    pads: &[PadItem],
    vias: &[ViaItem],
    side: Side,
) -> Option<MaskLayerIndex> {
    if !cu_layers.iter().any(|l| l == side.cu_layer()) {
        return None;
    }
    let items: Vec<(String, ResolvedShape, BBoxNm)> =
        collect_mask_openings(board, cu_layers, pads, vias, side)
            .into_iter()
            .map(|op| (op.net.to_string(), op.shape, op.bbox))
            .collect();
    if items.is_empty() {
        return None;
    }
    let grid = SpatialGrid::from_bboxes(items.iter().map(|(_, _, bb)| *bb), 2_000_000);
    Some(MaskLayerIndex { items, grid })
}

pub(crate) struct TuneScene<'a> {
    pub(crate) board: &'a BoardIr,
    outline_idx: Option<IndexedRing>,
    cutout_idxs: Vec<IndexedRing>,
    pads: Vec<PadItem>,
    pad_grid: SpatialGrid,
    vias: Vec<ViaItem>,
    via_grid: SpatialGrid,
    drills: Vec<DrillItem>,
    drill_grid: SpatialGrid,
    keepouts: Vec<KeepoutItem>,
    keepout_grid: SpatialGrid,
    pours: Vec<PourItem<'a>>,
    pub(crate) tracks: Vec<Option<Track>>,
    track_grid: SpatialGrid,
    mask_to_copper: Option<Nm>,
    top_masks: Option<MaskLayerIndex>,
    bot_masks: Option<MaskLayerIndex>,
    max_clr: Nm,
    candidates: Vec<usize>,
}

fn track_bbox(t: &Track) -> BBoxNm {
    BBoxNm::from_pts(&[t.a, t.b]).expand(t.width / 2)
}

impl<'a> TuneScene<'a> {
    pub(crate) fn new(board: &'a BoardIr) -> Self {
        let cu_layers = board.copper_layer_names();
        let outline_idx = (board.outline.len() >= 3).then(|| IndexedRing::new(&board.outline));
        let cutout_idxs = board
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
        let mask_to_copper = board.rules.mask_to_copper_nm.filter(|&v| v >= 0);
        let (top_masks, bot_masks) = if mask_to_copper.is_some() {
            (
                build_mask_layer_index(board, &cu_layers, &pads, &vias, Side::Top),
                build_mask_layer_index(board, &cu_layers, &pads, &vias, Side::Bottom),
            )
        } else {
            (None, None)
        };

        Self {
            board,
            outline_idx,
            cutout_idxs,
            pad_grid: SpatialGrid::from_bboxes(pads.iter().map(|p| p.bbox), 2_000_000),
            pads,
            via_grid: SpatialGrid::from_bboxes(vias.iter().map(|v| v.bbox), 2_000_000),
            vias,
            drill_grid: SpatialGrid::from_bboxes(drills.iter().map(|d| d.bbox), 2_000_000),
            drills,
            keepout_grid: SpatialGrid::from_bboxes(keepouts.iter().map(|k| k.bbox), 2_000_000),
            keepouts,
            pours,
            track_grid: SpatialGrid::from_bboxes(board.tracks.iter().map(track_bbox), 2_000_000),
            tracks: board.tracks.iter().cloned().map(Some).collect(),
            mask_to_copper,
            top_masks,
            bot_masks,
            max_clr,
            candidates: Vec::new(),
        }
    }

    pub(crate) fn board_max_extent(&self) -> f64 {
        let bb = self
            .outline_idx
            .as_ref()
            .map_or_else(|| BBoxNm::from_pts(&self.board.outline), |o| o.bbox);
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
            if pad.kind != PadKind::Npth
                && same_named_net(net, &pad.net)
                && pad.layers.iter().any(|l| l == layer)
                && pad.shape.dist_to_point(ep) <= TOUCH_TOL_NM
            {
                let half_diag = ((pad.bbox.width() as f64).hypot(pad.bbox.height() as f64)) * 0.5;
                lead = lead.max(ep.dist(pad.center) + half_diag + pad_margin);
            }
        }
        for (vi, via) in self.vias.iter().enumerate() {
            if same_named_net(net, &via.net)
                && self.board.vias[vi].touches_layer(layer, &self.board.layers)
                && via.pos.dist(ep) <= (via.dia / 2) as f64 + TOUCH_TOL_NM
            {
                lead = lead.max(ep.dist(via.pos) + (via.dia / 2) as f64 + pad_margin);
            }
        }
        lead
    }

    /// Finds merged protected copper-contact intervals `[s0, s1]` along `(a, b)`.
    pub(crate) fn protected_intervals(
        &mut self,
        orig_idx: usize,
        net: &str,
        layer: &str,
        a: Pt,
        b: Pt,
        width: Nm,
    ) -> Vec<(f64, f64)> {
        let len = a.dist(b);
        if len <= TOUCH_TOL_NM {
            return Vec::new();
        }
        let r = width / 2;
        let shape = ResolvedShape::Capsule { a, b, radius: r };
        let seg_bbox = shape.bbox();

        // Lifting copper off a touching same-net pour can sever an island.
        for pour in &self.pours {
            if pour.layer == layer
                && same_named_net(pour.net, net)
                && seg_bbox.expand(2).intersects(pour.bbox)
                && pour.indexed.dist_to_shape_bounded(&shape, 1) <= TOUCH_TOL_NM
            {
                return vec![(0.0, len)];
            }
        }

        let (ux, uy) = ((b.x - a.x) as f64 / len, (b.y - a.y) as f64 / len);
        let q = seg_bbox.expand(2);
        let mut intervals = Vec::new();

        self.track_grid.query(q, &mut self.candidates);
        for &ti in &self.candidates {
            let Some(t) = (ti != orig_idx).then(|| self.tracks[ti].as_ref()).flatten() else {
                continue;
            };
            if t.layer != layer || !same_named_net(net, &t.net) {
                continue;
            }
            let reach = (r + t.width / 2) as f64 + TOUCH_TOL_NM;
            if segment_to_segment_dist(a, b, t.a, t.b) <= reach {
                let (lo, hi) = track_contact_interval(a, ux, uy, len, t.a, t.b, reach);
                push_contact(
                    &mut intervals,
                    len,
                    point_to_segment_dist(a, t.a, t.b) <= reach,
                    point_to_segment_dist(b, t.a, t.b) <= reach,
                    lo,
                    hi,
                );
            }
        }

        self.via_grid.query(q, &mut self.candidates);
        for &vi in &self.candidates {
            let v = &self.vias[vi];
            if same_named_net(net, &v.net)
                && self.board.vias[vi].touches_layer(layer, &self.board.layers)
            {
                let reach = (r + v.dia / 2) as f64 + TOUCH_TOL_NM;
                push_circle_contact(&mut intervals, (a, b), (ux, uy, len), v.pos, reach);
            }
        }

        self.pad_grid.query(q, &mut self.candidates);
        for &pi in &self.candidates {
            let p = &self.pads[pi];
            if p.kind == PadKind::Npth || !same_named_net(net, &p.net) {
                continue;
            }
            if p.layers.iter().any(|l| l == layer) {
                if p.shape.dist_to_capsule(a, b, r) <= TOUCH_TOL_NM {
                    let end_tol = r as f64 + TOUCH_TOL_NM;
                    let (lo, hi) = project_shape_interval(&p.shape, a, ux, uy, r);
                    push_contact(
                        &mut intervals,
                        len,
                        p.shape.dist_to_point(a) <= end_tol,
                        p.shape.dist_to_point(b) <= end_tol,
                        lo,
                        hi,
                    );
                }
            } else if p.kind == PadKind::Tht
                && let Some(drill) = p.drill_nm
                && drill > 0
            {
                let reach = (r + drill / 2) as f64 + TOUCH_TOL_NM;
                push_circle_contact(&mut intervals, (a, b), (ux, uy, len), p.center, reach);
            }
        }

        intervals.sort_by(|x, y| x.0.total_cmp(&y.0).then_with(|| x.1.total_cmp(&y.1)));
        let mut merged: Vec<(f64, f64)> = Vec::with_capacity(intervals.len());
        for (s0, s1) in intervals {
            if let Some(last) = merged.last_mut()
                && s0 <= last.1 + 10.0
            {
                last.1 = last.1.max(s1);
            } else {
                merged.push((s0, s1));
            }
        }
        merged
    }

    pub(crate) fn commit_replacement(&mut self, orig_idx: usize, new_tracks: Vec<Track>) {
        self.tracks[orig_idx] = None;
        for t in new_tracks {
            self.track_grid.push(track_bbox(&t));
            self.tracks.push(Some(t));
        }
    }

    pub(crate) fn into_tracks(self) -> Vec<Track> {
        self.tracks.into_iter().flatten().collect()
    }

    /// Strict continuous DRC clearance gate for candidate polyline `pts`.
    pub(crate) fn segments_are_clean(&mut self, ctx: &SpanContext<'_>, pts: &[Pt]) -> bool {
        if !polyline_self_clear(pts, ctx.step) {
            return false;
        }
        let n_segs = pts.len() - 1;
        let same_net_clr = self.board.net_clearance(ctx.net).max(1);
        let m2c_pad = self.mask_to_copper.unwrap_or(0);
        let query_pad = self.max_clr.max(same_net_clr).max(m2c_pad) + ctx.width / 2;
        for (i, w) in pts.windows(2).enumerate() {
            let is_excursion = i > 0 && i + 1 < n_segs;
            if !self.single_segment_clean(ctx, same_net_clr, query_pad, w[0], w[1], is_excursion) {
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
        sa: Pt,
        sb: Pt,
        is_excursion: bool,
    ) -> bool {
        let (orig_idx, net, layer, span_a, span_b) =
            (ctx.orig_idx, ctx.net, ctx.layer, ctx.span_a, ctx.span_b);
        let shape = ResolvedShape::Capsule {
            a: sa,
            b: sb,
            radius: ctx.width / 2,
        };
        let seg_bbox = shape.bbox();
        let q_bbox = seg_bbox.expand(query_pad);

        if let Some(ref outline_idx) = self.outline_idx {
            let edge_clr = self.board.rules.edge_clearance_nm;
            let (dist, off) =
                indexed_shape_edge_clearance(&shape, outline_idx, &self.cutout_idxs, edge_clr);
            if off || dist < edge_clr as f64 - EPS_NM {
                return false;
            }
        }

        self.keepout_grid.query(seg_bbox, &mut self.candidates);
        for &ki in &self.candidates {
            let ko = &self.keepouts[ki];
            if ko.kind.blocks_tracks()
                && keepout_matches_layer(&ko.layers, layer)
                && (point_in_polygon(sa, &ko.poly)
                    || point_in_polygon(sb, &ko.poly)
                    || shape.dist_to_shape(&ko.shape) < -EPS_NM)
            {
                return false;
            }
        }

        let item_ok = |same: bool, at_end: bool, dist: f64, req_other: Nm| -> bool {
            let req = if same {
                if !is_excursion || at_end {
                    return true;
                }
                same_net_clr
            } else {
                req_other
            };
            dist >= req as f64 - EPS_NM
        };

        self.track_grid.query(q_bbox, &mut self.candidates);
        for &ti in &self.candidates {
            let Some(o) = (ti != orig_idx).then(|| self.tracks[ti].as_ref()).flatten() else {
                continue;
            };
            if o.layer == layer
                && !item_ok(
                    same_named_net(net, &o.net),
                    false,
                    shape.dist_to_capsule(o.a, o.b, o.width / 2),
                    self.board.pair_clearance(net, &o.net, None),
                )
            {
                return false;
            }
        }

        self.pad_grid.query(q_bbox, &mut self.candidates);
        for &pi in &self.candidates {
            let pad = &self.pads[pi];
            if pad.kind == PadKind::Npth || !pad.layers.iter().any(|l| l == layer) {
                continue;
            }
            let same = same_named_net(net, &pad.net);
            let at_end = same
                && (pad.shape.dist_to_point(span_a) <= TOUCH_TOL_NM
                    || pad.shape.dist_to_point(span_b) <= TOUCH_TOL_NM);
            if !item_ok(
                same,
                at_end,
                pad.shape.dist_to_shape(&shape),
                self.board.pair_clearance(net, &pad.net, None),
            ) {
                return false;
            }
        }

        self.via_grid.query(q_bbox, &mut self.candidates);
        for &vi in &self.candidates {
            let via = &self.vias[vi];
            if !self.board.vias[vi].touches_layer(layer, &self.board.layers) {
                continue;
            }
            let same = same_named_net(net, &via.net);
            let r_via = (via.dia / 2) as f64 + TOUCH_TOL_NM;
            let at_end = same && (via.pos.dist(span_a) <= r_via || via.pos.dist(span_b) <= r_via);
            let req = self
                .board
                .pair_clearance(net, &via.net, self.board.rules.via_to_track_nm);
            if !item_ok(
                same,
                at_end,
                shape.dist_to_circle(via.pos, via.dia / 2),
                req,
            ) {
                return false;
            }
        }

        let hole_clr = self.board.rules.effective_hole_clearance_nm();
        self.drill_grid.query(q_bbox, &mut self.candidates);
        for &di in &self.candidates {
            let d = &self.drills[di];
            if (!d.is_npth && same_named_net(&d.net, net)) || !d.layers.iter().any(|l| l == layer) {
                continue;
            }
            let d_net = if d.is_npth { "" } else { d.net.as_str() };
            let req = self.board.pair_clearance(d_net, net, None).max(hole_clr);
            if shape.dist_to_circle(d.pos, d.radius) < req as f64 - EPS_NM {
                return false;
            }
        }

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
            if pour.indexed.dist_to_shape_bounded(&shape, req) < req as f64 - EPS_NM {
                return false;
            }
        }

        if let Some(m2c) = self.mask_to_copper {
            let masks = match layer {
                "F.Cu" => self.top_masks.as_mut(),
                "B.Cu" => self.bot_masks.as_mut(),
                _ => None,
            };
            if let Some(masks) = masks {
                let mq = seg_bbox.expand(m2c);
                masks.grid.query(mq, &mut self.candidates);
                for &mi in &self.candidates {
                    let (m_net, m_shape, m_bbox) = &masks.items[mi];
                    if !same_named_net(net, m_net)
                        && m_bbox.intersects(mq)
                        && shape_violates_mask(m_shape, &shape, m2c).is_some()
                    {
                        return false;
                    }
                }
            }
        }
        true
    }
}
