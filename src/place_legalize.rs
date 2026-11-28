use std::cmp::Reverse;
use std::collections::{HashMap, HashSet};
use std::time::Instant;

use crate::drc_constraints::{cap_proximity_dist, is_passive_component};
use crate::drc_layout::{keepout_matches_layer, keepout_matches_side};
use crate::drc_scene::{BoardEdgeIndex, KeepoutItem, collect_pads, compute_max_clearance};
use crate::geom::{
    EPS_NM, GridQueryScratch, SpatialGrid, courtyard_world_polygon, local_to_board,
    pad_copper_layers, pad_query_bbox, pad_to_world_shape, point_in_polygon, point_to_segment_dist,
    polygon_signed_area_2, polygon_violates_clearance, polys_overlap_area, remap_layer_for_side,
};
use crate::ir::{
    BBoxNm, BoardIr, FootprintDef, KeepoutKind, Nm, PadKind, Placement, Pt, Side, same_named_net,
};
use crate::place::{
    CapProximityStatus, MovedPart, PlaceOptions, PlaceReport, UnresolvablePlacement,
};
use crate::poly_index::indexed_shape_edge_clearance;
use crate::shapes::ResolvedShape;

struct LocalPadTemplate {
    net: String,
    kind: PadKind,
    layers: Vec<String>,
    drill_nm: Option<Nm>,
    local_center: Pt,
    local_shape: ResolvedShape,
    local_query_bbox: BBoxNm,
}

struct LocalKeepoutTemplate {
    kind: KeepoutKind,
    layers: Vec<String>,
    local_poly: Vec<Pt>,
    local_bbox: BBoxNm,
}

struct RotTemplate {
    rot_deg: f64,
    local_cy: Vec<Pt>,
    local_cy_bbox: BBoxNm,
    local_pads: Vec<LocalPadTemplate>,
    local_pads_bbox: Option<BBoxNm>,
    local_edge_pads_bbox: Option<BBoxNm>,
    local_keepouts: Vec<LocalKeepoutTemplate>,
}

struct PlacedPad {
    net: String,
    kind: PadKind,
    layers: Vec<String>,
    drill_nm: Option<Nm>,
    center: Pt,
    shape: ResolvedShape,
    bbox: BBoxNm,
    query_bbox: BBoxNm,
}

struct PlacedEntry {
    pl_idx: usize,
    refdes: String,
    side: Side,
    cy: Vec<Pt>,
    cy_bbox: BBoxNm,
    pads: Vec<PlacedPad>,
    is_passive: bool,
}

struct BgaShadowEntry {
    refdes: String,
    side: Side,
    min_clr: Nm,
    allow_passives: bool,
    cy: Vec<Pt>,
    cy_bbox: BBoxNm,
}

fn rot_eq(a: f64, b: f64) -> bool {
    let d = (a - b).rem_euclid(360.0);
    d < 1e-6 || (360.0 - d) < 1e-6
}

fn translate_shape(shape: &ResolvedShape, d: Pt) -> ResolvedShape {
    match shape {
        ResolvedShape::Circle { center, radius } => ResolvedShape::Circle {
            center: Pt::new(center.x + d.x, center.y + d.y),
            radius: *radius,
        },
        ResolvedShape::Capsule { a, b, radius } => ResolvedShape::Capsule {
            a: Pt::new(a.x + d.x, a.y + d.y),
            b: Pt::new(b.x + d.x, b.y + d.y),
            radius: *radius,
        },
        ResolvedShape::RoundRect {
            corners,
            radius,
            bbox,
        } => ResolvedShape::RoundRect {
            corners: corners.map(|p| Pt::new(p.x + d.x, p.y + d.y)),
            radius: *radius,
            bbox: translate_bbox(*bbox, d),
        },
        ResolvedShape::Poly { verts, bbox } => ResolvedShape::Poly {
            verts: translate_pts(verts, d),
            bbox: translate_bbox(*bbox, d),
        },
    }
}

fn translate_pts(pts: &[Pt], d: Pt) -> Vec<Pt> {
    pts.iter().map(|p| Pt::new(p.x + d.x, p.y + d.y)).collect()
}

fn translate_bbox(b: BBoxNm, d: Pt) -> BBoxNm {
    BBoxNm::new(b.min_x + d.x, b.min_y + d.y, b.max_x + d.x, b.max_y + d.y)
}

fn build_placed_entry(
    board: &BoardIr,
    pl_idx: usize,
    pl: &Placement,
    cu_layers: &[String],
) -> PlacedEntry {
    let fp = board.footprints.get(&pl.footprint);
    let cy = fp
        .map(|f| courtyard_world_polygon(f, pl))
        .unwrap_or_default();
    let cy_bbox = BBoxNm::from_pts(&cy);
    let pads = fp
        .map(|f| {
            f.pads
                .iter()
                .map(|pad| {
                    let shape = pad_to_world_shape(pad, pl, 16);
                    let bbox = shape.bbox();
                    let center = local_to_board(pad.pos, pl.pos.x, pl.pos.y, pl.rot_deg, pl.side);
                    let query_bbox = pad_query_bbox(bbox, center, pad.drill_nm);
                    PlacedPad {
                        net: pl.pin_nets.get(&pad.name).cloned().unwrap_or_default(),
                        kind: pad.kind,
                        layers: pad_copper_layers(pad, pl.side, cu_layers),
                        drill_nm: pad.drill_nm,
                        center,
                        shape,
                        bbox,
                        query_bbox,
                    }
                })
                .collect()
        })
        .unwrap_or_default();
    PlacedEntry {
        pl_idx,
        refdes: pl.refdes.clone(),
        side: pl.side,
        cy,
        cy_bbox,
        pads,
        is_passive: is_passive_component(board, &pl.refdes, &pl.footprint),
    }
}

fn union_bbox(acc: &mut Option<BBoxNm>, bb: BBoxNm) {
    *acc = Some(acc.map_or(bb, |prev| prev.union(bb)));
}

fn build_rot_templates(
    fp: &FootprintDef,
    pl: &Placement,
    cu_layers: &[String],
    allow_rotation: bool,
) -> Vec<RotTemplate> {
    let norm_rot = pl.rot_deg.rem_euclid(360.0);
    let mut rots = Vec::new();
    if allow_rotation {
        rots.extend([0.0, 90.0, 180.0, 270.0]);
        if !rots.iter().any(|&r| rot_eq(r, norm_rot)) {
            rots.insert(0, norm_rot);
        }
    } else {
        rots.push(norm_rot);
    }
    rots.into_iter()
        .map(|rot_deg| {
            let tmp = Placement {
                pos: Pt::new(0, 0),
                rot_deg,
                side: pl.side,
                ..pl.clone()
            };
            let local_cy = courtyard_world_polygon(fp, &tmp);
            let local_cy_bbox = BBoxNm::from_pts(&local_cy);
            let mut local_pads_bbox = None;
            let mut local_edge_pads_bbox = None;
            let local_pads: Vec<LocalPadTemplate> = fp
                .pads
                .iter()
                .map(|pad| {
                    let layers = pad_copper_layers(pad, pl.side, cu_layers);
                    let local_center = local_to_board(pad.pos, 0, 0, rot_deg, pl.side);
                    let local_shape = pad_to_world_shape(pad, &tmp, 16);
                    let shape_bb = local_shape.bbox();
                    let local_query_bbox = pad_query_bbox(shape_bb, local_center, pad.drill_nm);
                    union_bbox(&mut local_pads_bbox, local_query_bbox);
                    if pad.kind != PadKind::Npth && !layers.is_empty() {
                        union_bbox(&mut local_edge_pads_bbox, shape_bb);
                    }
                    if let Some(d) = pad.drill_nm
                        && d > 0
                    {
                        union_bbox(
                            &mut local_edge_pads_bbox,
                            BBoxNm::from_pt(local_center, d / 2),
                        );
                    }
                    LocalPadTemplate {
                        net: pl.pin_nets.get(&pad.name).cloned().unwrap_or_default(),
                        kind: pad.kind,
                        layers,
                        drill_nm: pad.drill_nm,
                        local_center,
                        local_shape,
                        local_query_bbox,
                    }
                })
                .collect();
            let local_keepouts = fp
                .keepouts
                .iter()
                .filter(|ko| ko.poly.len() >= 3)
                .map(|ko| {
                    let local_poly: Vec<Pt> = ko
                        .poly
                        .iter()
                        .map(|&p| local_to_board(p, 0, 0, rot_deg, pl.side))
                        .collect();
                    let local_bbox = BBoxNm::from_pts(&local_poly);
                    LocalKeepoutTemplate {
                        kind: ko.kind,
                        layers: ko
                            .layers
                            .iter()
                            .map(|l| remap_layer_for_side(l, pl.side))
                            .collect(),
                        local_poly,
                        local_bbox,
                    }
                })
                .collect();
            RotTemplate {
                rot_deg,
                local_cy,
                local_cy_bbox,
                local_pads,
                local_pads_bbox,
                local_edge_pads_bbox,
                local_keepouts,
            }
        })
        .collect()
}

fn append_footprint_keepouts(
    board: &BoardIr,
    pl: &Placement,
    out: &mut Vec<KeepoutItem>,
    grid: &mut SpatialGrid,
) {
    let Some(fp) = board.footprints.get(&pl.footprint) else {
        return;
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
        let bbox = shape.bbox();
        let idx = out.len();
        out.push(KeepoutItem {
            label: format!("{}.keepout#{i}", pl.refdes),
            kind: ko.kind,
            layers: ko
                .layers
                .iter()
                .map(|l| remap_layer_for_side(l, pl.side))
                .collect(),
            poly: world,
            bbox,
            shape,
        });
        grid.insert(idx, bbox);
    }
}

fn collect_static_keepouts(board: &BoardIr) -> (Vec<KeepoutItem>, SpatialGrid) {
    let mut out = Vec::new();
    let mut grid = SpatialGrid::new_dynamic(2_000_000);
    for (i, ko) in board.keepouts.iter().enumerate() {
        if ko.poly.len() < 3 {
            continue;
        }
        let shape = ResolvedShape::polygon(ko.poly.clone());
        let bbox = shape.bbox();
        let idx = out.len();
        out.push(KeepoutItem {
            label: format!("keepout#{i}"),
            kind: ko.kind,
            layers: ko.layers.clone(),
            poly: ko.poly.clone(),
            bbox,
            shape,
        });
        grid.insert(idx, bbox);
    }
    (out, grid)
}

fn build_bga_courtyards(
    board: &BoardIr,
    placed: &[PlacedEntry],
    placed_by_refdes: &HashMap<String, usize>,
) -> (Vec<BgaShadowEntry>, HashMap<String, Vec<usize>>) {
    let mut out = Vec::new();
    let mut by_refdes: HashMap<String, Vec<usize>> = HashMap::new();
    for rule in &board.constraints.bga_shadows {
        let (side, cy, cy_bbox) = if let Some(&pi) = placed_by_refdes.get(&rule.bga_refdes) {
            let entry = &placed[pi];
            (entry.side, entry.cy.clone(), entry.cy_bbox)
        } else if let Some(bga) = board
            .placements
            .iter()
            .find(|p| p.refdes == rule.bga_refdes)
            && let Some(fp) = board.footprints.get(&bga.footprint)
        {
            let cy = courtyard_world_polygon(fp, bga);
            let cy_bbox = BBoxNm::from_pts(&cy);
            (bga.side, cy, cy_bbox)
        } else {
            continue;
        };
        if cy.len() >= 3 {
            let idx = out.len();
            out.push(BgaShadowEntry {
                refdes: rule.bga_refdes.clone(),
                side,
                min_clr: rule.min_clearance_nm,
                allow_passives: rule.allow_passives,
                cy,
                cy_bbox,
            });
            by_refdes
                .entry(rule.bga_refdes.clone())
                .or_default()
                .push(idx);
        }
    }
    (out, by_refdes)
}

fn target_anchors_for_placement(
    board: &BoardIr,
    pl: &Placement,
    placed: &[PlacedEntry],
    placed_by_refdes: &HashMap<String, usize>,
) -> Vec<Pt> {
    let mut rules: Vec<_> = board
        .constraints
        .cap_proximity
        .iter()
        .filter(|r| r.cap_refdes == pl.refdes)
        .collect();
    if rules.is_empty() {
        return vec![pl.pos];
    }
    rules.sort_by_key(|r| r.max_dist_nm);
    let primary = rules[0];
    let mut pts = Vec::new();
    if !primary.net.is_empty() {
        if let Some(&pi) = placed_by_refdes.get(&primary.ic_refdes) {
            for op in &placed[pi].pads {
                if op.net == primary.net {
                    pts.push(op.center);
                }
            }
        } else if let Some(ic_pl) = board
            .placements
            .iter()
            .find(|p| p.refdes == primary.ic_refdes)
            && let Some(ic_fp) = board.footprints.get(&ic_pl.footprint)
        {
            for pad in &ic_fp.pads {
                if ic_pl.pin_nets.get(&pad.name).map(String::as_str) == Some(&primary.net) {
                    pts.push(local_to_board(
                        pad.pos,
                        ic_pl.pos.x,
                        ic_pl.pos.y,
                        ic_pl.rot_deg,
                        ic_pl.side,
                    ));
                }
            }
        }
    }
    if pts.is_empty()
        && let Some(ic_pl) = board
            .placements
            .iter()
            .find(|p| p.refdes == primary.ic_refdes)
    {
        pts.push(ic_pl.pos);
    }
    if pts.is_empty() {
        pts.push(pl.pos);
    }
    pts.sort_by(|a, b| {
        a.dist_sq(pl.pos)
            .total_cmp(&b.dist_sq(pl.pos))
            .then_with(|| (a.x, a.y).cmp(&(b.x, b.y)))
    });
    pts.dedup();
    pts
}

fn build_spiral_offsets(fine_step: Nm, max_radius: Nm) -> Vec<(Nm, Nm, f64)> {
    let fine_step = fine_step.max(50_000);
    let fine_lim = max_radius.min(5_000_000);
    let n_fine = fine_lim / fine_step;
    let mut pts = Vec::new();
    for ix in -n_fine..=n_fine {
        for iy in -n_fine..=n_fine {
            let dx = ix * fine_step;
            let dy = iy * fine_step;
            let d = ((dx as f64).powi(2) + (dy as f64).powi(2)).sqrt();
            if d <= fine_lim as f64 + 1.0 {
                pts.push((dx, dy, d));
            }
        }
    }
    if max_radius > fine_lim {
        let coarse_step = (fine_step * 2).max(400_000);
        let n_coarse = max_radius / coarse_step;
        for ix in -n_coarse..=n_coarse {
            for iy in -n_coarse..=n_coarse {
                let dx = ix * coarse_step;
                let dy = iy * coarse_step;
                let d = ((dx as f64).powi(2) + (dy as f64).powi(2)).sqrt();
                if d > fine_lim as f64 && d <= max_radius as f64 + 1.0 {
                    pts.push((dx, dy, d));
                }
            }
        }
    }
    pts.sort_by(|a, b| {
        a.2.total_cmp(&b.2)
            .then_with(|| (a.0.abs() + a.1.abs()).cmp(&(b.0.abs() + b.1.abs())))
            .then_with(|| (a.0, a.1).cmp(&(b.0, b.1)))
    });
    pts
}

struct PlacedIndex {
    placed: Vec<PlacedEntry>,
    placed_by_refdes: HashMap<String, usize>,
    placed_pads_by_net: HashMap<String, Vec<Pt>>,
    placed_cy_grid: SpatialGrid,
    placed_pad_grid: SpatialGrid,
    placed_pads: Vec<(usize, usize)>,
    keepouts: Vec<KeepoutItem>,
    keepout_grid: SpatialGrid,
    bga_courtyards: Vec<BgaShadowEntry>,
    bga_by_refdes: HashMap<String, Vec<usize>>,
}

impl PlacedIndex {
    fn new(board: &BoardIr) -> Self {
        let (keepouts, keepout_grid) = collect_static_keepouts(board);
        Self {
            placed: Vec::new(),
            placed_by_refdes: HashMap::new(),
            placed_pads_by_net: HashMap::new(),
            placed_cy_grid: SpatialGrid::new_dynamic(2_000_000),
            placed_pad_grid: SpatialGrid::new_dynamic(2_000_000),
            placed_pads: Vec::new(),
            keepouts,
            keepout_grid,
            bga_courtyards: Vec::new(),
            bga_by_refdes: HashMap::new(),
        }
    }

    fn init_bga_courtyards(&mut self, board: &BoardIr) {
        let (courtyards, by_refdes) =
            build_bga_courtyards(board, &self.placed, &self.placed_by_refdes);
        self.bga_courtyards = courtyards;
        self.bga_by_refdes = by_refdes;
    }

    fn record(&mut self, board: &BoardIr, pl_idx: usize, pl: &Placement, cu_layers: &[String]) {
        let entry = build_placed_entry(board, pl_idx, pl, cu_layers);
        let pi = self.placed.len();
        self.placed_by_refdes.insert(entry.refdes.clone(), pi);
        if entry.cy.len() >= 3 {
            self.placed_cy_grid.insert(pi, entry.cy_bbox);
        }
        if let Some(idxs) = self.bga_by_refdes.get(&entry.refdes) {
            for &bi in idxs {
                self.bga_courtyards[bi].cy = entry.cy.clone();
                self.bga_courtyards[bi].cy_bbox = entry.cy_bbox;
            }
        }
        for (pad_i, pad) in entry.pads.iter().enumerate() {
            let pp_idx = self.placed_pads.len();
            self.placed_pads.push((pi, pad_i));
            self.placed_pad_grid.insert(pp_idx, pad.query_bbox);
            if !pad.net.is_empty() {
                self.placed_pads_by_net
                    .entry(pad.net.clone())
                    .or_default()
                    .push(pad.center);
            }
        }
        self.placed.push(entry);
        append_footprint_keepouts(board, pl, &mut self.keepouts, &mut self.keepout_grid);
    }
}

struct LegalizeCtx<'a> {
    board: &'a BoardIr,
    index: &'a PlacedIndex,
    edge_idx: Option<&'a BoardEdgeIndex>,
    track_grid: &'a SpatialGrid,
    track_bboxes: &'a [BBoxNm],
    via_grid: &'a SpatialGrid,
    via_bboxes: &'a [BBoxNm],
    max_clr: Nm,
    scratch: &'a mut GridQueryScratch,
    cands: &'a mut Vec<usize>,
}

impl LegalizeCtx<'_> {
    fn is_candidate_legal(
        &mut self,
        pl: &Placement,
        is_passive: bool,
        pos: Pt,
        tmpl: &RotTemplate,
    ) -> bool {
        let cy_bb = translate_bbox(tmpl.local_cy_bbox, pos);
        let cy_clr = self.board.rules.courtyard_clearance_nm;
        let has_cy = tmpl.local_cy.len() >= 3;

        if has_cy && let Some(edge) = self.edge_idx {
            let ob = edge.outline.bbox;
            if !pl.allow_edge_overhang {
                if cy_bb.min_x < ob.min_x + cy_clr - 1
                    || cy_bb.max_x > ob.max_x - cy_clr + 1
                    || cy_bb.min_y < ob.min_y + cy_clr - 1
                    || cy_bb.max_y > ob.max_y - cy_clr + 1
                {
                    return false;
                }
            } else if !cy_bb.intersects(ob) {
                return false;
            }
        }

        let edge_clr = self.board.rules.edge_clearance_nm;
        if let (Some(edge), Some(lepb)) = (self.edge_idx, tmpl.local_edge_pads_bbox) {
            let ob = edge.outline.bbox;
            let ep_bb = translate_bbox(lepb, pos);
            if ep_bb.min_x < ob.min_x + edge_clr - 1
                || ep_bb.max_x > ob.max_x - edge_clr + 1
                || ep_bb.min_y < ob.min_y + edge_clr - 1
                || ep_bb.max_y > ob.max_y - edge_clr + 1
            {
                return false;
            }
        }

        let mut cya_buf: Option<Vec<Pt>> = None;
        fn resolve_cya<'b>(buf: &'b mut Option<Vec<Pt>>, local_cy: &[Pt], pos: Pt) -> &'b [Pt] {
            buf.get_or_insert_with(|| translate_pts(local_cy, pos))
                .as_slice()
        }

        // Check nearby courtyards, placement keepouts, BGA shadows, and courtyard edges before translating pads.
        if has_cy {
            let cy_q = cy_bb.expand(cy_clr);
            self.index
                .placed_cy_grid
                .query_with(cy_q, self.scratch, self.cands);
            for &pi in self.cands.iter() {
                let other = &self.index.placed[pi];
                if other.side == pl.side && cy_q.intersects(other.cy_bbox) {
                    let cya = resolve_cya(&mut cya_buf, &tmpl.local_cy, pos);
                    if polygon_violates_clearance(cya, &other.cy, cy_clr) {
                        return false;
                    }
                }
            }

            self.index
                .keepout_grid
                .query_with(cy_bb, self.scratch, self.cands);
            for &ki in self.cands.iter() {
                let ko = &self.index.keepouts[ki];
                if ko.kind.blocks_placement()
                    && keepout_matches_side(&ko.layers, pl.side)
                    && cy_bb.intersects(ko.bbox)
                    && !ko
                        .label
                        .strip_prefix(&pl.refdes)
                        .is_some_and(|rest| rest.starts_with('.'))
                {
                    let cya = resolve_cya(&mut cya_buf, &tmpl.local_cy, pos);
                    if polys_overlap_area(cya, &ko.poly) > 0.0 {
                        return false;
                    }
                }
            }
        }

        for bga in &self.index.bga_courtyards {
            if bga.refdes == pl.refdes {
                if !has_cy {
                    continue;
                }
                let bga_q = cy_bb.expand(bga.min_clr);
                self.index
                    .placed_cy_grid
                    .query_with(bga_q, self.scratch, self.cands);
                for &pi in self.cands.iter() {
                    let other = &self.index.placed[pi];
                    if other.side == pl.side
                        || (bga.allow_passives && other.is_passive)
                        || !bga_q.intersects(other.cy_bbox)
                    {
                        continue;
                    }
                    let cya = resolve_cya(&mut cya_buf, &tmpl.local_cy, pos);
                    if polygon_violates_clearance(cya, &other.cy, bga.min_clr) {
                        return false;
                    }
                }
                continue;
            }
            if !has_cy || bga.side == pl.side || (bga.allow_passives && is_passive) {
                continue;
            }
            if cy_bb.expand(bga.min_clr).intersects(bga.cy_bbox) {
                let cya = resolve_cya(&mut cya_buf, &tmpl.local_cy, pos);
                if polygon_violates_clearance(cya, &bga.cy, bga.min_clr) {
                    return false;
                }
            }
        }

        if has_cy && let Some(edge) = self.edge_idx {
            let cya = resolve_cya(&mut cya_buf, &tmpl.local_cy, pos);
            if !pl.allow_edge_overhang {
                let cy_shape = ResolvedShape::Poly {
                    verts: cya.to_vec(),
                    bbox: cy_bb,
                };
                let (dist, off_board) =
                    indexed_shape_edge_clearance(&cy_shape, &edge.outline, &edge.cutouts, cy_clr);
                if off_board || (cy_clr > 0 && dist < cy_clr as f64 - EPS_NM) {
                    return false;
                }
            } else if polys_overlap_area(cya, &self.board.outline) <= 0.0
                || self
                    .board
                    .cutouts
                    .iter()
                    .any(|c| c.len() >= 3 && polygon_violates_clearance(cya, c, cy_clr))
            {
                return false;
            }
        }

        let hole_clr = self.board.rules.effective_hole_clearance_nm();
        let h2h = self.board.rules.hole_to_hole_nm;
        let world_pads: Vec<(ResolvedShape, BBoxNm, Pt, &LocalPadTemplate)> = tmpl
            .local_pads
            .iter()
            .map(|lp| {
                let ws = translate_shape(&lp.local_shape, pos);
                let wb = translate_bbox(lp.local_query_bbox, pos);
                let wc = Pt::new(lp.local_center.x + pos.x, lp.local_center.y + pos.y);
                (ws, wb, wc, lp)
            })
            .collect();

        if let Some(edge) = self.edge_idx {
            for (ws, _, wc, lp) in &world_pads {
                if lp.kind != PadKind::Npth && !lp.layers.is_empty() {
                    let (dist, off_board) =
                        indexed_shape_edge_clearance(ws, &edge.outline, &edge.cutouts, edge_clr);
                    if off_board || dist < edge_clr as f64 - EPS_NM {
                        return false;
                    }
                }
                if let Some(d) = lp.drill_nm
                    && d > 0
                {
                    let drill_shape = ResolvedShape::Circle {
                        center: *wc,
                        radius: d / 2,
                    };
                    let (dist, off_board) = indexed_shape_edge_clearance(
                        &drill_shape,
                        &edge.outline,
                        &edge.cutouts,
                        edge_clr,
                    );
                    if off_board || dist < edge_clr as f64 - EPS_NM {
                        return false;
                    }
                }
            }
        }

        if let Some(lpb) = tmpl.local_pads_bbox {
            let pads_bb = translate_bbox(lpb, pos);
            self.index
                .keepout_grid
                .query_with(pads_bb, self.scratch, self.cands);
            for &ki in self.cands.iter() {
                let ko = &self.index.keepouts[ki];
                if !ko.kind.blocks_pads()
                    || !pads_bb.intersects(ko.bbox)
                    || ko
                        .label
                        .strip_prefix(&pl.refdes)
                        .is_some_and(|rest| rest.starts_with('.'))
                {
                    continue;
                }
                for (ws, wb, wc, lp) in &world_pads {
                    if lp.kind == PadKind::Npth || !wb.intersects(ko.bbox) {
                        continue;
                    }
                    if !lp
                        .layers
                        .iter()
                        .any(|l| keepout_matches_layer(&ko.layers, l))
                    {
                        continue;
                    }
                    let overlaps = match ws {
                        ResolvedShape::Poly { verts, .. } => {
                            polys_overlap_area(verts, &ko.poly) > 0.0
                        }
                        _ => {
                            point_in_polygon(*wc, &ko.poly) || ws.dist_to_shape(&ko.shape) < -EPS_NM
                        }
                    };
                    if overlaps {
                        return false;
                    }
                }
            }
        }

        // Check candidate's own footprint keepouts against nearby already-placed components.
        for own_ko in &tmpl.local_keepouts {
            let ko_poly = translate_pts(&own_ko.local_poly, pos);
            let ko_bb = translate_bbox(own_ko.local_bbox, pos);
            if own_ko.kind.blocks_placement() {
                self.index
                    .placed_cy_grid
                    .query_with(ko_bb, self.scratch, self.cands);
                for &pi in self.cands.iter() {
                    let other = &self.index.placed[pi];
                    if keepout_matches_side(&own_ko.layers, other.side)
                        && ko_bb.intersects(other.cy_bbox)
                        && polys_overlap_area(&ko_poly, &other.cy) > 0.0
                    {
                        return false;
                    }
                }
            }
            if own_ko.kind.blocks_pads() {
                let ko_shape = ResolvedShape::Poly {
                    verts: ko_poly.clone(),
                    bbox: ko_bb,
                };
                self.index
                    .placed_pad_grid
                    .query_with(ko_bb, self.scratch, self.cands);
                for &pp_idx in self.cands.iter() {
                    let (pi, pad_i) = self.index.placed_pads[pp_idx];
                    let op = &self.index.placed[pi].pads[pad_i];
                    if op.kind == PadKind::Npth
                        || !op.bbox.intersects(ko_bb)
                        || !op
                            .layers
                            .iter()
                            .any(|l| keepout_matches_layer(&own_ko.layers, l))
                    {
                        continue;
                    }
                    let overlaps = match &op.shape {
                        ResolvedShape::Poly { verts, .. } => {
                            polys_overlap_area(verts, &ko_poly) > 0.0
                        }
                        _ => {
                            point_in_polygon(op.center, &ko_poly)
                                || op.shape.dist_to_shape(&ko_shape) < -EPS_NM
                        }
                    };
                    if overlaps {
                        return false;
                    }
                }
            }
        }

        let board = self.board;
        let max_clr = self.max_clr;
        let check_pad_pair = |ws: &ResolvedShape,
                              wb: BBoxNm,
                              wc: Pt,
                              lp: &LocalPadTemplate,
                              op: &PlacedPad|
         -> bool {
            if !wb.expand(max_clr).intersects(op.query_bbox) {
                return true;
            }
            if let (Some(d1), Some(d2)) = (lp.drill_nm, op.drill_nm)
                && d1 > 0
                && d2 > 0
                && wc.dist(op.center) - ((d1 + d2) as f64) * 0.5 < h2h as f64 - EPS_NM
            {
                return false;
            }
            if lp.kind == PadKind::Npth {
                if let Some(d1) = lp.drill_nm
                    && d1 > 0
                    && op.kind != PadKind::Npth
                    && !op.layers.is_empty()
                {
                    let req = board.pair_clearance("", &op.net, None).max(hole_clr);
                    if op.shape.dist_to_circle(wc, d1 / 2) < req as f64 - EPS_NM {
                        return false;
                    }
                }
                return true;
            }
            if op.kind == PadKind::Npth {
                if let Some(d2) = op.drill_nm
                    && d2 > 0
                    && !lp.layers.is_empty()
                {
                    let req = board.pair_clearance(&lp.net, "", None).max(hole_clr);
                    if ws.dist_to_circle(op.center, d2 / 2) < req as f64 - EPS_NM {
                        return false;
                    }
                }
                return true;
            }
            if same_named_net(&lp.net, &op.net) {
                return true;
            }
            if let Some(d1) = lp.drill_nm
                && d1 > 0
                && !op.layers.is_empty()
            {
                let req = board.pair_clearance(&lp.net, &op.net, None).max(hole_clr);
                if op.shape.dist_to_circle(wc, d1 / 2) < req as f64 - EPS_NM {
                    return false;
                }
            }
            if let Some(d2) = op.drill_nm
                && d2 > 0
                && !lp.layers.is_empty()
            {
                let req = board.pair_clearance(&lp.net, &op.net, None).max(hole_clr);
                if ws.dist_to_circle(op.center, d2 / 2) < req as f64 - EPS_NM {
                    return false;
                }
            }
            if !lp.layers.iter().any(|l| op.layers.contains(l)) {
                return true;
            }
            let rule_ov = (lp.kind == PadKind::Smd && op.kind == PadKind::Smd)
                .then_some(board.rules.smd_to_smd_nm)
                .flatten();
            let req = board.pair_clearance(&lp.net, &op.net, rule_ov);
            !(wb.expand(req).intersects(op.bbox)
                && ws.dist_to_shape(&op.shape) < req as f64 - EPS_NM)
        };

        if let Some(lpb) = tmpl.local_pads_bbox {
            if world_pads.len() <= 4 {
                let q = translate_bbox(lpb, pos).expand(max_clr);
                self.index
                    .placed_pad_grid
                    .query_with(q, self.scratch, self.cands);
                for &pp_idx in self.cands.iter() {
                    let (pi, pad_i) = self.index.placed_pads[pp_idx];
                    let op = &self.index.placed[pi].pads[pad_i];
                    if !q.intersects(op.query_bbox) {
                        continue;
                    }
                    for (ws, wb, wc, lp) in &world_pads {
                        if !check_pad_pair(ws, *wb, *wc, lp, op) {
                            return false;
                        }
                    }
                }
            } else {
                for (ws, wb, wc, lp) in &world_pads {
                    let q = wb.expand(max_clr);
                    self.index
                        .placed_pad_grid
                        .query_with(q, self.scratch, self.cands);
                    for &pp_idx in self.cands.iter() {
                        let (pi, pad_i) = self.index.placed_pads[pp_idx];
                        let op = &self.index.placed[pi].pads[pad_i];
                        if !check_pad_pair(ws, *wb, *wc, lp, op) {
                            return false;
                        }
                    }
                }
            }
        }

        // Check candidate pads and drills against pre-existing tracks and vias.
        if !self.board.tracks.is_empty() || !self.board.vias.is_empty() {
            for (ws, wb, wc, lp) in &world_pads {
                let q = wb.expand(self.max_clr);
                if !self.board.tracks.is_empty() {
                    self.track_grid.query_with(q, self.scratch, self.cands);
                    for &ti in self.cands.iter() {
                        if !q.intersects(self.track_bboxes[ti]) {
                            continue;
                        }
                        let t = &self.board.tracks[ti];
                        if let Some(d1) = lp.drill_nm
                            && d1 > 0
                            && (lp.kind == PadKind::Npth || !same_named_net(&lp.net, &t.net))
                        {
                            let d_net = if lp.kind == PadKind::Npth {
                                ""
                            } else {
                                &lp.net
                            };
                            let req = self.board.pair_clearance(d_net, &t.net, None).max(hole_clr);
                            let d = point_to_segment_dist(*wc, t.a, t.b)
                                - (d1 / 2 + t.width / 2) as f64;
                            if d < req as f64 - EPS_NM {
                                return false;
                            }
                        }
                        if lp.kind == PadKind::Npth
                            || same_named_net(&lp.net, &t.net)
                            || !lp.layers.iter().any(|l| l == &t.layer)
                        {
                            continue;
                        }
                        let req = self.board.pair_clearance(&lp.net, &t.net, None);
                        if ws.dist_to_capsule(t.a, t.b, t.width / 2) < req as f64 - EPS_NM {
                            return false;
                        }
                    }
                }
                if !self.board.vias.is_empty() {
                    self.via_grid.query_with(q, self.scratch, self.cands);
                    for &vi in self.cands.iter() {
                        if !q.intersects(self.via_bboxes[vi]) {
                            continue;
                        }
                        let v = &self.board.vias[vi];
                        if let Some(d1) = lp.drill_nm
                            && d1 > 0
                        {
                            if wc.dist(v.pos) - ((d1 + v.drill) as f64) * 0.5 < h2h as f64 - EPS_NM
                            {
                                return false;
                            }
                            if lp.kind == PadKind::Npth || !same_named_net(&lp.net, &v.net) {
                                let d_net = if lp.kind == PadKind::Npth {
                                    ""
                                } else {
                                    &lp.net
                                };
                                let req =
                                    self.board.pair_clearance(d_net, &v.net, None).max(hole_clr);
                                if wc.dist(v.pos) - ((d1 + v.diameter) as f64) * 0.5
                                    < req as f64 - EPS_NM
                                {
                                    return false;
                                }
                            }
                        }
                        if lp.kind == PadKind::Npth
                            || same_named_net(&lp.net, &v.net)
                            || !lp
                                .layers
                                .iter()
                                .any(|l| v.touches_layer(l, &self.board.layers))
                        {
                            continue;
                        }
                        let rule_ov = (lp.kind == PadKind::Smd)
                            .then_some(self.board.rules.via_to_smd_nm)
                            .flatten();
                        let req = self.board.pair_clearance(&lp.net, &v.net, rule_ov);
                        if wb.expand(req).intersects(self.via_bboxes[vi])
                            && ws.dist_to_circle(v.pos, v.diameter / 2) < req as f64 - EPS_NM
                        {
                            return false;
                        }
                    }
                }
            }
        }

        true
    }

    fn cap_rule_dist_at_candidate(
        &self,
        cap_pos: Pt,
        cap_pads: &[LocalPadTemplate],
        ic_refdes: &str,
        rule_net: &str,
    ) -> f64 {
        let ic_entry = self
            .index
            .placed_by_refdes
            .get(ic_refdes)
            .map(|&i| &self.index.placed[i]);
        let ic_pos = ic_entry
            .and_then(|e| self.board.placements.get(e.pl_idx).map(|p| p.pos))
            .or_else(|| {
                self.board
                    .placements
                    .iter()
                    .find(|p| p.refdes == ic_refdes)
                    .map(|p| p.pos)
            });
        let Some(ic_center) = ic_pos else {
            return 0.0;
        };
        let cap_pts: Vec<Pt> = if !rule_net.is_empty() {
            let v: Vec<Pt> = cap_pads
                .iter()
                .filter(|p| p.net == rule_net)
                .map(|p| Pt::new(cap_pos.x + p.local_center.x, cap_pos.y + p.local_center.y))
                .collect();
            if v.is_empty() { vec![cap_pos] } else { v }
        } else {
            vec![cap_pos]
        };
        let ic_pts: Vec<Pt> = if !rule_net.is_empty() {
            if let Some(ic) = ic_entry {
                let v: Vec<Pt> = ic
                    .pads
                    .iter()
                    .filter(|p| p.net == rule_net)
                    .map(|p| p.center)
                    .collect();
                if v.is_empty() { vec![ic_center] } else { v }
            } else {
                vec![ic_center]
            }
        } else {
            vec![ic_center]
        };
        let mut min_dist = f64::INFINITY;
        for &b in &ic_pts {
            min_dist = min_dist.min(cap_pos.dist(b));
            for &a in &cap_pts {
                min_dist = min_dist.min(a.dist(b));
            }
        }
        min_dist
    }

    fn ratsnest_wirelength(&self, pos: Pt, tmpl: &RotTemplate) -> f64 {
        let mut sum = 0.0;
        for lp in &tmpl.local_pads {
            if lp.net.is_empty() {
                continue;
            }
            let Some(other_pts) = self.index.placed_pads_by_net.get(&lp.net) else {
                continue;
            };
            let wp = Pt::new(pos.x + lp.local_center.x, pos.y + lp.local_center.y);
            let mut best = f64::INFINITY;
            for &op_center in other_pts {
                best = best.min(wp.dist(op_center));
            }
            if best.is_finite() {
                sum += best;
            }
        }
        sum
    }
}

pub(crate) fn legalize_placement(
    board: &mut BoardIr,
    cfg: &PlaceOptions,
) -> Result<PlaceReport, String> {
    let start = Instant::now();
    if board.outline.len() < 3 {
        return Err("board outline must have at least 3 vertices".into());
    }
    let cu_layers = board.copper_layer_names();

    let mut index = PlacedIndex::new(board);
    let mut movable_indices = Vec::new();
    for (idx, pl) in board.placements.iter().enumerate() {
        if pl.locked {
            index.record(board, idx, pl, &cu_layers);
        } else {
            movable_indices.push(idx);
        }
    }
    let fixed_count = index.placed.len();

    // Place movable cap_proximity target ICs first, then constrained caps (tightest first), then remaining parts by area.
    let cap_ic_targets: HashSet<&str> = board
        .constraints
        .cap_proximity
        .iter()
        .map(|r| r.ic_refdes.as_str())
        .collect();
    movable_indices.sort_by_cached_key(|&idx| {
        let pl = &board.placements[idx];
        let is_ic_target = cap_ic_targets.contains(pl.refdes.as_str());
        let tightest = board
            .constraints
            .cap_proximity
            .iter()
            .filter(|r| r.cap_refdes == pl.refdes)
            .map(|r| r.max_dist_nm)
            .min();
        let tier = if is_ic_target {
            0u8
        } else if tightest.is_some() {
            1u8
        } else {
            2u8
        };
        let area2 = board
            .footprints
            .get(&pl.footprint)
            .map(|fp| polygon_signed_area_2(&courtyard_world_polygon(fp, pl)).abs())
            .unwrap_or(0);
        (
            tier,
            tightest.unwrap_or(Nm::MAX),
            Reverse(area2),
            pl.refdes.clone(),
            idx,
        )
    });

    let edge_idx = BoardEdgeIndex::new(board);
    let track_bboxes: Vec<BBoxNm> = board
        .tracks
        .iter()
        .map(|t| BBoxNm::from_pts(&[t.a, t.b]).expand(t.width / 2))
        .collect();
    let track_grid = SpatialGrid::new(track_bboxes.iter().copied(), 2_000_000);
    let via_bboxes: Vec<BBoxNm> = board
        .vias
        .iter()
        .map(|v| BBoxNm::from_pt(v.pos, v.diameter / 2))
        .collect();
    let via_grid = SpatialGrid::new(via_bboxes.iter().copied(), 2_000_000);
    let max_clr = compute_max_clearance(board, &[]);
    index.init_bga_courtyards(board);
    let board_bb = BBoxNm::from_pts(&board.outline);
    let board_diag = Pt::new(board_bb.min_x, board_bb.min_y)
        .dist(Pt::new(board_bb.max_x, board_bb.max_y))
        .ceil() as Nm;
    let search_radius = cfg.max_search_radius_nm.max(board_diag.min(25_000_000));
    let offsets = build_spiral_offsets(cfg.grid_step_nm, search_radius);

    let mut moved_parts = Vec::new();
    let mut unresolvable = Vec::new();
    let mut scratch = GridQueryScratch::default();
    let mut cands = Vec::new();

    for idx in movable_indices {
        let pl = board.placements[idx].clone();
        let Some(fp) = board.footprints.get(&pl.footprint).cloned() else {
            unresolvable.push(UnresolvablePlacement {
                refdes: pl.refdes.clone(),
                reason: format!("missing footprint '{}'", pl.footprint),
            });
            continue;
        };

        let templates = build_rot_templates(&fp, &pl, &cu_layers, cfg.allow_rotation);
        let is_passive = is_passive_component(board, &pl.refdes, &pl.footprint);
        let cap_rules: Vec<_> = board
            .constraints
            .cap_proximity
            .iter()
            .filter(|r| r.cap_refdes == pl.refdes)
            .cloned()
            .collect();
        let has_cap_rules = !cap_rules.is_empty();

        let mut ctx = LegalizeCtx {
            board,
            index: &index,
            edge_idx: edge_idx.as_ref(),
            track_grid: &track_grid,
            track_bboxes: &track_bboxes,
            via_grid: &via_grid,
            via_bboxes: &via_bboxes,
            max_clr,
            scratch: &mut scratch,
            cands: &mut cands,
        };

        let seed_tmpl_idx = templates
            .iter()
            .position(|t| rot_eq(t.rot_deg, pl.rot_deg))
            .unwrap_or(0);
        let seed_legal = ctx.is_candidate_legal(&pl, is_passive, pl.pos, &templates[seed_tmpl_idx]);

        if !has_cap_rules && seed_legal {
            index.record(board, idx, &pl, &cu_layers);
            continue;
        }

        let anchors = target_anchors_for_placement(
            board,
            &pl,
            &ctx.index.placed,
            &ctx.index.placed_by_refdes,
        );
        let mut best: Option<(bool, f64, Pt, f64)> = None;

        let mut eval_candidate =
            |pos: Pt, tmpl: &RotTemplate, best: &mut Option<(bool, f64, Pt, f64)>| {
                if !ctx.is_candidate_legal(&pl, is_passive, pos, tmpl) {
                    return;
                }
                let mut cap_violated = false;
                let mut target_sum = 0.0;
                for r in &cap_rules {
                    let d =
                        ctx.cap_rule_dist_at_candidate(pos, &tmpl.local_pads, &r.ic_refdes, &r.net);
                    if d > r.max_dist_nm as f64 + EPS_NM {
                        cap_violated = true;
                    }
                    target_sum += d;
                }
                let ratsnest = ctx.ratsnest_wirelength(pos, tmpl);
                let disp = pos.dist(pl.pos);
                let rot_pen = if rot_eq(tmpl.rot_deg, pl.rot_deg) {
                    0.0
                } else {
                    10_000.0
                };
                let score = if has_cap_rules {
                    target_sum + 0.5 * ratsnest + 0.1 * disp + rot_pen
                } else {
                    disp + 0.25 * ratsnest + rot_pen
                };
                let replace = match best {
                    None => true,
                    Some((b_viol, b_score, b_pos, b_rot)) => {
                        (cap_violated, score, pos.x, pos.y, tmpl.rot_deg)
                            < (*b_viol, *b_score, b_pos.x, b_pos.y, *b_rot)
                    }
                };
                if replace {
                    *best = Some((cap_violated, score, pos, tmpl.rot_deg));
                }
            };

        for tmpl in &templates {
            eval_candidate(pl.pos, tmpl, &mut best);
        }

        let mut search_centers = anchors;
        if has_cap_rules && !search_centers.contains(&pl.pos) {
            search_centers.push(pl.pos);
        }

        for &center in &search_centers {
            let mut found_satisfied_r: Option<f64> = None;
            let mut found_any_r: Option<f64> = None;
            for &(dx, dy, r_dist) in &offsets {
                if let Some(r0) = found_satisfied_r
                    && r_dist > r0 + 1_200_000.0
                {
                    break;
                }
                if !has_cap_rules
                    && let Some(r0) = found_any_r
                    && r_dist > r0 + 1_200_000.0
                {
                    break;
                }
                let cand = Pt::new(center.x + dx, center.y + dy);
                for tmpl in &templates {
                    eval_candidate(cand, tmpl, &mut best);
                }
                if let Some((viol, ..)) = best {
                    if !viol && found_satisfied_r.is_none() {
                        found_satisfied_r = Some(r_dist);
                    }
                    if found_any_r.is_none() {
                        found_any_r = Some(r_dist);
                    }
                }
            }
            if best.as_ref().is_some_and(|(viol, ..)| !*viol) {
                break;
            }
        }

        if let Some((_, _, new_pos, new_rot)) = best {
            let disp = pl.pos.dist(new_pos).round() as Nm;
            if new_pos != pl.pos || !rot_eq(new_rot, pl.rot_deg) {
                moved_parts.push(MovedPart {
                    refdes: pl.refdes.clone(),
                    from_pos: pl.pos,
                    to_pos: new_pos,
                    from_rot_deg: pl.rot_deg,
                    to_rot_deg: new_rot,
                    displacement_nm: disp,
                });
                board.placements[idx].pos = new_pos;
                board.placements[idx].rot_deg = new_rot;
            }
            let updated = board.placements[idx].clone();
            index.record(board, idx, &updated, &cu_layers);
        } else {
            unresolvable.push(UnresolvablePlacement {
                refdes: pl.refdes.clone(),
                reason: "no legal non-overlapping placement found within search radius".into(),
            });
            index.record(board, idx, &pl, &cu_layers);
        }
    }

    let final_pads = collect_pads(board, &cu_layers);
    let mut cap_proximity = Vec::with_capacity(board.constraints.cap_proximity.len());
    for rule in &board.constraints.cap_proximity {
        let Some((_, _, min_dist)) = cap_proximity_dist(board, &final_pads, rule) else {
            continue;
        };
        let final_dist_nm = min_dist.round() as Nm;
        let satisfied = min_dist <= rule.max_dist_nm as f64 + EPS_NM;
        if !satisfied && !unresolvable.iter().any(|u| u.refdes == rule.cap_refdes) {
            unresolvable.push(UnresolvablePlacement {
                refdes: rule.cap_refdes.clone(),
                reason: format!(
                    "cap_proximity to {} ({final_dist_nm} nm > max {} nm)",
                    rule.ic_refdes, rule.max_dist_nm
                ),
            });
        }
        cap_proximity.push(CapProximityStatus {
            cap_refdes: rule.cap_refdes.clone(),
            ic_refdes: rule.ic_refdes.clone(),
            net: rule.net.clone(),
            final_dist_nm,
            max_dist_nm: rule.max_dist_nm,
            satisfied,
        });
    }

    let ok = unresolvable.is_empty() && cap_proximity.iter().all(|c| c.satisfied);
    Ok(PlaceReport {
        total_placements: board.placements.len(),
        fixed_count,
        moved_parts,
        cap_proximity,
        unresolvable,
        ok,
        elapsed_ms: start.elapsed().as_millis(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::drc::check_board_drc;
    use crate::ir::{BgaShadowRule, BoardSpec, PadDef, PadShape, mm};

    #[test]
    fn placed_index_dynamic_grid_oversized_drill_and_bga_shadows() {
        let spec: BoardSpec = toml::from_str(
            r#"
name = "place_legalize_unit"
size_nm = [30000000, 30000000]
[[layers]]
name = "F.Cu"
kind = "signal"
side = "top"
[[layers]]
name = "B.Cu"
kind = "signal"
side = "bottom"
[rules]
clearance_nm = 150000
hole_to_hole_nm = 250000
edge_clearance_nm = 300000
courtyard_clearance_nm = 150000
"#,
        )
        .unwrap();
        let mut board = BoardIr::from_spec(&spec).unwrap();
        board.footprints.insert(
            "NPTH_BIG".into(),
            FootprintDef {
                name: "NPTH_BIG".into(),
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
                courtyard: vec![
                    Pt::new(-100_000, -100_000),
                    Pt::new(100_000, -100_000),
                    Pt::new(100_000, 100_000),
                    Pt::new(-100_000, 100_000),
                ],
                graphics: Vec::new(),
                keepouts: Vec::new(),
                models: Vec::new(),
                pin_types: Default::default(),
            },
        );
        board.footprints.insert(
            "CHIP".into(),
            FootprintDef {
                name: "CHIP".into(),
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
                courtyard: vec![
                    Pt::new(-400_000, -400_000),
                    Pt::new(400_000, -400_000),
                    Pt::new(400_000, 400_000),
                    Pt::new(-400_000, 400_000),
                ],
                graphics: Vec::new(),
                keepouts: Vec::new(),
                models: Vec::new(),
                pin_types: Default::default(),
            },
        );
        board.footprints.insert(
            "BGA".into(),
            FootprintDef {
                name: "BGA".into(),
                pads: vec![PadDef {
                    name: "A1".into(),
                    kind: PadKind::Smd,
                    shape: PadShape::Circle { diameter: 300_000 },
                    pos: Pt::new(0, 0),
                    rot_deg: 0.0,
                    drill_nm: None,
                    layers: vec!["F.Cu".into()],
                    mask_margin_nm: None,
                    paste_margin_nm: None,
                }],
                courtyard: vec![
                    Pt::new(-2_000_000, -2_000_000),
                    Pt::new(2_000_000, -2_000_000),
                    Pt::new(2_000_000, 2_000_000),
                    Pt::new(-2_000_000, 2_000_000),
                ],
                graphics: Vec::new(),
                keepouts: Vec::new(),
                models: Vec::new(),
                pin_types: Default::default(),
            },
        );
        board.placements = vec![
            Placement {
                refdes: "H1".into(),
                footprint: "NPTH_BIG".into(),
                pos: Pt::new(mm(8.0), mm(15.0)),
                side: Side::Top,
                locked: true,
                ..Default::default()
            },
            Placement {
                refdes: "C1".into(),
                footprint: "CHIP".into(),
                pos: Pt::new(mm(9.3), mm(15.0)),
                side: Side::Top,
                locked: false,
                pin_nets: [("1".into(), "VCC".into())].into_iter().collect(),
                ..Default::default()
            },
            Placement {
                refdes: "U1".into(),
                footprint: "BGA".into(),
                pos: Pt::new(mm(20.0), mm(15.0)),
                side: Side::Top,
                locked: false,
                ..Default::default()
            },
            Placement {
                refdes: "U2".into(),
                footprint: "CHIP".into(),
                pos: Pt::new(mm(20.0), mm(15.0)),
                side: Side::Bottom,
                locked: false,
                ..Default::default()
            },
        ];
        board.constraints.bga_shadows.push(BgaShadowRule {
            bga_refdes: "U1".into(),
            min_clearance_nm: mm(0.5),
            allow_passives: false,
        });

        let rep = legalize_placement(&mut board, &PlaceOptions::default()).unwrap();
        assert!(rep.ok, "unexpected unresolvable: {:?}", rep.unresolvable);
        assert!(rep.moved_parts.iter().any(|m| m.refdes == "C1"));
        assert!(rep.moved_parts.iter().any(|m| m.refdes == "U2"));
        let drc = check_board_drc(&board);
        assert!(drc.passed, "DRC errors: {:?}", drc.violations);
    }
}
