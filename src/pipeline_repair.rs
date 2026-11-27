use std::collections::HashSet;
use std::time::Instant;

use crate::drc::{
    DEFAULT_CLUSTER_RADIUS_NM, Hotspot, Severity, Violation, ViolationKind, check_board_drc,
};
use crate::drc_cluster::{cluster_hotspots, routable_board_nets};
use crate::drc_constraints::count_length_violations;
use crate::geom::{local_to_board, point_to_segment_dist};
use crate::ir::{BoardIr, Nm, Pt};
use crate::route::{RouteOptions, RouteSummary, apply_match_group_tuning, route_board_ir};

const BASE_REROUTE_PAD_MM: f64 = 2.0;
const REROUTE_PAD_STEP_MM: f64 = 2.0;
const NEIGHBOR_RADIUS_STEP_NM: Nm = DEFAULT_CLUSTER_RADIUS_NM;

fn is_repairable_kind(kind: ViolationKind) -> bool {
    matches!(
        kind,
        ViolationKind::Clearance
            | ViolationKind::SolderMaskToCopper
            | ViolationKind::HoleToHole
            | ViolationKind::DrillToCopper
            | ViolationKind::EdgeClearance
            | ViolationKind::Keepout
            | ViolationKind::ViaAtSmd
            | ViolationKind::OpenNet
            | ViolationKind::TrackDangling
            | ViolationKind::ViaDangling
            | ViolationKind::MinTraceWidth
    )
}

fn is_unlocked_routed_item(board: &BoardIr, item: &str) -> bool {
    if let Some(idx) = item
        .strip_prefix("track#")
        .and_then(|s| s.parse::<usize>().ok())
    {
        return board.tracks.get(idx).is_some_and(|t| !t.locked);
    }
    if let Some(idx) = item
        .strip_prefix("via#")
        .and_then(|s| s.parse::<usize>().ok())
    {
        return board.vias.get(idx).is_some_and(|v| !v.locked);
    }
    false
}

struct RepairEval {
    repairable: Vec<Violation>,
    score: (usize, usize),
}

fn eval_repair_state(
    board: &BoardIr,
    ignored: &HashSet<String>,
    unconnected_pairs: usize,
) -> RepairEval {
    let drc = check_board_drc(board);
    let pre_zone = board.filled_pours.is_empty();
    let pour_nets: HashSet<&str> = if pre_zone {
        board
            .pours
            .iter()
            .filter(|p| !p.net.is_empty())
            .map(|p| p.net.as_str())
            .collect()
    } else {
        HashSet::new()
    };
    let repairable: Vec<Violation> = drc
        .violations
        .into_iter()
        .filter(|v| {
            if v.severity != Severity::Error || !is_repairable_kind(v.kind) {
                return false;
            }
            let skip_conn = v.net_a.is_empty()
                || board.is_net_ignored(&v.net_a, ignored)
                || (pre_zone && pour_nets.contains(v.net_a.as_str()));
            if v.kind == ViolationKind::OpenNet {
                return !skip_conn;
            }
            if matches!(
                v.kind,
                ViolationKind::TrackDangling | ViolationKind::ViaDangling
            ) && skip_conn
            {
                return false;
            }
            is_unlocked_routed_item(board, &v.item_a)
                || v.item_b
                    .as_deref()
                    .is_some_and(|b| is_unlocked_routed_item(board, b))
        })
        .collect();
    let score = (unconnected_pairs, repairable.len());
    RepairEval { repairable, score }
}

fn net_can_reroute(board: &BoardIr, net: &str, open_nets: &HashSet<&str>) -> bool {
    if open_nets.contains(net) {
        return true;
    }
    let has_unlocked = board.tracks.iter().any(|t| !t.locked && t.net == net)
        || board.vias.iter().any(|v| !v.locked && v.net == net);
    if has_unlocked {
        return true;
    }
    let has_locked = board.tracks.iter().any(|t| t.locked && t.net == net)
        || board.vias.iter().any(|v| v.locked && v.net == net);
    // An unrouted net with zero copper can still be routed; a purely locked net cannot.
    !has_locked
}

struct PriorCluster {
    centroid_nm: Pt,
    radius_nm: Nm,
    base_nets: HashSet<String>,
    attempts: usize,
}

impl PriorCluster {
    fn matches(&self, h: &Hotspot, base_nets: &HashSet<String>) -> bool {
        let match_dist = DEFAULT_CLUSTER_RADIUS_NM
            .max(h.radius_nm)
            .max(self.radius_nm) as f64;
        h.centroid_nm.dist(self.centroid_nm) <= match_dist
            || (!base_nets.is_empty() && &self.base_nets == base_nets)
    }
}

fn collect_hotspot_anchors(board: &BoardIr, hotspot: &Hotspot) -> Vec<Pt> {
    let mut anchors = vec![hotspot.centroid_nm];
    let open_pads: HashSet<&str> = hotspot
        .violations
        .iter()
        .filter(|v| v.kind == ViolationKind::OpenNet)
        .flat_map(|v| {
            [Some(v.item_a.as_str()), v.item_b.as_deref()]
                .into_iter()
                .flatten()
                .filter(|s| !s.is_empty())
        })
        .collect();
    if open_pads.is_empty() {
        return anchors;
    }
    for pl in &board.placements {
        let Some(fp) = board.footprints.get(&pl.footprint) else {
            continue;
        };
        for pad in &fp.pads {
            let matched = open_pads.iter().any(|lbl| {
                lbl.strip_prefix(&pl.refdes)
                    .and_then(|rest| rest.strip_prefix('.'))
                    == Some(pad.name.as_str())
            });
            if matched {
                anchors.push(local_to_board(
                    pad.pos, pl.pos.x, pl.pos.y, pl.rot_deg, pl.side,
                ));
            }
        }
    }
    anchors
}

fn collect_hotspot_targets(
    board: &BoardIr,
    eligible_nets: &HashSet<String>,
    base_nets: &HashSet<String>,
    hotspot: &Hotspot,
    neighbor_radius_nm: Nm,
) -> HashSet<String> {
    let mut targets = base_nets.clone();
    if neighbor_radius_nm <= 0 {
        return targets;
    }
    let search_r = (hotspot.radius_nm.max(0) + neighbor_radius_nm) as f64;
    let anchors = collect_hotspot_anchors(board, hotspot);
    for t in &board.tracks {
        if t.locked || t.net.is_empty() || !eligible_nets.contains(&t.net) {
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
        if v.locked || v.net.is_empty() || !eligible_nets.contains(&v.net) {
            continue;
        }
        if anchors.iter().any(|&a| a.dist(v.pos) <= search_r) {
            targets.insert(v.net.clone());
        }
    }
    targets
}

/// Runs `route_board_ir`, optionally followed by bounded hotspot auto-repair and final `tune_match_groups`.
pub(crate) fn route_with_repair(
    board: &mut BoardIr,
    cfg: &RouteOptions,
    repair_passes: usize,
) -> Result<RouteSummary, String> {
    if repair_passes == 0 {
        return route_board_ir(board, cfg);
    }

    let start = Instant::now();
    if !board.pours.is_empty() {
        board.filled_pours.clear();
    }
    let eligible_nets: HashSet<String> = routable_board_nets(board)
        .into_iter()
        .filter(|n| !board.is_net_ignored(n, &cfg.ignored_nets))
        .map(str::to_string)
        .collect();
    let scope_nets: Option<HashSet<String>> = cfg.reroute_nets.as_ref().map(|scope| {
        eligible_nets
            .iter()
            .filter(|net| {
                let class_name = board
                    .nets
                    .get(net.as_str())
                    .map_or("Default", |d| d.class.as_str());
                scope.contains(net.as_str()) || scope.contains(class_name)
            })
            .cloned()
            .collect()
    });

    let mut pass_cfg = cfg.clone();
    pass_cfg.tune_match_groups = false;
    let mut summary = route_board_ir(board, &pass_cfg)?;
    let mut passes_run = summary.passes_run;
    let mut starts_run = summary.starts_run;
    let mut stopped_early = summary.stopped_early;
    let mut repair_passes_run = 0usize;
    let mut repaired_nets: HashSet<String> = HashSet::new();
    let mut history: Vec<PriorCluster> = Vec::new();
    let mut cur_eval = eval_repair_state(board, &cfg.ignored_nets, summary.unconnected_pairs);

    for _ in 0..repair_passes {
        if cur_eval.repairable.is_empty()
            || stopped_early
            || cfg
                .timeout_secs
                .is_some_and(|limit| start.elapsed().as_secs() >= limit)
        {
            break;
        }
        let hotspots =
            cluster_hotspots(Some(board), &cur_eval.repairable, DEFAULT_CLUSTER_RADIUS_NM);
        let open_nets: HashSet<&str> = cur_eval
            .repairable
            .iter()
            .filter(|v| v.kind == ViolationKind::OpenNet && !v.net_a.is_empty())
            .map(|v| v.net_a.as_str())
            .collect();

        let candidates: Vec<(usize, HashSet<String>, usize)> = hotspots
            .iter()
            .enumerate()
            .filter_map(|(idx, h)| {
                let base_nets: HashSet<String> = h
                    .affected_nets
                    .iter()
                    .filter(|n| {
                        eligible_nets.contains(n.as_str()) && net_can_reroute(board, n, &open_nets)
                    })
                    .cloned()
                    .collect();
                if base_nets.is_empty()
                    || scope_nets
                        .as_ref()
                        .is_some_and(|scope| !base_nets.iter().any(|n| scope.contains(n)))
                {
                    return None;
                }
                let prev_attempts = history
                    .iter()
                    .filter(|p| p.matches(h, &base_nets))
                    .map(|p| p.attempts)
                    .max()
                    .unwrap_or(0);
                Some((idx, base_nets, prev_attempts))
            })
            .collect();

        let Some(&(h_idx, ref base_nets, prev_attempts)) = candidates
            .iter()
            .find(|(_, _, prev)| *prev < 2)
            .or_else(|| candidates.iter().min_by_key(|(idx, _, prev)| (*prev, *idx)))
        else {
            break;
        };

        let hotspot = &hotspots[h_idx];
        let attempt = prev_attempts + 1;
        let neighbor_radius_nm = ((attempt - 1) as Nm) * NEIGHBOR_RADIUS_STEP_NM;
        let base_pad_mm = cfg.reroute_pad_mm.unwrap_or(BASE_REROUTE_PAD_MM);
        let pad_mm = base_pad_mm + ((attempt - 1) as f64) * REROUTE_PAD_STEP_MM;
        let targets = collect_hotspot_targets(
            board,
            &eligible_nets,
            base_nets,
            hotspot,
            neighbor_radius_nm,
        );

        repair_passes_run += 1;
        let mut matched = false;
        for prior in &mut history {
            if prior.matches(hotspot, base_nets) {
                prior.attempts = prior.attempts.max(attempt);
                prior.centroid_nm = hotspot.centroid_nm;
                prior.radius_nm = prior.radius_nm.max(hotspot.radius_nm);
                prior.base_nets.extend(base_nets.iter().cloned());
                matched = true;
            }
        }
        if !matched {
            history.push(PriorCluster {
                centroid_nm: hotspot.centroid_nm,
                radius_nm: hotspot.radius_nm,
                base_nets: base_nets.clone(),
                attempts: attempt,
            });
        }

        let saved_tracks = board.tracks.clone();
        let saved_vias = board.vias.clone();
        pass_cfg.reroute_nets = Some(targets.clone());
        pass_cfg.reroute_pad_mm = Some(pad_mm);
        pass_cfg.timeout_secs = cfg
            .timeout_secs
            .map(|limit| limit.saturating_sub(start.elapsed().as_secs()).max(1));

        let cand_summary = route_board_ir(board, &pass_cfg)?;
        passes_run += cand_summary.passes_run;
        starts_run += cand_summary.starts_run;
        stopped_early |= cand_summary.stopped_early;
        let cand_eval = eval_repair_state(board, &cfg.ignored_nets, cand_summary.unconnected_pairs);

        if cand_eval.score < cur_eval.score {
            summary = cand_summary;
            repaired_nets.extend(targets);
            cur_eval = cand_eval;
        } else {
            board.tracks = saved_tracks;
            board.vias = saved_vias;
        }
    }

    if cfg.tune_match_groups {
        let only_nets = cfg.reroute_nets.clone().map(|mut s| {
            s.extend(repaired_nets.iter().cloned());
            for rule in &board.constraints.match_groups {
                if rule.nets.iter().any(|n| repaired_nets.contains(n)) {
                    s.extend(rule.nets.iter().cloned());
                }
            }
            s
        });
        apply_match_group_tuning(board, &mut summary, only_nets, &cfg.ignored_nets);
    } else if !board.constraints.match_groups.is_empty()
        || !board.constraints.max_lengths.is_empty()
    {
        summary.length_violations = count_length_violations(board);
    }

    summary.passes_run = passes_run;
    summary.starts_run = starts_run;
    summary.stopped_early = stopped_early;
    summary.repair_passes_run = repair_passes_run;
    summary.elapsed_ms = start.elapsed().as_millis();
    Ok(summary)
}
