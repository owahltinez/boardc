use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet};

use serde::Deserialize;

use crate::drc::{Hotspot, Severity, Violation, ViolationKind};
use crate::erc::{is_unconnected_net, net_def_pin_index};
use crate::geom::{EPS_NM, UnionFind, local_to_board, pad_copper_layers, pad_to_world_shape};
use crate::ir::{BoardIr, Nm, PadDef, PadKind, Pt, mm, um};

const KICAD_CLUSTER_RADIUS_NM: Nm = 2_000_000;

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
    is_local_item(item) && !item.starts_with("track#")
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

fn shares_localized_object(a: &Violation, b: &Violation, cluster_radius_nm: Nm) -> bool {
    let dist = a.pos_nm.dist(b.pos_nm);
    let track_limit = (cluster_radius_nm.max(1_000_000) as f64) * 4.0;
    for (ia, _) in violation_items(a) {
        if !is_local_item(ia) {
            continue;
        }
        for (ib, _) in violation_items(b) {
            if ia == ib && (is_point_local_item(ia) || dist <= track_limit) {
                return true;
            }
        }
    }
    false
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
        ViolationKind::TrackDangling => {
            format!("Trim or connect dangling track {root_item}{net_suffix}")
        }
        ViolationKind::ViaDangling => {
            format!("Connect or remove dangling via {root_item}{net_suffix}")
        }
        ViolationKind::Other => {
            format!("Inspect {count} DRC violations around {root_item}{net_suffix}")
        }
    }
}

fn routable_board_nets(board: &BoardIr) -> HashSet<&str> {
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
    let r_f = cluster_radius_nm.max(0) as f64;
    for i in 0..n {
        for j in (i + 1)..n {
            if uf.find(i) == uf.find(j) {
                continue;
            }
            if violations[i].pos_nm.dist(violations[j].pos_nm) <= r_f
                || shares_localized_object(&violations[i], &violations[j], cluster_radius_nm)
            {
                uf.union(i, j);
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
            .unwrap_or(ViolationKind::Other);
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
    });
    hotspots
}

#[derive(Deserialize)]
pub(crate) struct KicadDrcDoc {
    #[serde(default = "default_mm")]
    pub(crate) coordinate_units: String,
    #[serde(default)]
    pub(crate) violations: Vec<KicadViolation>,
    #[serde(default)]
    pub(crate) unconnected_items: Vec<KicadViolation>,
}

fn default_mm() -> String {
    "mm".into()
}

#[derive(Deserialize)]
pub(crate) struct KicadViolation {
    #[serde(default, rename = "type")]
    pub(crate) kind: String,
    #[serde(default)]
    pub(crate) severity: String,
    #[serde(default)]
    pub(crate) description: String,
    #[serde(default)]
    pub(crate) items: Vec<KicadItem>,
}

#[derive(Deserialize)]
pub(crate) struct KicadItem {
    #[serde(default)]
    pub(crate) description: String,
    #[serde(default)]
    pub(crate) pos: Option<KicadPos>,
}

#[derive(Deserialize)]
pub(crate) struct KicadPos {
    pub(crate) x: f64,
    pub(crate) y: f64,
}

pub(crate) fn unit_to_nm(val: f64, units: &str) -> Nm {
    match units {
        "um" | "µm" => um(val),
        "mil" => (val * 25_400.0).round() as Nm,
        "in" => (val * 25_400_000.0).round() as Nm,
        _ => mm(val),
    }
}

fn map_kicad_kind(t: &str) -> ViolationKind {
    match t {
        "clearance" | "shorting_items" | "tracks_crossing" | "zone_to_zone" => {
            ViolationKind::Clearance
        }
        "solder_mask_bridge" | "solder_mask_to_copper" => ViolationKind::SolderMaskToCopper,
        "hole_to_hole" | "hole_near_hole" => ViolationKind::HoleToHole,
        "annular_width" => ViolationKind::MinAnnularRing,
        "track_width" => ViolationKind::MinTraceWidth,
        "copper_edge_clearance" => ViolationKind::EdgeClearance,
        "courtyards_overlap" => ViolationKind::CourtyardOverlap,
        "unconnected_items" => ViolationKind::OpenNet,
        "track_dangling" => ViolationKind::TrackDangling,
        "via_dangling" => ViolationKind::ViaDangling,
        "keepout" | "rule_area" => ViolationKind::Keepout,
        _ => ViolationKind::Other,
    }
}

fn extract_bracket_net(desc: &str) -> String {
    if let (Some(l), Some(r)) = (desc.find('['), desc.find(']'))
        && r > l
    {
        let net = desc[l + 1..r].trim();
        if net.eq_ignore_ascii_case("no net") || net.eq_ignore_ascii_case("<no net>") {
            return String::new();
        }
        return net.to_string();
    }
    String::new()
}

fn extract_layer(desc: &str) -> String {
    if let Some(pos) = desc.find(" on ") {
        let rest = &desc[pos + 4..];
        let end = rest.find([' ', ',']).unwrap_or(rest.len());
        return rest[..end].trim().to_string();
    }
    String::new()
}

fn map_kicad_item_to_board(board: &BoardIr, item: &KicadItem, units: &str) -> (String, Pt, String) {
    let net = extract_bracket_net(&item.description);
    // KiCad coordinates are Y-down; BoardIr canonical coordinates are Y-up.
    let pos = item
        .pos
        .as_ref()
        .map(|p| Pt::new(unit_to_nm(p.x, units), -unit_to_nm(p.y, units)))
        .unwrap_or_default();
    let desc = &item.description;

    // Match explicit Pad "<pin> ... of <refdes>"
    if let Some(rest) = desc.strip_prefix("Pad ")
        && let Some(of_pos) = rest.find(" of ")
    {
        let pin = rest[..rest.find(' ').unwrap_or(of_pos)].trim();
        let after_of = &rest[of_pos + 4..];
        let refdes = after_of
            .split_whitespace()
            .next()
            .unwrap_or("")
            .trim_end_matches(',');
        if !refdes.is_empty() && !pin.is_empty() {
            return (format!("{refdes}.{pin}"), pos, net);
        }
    }

    if desc.starts_with("Via")
        && let Some((vi, v)) = board
            .vias
            .iter()
            .enumerate()
            .filter(|(_, v)| net.is_empty() || v.net == net)
            .min_by(|(_, a), (_, b)| a.pos.dist_sq(pos).total_cmp(&b.pos.dist_sq(pos)))
        && v.pos.dist(pos) <= 300_000.0
    {
        return (format!("via#{vi}"), pos, net);
    }

    if desc.starts_with("Track") {
        let layer = extract_layer(desc);
        if let Some((ti, d)) = board
            .tracks
            .iter()
            .enumerate()
            .filter(|(_, t)| {
                (layer.is_empty() || t.layer == layer) && (net.is_empty() || t.net == net)
            })
            .map(|(i, t)| (i, crate::geom::point_to_segment_dist(pos, t.a, t.b)))
            .min_by(|a, b| a.1.total_cmp(&b.1))
            && d <= 300_000.0
        {
            return (format!("track#{ti}"), pos, net);
        }
    }

    // Fallback spatial match across vias, pads, and tracks.
    let mut best: Option<(f64, String)> = None;
    for (vi, v) in board.vias.iter().enumerate() {
        let d = v.pos.dist(pos);
        if d <= (v.diameter / 2).max(200_000) as f64 && best.as_ref().is_none_or(|b| d < b.0) {
            best = Some((d, format!("via#{vi}")));
        }
    }
    for pl in &board.placements {
        if let Some(fp) = board.footprints.get(&pl.footprint) {
            for pad in &fp.pads {
                let wp = local_to_board(pad.pos, pl.pos.x, pl.pos.y, pl.rot_deg, pl.side);
                let d = wp.dist(pos);
                if d <= 250_000.0 && best.as_ref().is_none_or(|b| d < b.0) {
                    best = Some((d, format!("{}.{}", pl.refdes, pad.name)));
                }
            }
        }
    }
    if let Some((_, label)) = best {
        return (label, pos, net);
    }

    if desc.starts_with("Zone") {
        let layer = extract_layer(desc);
        if let Some((idx, _)) = board.pours.iter().enumerate().find(|(_, p)| {
            (layer.is_empty() || p.layer == layer) && (net.is_empty() || p.net == net)
        }) {
            return (format!("pour#{idx}"), pos, net);
        }
    }

    let fallback = desc
        .split(['(', '['])
        .next()
        .unwrap_or(desc)
        .trim()
        .to_string();
    (fallback, pos, net)
}

fn parse_first_number_before(text: &str, keyword: &str, units: &str) -> Option<Nm> {
    let idx = text.find(keyword)?;
    let tail = text[idx + keyword.len()..].trim_start();
    let num_start = tail.find(|c: char| c.is_ascii_digit() || c == '.' || c == '-')?;
    let after_start = &tail[num_start..];
    let num_len = after_start
        .find(|c: char| !c.is_ascii_digit() && c != '.' && c != '-')
        .unwrap_or(after_start.len());
    let num_str = &after_start[..num_len];
    let val: f64 = num_str.parse().ok()?;
    let unit_tok: String = after_start[num_len..]
        .trim_start()
        .chars()
        .take_while(|c| c.is_alphabetic() || *c == 'µ')
        .collect();
    let unit = match unit_tok.as_str() {
        "um" | "µm" => "um",
        "mil" | "mils" => "mil",
        "mm" => "mm",
        "in" => "in",
        _ => units,
    };
    Some(unit_to_nm(val, unit))
}

pub(crate) fn cluster_kicad_drc_json(
    board: &BoardIr,
    kicad_drc_json: &str,
) -> Result<Vec<Hotspot>, String> {
    let doc: KicadDrcDoc = serde_json::from_str(kicad_drc_json)
        .map_err(|e| format!("failed to parse KiCad DRC JSON: {e}"))?;
    let units = doc.coordinate_units.as_str();
    let mut violations = Vec::new();
    for kv in doc.violations.into_iter().chain(doc.unconnected_items) {
        let kind = map_kicad_kind(&kv.kind);
        let severity = if kv.severity.eq_ignore_ascii_case("warning") {
            Severity::Warning
        } else {
            Severity::Error
        };
        let mapped: Vec<(String, Pt, String)> = kv
            .items
            .iter()
            .map(|it| map_kicad_item_to_board(board, it, units))
            .collect();
        let layer = kv
            .items
            .iter()
            .map(|it| extract_layer(&it.description))
            .find(|l| !l.is_empty())
            .unwrap_or_default();
        let pos_nm = if mapped.is_empty() {
            Pt::default()
        } else {
            let n = mapped.len() as i128;
            Pt::new(
                (mapped.iter().map(|m| m.1.x as i128).sum::<i128>() / n) as Nm,
                (mapped.iter().map(|m| m.1.y as i128).sum::<i128>() / n) as Nm,
            )
        };
        let (item_a, net_a) = mapped
            .first()
            .map(|m| (m.0.clone(), m.2.clone()))
            .unwrap_or_else(|| (kv.kind.clone(), String::new()));
        let (item_b, net_b) = mapped
            .get(1)
            .map(|m| (Some(m.0.clone()), m.2.clone()))
            .unwrap_or((None, String::new()));
        let required_nm = parse_first_number_before(&kv.description, "clearance ", units)
            .or_else(|| parse_first_number_before(&kv.description, "minimum ", units))
            .or_else(|| parse_first_number_before(&kv.description, "min ", units))
            .or_else(|| parse_first_number_before(&kv.description, "min annular width ", units))
            .unwrap_or(0);
        let measured_nm = parse_first_number_before(&kv.description, "actual ", units).unwrap_or(0);
        violations.push(Violation {
            kind,
            severity,
            layer,
            pos_nm,
            measured_nm,
            required_nm,
            item_a,
            item_b,
            net_a,
            net_b,
            message: kv.description,
        });
    }
    Ok(cluster_hotspots(
        Some(board),
        &violations,
        KICAD_CLUSTER_RADIUS_NM,
    ))
}
