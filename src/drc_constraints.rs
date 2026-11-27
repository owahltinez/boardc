use std::collections::{BTreeMap, BTreeSet};

use crate::drc::{Severity, Violation, ViolationKind};
use crate::drc_scene::PadItem;
use crate::geom::{
    EPS_NM, courtyard_world_polygon, point_in_polygon, polygon_to_polygon_dist,
    polygon_violates_clearance,
};
use crate::ir::{BBoxNm, BoardIr, CapProximityRule, MinViasRule, Nm, Pt};

pub(crate) fn is_passive_component(board: &BoardIr, refdes: &str, fp_name: &str) -> bool {
    let Some(fp) = board.footprints.get(fp_name) else {
        return false;
    };
    if fp.pads.len() > 2 {
        return false;
    }
    let u = refdes.to_ascii_uppercase();
    u.starts_with('R') || u.starts_with('C') || u.starts_with('L') || u.starts_with("FB")
}

pub(crate) fn cap_proximity_dist(
    board: &BoardIr,
    pads: &[PadItem],
    rule: &CapProximityRule,
) -> Option<(usize, usize, f64)> {
    let (cap_idx, cap) = board
        .placements
        .iter()
        .enumerate()
        .find(|(_, p)| p.refdes == rule.cap_refdes)?;
    let (ic_idx, ic) = board
        .placements
        .iter()
        .enumerate()
        .find(|(_, p)| p.refdes == rule.ic_refdes)?;
    let pts_for = |pl_idx: usize, fallback: Pt| -> Vec<Pt> {
        if rule.net.is_empty() {
            return vec![fallback];
        }
        let v: Vec<Pt> = pads
            .iter()
            .filter(|p| p.pl_idx == pl_idx && p.net == rule.net)
            .map(|p| p.center)
            .collect();
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
    Some((cap_idx, ic_idx, min_dist))
}

pub(crate) fn count_rule_vias(
    board: &BoardIr,
    pads: &[PadItem],
    rule: &MinViasRule,
) -> (usize, Pt) {
    if let Some(refdes) = &rule.refdes {
        let Some((pl_idx, pl)) = board
            .placements
            .iter()
            .enumerate()
            .find(|(_, p)| &p.refdes == refdes)
        else {
            return (0, Pt::default());
        };
        let cy = board
            .footprints
            .get(&pl.footprint)
            .map(|fp| courtyard_world_polygon(fp, pl))
            .unwrap_or_default();
        let pad_pts: BTreeSet<(Nm, Nm)> = pads
            .iter()
            .filter(|p| p.pl_idx == pl_idx && p.net == rule.net)
            .map(|p| (p.center.x, p.center.y))
            .collect();
        let c = board
            .vias
            .iter()
            .filter(|v| {
                v.net == rule.net
                    && (point_in_polygon(v.pos, &cy)
                        || pad_pts
                            .iter()
                            .any(|&(px, py)| v.pos.dist(Pt::new(px, py)) <= 500_000.0))
            })
            .count();
        (c, pl.pos)
    } else {
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

pub(crate) fn check_length_constraints(
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

    check_cap_proximity(board, pads, out);

    for rule in &board.constraints.min_vias {
        let (count, mut pos) = count_rule_vias(board, pads, rule);
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

    for rule in &board.constraints.bga_shadows {
        let Some(bga) = board
            .placements
            .iter()
            .find(|p| p.refdes == rule.bga_refdes)
        else {
            continue;
        };
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
