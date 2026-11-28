use std::collections::{BTreeMap, BTreeSet, HashMap};

use crate::drc::{Severity, Violation, ViolationKind};
use crate::drc_scene::PadItem;
use crate::geom::{
    EPS_NM, courtyard_world_polygon, point_in_polygon, polygon_to_polygon_dist,
    polygon_violates_clearance,
};
use crate::ir::{BBoxNm, BoardIr, CapProximityRule, MinViasRule, Nm, Pt};

const NON_PASSIVE_DESIGNATORS: &[&str] = &[
    "D", "J", "Q", "U", "P", "S", "K", "F", "Y", "X", "M", "T", "H", "W", "Z", "SW", "TP", "BT",
    "CN", "CR", "VR", "IC", "MH", "ZD", "LED", "CON", "CONN", "TVS", "ESD", "PT", "NT", "LDO",
    "BAT", "FET", "MOD", "ANT", "OSC", "XTAL",
];

pub(crate) fn is_passive_component(board: &BoardIr, refdes: &str, fp_name: &str) -> bool {
    let Some(fp) = board.footprints.get(fp_name) else {
        return false;
    };
    if fp.pads.len() > 2 {
        return false;
    }
    let u = refdes.to_ascii_uppercase();
    let segs: Vec<&str> = u.split('_').filter(|s| !s.is_empty()).collect();
    let Some(&last) = segs.last() else {
        return false;
    };
    let idx = last.trim_end_matches(|c: char| c.is_ascii_digit()).len();
    let (last_alpha, last_digits) = (&last[..idx], &last[idx..]);
    if matches!(last_alpha, "R" | "C" | "L" | "FB") {
        return true;
    }
    if segs.len() >= 2 && matches!(segs[segs.len() - 2], "R" | "C" | "L" | "FB") {
        let is_non_passive =
            !last_digits.is_empty() && NON_PASSIVE_DESIGNATORS.contains(&last_alpha);
        return !is_non_passive;
    }
    false
}

fn pl_net_pad_pts(
    pads: &[PadItem],
    pl_idx: usize,
    pl_pads: Option<&[usize]>,
    net: &str,
) -> Vec<Pt> {
    match pl_pads {
        Some(idxs) => idxs
            .iter()
            .map(|&pi| &pads[pi])
            .filter(|p| p.net == net)
            .map(|p| p.center)
            .collect(),
        None => pads
            .iter()
            .filter(|p| p.pl_idx == pl_idx && p.net == net)
            .map(|p| p.center)
            .collect(),
    }
}

fn cap_proximity_dist_for(
    board: &BoardIr,
    pads: &[PadItem],
    rule: &CapProximityRule,
    cap_idx: usize,
    ic_idx: usize,
    pads_by_pl: Option<&[Vec<usize>]>,
) -> f64 {
    let cap = &board.placements[cap_idx];
    let ic = &board.placements[ic_idx];
    let pts_for = |pl_idx: usize, fallback: Pt| -> Vec<Pt> {
        if rule.net.is_empty() {
            return vec![fallback];
        }
        let v = pl_net_pad_pts(
            pads,
            pl_idx,
            pads_by_pl.map(|m| m[pl_idx].as_slice()),
            &rule.net,
        );
        if v.is_empty() { vec![fallback] } else { v }
    };
    let cap_pts = pts_for(cap_idx, cap.pos);
    let ic_pts = pts_for(ic_idx, ic.pos);
    let mut min_dist = f64::INFINITY;
    for &b in &ic_pts {
        min_dist = min_dist.min(cap.pos.dist(b));
        for &a in &cap_pts {
            min_dist = min_dist.min(a.dist(b));
        }
    }
    min_dist
}

pub(crate) fn cap_proximity_dist(
    board: &BoardIr,
    pads: &[PadItem],
    rule: &CapProximityRule,
) -> Option<(usize, usize, f64)> {
    let cap_idx = board
        .placements
        .iter()
        .position(|p| p.refdes == rule.cap_refdes)?;
    let ic_idx = board
        .placements
        .iter()
        .position(|p| p.refdes == rule.ic_refdes)?;
    let min_dist = cap_proximity_dist_for(board, pads, rule, cap_idx, ic_idx, None);
    Some((cap_idx, ic_idx, min_dist))
}

fn count_rule_vias_for(
    board: &BoardIr,
    pads: &[PadItem],
    rule: &MinViasRule,
    pl_idx: Option<usize>,
    pl_pads: Option<&[usize]>,
    net_vias: Option<&[usize]>,
) -> (usize, Pt) {
    if rule.refdes.is_some() {
        let Some(pl_idx) = pl_idx else {
            return (0, Pt::default());
        };
        let pl = &board.placements[pl_idx];
        let cy = board
            .footprints
            .get(&pl.footprint)
            .map(|fp| courtyard_world_polygon(fp, pl))
            .unwrap_or_default();
        let pad_pts: BTreeSet<(Nm, Nm)> = pl_net_pad_pts(pads, pl_idx, pl_pads, &rule.net)
            .into_iter()
            .map(|p| (p.x, p.y))
            .collect();
        let matches = |pos: Pt| {
            point_in_polygon(pos, &cy)
                || pad_pts
                    .iter()
                    .any(|&(px, py)| pos.dist(Pt::new(px, py)) <= 500_000.0)
        };
        let c = match net_vias {
            Some(vis) => vis
                .iter()
                .filter(|&&vi| matches(board.vias[vi].pos))
                .count(),
            None => board
                .vias
                .iter()
                .filter(|v| v.net == rule.net && matches(v.pos))
                .count(),
        };
        (c, pl.pos)
    } else {
        match net_vias {
            Some(vis) => (
                vis.len(),
                vis.first()
                    .map(|&vi| board.vias[vi].pos)
                    .unwrap_or_default(),
            ),
            None => {
                let c = board.vias.iter().filter(|v| v.net == rule.net).count();
                let p = board
                    .vias
                    .iter()
                    .find(|v| v.net == rule.net)
                    .map(|v| v.pos)
                    .unwrap_or_default();
                (c, p)
            }
        }
    }
}

pub(crate) fn count_rule_vias(
    board: &BoardIr,
    pads: &[PadItem],
    rule: &MinViasRule,
) -> (usize, Pt) {
    let pl_idx = rule
        .refdes
        .as_ref()
        .and_then(|rd| board.placements.iter().position(|p| &p.refdes == rd));
    count_rule_vias_for(board, pads, rule, pl_idx, None, None)
}

pub(crate) fn check_cap_proximity(board: &BoardIr, pads: &[PadItem], out: &mut Vec<Violation>) {
    for rule in &board.constraints.cap_proximity {
        let Some((cap_idx, ic_idx, min_dist)) = cap_proximity_dist(board, pads, rule) else {
            continue;
        };
        let cap = &board.placements[cap_idx];
        let ic = &board.placements[ic_idx];
        if min_dist > rule.max_dist_nm as f64 + EPS_NM {
            let meas = min_dist.round() as Nm;
            out.push(Violation {
                kind: ViolationKind::CapProximity,
                severity: Severity::Error,
                layer: cap.side.cu_layer().to_string(),
                pos_nm: cap.pos,
                measured_nm: meas,
                required_nm: rule.max_dist_nm,
                item_a: cap.refdes.clone(),
                item_b: Some(ic.refdes.clone()),
                net_a: rule.net.clone(),
                net_b: String::new(),
                message: format!(
                    "{} distance to {} is {meas} nm > max {} nm",
                    cap.refdes, ic.refdes, rule.max_dist_nm
                ),
            });
        }
    }
}

pub(crate) fn collect_net_lengths(board: &BoardIr) -> BTreeMap<&str, f64> {
    let mut net_lengths: BTreeMap<&str, f64> = BTreeMap::new();
    for t in &board.tracks {
        *net_lengths.entry(t.net.as_str()).or_insert(0.0) += t.a.dist(t.b);
    }
    net_lengths
}

fn check_length_constraints(
    board: &BoardIr,
    net_lengths: &BTreeMap<&str, f64>,
    net_pos: &BTreeMap<&str, Pt>,
    out: &mut Vec<Violation>,
) {
    for rule in &board.constraints.max_lengths {
        for net in &rule.nets {
            let len = net_lengths.get(net.as_str()).copied().unwrap_or(0.0);
            if len > rule.max_nm as f64 + EPS_NM {
                let meas = len.round() as Nm;
                out.push(Violation {
                    kind: ViolationKind::MaxLength,
                    severity: Severity::Error,
                    layer: String::new(),
                    pos_nm: net_pos.get(net.as_str()).copied().unwrap_or_default(),
                    measured_nm: meas,
                    required_nm: rule.max_nm,
                    item_a: format!("net:{net}"),
                    item_b: None,
                    net_a: net.clone(),
                    net_b: String::new(),
                    message: format!("net {net} length {meas} nm > max {} nm", rule.max_nm),
                });
            }
        }
    }

    for rule in &board.constraints.match_groups {
        if let Some(target) = rule.target_nm {
            for net in &rule.nets {
                let len = net_lengths.get(net.as_str()).copied().unwrap_or(0.0);
                let delta = (len - target as f64).abs();
                if delta > rule.tolerance_nm as f64 + EPS_NM {
                    let meas = delta.round() as Nm;
                    out.push(Violation {
                        kind: ViolationKind::MatchGroup,
                        severity: Severity::Error,
                        layer: String::new(),
                        pos_nm: net_pos.get(net.as_str()).copied().unwrap_or_default(),
                        measured_nm: meas,
                        required_nm: rule.tolerance_nm,
                        item_a: format!("match_group:{}", rule.name),
                        item_b: Some(format!("net:{net}")),
                        net_a: net.clone(),
                        net_b: String::new(),
                        message: format!(
                            "match group {} net {net} skew {meas} nm > tol {} nm",
                            rule.name, rule.tolerance_nm
                        ),
                    });
                }
            }
        } else if rule.nets.len() >= 2 {
            let mut min_e = ("", f64::INFINITY);
            let mut max_e = ("", f64::NEG_INFINITY);
            for net in &rule.nets {
                let len = net_lengths.get(net.as_str()).copied().unwrap_or(0.0);
                if len < min_e.1 {
                    min_e = (net.as_str(), len);
                }
                if len > max_e.1 {
                    max_e = (net.as_str(), len);
                }
            }
            let skew = max_e.1 - min_e.1;
            if skew > rule.tolerance_nm as f64 + EPS_NM {
                let meas = skew.round() as Nm;
                out.push(Violation {
                    kind: ViolationKind::MatchGroup,
                    severity: Severity::Error,
                    layer: String::new(),
                    pos_nm: net_pos.get(max_e.0).copied().unwrap_or_default(),
                    measured_nm: meas,
                    required_nm: rule.tolerance_nm,
                    item_a: format!("match_group:{}", rule.name),
                    item_b: Some(format!("net:{}..net:{}", min_e.0, max_e.0)),
                    net_a: max_e.0.to_string(),
                    net_b: min_e.0.to_string(),
                    message: format!(
                        "match group {} skew {meas} nm > tol {} nm",
                        rule.name, rule.tolerance_nm
                    ),
                });
            }
        }
    }
}

pub(crate) fn count_length_violations(board: &BoardIr) -> usize {
    let net_lengths = collect_net_lengths(board);
    let net_pos = BTreeMap::new();
    let mut out = Vec::new();
    check_length_constraints(board, &net_lengths, &net_pos, &mut out);
    out.len()
}

pub(crate) fn check_constraints(board: &BoardIr, pads: &[PadItem], out: &mut Vec<Violation>) {
    let net_lengths = collect_net_lengths(board);
    let mut net_pos: BTreeMap<&str, Pt> = BTreeMap::new();
    for t in &board.tracks {
        net_pos
            .entry(t.net.as_str())
            .or_insert_with(|| t.a.midpoint(t.b));
    }
    for pad in pads {
        if !pad.net.is_empty() {
            net_pos.entry(pad.net.as_str()).or_insert(pad.center);
        }
    }

    check_length_constraints(board, &net_lengths, &net_pos, out);

    let need_refdes = !board.constraints.cap_proximity.is_empty()
        || board
            .constraints
            .min_vias
            .iter()
            .any(|r| r.refdes.is_some())
        || !board.constraints.bga_shadows.is_empty();
    let pl_by_refdes: HashMap<&str, usize> = if need_refdes {
        board
            .placements
            .iter()
            .enumerate()
            .map(|(i, p)| (p.refdes.as_str(), i))
            .collect()
    } else {
        HashMap::new()
    };
    let need_pads = !board.constraints.cap_proximity.is_empty()
        || board
            .constraints
            .min_vias
            .iter()
            .any(|r| r.refdes.is_some());
    let mut pads_by_pl: Vec<Vec<usize>> = if need_pads {
        vec![Vec::new(); board.placements.len()]
    } else {
        Vec::new()
    };
    if need_pads {
        for (pi, pad) in pads.iter().enumerate() {
            if pad.pl_idx < pads_by_pl.len() {
                pads_by_pl[pad.pl_idx].push(pi);
            }
        }
    }
    let need_vias =
        !board.constraints.min_vias.is_empty() || !board.constraints.max_vias.is_empty();
    let mut vias_by_net: HashMap<&str, Vec<usize>> = HashMap::new();
    if need_vias {
        for (vi, v) in board.vias.iter().enumerate() {
            vias_by_net.entry(v.net.as_str()).or_default().push(vi);
        }
    }

    for rule in &board.constraints.cap_proximity {
        let Some(&cap_idx) = pl_by_refdes.get(rule.cap_refdes.as_str()) else {
            continue;
        };
        let Some(&ic_idx) = pl_by_refdes.get(rule.ic_refdes.as_str()) else {
            continue;
        };
        let min_dist =
            cap_proximity_dist_for(board, pads, rule, cap_idx, ic_idx, Some(&pads_by_pl));
        if min_dist > rule.max_dist_nm as f64 + EPS_NM {
            let cap = &board.placements[cap_idx];
            let ic = &board.placements[ic_idx];
            let meas = min_dist.round() as Nm;
            out.push(Violation {
                kind: ViolationKind::CapProximity,
                severity: Severity::Error,
                layer: cap.side.cu_layer().to_string(),
                pos_nm: cap.pos,
                measured_nm: meas,
                required_nm: rule.max_dist_nm,
                item_a: cap.refdes.clone(),
                item_b: Some(ic.refdes.clone()),
                net_a: rule.net.clone(),
                net_b: String::new(),
                message: format!(
                    "{} distance to {} is {meas} nm > max {} nm",
                    cap.refdes, ic.refdes, rule.max_dist_nm
                ),
            });
        }
    }

    for rule in &board.constraints.min_vias {
        let pl_idx = rule
            .refdes
            .as_deref()
            .and_then(|rd| pl_by_refdes.get(rd).copied());
        let pl_pads = pl_idx.map(|i| pads_by_pl[i].as_slice());
        let net_vias = vias_by_net
            .get(rule.net.as_str())
            .map_or(&[][..], Vec::as_slice);
        let (count, mut pos) =
            count_rule_vias_for(board, pads, rule, pl_idx, pl_pads, Some(net_vias));
        if rule.refdes.is_none() && pos == Pt::default() {
            pos = net_pos.get(rule.net.as_str()).copied().unwrap_or_default();
        }
        if count < rule.min_vias {
            out.push(Violation {
                kind: ViolationKind::MinVias,
                severity: Severity::Error,
                layer: String::new(),
                pos_nm: pos,
                measured_nm: count as Nm,
                required_nm: rule.min_vias as Nm,
                item_a: rule
                    .refdes
                    .clone()
                    .unwrap_or_else(|| format!("net:{}", rule.net)),
                item_b: None,
                net_a: rule.net.clone(),
                net_b: String::new(),
                message: format!("net {} has {count} vias < min {}", rule.net, rule.min_vias),
            });
        }
    }

    for rule in &board.constraints.max_vias {
        for net in &rule.nets {
            let Some(vis) = vias_by_net.get(net.as_str()) else {
                continue;
            };
            let count = vis.len();
            if count > rule.max_vias {
                out.push(Violation {
                    kind: ViolationKind::MaxVias,
                    severity: Severity::Error,
                    layer: String::new(),
                    pos_nm: board.vias[vis[0]].pos,
                    measured_nm: count as Nm,
                    required_nm: rule.max_vias as Nm,
                    item_a: format!("net:{net}"),
                    item_b: None,
                    net_a: net.clone(),
                    net_b: String::new(),
                    message: format!("net {net} has {count} vias > max {}", rule.max_vias),
                });
            }
        }
    }

    for rule in &board.constraints.bga_shadows {
        let Some(&bga_idx) = pl_by_refdes.get(rule.bga_refdes.as_str()) else {
            continue;
        };
        let bga = &board.placements[bga_idx];
        let Some(bga_fp) = board.footprints.get(&bga.footprint) else {
            continue;
        };
        let bga_cy = courtyard_world_polygon(bga_fp, bga);
        if bga_cy.len() < 3 {
            continue;
        }
        let bga_bb = BBoxNm::from_pts(&bga_cy).expand(rule.min_clearance_nm);
        for other in &board.placements {
            if other.refdes == bga.refdes || other.side == bga.side {
                continue;
            }
            if rule.allow_passives && is_passive_component(board, &other.refdes, &other.footprint) {
                continue;
            }
            let Some(other_fp) = board.footprints.get(&other.footprint) else {
                continue;
            };
            let other_cy = courtyard_world_polygon(other_fp, other);
            if other_cy.len() < 3 || !bga_bb.intersects(BBoxNm::from_pts(&other_cy)) {
                continue;
            }
            if polygon_violates_clearance(&other_cy, &bga_cy, rule.min_clearance_nm) {
                let meas = polygon_to_polygon_dist(&other_cy, &bga_cy).max(0.0).round() as Nm;
                out.push(Violation {
                    kind: ViolationKind::BgaShadow,
                    severity: Severity::Error,
                    layer: other.side.cu_layer().to_string(),
                    pos_nm: other.pos,
                    measured_nm: meas,
                    required_nm: rule.min_clearance_nm,
                    item_a: other.refdes.clone(),
                    item_b: Some(bga.refdes.clone()),
                    net_a: String::new(),
                    net_b: String::new(),
                    message: format!(
                        "{} inside BGA shadow of {} ({meas} nm < {} nm)",
                        other.refdes, bga.refdes, rule.min_clearance_nm
                    ),
                });
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn min_vias_zero_with_missing_refdes_produces_no_violation() {
        let mut board: BoardIr = serde_json::from_str(
            r#"{"name":"t","outline":[],"layers":[],"footprints":{},"placements":[]}"#,
        )
        .unwrap();
        board.constraints.min_vias.push(MinViasRule {
            net: "GND".into(),
            refdes: Some("U_MISSING".into()),
            min_vias: 0,
        });
        let mut out = Vec::new();
        check_constraints(&board, &[], &mut out);
        assert!(out.is_empty());

        board.constraints.min_vias[0].min_vias = 1;
        check_constraints(&board, &[], &mut out);
        assert_eq!(out.len(), 1);
        assert_eq!(out[0].kind, ViolationKind::MinVias);
        assert_eq!(out[0].item_a, "U_MISSING");
    }
}
