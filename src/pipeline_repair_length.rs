use std::cmp::Reverse;
use std::collections::{HashMap, HashSet};

use crate::drc::DEFAULT_CLUSTER_RADIUS_NM;
use crate::geom::{
    local_to_board, pad_copper_layers, point_to_segment_dist, segment_to_segment_dist,
};
use crate::ir::{BoardIr, Nm, PadKind, Pt, Track};

const CORRIDOR_RADIUS_NM: Nm = DEFAULT_CLUSTER_RADIUS_NM * 2;
const MAX_CORRIDOR_NEIGHBORS: usize = 6;

pub(crate) fn collect_pour_nets(board: &BoardIr) -> HashSet<&str> {
    board
        .pours
        .iter()
        .map(|p| p.net.as_str())
        .chain(board.filled_pours.iter().map(|p| p.net.as_str()))
        .filter(|n| !n.is_empty())
        .collect()
}

fn is_power_net(board: &BoardIr, net: &str) -> bool {
    board.nets.get(net).is_some_and(|nd| {
        if nd.power_driven {
            return true;
        }
        let cls = nd.class.to_ascii_lowercase();
        cls.contains("power") || cls.starts_with("pwr")
    })
}

fn target_pad_points_and_layers(
    board: &BoardIr,
    net: &str,
    cu_layers: &[String],
) -> (Vec<Pt>, HashSet<String>) {
    let mut pts = Vec::new();
    let mut layers = HashSet::new();
    for pl in &board.placements {
        let Some(fp) = board.footprints.get(&pl.footprint) else {
            continue;
        };
        for pad in &fp.pads {
            if pad.kind == PadKind::Npth || pad.name.is_empty() {
                continue;
            }
            let matched = pl
                .pin_nets
                .get(&pad.name)
                .map(String::as_str)
                .is_some_and(|n| n == net)
                || board.nets.get(net).is_some_and(|nd| {
                    nd.pins
                        .iter()
                        .any(|(r, p)| r == &pl.refdes && p == &pad.name)
                });
            if matched {
                pts.push(local_to_board(
                    pad.pos, pl.pos.x, pl.pos.y, pl.rot_deg, pl.side,
                ));
                layers.extend(pad_copper_layers(pad, pl.side, cu_layers));
            }
        }
    }
    (pts, layers)
}

fn mst_corridor_segments(pts: &[Pt]) -> Vec<(Pt, Pt)> {
    let n = pts.len();
    if n < 2 {
        return Vec::new();
    }
    let mut visited = vec![false; n];
    visited[0] = true;
    let mut segs = Vec::with_capacity(n - 1);
    for _ in 1..n {
        let mut best: Option<(f64, usize, usize)> = None;
        for (i, &vi) in visited.iter().enumerate() {
            if !vi {
                continue;
            }
            for (j, &vj) in visited.iter().enumerate() {
                if vj {
                    continue;
                }
                let d2 = pts[i].dist_sq(pts[j]);
                if best.is_none_or(|(bd, _, _)| d2 < bd) {
                    best = Some((d2, i, j));
                }
            }
        }
        let Some((_, i, j)) = best else { break };
        visited[j] = true;
        segs.push((pts[i], pts[j]));
    }
    segs
}

pub(crate) fn collect_corridor_neighbors(
    board: &BoardIr,
    target_net: &str,
    eligible_nets: &HashSet<String>,
    scope_nets: Option<&HashSet<String>>,
) -> HashSet<String> {
    let pour_nets = collect_pour_nets(board);
    let target_tracks: Vec<&Track> = board
        .tracks
        .iter()
        .filter(|t| t.net == target_net)
        .collect();
    let cu_layers = board.copper_layer_names();
    let (pad_pts, mut net_layers) = target_pad_points_and_layers(board, target_net, &cu_layers);
    net_layers.extend(target_tracks.iter().map(|t| t.layer.clone()));
    if net_layers.is_empty() {
        return HashSet::new();
    }
    let constrained_out_of_scope: HashSet<&str> = scope_nets.map_or_else(HashSet::new, |scope| {
        board
            .constraints
            .max_lengths
            .iter()
            .flat_map(|r| r.nets.iter().map(String::as_str))
            .chain(
                board
                    .constraints
                    .max_vias
                    .iter()
                    .flat_map(|r| r.nets.iter().map(String::as_str)),
            )
            .filter(|&n| !scope.contains(n))
            .collect()
    });
    let candidates: HashSet<&str> = eligible_nets
        .iter()
        .map(String::as_str)
        .filter(|&n| {
            n != target_net
                && !pour_nets.contains(n)
                && !is_power_net(board, n)
                && !constrained_out_of_scope.contains(n)
        })
        .collect();
    if candidates.is_empty() {
        return HashSet::new();
    }

    let corridor_segs = mst_corridor_segments(&pad_pts);
    let radius = CORRIDOR_RADIUS_NM as f64;
    let mut scores: HashMap<&str, (f64, f64, f64)> = HashMap::new();

    for t in &board.tracks {
        if t.locked || !candidates.contains(t.net.as_str()) || !net_layers.contains(&t.layer) {
            continue;
        }
        let d_c = corridor_segs
            .iter()
            .map(|&(a, b)| segment_to_segment_dist(t.a, t.b, a, b))
            .fold(f64::INFINITY, f64::min);
        let d_t = target_tracks
            .iter()
            .filter(|tr| tr.layer == t.layer)
            .map(|tr| segment_to_segment_dist(t.a, t.b, tr.a, tr.b))
            .fold(f64::INFINITY, f64::min);
        if d_c.min(d_t) <= radius {
            let e = scores
                .entry(t.net.as_str())
                .or_insert((f64::INFINITY, f64::INFINITY, 0.0));
            e.0 = e.0.min(d_c);
            e.1 = e.1.min(d_t);
            e.2 += t.a.dist(t.b);
        }
    }
    for v in &board.vias {
        if v.locked
            || !candidates.contains(v.net.as_str())
            || !net_layers.iter().any(|l| v.touches_layer(l, &board.layers))
        {
            continue;
        }
        let vr = (v.diameter as f64) * 0.5;
        let d_c = corridor_segs
            .iter()
            .map(|&(a, b)| (point_to_segment_dist(v.pos, a, b) - vr).max(0.0))
            .fold(f64::INFINITY, f64::min);
        let d_t = target_tracks
            .iter()
            .filter(|tr| v.touches_layer(&tr.layer, &board.layers))
            .map(|tr| (point_to_segment_dist(v.pos, tr.a, tr.b) - vr).max(0.0))
            .fold(f64::INFINITY, f64::min);
        if d_c.min(d_t) <= radius {
            let e = scores
                .entry(v.net.as_str())
                .or_insert((f64::INFINITY, f64::INFINITY, 0.0));
            e.0 = e.0.min(d_c);
            e.1 = e.1.min(d_t);
            e.2 += v.diameter as f64;
        }
    }

    let mut ranked: Vec<(&str, Nm, Nm, Reverse<Nm>)> = scores
        .into_iter()
        .map(|(n, (dc, dt, len))| {
            (
                n,
                dc.round() as Nm,
                dt.round() as Nm,
                Reverse(len.round() as Nm),
            )
        })
        .collect();
    ranked.sort_by_key(|&(n, dc, dt, len)| (dc, dt, len, n));
    ranked
        .into_iter()
        .take(MAX_CORRIDOR_NEIGHBORS)
        .map(|(n, ..)| n.to_string())
        .collect()
}
