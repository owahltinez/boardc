use std::collections::{HashMap, HashSet};
use std::time::Instant;

use crate::drc::{
    DEFAULT_CLUSTER_RADIUS_NM, Hotspot, Severity, Violation, ViolationKind, check_board_drc,
};
use crate::drc_cluster::{cluster_hotspots, routable_board_nets};
use crate::drc_constraints::count_length_violations;
use crate::geom::{local_to_board, point_to_segment_dist};
use crate::ir::{BoardIr, Nm, Pt};
use crate::pipeline_repair_length::{collect_corridor_neighbors, collect_pour_nets};
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
            | ViolationKind::MaxLength
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

struct MaxLengthViol {
    net: String,
    excess_nm: Nm,
}

struct RepairEval {
    repairable: Vec<Violation>,
    max_lengths: Vec<MaxLengthViol>,
    score: (usize, usize, usize, Nm),
}

fn eval_repair_state(
    board: &BoardIr,
    ignored: &HashSet<String>,
    unconnected_pairs: usize,
) -> RepairEval {
    let drc = check_board_drc(board);
    let pour_nets = if board.filled_pours.is_empty() {
        collect_pour_nets(board)
    } else {
        HashSet::new()
    };
    let mut repairable = Vec::new();
    let mut max_lengths = Vec::new();
    for v in drc.violations {
        if v.severity != Severity::Error || !is_repairable_kind(v.kind) {
            continue;
        }
        let net_ignored = v.net_a.is_empty() || board.is_net_ignored(&v.net_a, ignored);
        if v.kind == ViolationKind::MaxLength {
            if !net_ignored && net_can_reroute(board, &v.net_a) {
                max_lengths.push(MaxLengthViol {
                    net: v.net_a,
                    excess_nm: (v.measured_nm - v.required_nm).max(0),
                });
            }
            continue;
        }
        let skip_conn = net_ignored || pour_nets.contains(v.net_a.as_str());
        if v.kind == ViolationKind::OpenNet {
            if !skip_conn {
                repairable.push(v);
            }
            continue;
        }
        if matches!(
            v.kind,
            ViolationKind::TrackDangling | ViolationKind::ViaDangling
        ) && skip_conn
        {
            continue;
        }
        if is_unlocked_routed_item(board, &v.item_a)
            || v.item_b
                .as_deref()
                .is_some_and(|b| is_unlocked_routed_item(board, b))
        {
            repairable.push(v);
        }
    }
    let total_excess_nm = max_lengths
        .iter()
        .fold(0 as Nm, |acc, m| acc.saturating_add(m.excess_nm));
    let score = (
        unconnected_pairs,
        repairable.len(),
        max_lengths.len(),
        total_excess_nm,
    );
    RepairEval {
        repairable,
        max_lengths,
        score,
    }
}

fn net_can_reroute(board: &BoardIr, net: &str) -> bool {
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

struct RepairState<'a> {
    cfg: &'a RouteOptions,
    pass_cfg: RouteOptions,
    start: Instant,
    repair_passes: usize,
    repair_passes_run: usize,
    passes_run: usize,
    starts_run: usize,
    stopped_early: bool,
    summary: RouteSummary,
    repaired_nets: HashSet<String>,
    cur_eval: RepairEval,
}

impl<'a> RepairState<'a> {
    fn can_step(&self) -> bool {
        self.repair_passes_run < self.repair_passes
            && !self.stopped_early
            && self
                .cfg
                .timeout_secs
                .is_none_or(|limit| self.start.elapsed().as_secs() < limit)
    }

    fn try_candidate(
        &mut self,
        board: &mut BoardIr,
        targets: HashSet<String>,
        pad_mm: f64,
    ) -> Result<bool, String> {
        if !self.can_step() || targets.is_empty() {
            return Ok(false);
        }
        self.repair_passes_run += 1;
        let saved_tracks = board.tracks.clone();
        let saved_vias = board.vias.clone();
        self.pass_cfg.reroute_nets = Some(targets.clone());
        self.pass_cfg.reroute_pad_mm = Some(pad_mm);
        self.pass_cfg.timeout_secs = self
            .cfg
            .timeout_secs
            .map(|limit| limit.saturating_sub(self.start.elapsed().as_secs()).max(1));

        let cand_summary = route_board_ir(board, &self.pass_cfg)?;
        self.passes_run += cand_summary.passes_run;
        self.starts_run += cand_summary.starts_run;
        self.stopped_early |= cand_summary.stopped_early;
        let cand_eval = eval_repair_state(
            board,
            &self.cfg.ignored_nets,
            cand_summary.unconnected_pairs,
        );
        if cand_eval.score < self.cur_eval.score {
            self.summary = cand_summary;
            self.repaired_nets.extend(targets);
            self.cur_eval = cand_eval;
            Ok(true)
        } else {
            board.tracks = saved_tracks;
            board.vias = saved_vias;
            Ok(false)
        }
    }
}

fn ordered_over_length_nets(eval: &RepairEval, allowed_nets: &HashSet<String>) -> Vec<String> {
    let mut by_net: HashMap<&str, Nm> = HashMap::new();
    for mv in &eval.max_lengths {
        if !allowed_nets.contains(&mv.net) {
            continue;
        }
        let e = by_net.entry(mv.net.as_str()).or_insert(0);
        *e = (*e).max(mv.excess_nm);
    }
    let mut ordered: Vec<(&str, Nm)> = by_net.into_iter().collect();
    ordered.sort_by(|a, b| b.1.cmp(&a.1).then_with(|| a.0.cmp(b.0)));
    ordered.into_iter().map(|(n, _)| n.to_string()).collect()
}

fn repair_max_lengths(
    board: &mut BoardIr,
    state: &mut RepairState<'_>,
    eligible_nets: &HashSet<String>,
    scope_nets: Option<&HashSet<String>>,
    pad_mm: f64,
) -> Result<(), String> {
    let allowed_nets = scope_nets.unwrap_or(eligible_nets);
    let initial = ordered_over_length_nets(&state.cur_eval, allowed_nets);
    if initial.is_empty() {
        return Ok(());
    }
    let batch_solo = (initial.len() == 1).then(|| initial[0].clone());
    let all_set: HashSet<String> = initial.into_iter().collect();
    state.try_candidate(board, all_set, pad_mm)?;

    let mut tried = HashSet::new();
    while state.can_step() {
        let Some(net) = ordered_over_length_nets(&state.cur_eval, allowed_nets)
            .into_iter()
            .find(|n| !tried.contains(n))
        else {
            break;
        };
        tried.insert(net.clone());
        let mut targets = collect_corridor_neighbors(board, &net, eligible_nets, scope_nets);
        if !targets.is_empty() {
            targets.insert(net.clone());
            let improved = state.try_candidate(board, targets, pad_mm)?;
            if improved && state.cur_eval.max_lengths.iter().any(|m| m.net == net) {
                state.try_candidate(board, HashSet::from([net]), pad_mm)?;
            }
        } else if batch_solo.as_deref() != Some(net.as_str()) {
            state.try_candidate(board, HashSet::from([net]), pad_mm)?;
        }
    }
    Ok(())
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
    let base_pad_mm = cfg.reroute_pad_mm.unwrap_or(BASE_REROUTE_PAD_MM);

    let mut pass_cfg = cfg.clone();
    pass_cfg.tune_match_groups = false;
    let initial_summary = route_board_ir(board, &pass_cfg)?;
    let initial_eval =
        eval_repair_state(board, &cfg.ignored_nets, initial_summary.unconnected_pairs);
    let mut history: Vec<PriorCluster> = Vec::new();
    let mut state = RepairState {
        cfg,
        pass_cfg,
        start,
        repair_passes,
        repair_passes_run: 0,
        passes_run: initial_summary.passes_run,
        starts_run: initial_summary.starts_run,
        stopped_early: initial_summary.stopped_early,
        summary: initial_summary,
        repaired_nets: HashSet::new(),
        cur_eval: initial_eval,
    };

    while state.can_step() && !state.cur_eval.repairable.is_empty() {
        let hotspots = cluster_hotspots(
            Some(board),
            &state.cur_eval.repairable,
            DEFAULT_CLUSTER_RADIUS_NM,
        );
        let open_nets: HashSet<&str> = state
            .cur_eval
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
                        eligible_nets.contains(n.as_str())
                            && (open_nets.contains(n.as_str()) || net_can_reroute(board, n))
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
        let pad_mm = base_pad_mm + ((attempt - 1) as f64) * REROUTE_PAD_STEP_MM;
        let targets = collect_hotspot_targets(
            board,
            &eligible_nets,
            base_nets,
            hotspot,
            neighbor_radius_nm,
        );

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

        state.try_candidate(board, targets, pad_mm)?;
    }

    if state.can_step() && !state.cur_eval.max_lengths.is_empty() {
        repair_max_lengths(
            board,
            &mut state,
            &eligible_nets,
            scope_nets.as_ref(),
            base_pad_mm,
        )?;
    }

    let mut summary = state.summary;
    let repaired_nets = state.repaired_nets;
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

    summary.passes_run = state.passes_run;
    summary.starts_run = state.starts_run;
    summary.stopped_early = state.stopped_early;
    summary.repair_passes_run = state.repair_passes_run;
    summary.elapsed_ms = start.elapsed().as_millis();
    Ok(summary)
}

#[cfg(test)]
mod tests {
    use std::collections::HashSet;

    use super::eval_repair_state;
    use crate::ir::{BoardIr, BoardSpec, MaxLengthRule, Pt, Track, mm};

    #[test]
    fn eval_repair_state_prioritizes_geom_drc_over_max_length() {
        let spec: BoardSpec = toml::from_str(
            r#"
name = "score_test"
size_nm = [30000000, 22000000]
[[layers]]
name = "F.Cu"
kind = "signal"
side = "top"
[footprints.PAD1]
kind = "custom"
[[footprints.PAD1.pads]]
name = "1"
kind = "smd"
shape = { shape = "circle", diameter = 600000 }
pos = { x = 0, y = 0 }
layers = ["F.Cu"]
[[placements]]
refdes = "A1"
footprint = "PAD1"
pos = { x = 2000000, y = 10000000 }
locked = true
pin_nets = { "1" = "SIG_A" }
[[placements]]
refdes = "A2"
footprint = "PAD1"
pos = { x = 16000000, y = 10000000 }
locked = true
pin_nets = { "1" = "SIG_A" }
[rules]
edge_clearance_nm = 300000
"#,
        )
        .unwrap();
        let mut board = BoardIr::from_spec(&spec).unwrap();
        board.constraints.max_lengths.push(MaxLengthRule {
            nets: vec!["SIG_A".into()],
            max_nm: mm(14.0),
        });
        board.tracks = vec![
            Track {
                net: "SIG_A".into(),
                layer: "F.Cu".into(),
                a: Pt::new(mm(2.0), mm(10.0)),
                b: Pt::new(mm(9.0), mm(16.0)),
                width: mm(0.2),
                locked: false,
            },
            Track {
                net: "SIG_A".into(),
                layer: "F.Cu".into(),
                a: Pt::new(mm(9.0), mm(16.0)),
                b: Pt::new(mm(16.0), mm(10.0)),
                width: mm(0.2),
                locked: false,
            },
        ];
        let ignored = HashSet::new();
        let s_long = eval_repair_state(&board, &ignored, 0);
        assert_eq!(s_long.score.0, 0);
        assert_eq!(s_long.score.1, 0);
        assert_eq!(s_long.score.2, 1);
        assert!(s_long.score.3 > mm(4.0));

        // Shorter excess length improves score when geom_viols and max_length_count are equal.
        board.tracks[0].b = Pt::new(mm(9.0), mm(13.0));
        board.tracks[1].a = Pt::new(mm(9.0), mm(13.0));
        let s_shorter = eval_repair_state(&board, &ignored, 0);
        assert_eq!(
            (s_shorter.score.0, s_shorter.score.1, s_shorter.score.2),
            (0, 0, 1)
        );
        assert!(s_shorter.score < s_long.score);

        // Fixing MaxLength while introducing a geometric violation must score worse.
        board.tracks = vec![
            Track {
                net: "SIG_A".into(),
                layer: "F.Cu".into(),
                a: Pt::new(mm(2.0), mm(10.0)),
                b: Pt::new(mm(16.0), mm(10.0)),
                width: mm(0.2),
                locked: false,
            },
            Track {
                net: "SIG_B".into(),
                layer: "F.Cu".into(),
                a: Pt::new(mm(2.0), mm(0.05)),
                b: Pt::new(mm(6.0), mm(0.05)),
                width: mm(0.2),
                locked: false,
            },
        ];
        let s_geom_bad = eval_repair_state(&board, &ignored, 0);
        assert!(s_geom_bad.score.1 > 0);
        assert_eq!(s_geom_bad.score.2, 0);
        assert!(s_geom_bad.score > s_long.score);
    }
}
