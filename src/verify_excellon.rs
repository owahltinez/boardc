use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::fs;
use std::path::Path;

use crate::geom::{UnionFind, local_to_board, pad_copper_layers, pad_to_world_shape};
use crate::ir::{BoardIr, Nm, PadKind, Pt};
use crate::poly_index::IndexedPour;
use crate::shapes::ResolvedShape;
use crate::verify::ConnectivityCheck;

pub(crate) struct GerberIsland {
    pub(crate) layer: String,
    pub(crate) indexed: IndexedPour,
}

pub(crate) type DrillHits = Vec<(Nm, Pt)>;

fn parse_excellon_coord(token: &str, metric: bool) -> Result<Nm, String> {
    let scale = if metric { 1_000_000.0 } else { 25_400_000.0 };
    if token.contains('.') {
        let val: f64 = token
            .parse()
            .map_err(|e| format!("invalid Excellon coord '{token}': {e}"))?;
        return Ok((val * scale).round() as Nm);
    }
    let sign = if token.starts_with('-') { -1 } else { 1 };
    let digits = token.trim_start_matches(['+', '-']);
    if digits.is_empty() {
        return Err("empty Excellon coordinate".into());
    }
    let raw: i64 = digits
        .parse()
        .map_err(|e| format!("invalid Excellon digits '{token}': {e}"))?;
    let val = raw as f64 / 10_000.0;
    Ok(sign * (val * scale).round() as Nm)
}

pub(crate) fn parse_excellon_file(path: &Path) -> Result<DrillHits, String> {
    let text = fs::read_to_string(path)
        .map_err(|e| format!("failed to read Excellon '{}': {e}", path.display()))?;
    let mut metric = true;
    let mut in_body = false;
    let mut tools: HashMap<u32, Nm> = HashMap::new();
    let mut cur_tool: Option<u32> = None;
    let mut hits = Vec::new();

    for raw_line in text.lines() {
        let line = raw_line.trim();
        if line.is_empty() || line.starts_with(';') {
            continue;
        }
        if line == "%" {
            in_body = true;
            continue;
        }
        if line.starts_with("METRIC") {
            metric = true;
            continue;
        }
        if line.starts_with("INCH") {
            metric = false;
            continue;
        }
        if line == "M30" {
            break;
        }
        if let Some(rest) = line.strip_prefix('T') {
            if let Some(c_pos) = rest.find('C') {
                let tid: u32 = rest[..c_pos]
                    .parse()
                    .map_err(|e| format!("invalid tool id in '{line}': {e}"))?;
                let dia_val: f64 = rest[c_pos + 1..]
                    .split(|ch: char| !ch.is_ascii_digit() && ch != '.')
                    .next()
                    .unwrap_or("")
                    .parse()
                    .map_err(|e| format!("invalid tool diameter in '{line}': {e}"))?;
                let scale = if metric { 1_000_000.0 } else { 25_400_000.0 };
                tools.insert(tid, (dia_val * scale).round() as Nm);
            } else if !rest.is_empty() && rest.chars().all(|c| c.is_ascii_digit()) {
                let tid: u32 = rest
                    .parse()
                    .map_err(|e| format!("invalid tool select '{line}': {e}"))?;
                if tid == 0 {
                    cur_tool = None;
                } else {
                    if !tools.contains_key(&tid) {
                        return Err(format!("undefined tool T{tid} in '{}'", path.display()));
                    }
                    cur_tool = Some(tid);
                    in_body = true;
                }
            }
            continue;
        }
        if in_body
            && line.starts_with('X')
            && let Some(y_pos) = line.find('Y')
        {
            let tid = cur_tool.ok_or_else(|| {
                format!(
                    "drill hit '{line}' before tool selection in '{}'",
                    path.display()
                )
            })?;
            let dia = tools[&tid];
            let x = parse_excellon_coord(&line[1..y_pos], metric)?;
            let y = parse_excellon_coord(&line[y_pos + 1..], metric)?;
            hits.push((dia, Pt::new(x, y)));
        }
    }
    Ok(hits)
}

pub(crate) fn match_drill_lists(
    expected: &[(Nm, Pt)],
    parsed: &[(Nm, Pt)],
    tol_nm: Nm,
) -> (usize, usize) {
    let mut used = vec![false; parsed.len()];
    let mut matched = 0usize;
    let mut mismatches = 0usize;
    for &(ed, ep) in expected {
        if let Some((idx, _)) = parsed
            .iter()
            .enumerate()
            .filter(|&(i, &(pd, pp))| {
                !used[i] && (pd - ed).abs() <= tol_nm && ep.dist(pp) <= tol_nm as f64
            })
            .min_by(|(_, a), (_, b)| ep.dist(a.1).total_cmp(&ep.dist(b.1)))
        {
            used[idx] = true;
            matched += 1;
        } else {
            mismatches += 1;
        }
    }
    let extra = used.iter().filter(|u| !**u).count();
    (matched, mismatches + extra)
}

pub(crate) struct ExpectedDrills {
    pub(crate) pth_through: DrillHits,
    pub(crate) npth: DrillHits,
    pub(crate) pth_spans: BTreeMap<(String, String), DrillHits>,
}

pub(crate) fn expected_drills(board: &BoardIr) -> ExpectedDrills {
    let cu_layers = board.copper_layer_names();
    let mut pth_through = Vec::new();
    let mut npth = Vec::new();
    let mut pth_spans: BTreeMap<(String, String), DrillHits> = BTreeMap::new();
    for via in &board.vias {
        if via.drill <= 0 {
            continue;
        }
        if via.is_through_hole(&cu_layers, &board.layers) {
            pth_through.push((via.drill, via.pos));
        } else {
            let span = via.span_layer_pair(&cu_layers, &board.layers);
            pth_spans
                .entry(span)
                .or_default()
                .push((via.drill, via.pos));
        }
    }
    for pl in &board.placements {
        let Some(fp) = board.footprints.get(&pl.footprint) else {
            continue;
        };
        for pad in &fp.pads {
            if let Some(drill) = pad.drill_nm
                && drill > 0
            {
                let world = local_to_board(pad.pos, pl.pos.x, pl.pos.y, pl.rot_deg, pl.side);
                match pad.kind {
                    PadKind::Tht => pth_through.push((drill, world)),
                    PadKind::Npth => npth.push((drill, world)),
                    PadKind::Smd => {}
                }
            }
        }
    }
    ExpectedDrills {
        pth_through,
        npth,
        pth_spans,
    }
}

fn island_near_shape(shape: &ResolvedShape, island: &GerberIsland, tol_nm: f64) -> bool {
    let tol_int = tol_nm.ceil().max(0.0) as Nm;
    island.indexed.dist_to_shape_bounded(shape, tol_int) <= tol_nm
}

fn island_contains_or_near(pt: Pt, island: &GerberIsland, tol_nm: f64) -> bool {
    if island.indexed.contains_pt(pt) {
        return true;
    }
    let shape = ResolvedShape::Circle {
        center: pt,
        radius: 0,
    };
    island_near_shape(&shape, island, tol_nm)
}

pub(crate) fn check_gerber_connectivity(
    board: &BoardIr,
    islands: &[GerberIsland],
    pth_groups: &[(Vec<String>, DrillHits)],
) -> ConnectivityCheck {
    let cu_layers = board.copper_layer_names();
    let mut by_layer: HashMap<&str, Vec<usize>> = HashMap::new();
    for (idx, isl) in islands.iter().enumerate() {
        by_layer.entry(isl.layer.as_str()).or_default().push(idx);
    }

    let mut uf = UnionFind::new(islands.len());
    for (span_layers, hits) in pth_groups {
        for &(dia, pos) in hits {
            let reach = (dia as f64 * 0.5) + 2_000.0;
            let reach_i = reach.ceil() as Nm;
            let mut first: Option<usize> = None;
            for layer in span_layers {
                let Some(idxs) = by_layer.get(layer.as_str()) else {
                    continue;
                };
                for &idx in idxs {
                    let isl = &islands[idx];
                    if isl.indexed.outer.bbox.expand(reach_i).contains_pt(pos)
                        && island_contains_or_near(pos, isl, reach)
                    {
                        match first {
                            Some(f) => uf.union(f, idx),
                            None => first = Some(idx),
                        }
                    }
                }
            }
        }
    }

    let mut comp_nets: HashMap<usize, BTreeSet<String>> = HashMap::new();
    let mut net_pad_comps: BTreeMap<String, Vec<(String, usize)>> = BTreeMap::new();
    let mut missing_pad_copper = Vec::new();

    for pl in &board.placements {
        let Some(fp) = board.footprints.get(&pl.footprint) else {
            continue;
        };
        for pad in &fp.pads {
            if pad.kind == PadKind::Npth {
                continue;
            }
            let shape = pad_to_world_shape(pad, pl, 16);
            let layers = pad_copper_layers(pad, pl.side, &cu_layers);
            let net = pl.pin_nets.get(&pad.name).cloned().unwrap_or_default();
            let label = format!("{}.{}", pl.refdes, pad.name);
            let mut found_comp: Option<usize> = None;

            for layer in &layers {
                let mut layer_hit: Option<usize> = None;
                if let Some(idxs) = by_layer.get(layer.as_str()) {
                    for &idx in idxs {
                        if island_near_shape(&shape, &islands[idx], 5_000.0) {
                            layer_hit = Some(idx);
                            break;
                        }
                    }
                }
                if let Some(idx) = layer_hit {
                    let root = uf.find(idx);
                    if !net.is_empty() {
                        comp_nets.entry(root).or_default().insert(net.clone());
                    }
                    found_comp.get_or_insert(root);
                } else {
                    missing_pad_copper.push(format!("{label} on {layer}"));
                }
            }

            if !net.is_empty()
                && let Some(comp) = found_comp
            {
                net_pad_comps.entry(net).or_default().push((label, comp));
            }
        }
    }

    // Tag track capsules and spanned via layers so padless copper shorts are caught accurately.
    for tr in &board.tracks {
        if tr.net.is_empty() {
            continue;
        }
        let shape = ResolvedShape::Capsule {
            a: tr.a,
            b: tr.b,
            radius: tr.width / 2,
        };
        if let Some(idxs) = by_layer.get(tr.layer.as_str()) {
            for &idx in idxs {
                if island_near_shape(&shape, &islands[idx], 5_000.0) {
                    comp_nets
                        .entry(uf.find(idx))
                        .or_default()
                        .insert(tr.net.clone());
                }
            }
        }
    }
    for via in &board.vias {
        if via.net.is_empty() {
            continue;
        }
        for layer in via.spanned_layers(&cu_layers, &board.layers) {
            let Some(idxs) = by_layer.get(layer.as_str()) else {
                continue;
            };
            for &idx in idxs {
                if island_contains_or_near(via.pos, &islands[idx], 5_000.0) {
                    comp_nets
                        .entry(uf.find(idx))
                        .or_default()
                        .insert(via.net.clone());
                }
            }
        }
    }

    let mut shorts = Vec::new();
    for nets in comp_nets.values() {
        if nets.len() > 1 {
            let list: Vec<&str> = nets.iter().map(String::as_str).collect();
            shorts.push(format!(
                "shorted nets in Gerber copper: {}",
                list.join(" <-> ")
            ));
        }
    }

    let mut opens = Vec::new();
    for (net, pads) in &net_pad_comps {
        if pads.len() <= 1 || board.nets.get(net).is_some_and(|nd| nd.ignore_route) {
            continue;
        }
        let roots: BTreeSet<usize> = pads.iter().map(|(_, c)| uf.find(*c)).collect();
        if roots.len() > 1 {
            opens.push(format!(
                "open net '{net}' across {} disconnected Gerber copper islands ({} pads)",
                roots.len(),
                pads.len()
            ));
        }
    }

    let passed = shorts.is_empty() && opens.is_empty() && missing_pad_copper.is_empty();
    ConnectivityCheck {
        passed,
        island_count: islands.len(),
        checked_nets: net_pad_comps.len(),
        shorts,
        opens,
        missing_pad_copper,
    }
}
