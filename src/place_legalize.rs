use std::cmp::Reverse;
use std::collections::HashSet;
use std::time::Instant;

use crate::drc_constraints::{cap_proximity_dist, is_passive_component};
use crate::drc_layout::{keepout_matches_layer, keepout_matches_side};
use crate::drc_scene::{KeepoutItem, collect_pads};
use crate::geom::{
    EPS_NM, courtyard_world_polygon, local_to_board, pad_copper_layers, pad_to_world_shape,
    point_in_polygon, point_to_segment_dist, polygon_signed_area_2, polygon_violates_clearance,
    polys_overlap_area, remap_layer_for_side, shape_edge_clearance,
};
use crate::ir::{
    BBoxNm, BoardIr, FootprintDef, KeepoutKind, Nm, PadKind, Placement, Pt, Side, same_named_net,
};
use crate::place::{
    CapProximityStatus, MovedPart, PlaceOptions, PlaceReport, UnresolvablePlacement,
};
use crate::shapes::ResolvedShape;

struct LocalPadTemplate {
    net: String,
    kind: PadKind,
    layers: Vec<String>,
    drill_nm: Option<Nm>,
    local_center: Pt,
    local_shape: ResolvedShape,
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
                    PlacedPad {
                        net: pl.pin_nets.get(&pad.name).cloned().unwrap_or_default(),
                        kind: pad.kind,
                        layers: pad_copper_layers(pad, pl.side, cu_layers),
                        drill_nm: pad.drill_nm,
                        center: local_to_board(pad.pos, pl.pos.x, pl.pos.y, pl.rot_deg, pl.side),
                        shape,
                        bbox,
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
            let local_pads = fp
                .pads
                .iter()
                .map(|pad| LocalPadTemplate {
                    net: pl.pin_nets.get(&pad.name).cloned().unwrap_or_default(),
                    kind: pad.kind,
                    layers: pad_copper_layers(pad, pl.side, cu_layers),
                    drill_nm: pad.drill_nm,
                    local_center: local_to_board(pad.pos, 0, 0, rot_deg, pl.side),
                    local_shape: pad_to_world_shape(pad, &tmp, 16),
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
                local_keepouts,
            }
        })
        .collect()
}

fn append_footprint_keepouts(board: &BoardIr, pl: &Placement, out: &mut Vec<KeepoutItem>) {
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

fn collect_static_keepouts(board: &BoardIr) -> Vec<KeepoutItem> {
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
        if pl.locked {
            append_footprint_keepouts(board, pl, &mut out);
        }
    }
    out
}

fn build_bga_courtyards(board: &BoardIr, placed: &[PlacedEntry]) -> Vec<BgaShadowEntry> {
    let mut out = Vec::new();
    for rule in &board.constraints.bga_shadows {
        if let Some(entry) = placed.iter().find(|e| e.refdes == rule.bga_refdes) {
            if entry.cy.len() >= 3 {
                out.push(BgaShadowEntry {
                    refdes: rule.bga_refdes.clone(),
                    side: entry.side,
                    min_clr: rule.min_clearance_nm,
                    allow_passives: rule.allow_passives,
                    cy: entry.cy.clone(),
                    cy_bbox: entry.cy_bbox,
                });
            }
        } else if let Some(bga) = board
            .placements
            .iter()
            .find(|p| p.refdes == rule.bga_refdes)
            && let Some(fp) = board.footprints.get(&bga.footprint)
        {
            let cy = courtyard_world_polygon(fp, bga);
            if cy.len() >= 3 {
                let cy_bbox = BBoxNm::from_pts(&cy);
                out.push(BgaShadowEntry {
                    refdes: rule.bga_refdes.clone(),
                    side: bga.side,
                    min_clr: rule.min_clearance_nm,
                    allow_passives: rule.allow_passives,
                    cy,
                    cy_bbox,
                });
            }
        }
    }
    out
}

fn target_anchors_for_placement(
    board: &BoardIr,
    pl: &Placement,
    placed: &[PlacedEntry],
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
        if let Some(ic_entry) = placed.iter().find(|e| e.refdes == primary.ic_refdes) {
            for op in &ic_entry.pads {
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

struct LegalizeCtx<'a> {
    board: &'a BoardIr,
    placed: &'a [PlacedEntry],
    keepouts: &'a [KeepoutItem],
    bga_courtyards: &'a [BgaShadowEntry],
}

impl LegalizeCtx<'_> {
    fn is_candidate_legal(
        &self,
        pl: &Placement,
        is_passive: bool,
        pos: Pt,
        tmpl: &RotTemplate,
    ) -> bool {
        let cya = translate_pts(&tmpl.local_cy, pos);
        let cy_bb = translate_bbox(tmpl.local_cy_bbox, pos);
        let cy_clr = self.board.rules.courtyard_clearance_nm;

        if !pl.allow_edge_overhang && cya.len() >= 3 {
            let cy_shape = ResolvedShape::Poly {
                verts: cya.clone(),
                bbox: cy_bb,
            };
            let (dist, off_board) =
                shape_edge_clearance(&cy_shape, &self.board.outline, &self.board.cutouts);
            if off_board || (cy_clr > 0 && dist < cy_clr as f64 - EPS_NM) {
                return false;
            }
        }

        let edge_clr = self.board.rules.edge_clearance_nm;
        let hole_clr = self.board.rules.effective_hole_clearance_nm();
        let h2h = self.board.rules.hole_to_hole_nm;
        let world_pads: Vec<(ResolvedShape, BBoxNm, Pt, &LocalPadTemplate)> = tmpl
            .local_pads
            .iter()
            .map(|lp| {
                let ws = translate_shape(&lp.local_shape, pos);
                let wb = ws.bbox();
                let wc = Pt::new(lp.local_center.x + pos.x, lp.local_center.y + pos.y);
                (ws, wb, wc, lp)
            })
            .collect();

        for (ws, _, _, lp) in &world_pads {
            if lp.kind == PadKind::Npth || lp.layers.is_empty() {
                continue;
            }
            let (dist, off_board) =
                shape_edge_clearance(ws, &self.board.outline, &self.board.cutouts);
            if off_board || dist < edge_clr as f64 - EPS_NM {
                return false;
            }
        }

        let own_ko_prefix = format!("{}.", pl.refdes);
        for ko in self.keepouts {
            if ko.label.starts_with(&own_ko_prefix) {
                continue;
            }
            if ko.kind.blocks_placement()
                && keepout_matches_side(&ko.layers, pl.side)
                && cy_bb.intersects(ko.bbox)
                && polys_overlap_area(&cya, &ko.poly) > 0.0
            {
                return false;
            }
            if ko.kind.blocks_pads() {
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

        // Check candidate's own footprint keepouts against already-placed components.
        for own_ko in &tmpl.local_keepouts {
            let ko_poly = translate_pts(&own_ko.local_poly, pos);
            let ko_bb = translate_bbox(own_ko.local_bbox, pos);
            let ko_shape = ResolvedShape::Poly {
                verts: ko_poly.clone(),
                bbox: ko_bb,
            };
            for other in self.placed {
                if own_ko.kind.blocks_placement()
                    && keepout_matches_side(&own_ko.layers, other.side)
                    && ko_bb.intersects(other.cy_bbox)
                    && polys_overlap_area(&ko_poly, &other.cy) > 0.0
                {
                    return false;
                }
                if own_ko.kind.blocks_pads() {
                    for op in &other.pads {
                        if op.kind == PadKind::Npth || !op.bbox.intersects(ko_bb) {
                            continue;
                        }
                        if !op
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
        }

        for bga in self.bga_courtyards {
            if bga.refdes == pl.refdes {
                for other in self.placed {
                    if other.side == pl.side || (bga.allow_passives && other.is_passive) {
                        continue;
                    }
                    if cy_bb.expand(bga.min_clr).intersects(other.cy_bbox)
                        && polygon_violates_clearance(&cya, &other.cy, bga.min_clr)
                    {
                        return false;
                    }
                }
                continue;
            }
            if bga.side == pl.side || (bga.allow_passives && is_passive) {
                continue;
            }
            if cy_bb.expand(bga.min_clr).intersects(bga.cy_bbox)
                && polygon_violates_clearance(&cya, &bga.cy, bga.min_clr)
            {
                return false;
            }
        }

        for other in self.placed {
            if other.side == pl.side
                && cy_bb.expand(cy_clr).intersects(other.cy_bbox)
                && polygon_violates_clearance(&cya, &other.cy, cy_clr)
            {
                return false;
            }
            for (ws, wb, wc, lp) in &world_pads {
                for op in &other.pads {
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
                            let req = self.board.pair_clearance("", &op.net, None).max(hole_clr);
                            if op.shape.dist_to_circle(*wc, d1 / 2) < req as f64 - EPS_NM {
                                return false;
                            }
                        }
                        continue;
                    }
                    if op.kind == PadKind::Npth {
                        if let Some(d2) = op.drill_nm
                            && d2 > 0
                            && !lp.layers.is_empty()
                        {
                            let req = self.board.pair_clearance(&lp.net, "", None).max(hole_clr);
                            if ws.dist_to_circle(op.center, d2 / 2) < req as f64 - EPS_NM {
                                return false;
                            }
                        }
                        continue;
                    }
                    if same_named_net(&lp.net, &op.net) {
                        continue;
                    }
                    if let Some(d1) = lp.drill_nm
                        && d1 > 0
                        && !op.layers.is_empty()
                    {
                        let req = self
                            .board
                            .pair_clearance(&lp.net, &op.net, None)
                            .max(hole_clr);
                        if op.shape.dist_to_circle(*wc, d1 / 2) < req as f64 - EPS_NM {
                            return false;
                        }
                    }
                    if let Some(d2) = op.drill_nm
                        && d2 > 0
                        && !lp.layers.is_empty()
                    {
                        let req = self
                            .board
                            .pair_clearance(&lp.net, &op.net, None)
                            .max(hole_clr);
                        if ws.dist_to_circle(op.center, d2 / 2) < req as f64 - EPS_NM {
                            return false;
                        }
                    }
                    if !lp.layers.iter().any(|l| op.layers.contains(l)) {
                        continue;
                    }
                    let rule_ov = (lp.kind == PadKind::Smd && op.kind == PadKind::Smd)
                        .then_some(self.board.rules.smd_to_smd_nm)
                        .flatten();
                    let req = self.board.pair_clearance(&lp.net, &op.net, rule_ov);
                    if wb.expand(req).intersects(op.bbox)
                        && ws.dist_to_shape(&op.shape) < req as f64 - EPS_NM
                    {
                        return false;
                    }
                }
            }
        }

        // Check candidate pads and drills against pre-existing tracks and vias.
        if !self.board.tracks.is_empty() || !self.board.vias.is_empty() {
            for (ws, wb, wc, lp) in &world_pads {
                for t in &self.board.tracks {
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
                        let d =
                            point_to_segment_dist(*wc, t.a, t.b) - (d1 / 2 + t.width / 2) as f64;
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
                for v in &self.board.vias {
                    if let Some(d1) = lp.drill_nm
                        && d1 > 0
                    {
                        if wc.dist(v.pos) - ((d1 + v.drill) as f64) * 0.5 < h2h as f64 - EPS_NM {
                            return false;
                        }
                        if lp.kind == PadKind::Npth || !same_named_net(&lp.net, &v.net) {
                            let d_net = if lp.kind == PadKind::Npth {
                                ""
                            } else {
                                &lp.net
                            };
                            let req = self.board.pair_clearance(d_net, &v.net, None).max(hole_clr);
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
                    if wb
                        .expand(req)
                        .intersects(BBoxNm::from_pt(v.pos, v.diameter / 2))
                        && ws.dist_to_circle(v.pos, v.diameter / 2) < req as f64 - EPS_NM
                    {
                        return false;
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
        let ic_entry = self.placed.iter().find(|e| e.refdes == ic_refdes);
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
            let wp = Pt::new(pos.x + lp.local_center.x, pos.y + lp.local_center.y);
            let mut best = f64::INFINITY;
            for other in self.placed {
                for op in &other.pads {
                    if op.net == lp.net {
                        best = best.min(wp.dist(op.center));
                    }
                }
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

    let mut placed = Vec::new();
    let mut movable_indices = Vec::new();
    for (idx, pl) in board.placements.iter().enumerate() {
        if pl.locked {
            placed.push(build_placed_entry(board, idx, pl, &cu_layers));
        } else {
            movable_indices.push(idx);
        }
    }
    let fixed_count = placed.len();

    // Place movable cap_proximity target ICs first, then constrained caps (tightest first), then remaining parts by area.
    let cap_ic_targets: HashSet<&str> = board
        .constraints
        .cap_proximity
        .iter()
        .map(|r| r.ic_refdes.as_str())
        .collect();
    movable_indices.sort_by_key(|&idx| {
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

    let mut keepouts = collect_static_keepouts(board);
    let board_bb = BBoxNm::from_pts(&board.outline);
    let board_diag = Pt::new(board_bb.min_x, board_bb.min_y)
        .dist(Pt::new(board_bb.max_x, board_bb.max_y))
        .ceil() as Nm;
    let search_radius = cfg.max_search_radius_nm.max(board_diag.min(25_000_000));
    let offsets = build_spiral_offsets(cfg.grid_step_nm, search_radius);

    let mut moved_parts = Vec::new();
    let mut unresolvable = Vec::new();

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
        let bga_courtyards = build_bga_courtyards(board, &placed);
        let is_passive = is_passive_component(board, &pl.refdes, &pl.footprint);
        let cap_rules: Vec<_> = board
            .constraints
            .cap_proximity
            .iter()
            .filter(|r| r.cap_refdes == pl.refdes)
            .cloned()
            .collect();
        let has_cap_rules = !cap_rules.is_empty();

        let ctx = LegalizeCtx {
            board,
            placed: &placed,
            keepouts: &keepouts,
            bga_courtyards: &bga_courtyards,
        };

        let seed_tmpl_idx = templates
            .iter()
            .position(|t| rot_eq(t.rot_deg, pl.rot_deg))
            .unwrap_or(0);
        let seed_legal = ctx.is_candidate_legal(&pl, is_passive, pl.pos, &templates[seed_tmpl_idx]);

        if !has_cap_rules && seed_legal {
            placed.push(build_placed_entry(board, idx, &pl, &cu_layers));
            append_footprint_keepouts(board, &pl, &mut keepouts);
            continue;
        }

        let anchors = target_anchors_for_placement(board, &pl, &placed);
        let mut best: Option<(bool, f64, Pt, f64)> = None;

        let eval_candidate =
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
            placed.push(build_placed_entry(board, idx, &updated, &cu_layers));
            append_footprint_keepouts(board, &updated, &mut keepouts);
        } else {
            unresolvable.push(UnresolvablePlacement {
                refdes: pl.refdes.clone(),
                reason: "no legal non-overlapping placement found within search radius".into(),
            });
            placed.push(build_placed_entry(board, idx, &pl, &cu_layers));
            append_footprint_keepouts(board, &pl, &mut keepouts);
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
