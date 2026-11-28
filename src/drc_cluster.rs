use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet};

use crate::drc::{Hotspot, Severity, Violation, ViolationKind};
use crate::erc::{is_unconnected_net, net_def_pin_index};
use crate::geom::{
    EPS_NM, GridQueryScratch, SpatialGrid, UnionFind, pad_copper_layers, pad_to_world_shape,
};
use crate::ir::{BBoxNm, BoardIr, Nm, PadDef, PadKind, Pt};

fn is_local_item(item: &str) -> bool {
    !item.is_empty()
        && item != "board_edge"
        && !item.starts_with("cutout#")
        && !item.starts_with("pour#")
        && !item.starts_with("keepout#")
        && !item.starts_with("net:")
        && !item.starts_with("match_group:")
        && !item.contains(".keepout")
}

fn is_point_local_item(item: &str) -> bool {
    is_local_item(item)
        && !item.starts_with("track#")
        && !item.starts_with("silk#")
        && !item.starts_with("mask#")
}

fn violation_items(v: &Violation) -> impl Iterator<Item = (&str, &str)> {
    let a = (!v.item_a.is_empty()).then_some((v.item_a.as_str(), v.net_a.as_str()));
    let b = v
        .item_b
        .as_deref()
        .filter(|s| !s.is_empty())
        .map(|s| (s, v.net_b.as_str()));
    a.into_iter().chain(b)
}

fn build_action_hint(
    root_item: &str,
    root_net: &str,
    root_item_has_net: bool,
    affected_nets: &[String],
    dominant_kind: ViolationKind,
    count: usize,
) -> String {
    let net_suffix = if root_net.is_empty() || !root_item_has_net {
        String::new()
    } else {
        format!(" (net {root_net})")
    };
    let others: Vec<&str> = affected_nets
        .iter()
        .map(String::as_str)
        .filter(|n| !n.is_empty() && (!root_item_has_net || *n != root_net))
        .collect();
    match dominant_kind {
        ViolationKind::Clearance | ViolationKind::DrillToCopper => {
            if others.is_empty() {
                format!(
                    "Reposition or resize {root_item}{net_suffix} to resolve {count} clearance violations"
                )
            } else {
                format!(
                    "Reposition or resize {root_item}{net_suffix} to restore clearance to {}",
                    others.join(", ")
                )
            }
        }
        ViolationKind::SolderMaskToCopper => {
            if others.is_empty() {
                format!(
                    "Move copper or adjust mask expansion around {root_item}{net_suffix} to restore mask-to-copper clearance"
                )
            } else {
                format!(
                    "Move copper or adjust mask expansion between {root_item}{net_suffix} and {}",
                    others.join(", ")
                )
            }
        }
        ViolationKind::HoleToHole | ViolationKind::MinAnnularRing => {
            format!("Adjust drill size or move {root_item}{net_suffix} to satisfy drill rules")
        }
        ViolationKind::MinTraceWidth => {
            format!("Widen trace {root_item}{net_suffix} to meet minimum trace width")
        }
        ViolationKind::EdgeClearance | ViolationKind::ComponentToEdge => {
            format!("Move {root_item}{net_suffix} inward from board/cutout edge")
        }
        ViolationKind::SilkOverCopper => {
            format!(
                "Adjust silkscreen around {root_item}{net_suffix} to clear exposed mask openings"
            )
        }
        ViolationKind::SilkEdgeClearance => {
            format!("Move silkscreen {root_item} inward from board/cutout edge")
        }
        ViolationKind::Keepout => format!("Move {root_item}{net_suffix} outside keepout area"),
        ViolationKind::ViaAtSmd => format!("Offset via {root_item}{net_suffix} outside SMD pad"),
        ViolationKind::CourtyardOverlap | ViolationKind::BgaShadow => {
            format!("Separate {root_item} from overlapping placement")
        }
        ViolationKind::OpenNet => format!("Route missing connection on {root_item}{net_suffix}"),
        ViolationKind::MaxLength | ViolationKind::MatchGroup => {
            format!("Retune trace length for {root_item}{net_suffix}")
        }
        ViolationKind::CapProximity => {
            format!("Place decoupling capacitor {root_item} closer to target IC")
        }
        ViolationKind::MinVias => format!("Add vias on {root_item}{net_suffix}"),
        ViolationKind::MaxVias => format!("Reduce vias on {root_item}{net_suffix}"),
        ViolationKind::TrackDangling => {
            format!("Trim or connect dangling track {root_item}{net_suffix}")
        }
        ViolationKind::ViaDangling => {
            format!("Connect or remove dangling via {root_item}{net_suffix}")
        }
    }
}

pub(crate) fn routable_board_nets(board: &BoardIr) -> HashSet<&str> {
    let cu_layers = board.copper_layer_names();
    let net_def_pins = net_def_pin_index(board);
    let mut pad_counts: HashMap<&str, usize> = HashMap::new();

    for pl in &board.placements {
        let Some(fp) = board.footprints.get(&pl.footprint) else {
            continue;
        };
        struct PinCopper<'a> {
            net: &'a str,
            pads: Vec<(&'a PadDef, Vec<String>)>,
        }
        let mut pin_pads: HashMap<&str, PinCopper<'_>> = HashMap::new();
        for pad in &fp.pads {
            if pad.kind == PadKind::Npth || pad.name.is_empty() {
                continue;
            }
            let cu = pad_copper_layers(pad, pl.side, &cu_layers);
            if cu.is_empty() {
                continue;
            }
            let Some(net) = pl
                .pin_nets
                .get(&pad.name)
                .map(String::as_str)
                .or_else(|| {
                    net_def_pins
                        .get(&(pl.refdes.as_str(), pad.name.as_str()))
                        .copied()
                })
                .filter(|s| !s.trim().is_empty())
            else {
                continue;
            };
            pin_pads
                .entry(pad.name.as_str())
                .or_insert_with(|| PinCopper {
                    net,
                    pads: Vec::new(),
                })
                .pads
                .push((pad, cu));
        }
        for PinCopper { net, pads } in pin_pads.into_values() {
            let count = if pads.len() <= 1 {
                1
            } else {
                let shapes: Vec<_> = pads
                    .iter()
                    .map(|(pad, _)| pad_to_world_shape(pad, pl, 16))
                    .collect();
                let separated = (0..pads.len()).any(|i| {
                    ((i + 1)..pads.len()).any(|j| {
                        !pads[i].1.iter().any(|l| pads[j].1.contains(l))
                            || shapes[i].dist_to_shape(&shapes[j]) > EPS_NM
                    })
                });
                if separated { 2 } else { 1 }
            };
            *pad_counts.entry(net).or_insert(0) += count;
        }
    }

    pad_counts
        .into_iter()
        .filter(|(net, count)| {
            *count >= 2 && !board.nets.get(*net).is_some_and(|nd| nd.ignore_route)
        })
        .map(|(net, _)| net)
        .collect()
}

pub(crate) fn cluster_hotspots(
    board: Option<&BoardIr>,
    violations: &[Violation],
    cluster_radius_nm: Nm,
) -> Vec<Hotspot> {
    if violations.is_empty() {
        return Vec::new();
    }
    let routable = board.map(routable_board_nets);
    let is_routable = |net: &str| -> bool {
        match &routable {
            Some(set) => set.contains(net),
            None => !net.is_empty() && !is_unconnected_net(net),
        }
    };
    let n = violations.len();
    let mut uf = UnionFind::new(n);
    let r_nm = cluster_radius_nm.max(0);
    let r_f = r_nm as f64;
    let grid = SpatialGrid::new(
        violations.iter().map(|v| BBoxNm::from_pt(v.pos_nm, 0)),
        r_nm.max(250_000),
    );
    let mut scratch = GridQueryScratch::default();
    let mut cands = Vec::new();
    for i in 0..n {
        let pi = violations[i].pos_nm;
        grid.query_with(BBoxNm::from_pt(pi, r_nm), &mut scratch, &mut cands);
        for &j in &cands {
            if j > i && uf.find(i) != uf.find(j) && pi.dist(violations[j].pos_nm) <= r_f {
                uf.union(i, j);
            }
        }
    }

    let track_limit = (cluster_radius_nm.max(1_000_000) as f64) * 4.0;
    let track_limit_nm = track_limit.ceil() as Nm;
    let mut by_item: BTreeMap<&str, Vec<usize>> = BTreeMap::new();
    for (i, v) in violations.iter().enumerate() {
        for (item, _) in violation_items(v) {
            if is_local_item(item) {
                by_item.entry(item).or_default().push(i);
            }
        }
    }
    for (item, idxs) in by_item {
        if idxs.len() <= 1 {
            continue;
        }
        if is_point_local_item(item) {
            let first = idxs[0];
            for &j in &idxs[1..] {
                uf.union(first, j);
            }
        } else if idxs.len() <= 32 {
            for a in 0..idxs.len() {
                for b in (a + 1)..idxs.len() {
                    let (ia, ib) = (idxs[a], idxs[b]);
                    if uf.find(ia) != uf.find(ib)
                        && violations[ia].pos_nm.dist(violations[ib].pos_nm) <= track_limit
                    {
                        uf.union(ia, ib);
                    }
                }
            }
        } else {
            let sub_grid = SpatialGrid::new(
                idxs.iter()
                    .map(|&vi| BBoxNm::from_pt(violations[vi].pos_nm, 0)),
                track_limit_nm.max(250_000),
            );
            for (loc_a, &ia) in idxs.iter().enumerate() {
                let pa = violations[ia].pos_nm;
                sub_grid.query_with(
                    BBoxNm::from_pt(pa, track_limit_nm),
                    &mut scratch,
                    &mut cands,
                );
                for &loc_b in &cands {
                    if loc_b <= loc_a {
                        continue;
                    }
                    let ib = idxs[loc_b];
                    if uf.find(ia) != uf.find(ib) && pa.dist(violations[ib].pos_nm) <= track_limit {
                        uf.union(ia, ib);
                    }
                }
            }
        }
    }

    let mut groups: BTreeMap<usize, Vec<usize>> = BTreeMap::new();
    for i in 0..n {
        groups.entry(uf.find(i)).or_default().push(i);
    }

    let mut hotspots = Vec::with_capacity(groups.len());
    for (_, idxs) in groups {
        let mut sum_x: i128 = 0;
        let mut sum_y: i128 = 0;
        let mut err_count = 0usize;
        let mut warn_count = 0usize;
        let mut layers: BTreeSet<String> = BTreeSet::new();
        let mut kinds_set: BTreeSet<ViolationKind> = BTreeSet::new();
        let mut kind_counts: BTreeMap<ViolationKind, (bool, usize)> = BTreeMap::new();
        let mut item_counts: HashMap<String, (usize, BTreeMap<String, usize>)> = HashMap::new();
        let mut net_counts: BTreeMap<String, usize> = BTreeMap::new();
        let mut cluster_violations = Vec::with_capacity(idxs.len());

        for &idx in &idxs {
            let v = &violations[idx];
            sum_x += v.pos_nm.x as i128;
            sum_y += v.pos_nm.y as i128;
            match v.severity {
                Severity::Error => err_count += 1,
                Severity::Warning => warn_count += 1,
            }
            if !v.layer.is_empty() {
                layers.insert(v.layer.clone());
            }
            kinds_set.insert(v.kind);
            let entry = kind_counts.entry(v.kind).or_insert((false, 0));
            entry.0 |= v.severity == Severity::Error;
            entry.1 += 1;
            for (item, net) in violation_items(v) {
                let entry = item_counts
                    .entry(item.to_string())
                    .or_insert_with(|| (0, BTreeMap::new()));
                entry.0 += 1;
                if !net.is_empty() {
                    *entry.1.entry(net.to_string()).or_insert(0) += 1;
                }
            }
            for net in [&v.net_a, &v.net_b] {
                if !net.is_empty() {
                    *net_counts.entry(net.clone()).or_insert(0) += 1;
                }
            }
            cluster_violations.push(v.clone());
        }

        let cnt = idxs.len() as i128;
        let centroid_nm = Pt::new((sum_x / cnt) as Nm, (sum_y / cnt) as Nm);
        let radius_nm = cluster_violations
            .iter()
            .map(|v| v.pos_nm.dist(centroid_nm))
            .fold(0.0_f64, f64::max)
            .round() as Nm;

        let (root_cause_item, item_nets) = item_counts
            .into_iter()
            .max_by(|(ka, (ca, _)), (kb, (cb, _))| {
                (is_local_item(ka), *ca)
                    .cmp(&(is_local_item(kb), *cb))
                    .then_with(|| kb.cmp(ka))
            })
            .map(|(item, (_, n_map))| (item, n_map))
            .unwrap_or_default();
        let root_cause_net = item_nets
            .iter()
            .map(|(n, &c)| (n, c, true))
            .chain(net_counts.iter().map(|(n, &c)| (n, c, false)))
            .max_by_key(|&(n, c, from_item)| {
                (is_routable(n), from_item, c, std::cmp::Reverse(n.as_str()))
            })
            .map(|(n, _, _)| n.clone())
            .unwrap_or_default();
        let root_item_has_net = item_nets.contains_key(&root_cause_net);

        let dominant_kind = kind_counts
            .into_iter()
            .max_by_key(|&(_, (has_err, count))| (has_err, count))
            .map(|(k, _)| k)
            .unwrap_or(cluster_violations[0].kind);
        let affected_nets: Vec<String> =
            net_counts.into_keys().filter(|n| is_routable(n)).collect();
        let action_hint = build_action_hint(
            &root_cause_item,
            &root_cause_net,
            root_item_has_net,
            &affected_nets,
            dominant_kind,
            cluster_violations.len(),
        );

        hotspots.push(Hotspot {
            centroid_nm,
            radius_nm,
            violation_count: cluster_violations.len(),
            error_count: err_count,
            warning_count: warn_count,
            root_cause_item,
            root_cause_net,
            affected_nets,
            layers: layers.into_iter().collect(),
            kinds: kinds_set.into_iter().collect(),
            action_hint,
            violations: cluster_violations,
        });
    }

    hotspots.sort_by(|a, b| {
        b.error_count
            .cmp(&a.error_count)
            .then_with(|| b.violation_count.cmp(&a.violation_count))
            .then_with(|| a.centroid_nm.x.cmp(&b.centroid_nm.x))
            .then_with(|| a.centroid_nm.y.cmp(&b.centroid_nm.y))
            .then_with(|| a.root_cause_item.cmp(&b.root_cause_item))
            .then_with(|| a.root_cause_net.cmp(&b.root_cause_net))
    });
    hotspots
}

#[cfg(test)]
mod tests {
    use super::*;

    fn mk_violation(pos: Pt, item_a: &str, item_b: Option<&str>) -> Violation {
        Violation {
            kind: ViolationKind::Clearance,
            severity: Severity::Error,
            layer: "F.Cu".into(),
            pos_nm: pos,
            measured_nm: 50_000,
            required_nm: 150_000,
            item_a: item_a.into(),
            item_b: item_b.map(str::to_string),
            net_a: "N1".into(),
            net_b: "N2".into(),
            message: "test".into(),
        }
    }

    #[test]
    fn cluster_hotspots_spatial_point_local_and_track_limit() {
        let mut violations = vec![
            // Two spatially close violations (within 1mm radius).
            mk_violation(Pt::new(0, 0), "via#0", Some("U1.1")),
            mk_violation(Pt::new(800_000, 0), "via#1", Some("U1.2")),
            // Far away violation sharing point-local item "via#0" merges into cluster 0.
            mk_violation(Pt::new(20_000_000, 0), "via#0", None),
            // Two track#0 violations within 4*radius (4mm) merge together, third at 10mm stays separate.
            mk_violation(Pt::new(0, 20_000_000), "track#0", Some("U2.1")),
            mk_violation(Pt::new(3_500_000, 20_000_000), "track#0", Some("U2.2")),
            mk_violation(Pt::new(10_000_000, 20_000_000), "track#0", Some("U2.3")),
        ];
        // Also exercise > 32 violations sharing one track (sub_grid path).
        for k in 0..40 {
            violations.push(mk_violation(
                Pt::new(50_000_000 + k * 2_000_000, 50_000_000),
                "track#99",
                None,
            ));
        }
        let hotspots = cluster_hotspots(None, &violations, 1_000_000);
        assert_eq!(hotspots.len(), 4);
        assert_eq!(hotspots[0].violation_count, 40);
        assert_eq!(hotspots[1].violation_count, 3);
        assert_eq!(hotspots[2].violation_count, 2);
        assert_eq!(hotspots[3].violation_count, 1);
    }
}
