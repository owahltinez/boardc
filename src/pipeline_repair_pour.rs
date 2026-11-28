use std::collections::{HashMap, HashSet};
use std::f64::consts::PI;

use crate::drc::{DEFAULT_CLUSTER_RADIUS_NM, Violation};
use crate::geom::{local_to_board, pad_copper_layers, pad_to_world_shape, point_to_segment_dist};
use crate::ir::{BBoxNm, BoardIr, Keepout, KeepoutKind, Nm, PadKind, PourSpec, Pt};
use crate::poly_index::{IndexedPour, IndexedRing};
use crate::shapes::{ResolvedShape, capsule_poly, circle_poly};
use crate::zones::{BlockerKind, ZoneChoke};

const KEEPOUT_EXPAND_STEP_NM: Nm = 250_000;
const TRACK_CLIP_BASE_NM: Nm = 1_500_000;
const MIN_VIA_KEEPOUT_HALF_NM: Nm = 300_000;
const MIN_TRACK_KEEPOUT_HALF_NM: Nm = 200_000;
const PAD_ESCAPE_CLEAR_NM: Nm = 150_000;

enum PourBlocker {
    Via {
        pos: Pt,
        base_half: Nm,
        layer: String,
    },
    Track {
        a: Pt,
        b: Pt,
        focus_pt: Pt,
        base_half: Nm,
        layer: String,
    },
}

impl PourBlocker {
    fn anchor(&self) -> Pt {
        match self {
            Self::Via { pos, .. } => *pos,
            Self::Track { focus_pt, .. } => *focus_pt,
        }
    }
}

pub(crate) struct PourHotspot {
    pub pour_net: String,
    pub centroid_nm: Pt,
    pub radius_nm: Nm,
    pub anchors: Vec<Pt>,
    pub base_nets: HashSet<String>,
    blockers: Vec<PourBlocker>,
}

pub(crate) struct PadCenter {
    pub pos: Pt,
    pub radius: Nm,
    pub layers: Vec<String>,
}

pub(crate) fn resolve_pad_centers(
    board: &BoardIr,
    labels: &HashSet<&str>,
    cu_layers: &[String],
) -> Vec<PadCenter> {
    if labels.is_empty() {
        return Vec::new();
    }
    let mut out = Vec::new();
    for pl in &board.placements {
        let Some(fp) = board.footprints.get(&pl.footprint) else {
            continue;
        };
        for pad in &fp.pads {
            let matched = labels.iter().any(|lbl| {
                lbl.strip_prefix(&pl.refdes)
                    .and_then(|rest| rest.strip_prefix('.'))
                    == Some(pad.name.as_str())
            });
            if matched {
                let pos = local_to_board(pad.pos, pl.pos.x, pl.pos.y, pl.rot_deg, pl.side);
                let (radius, layers) = if cu_layers.is_empty() {
                    (0, Vec::new())
                } else {
                    let bb = pad_to_world_shape(pad, pl, 16).bbox();
                    let r = (bb.width() / 2).max(bb.height() / 2);
                    (r, pad_copper_layers(pad, pl.side, cu_layers))
                };
                out.push(PadCenter {
                    pos,
                    radius,
                    layers,
                });
            }
        }
    }
    out
}

pub(crate) fn collect_hotspot_targets(
    board: &BoardIr,
    eligible_nets: &HashSet<String>,
    base_nets: &HashSet<String>,
    anchors: &[Pt],
    radius_nm: Nm,
    neighbor_radius_nm: Nm,
    exclude_net: Option<&str>,
) -> HashSet<String> {
    let mut targets = base_nets.clone();
    if neighbor_radius_nm <= 0 {
        return targets;
    }
    let search_r = (radius_nm.max(0) + neighbor_radius_nm) as f64;
    for t in &board.tracks {
        if t.locked
            || t.net.is_empty()
            || exclude_net == Some(t.net.as_str())
            || !eligible_nets.contains(&t.net)
        {
            continue;
        }
        if anchors
            .iter()
            .any(|&a| point_to_segment_dist(a, t.a, t.b) <= search_r)
        {
            targets.insert(t.net.clone());
        }
    }
    for v in &board.vias {
        if v.locked
            || v.net.is_empty()
            || exclude_net == Some(v.net.as_str())
            || !eligible_nets.contains(&v.net)
        {
            continue;
        }
        if anchors.iter().any(|&a| a.dist(v.pos) <= search_r) {
            targets.insert(v.net.clone());
        }
    }
    targets
}

fn collect_layer_pad_zones(board: &BoardIr) -> HashMap<String, Vec<(Pt, Nm)>> {
    let cu_layers = board.copper_layer_names();
    let mut map: HashMap<String, Vec<(Pt, Nm)>> = HashMap::new();
    for pl in &board.placements {
        let Some(fp) = board.footprints.get(&pl.footprint) else {
            continue;
        };
        for pad in &fp.pads {
            if pad.kind == PadKind::Npth {
                continue;
            }
            let bb = pad_to_world_shape(pad, pl, 16).bbox();
            let center = bb.center();
            let half_ext = (bb.width() / 2).max(bb.height() / 2);
            for layer in pad_copper_layers(pad, pl.side, &cu_layers) {
                map.entry(layer).or_default().push((center, half_ext));
            }
        }
    }
    map
}

fn trim_segment_outside_pads(ca: Pt, cb: Pt, half_w: Nm, pads: &[(Pt, Nm)]) -> Vec<(Pt, Pt)> {
    let seg_len = ca.dist(cb);
    if seg_len < 1.0 {
        let blocked = pads
            .iter()
            .any(|&(c, ext)| ca.dist(c) <= (ext + half_w + PAD_ESCAPE_CLEAR_NM) as f64);
        return if blocked { Vec::new() } else { vec![(ca, cb)] };
    }
    let dx = (cb.x - ca.x) as f64;
    let dy = (cb.y - ca.y) as f64;
    let a_q = dx * dx + dy * dy;
    let mut blocked: Vec<(f64, f64)> = Vec::new();
    for &(center, ext) in pads {
        let r = (ext + half_w + PAD_ESCAPE_CLEAR_NM) as f64;
        let fx = (ca.x - center.x) as f64;
        let fy = (ca.y - center.y) as f64;
        let b_q = fx * dx + fy * dy;
        let c_q = fx * fx + fy * fy - r * r;
        let disc = b_q * b_q - a_q * c_q;
        if disc > 0.0 {
            let s = disc.sqrt();
            let u0 = ((-b_q - s) / a_q).clamp(0.0, 1.0);
            let u1 = ((-b_q + s) / a_q).clamp(0.0, 1.0);
            if u1 > u0 {
                blocked.push((u0, u1));
            }
        }
    }
    if blocked.is_empty() {
        return vec![(ca, cb)];
    }
    blocked.sort_by(|x, y| x.0.total_cmp(&y.0));
    let mut out = Vec::new();
    let mut cur = 0.0_f64;
    let lerp = |u: f64| Pt::new(ca.x + (dx * u).round() as Nm, ca.y + (dy * u).round() as Nm);
    let mut push_clear = |lo: f64, hi: f64| {
        if (hi - lo) * seg_len >= 1_000.0 {
            let pa = lerp(lo);
            let pb = lerp(hi);
            if pa != pb {
                out.push((pa, pb));
            }
        }
    };
    for (blo, bhi) in blocked {
        if blo > cur {
            push_clear(cur, blo);
        }
        cur = cur.max(bhi);
    }
    if cur < 1.0 {
        push_clear(cur, 1.0);
    }
    out
}

impl PourHotspot {
    pub(crate) fn build_keepouts(&self, board: &BoardIr, attempt: usize) -> Vec<Keepout> {
        let expand_nm = (attempt.saturating_sub(1) as Nm) * KEEPOUT_EXPAND_STEP_NM;
        let has_track = self
            .blockers
            .iter()
            .any(|b| matches!(b, PourBlocker::Track { .. }));
        let layer_pads = has_track.then(|| collect_layer_pad_zones(board));
        let mut keepouts = Vec::with_capacity(self.blockers.len());
        for blocker in &self.blockers {
            match blocker {
                PourBlocker::Via {
                    pos,
                    base_half,
                    layer,
                } => {
                    let half = *base_half + expand_nm;
                    let oct_r = ((half as f64) / (PI / 8.0).cos()).ceil() as Nm;
                    keepouts.push(Keepout {
                        kind: KeepoutKind::Via,
                        layers: vec![layer.clone()],
                        poly: circle_poly(*pos, oct_r, 8),
                    });
                }
                PourBlocker::Track {
                    a,
                    b,
                    focus_pt,
                    base_half,
                    layer,
                } => {
                    let half_w = *base_half + expand_nm;
                    let clip_half = (TRACK_CLIP_BASE_NM + expand_nm).max(half_w * 2);
                    let (ca, cb) = clip_segment_near(*a, *b, *focus_pt, clip_half);
                    let pads = layer_pads
                        .as_ref()
                        .and_then(|m| m.get(layer.as_str()))
                        .map(Vec::as_slice)
                        .unwrap_or(&[]);
                    for (sa, sb) in trim_segment_outside_pads(ca, cb, half_w, pads) {
                        keepouts.push(Keepout {
                            kind: KeepoutKind::Track,
                            layers: vec![layer.clone()],
                            poly: capsule_poly(sa, sb, half_w, 8),
                        });
                    }
                }
            }
        }
        keepouts
    }
}

fn closest_point_on_segment(p: Pt, a: Pt, b: Pt) -> Pt {
    let dx = (b.x - a.x) as f64;
    let dy = (b.y - a.y) as f64;
    let len_sq = dx * dx + dy * dy;
    if len_sq == 0.0 {
        return a;
    }
    let u = ((((p.x - a.x) as f64) * dx + ((p.y - a.y) as f64) * dy) / len_sq).clamp(0.0, 1.0);
    Pt::new(a.x + (dx * u).round() as Nm, a.y + (dy * u).round() as Nm)
}

fn clip_segment_near(a: Pt, b: Pt, focus: Pt, max_half_len: Nm) -> (Pt, Pt) {
    let seg_len = a.dist(b);
    if seg_len <= (max_half_len as f64) * 2.0 || seg_len < 1.0 {
        return (a, b);
    }
    let dx = (b.x - a.x) as f64;
    let dy = (b.y - a.y) as f64;
    let u0 = ((((focus.x - a.x) as f64) * dx + ((focus.y - a.y) as f64) * dy)
        / (seg_len * seg_len))
        .clamp(0.0, 1.0);
    let du = (max_half_len as f64) / seg_len;
    let u_a = (u0 - du).clamp(0.0, 1.0);
    let u_b = (u0 + du).clamp(0.0, 1.0);
    let lerp = |u: f64| Pt::new(a.x + (dx * u).round() as Nm, a.y + (dy * u).round() as Nm);
    (lerp(u_a), lerp(u_b))
}

fn parse_blocker_idx(label: &str, prefix: &str) -> Option<usize> {
    label.strip_prefix(prefix)?.split(':').next()?.parse().ok()
}

fn pt_touches_indexed_pours(pours: &[IndexedPour], pt: Pt, radius: Nm) -> bool {
    let tol = radius.max(0) + 2_000;
    let shape = ResolvedShape::Circle {
        center: pt,
        radius: tol,
    };
    pours.iter().any(|ip| {
        if !ip.outer.bbox.expand(tol).contains_pt(pt) {
            return false;
        }
        if ip.outer.contains_pt(pt) {
            return ip.contains_pt(pt);
        }
        ip.outer.min_dist_to_shape_bounded(&shape, 0) <= 0.0
    })
}

fn outline_pt_within(outline: &IndexedRing, pt: Pt, max_dist: f64) -> bool {
    let limit = max_dist.ceil() as Nm;
    if !outline.bbox.expand(limit).contains_pt(pt) {
        return false;
    }
    if outline.contains_pt(pt) {
        return true;
    }
    let shape = ResolvedShape::Circle {
        center: pt,
        radius: 0,
    };
    outline.min_dist_to_shape_bounded(&shape, limit) <= max_dist
}

fn outline_seg_within(outline: &IndexedRing, a: Pt, b: Pt, max_dist: f64) -> bool {
    let limit = max_dist.ceil() as Nm;
    let sb = BBoxNm::from_pts(&[a, b]).expand(limit);
    if !outline.bbox.intersects(sb) {
        return false;
    }
    if outline.contains_pt(a) || outline.contains_pt(b) {
        return true;
    }
    let shape = ResolvedShape::Capsule { a, b, radius: 0 };
    outline.min_dist_to_shape_bounded(&shape, limit) <= max_dist
}

fn collect_open_fallback_hotspot(
    board: &BoardIr,
    viol: &Violation,
    eligible_nets: &HashSet<String>,
) -> Option<(PourHotspot, Vec<Pt>)> {
    let specs: Vec<&PourSpec> = board
        .pours
        .iter()
        .filter(|p| p.net == viol.net_a && p.outline.len() >= 3)
        .collect();
    if specs.is_empty() {
        return None;
    }
    let cu_layers = board.copper_layer_names();
    let open_labels: HashSet<&str> = [Some(viol.item_a.as_str()), viol.item_b.as_deref()]
        .into_iter()
        .flatten()
        .filter(|s| !s.is_empty())
        .collect();
    let open_pads = resolve_pad_centers(board, &open_labels, &cu_layers);
    let pad_centers: Vec<Pt> = if open_pads.is_empty() {
        vec![viol.pos_nm]
    } else {
        open_pads.iter().map(|p| p.pos).collect()
    };
    let max_outline_dist = DEFAULT_CLUSTER_RADIUS_NM as f64;
    let max_blocker_dist = (DEFAULT_CLUSTER_RADIUS_NM * 2) as f64;

    let mut best: Option<(f64, PourHotspot, Vec<Pt>)> = None;
    for spec in specs {
        let outline_idx = IndexedRing::new(&spec.outline);
        let indexed_pours: Vec<IndexedPour> = board
            .filled_pours
            .iter()
            .filter(|fp| fp.net == spec.net && fp.layer == spec.layer)
            .flat_map(|fp| fp.polys.iter().map(IndexedPour::new))
            .collect();
        let pad_disconnected: Vec<Pt> = open_pads
            .iter()
            .filter(|p| {
                p.layers.iter().any(|l| l == &spec.layer)
                    && !pt_touches_indexed_pours(&indexed_pours, p.pos, p.radius)
                    && outline_pt_within(&outline_idx, p.pos, max_outline_dist)
            })
            .map(|p| p.pos)
            .collect();
        let mut via_disconnected: Vec<(Pt, f64)> = board
            .vias
            .iter()
            .filter(|v| {
                v.net == spec.net
                    && v.touches_layer(&spec.layer, &board.layers)
                    && !pt_touches_indexed_pours(&indexed_pours, v.pos, v.diameter / 2)
                    && outline_pt_within(&outline_idx, v.pos, max_outline_dist)
            })
            .map(|v| {
                let d = pad_centers
                    .iter()
                    .map(|&c| c.dist(v.pos))
                    .fold(f64::INFINITY, f64::min);
                (v.pos, d)
            })
            .collect();
        via_disconnected.sort_by(|a, b| a.1.total_cmp(&b.1));
        let near_vias: Vec<Pt> = via_disconnected
            .iter()
            .filter(|(_, d)| *d <= max_blocker_dist)
            .map(|(p, _)| *p)
            .collect();
        let mut focus = pad_disconnected;
        if !near_vias.is_empty() {
            focus.extend(near_vias);
        } else if focus.is_empty() {
            focus.extend(via_disconnected.into_iter().map(|(p, _)| p));
        }
        if focus.is_empty() {
            continue;
        }

        let mut cands: Vec<(f64, String, PourBlocker)> = Vec::new();
        for via in &board.vias {
            if via.locked
                || via.net.is_empty()
                || via.net == spec.net
                || !eligible_nets.contains(&via.net)
                || !via.touches_layer(&spec.layer, &board.layers)
            {
                continue;
            }
            let pour_clr = board.pour_pair_clearance(&spec.net, spec.clearance_nm, &via.net);
            let margin = (via.diameter / 2 + pour_clr + 50_000) as f64;
            if !outline_pt_within(&outline_idx, via.pos, margin) {
                continue;
            }
            let dist = focus
                .iter()
                .map(|&a| a.dist(via.pos))
                .fold(f64::INFINITY, f64::min);
            if dist <= max_blocker_dist {
                let base_half = (via.diameter / 2 + pour_clr).max(MIN_VIA_KEEPOUT_HALF_NM);
                cands.push((
                    dist,
                    via.net.clone(),
                    PourBlocker::Via {
                        pos: via.pos,
                        base_half,
                        layer: spec.layer.clone(),
                    },
                ));
            }
        }
        for tr in &board.tracks {
            if tr.locked
                || tr.net.is_empty()
                || tr.net == spec.net
                || tr.layer != spec.layer
                || !eligible_nets.contains(&tr.net)
            {
                continue;
            }
            let pour_clr = board.pour_pair_clearance(&spec.net, spec.clearance_nm, &tr.net);
            let margin = (tr.width / 2 + pour_clr + 50_000) as f64;
            if !outline_seg_within(&outline_idx, tr.a, tr.b, margin) {
                continue;
            }
            let mut best_pt = tr.a;
            let mut dist = f64::INFINITY;
            for &a in &focus {
                let cp = closest_point_on_segment(a, tr.a, tr.b);
                let d = a.dist(cp);
                if d < dist {
                    dist = d;
                    best_pt = cp;
                }
            }
            if dist <= max_blocker_dist {
                let base_half = (tr.width / 2 + pour_clr).max(MIN_TRACK_KEEPOUT_HALF_NM);
                cands.push((
                    dist,
                    tr.net.clone(),
                    PourBlocker::Track {
                        a: tr.a,
                        b: tr.b,
                        focus_pt: best_pt,
                        base_half,
                        layer: spec.layer.clone(),
                    },
                ));
            }
        }
        cands.sort_by(|a, b| a.0.total_cmp(&b.0));
        let Some(&(min_dist, _, _)) = cands.first() else {
            continue;
        };
        let cutoff = min_dist + 600_000.0;
        let mut base_nets = HashSet::new();
        let mut blockers = Vec::new();
        let mut anchors = focus.clone();
        for (_, net, blocker) in cands.into_iter().filter(|(d, _, _)| *d <= cutoff).take(3) {
            base_nets.insert(net);
            anchors.push(blocker.anchor());
            blockers.push(blocker);
        }
        let centroid_nm = blockers.first().map_or(viol.pos_nm, PourBlocker::anchor);
        let radius_nm = anchors
            .iter()
            .map(|&a| a.dist(centroid_nm).round() as Nm)
            .max()
            .unwrap_or(0)
            .max(DEFAULT_CLUSTER_RADIUS_NM / 2);
        let hotspot = PourHotspot {
            pour_net: spec.net.clone(),
            centroid_nm,
            radius_nm,
            anchors,
            base_nets,
            blockers,
        };
        if best.as_ref().is_none_or(|(d, _, _)| min_dist < *d) {
            best = Some((min_dist, hotspot, focus));
        }
    }
    best.map(|(_, h, focus)| (h, focus))
}

pub(crate) fn collect_pour_hotspots(
    board: &BoardIr,
    chokes: &[ZoneChoke],
    pour_opens: &[Violation],
    eligible_nets: &HashSet<String>,
    scope_nets: Option<&HashSet<String>>,
) -> Vec<PourHotspot> {
    let mut hotspots = Vec::new();

    for choke in chokes {
        let spec_clr = board
            .pours
            .iter()
            .find(|p| p.net == choke.net && p.layer == choke.layer)
            .and_then(|p| p.clearance_nm);
        let mut base_nets = HashSet::new();
        let mut blockers = Vec::new();
        let mut anchors = vec![choke.pos];

        for b in &choke.blockers {
            match b.kind {
                BlockerKind::Via => {
                    let Some(idx) = parse_blocker_idx(&b.label, "via#") else {
                        continue;
                    };
                    let Some(via) = board.vias.get(idx) else {
                        continue;
                    };
                    if via.locked
                        || via.net.is_empty()
                        || via.net == choke.net
                        || !eligible_nets.contains(&via.net)
                    {
                        continue;
                    }
                    let pour_clr = board.pour_pair_clearance(&choke.net, spec_clr, &via.net);
                    let base_half = (via.diameter / 2 + pour_clr).max(MIN_VIA_KEEPOUT_HALF_NM);
                    base_nets.insert(via.net.clone());
                    anchors.push(via.pos);
                    blockers.push(PourBlocker::Via {
                        pos: via.pos,
                        base_half,
                        layer: choke.layer.clone(),
                    });
                }
                BlockerKind::Track => {
                    let Some(idx) = parse_blocker_idx(&b.label, "track#") else {
                        continue;
                    };
                    let Some(tr) = board.tracks.get(idx) else {
                        continue;
                    };
                    if tr.locked
                        || tr.net.is_empty()
                        || tr.net == choke.net
                        || tr.layer != choke.layer
                        || !eligible_nets.contains(&tr.net)
                    {
                        continue;
                    }
                    let pour_clr = board.pour_pair_clearance(&choke.net, spec_clr, &tr.net);
                    let base_half = (tr.width / 2 + pour_clr).max(MIN_TRACK_KEEPOUT_HALF_NM);
                    let focus_pt = closest_point_on_segment(choke.pos, tr.a, tr.b);
                    base_nets.insert(tr.net.clone());
                    anchors.push(focus_pt);
                    blockers.push(PourBlocker::Track {
                        a: tr.a,
                        b: tr.b,
                        focus_pt,
                        base_half,
                        layer: choke.layer.clone(),
                    });
                }
                BlockerKind::Pad | BlockerKind::Keepout | BlockerKind::Cutout => {}
            }
        }

        if base_nets.is_empty()
            || scope_nets.is_some_and(|scope| !base_nets.iter().any(|n| scope.contains(n)))
        {
            continue;
        }
        let radius_nm = anchors
            .iter()
            .map(|&a| a.dist(choke.pos).round() as Nm)
            .max()
            .unwrap_or(0)
            .max(DEFAULT_CLUSTER_RADIUS_NM / 2);
        hotspots.push(PourHotspot {
            pour_net: choke.net.clone(),
            centroid_nm: choke.pos,
            radius_nm,
            anchors,
            base_nets,
            blockers,
        });
    }

    let choke_hotspot_count = hotspots.len();
    for viol in pour_opens {
        if viol.net_a.is_empty() {
            continue;
        }
        let Some((hotspot, focus)) = collect_open_fallback_hotspot(board, viol, eligible_nets)
        else {
            continue;
        };
        if scope_nets.is_some_and(|scope| !hotspot.base_nets.iter().any(|n| scope.contains(n))) {
            continue;
        }
        let covered = hotspots[..choke_hotspot_count].iter().any(|h| {
            h.pour_net == viol.net_a
                && (h.centroid_nm.dist(hotspot.centroid_nm) <= DEFAULT_CLUSTER_RADIUS_NM as f64
                    || focus
                        .iter()
                        .any(|&p| p.dist(h.centroid_nm) <= DEFAULT_CLUSTER_RADIUS_NM as f64))
        });
        if !covered {
            hotspots.push(hotspot);
        }
    }

    hotspots
}
