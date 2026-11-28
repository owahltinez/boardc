use std::collections::{HashMap, HashSet};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, mpsc};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use crate::drc::{
    DEFAULT_CLUSTER_RADIUS_NM, Hotspot, RepairDrcStatic, Severity, Violation, ViolationKind,
    check_repair_drc,
};
use crate::drc_cluster::{cluster_hotspots, routable_board_nets};
use crate::drc_constraints::count_length_violations;
use crate::ir::{BoardIr, FilledPour, Keepout, Nm, Pt, Track, Via};
use crate::pipeline_repair_length::collect_corridor_neighbors;
use crate::pipeline_repair_pour::{
    collect_hotspot_targets, collect_pour_hotspots, resolve_pad_centers,
};
use crate::route::{
    RouteOptions, RouteSummary, apply_match_group_tuning, route_board_ir, route_board_ir_inner,
};
use crate::zones::{ZoneReport, fill_zones};

const BASE_REROUTE_PAD_MM: f64 = 2.0;
const REROUTE_PAD_STEP_MM: f64 = 2.0;
const NEIGHBOR_RADIUS_STEP_NM: Nm = DEFAULT_CLUSTER_RADIUS_NM;
const REPAIR_PASS_MAX_PASSES: usize = 30;
const REPAIR_PASS_MAX_MULTI_START: usize = 2;
const MIN_STAGE2_DURATION: Duration = Duration::from_millis(500);

struct DeadlineWatchdog {
    flag: Arc<AtomicBool>,
    cancel_tx: Option<mpsc::Sender<()>>,
    handle: Option<JoinHandle<()>>,
}

impl DeadlineWatchdog {
    fn spawn(deadline: Instant) -> Self {
        let flag = Arc::new(AtomicBool::new(false));
        let wait = deadline.saturating_duration_since(Instant::now());
        if wait.is_zero() {
            flag.store(true, Ordering::Release);
            return Self {
                flag,
                cancel_tx: None,
                handle: None,
            };
        }
        let (cancel_tx, cancel_rx) = mpsc::channel();
        let flag_bg = Arc::clone(&flag);
        let handle = std::thread::spawn(move || {
            if matches!(
                cancel_rx.recv_timeout(wait),
                Err(mpsc::RecvTimeoutError::Timeout)
            ) {
                flag_bg.store(true, Ordering::Release);
            }
        });
        Self {
            flag,
            cancel_tx: Some(cancel_tx),
            handle: Some(handle),
        }
    }

    fn flag(&self) -> Arc<AtomicBool> {
        Arc::clone(&self.flag)
    }

    #[cfg(test)]
    fn tripped(&self) -> bool {
        self.flag.load(Ordering::Acquire)
    }
}

impl Drop for DeadlineWatchdog {
    fn drop(&mut self) {
        self.cancel_tx.take();
        if let Some(handle) = self.handle.take() {
            let _ = handle.join();
        }
    }
}

fn budget_margins(limit: u64) -> Option<(Duration, Duration)> {
    match limit {
        0..=1 => None,
        2 => Some((Duration::from_millis(750), Duration::from_millis(350))),
        3..=7 => Some((Duration::from_millis(1000), Duration::from_millis(500))),
        8..=11 => Some((Duration::from_millis(2000), Duration::from_millis(1200))),
        12..=15 => Some((Duration::from_millis(3000), Duration::from_millis(1800))),
        16..=19 => Some((Duration::from_millis(4000), Duration::from_millis(2400))),
        _ => Some((Duration::from_millis(5000), Duration::from_millis(3000))),
    }
}

fn initial_route_budget(limit: u64) -> Duration {
    match limit {
        0 => Duration::ZERO,
        1 => Duration::from_secs(1),
        2 => Duration::from_millis(900),
        _ => {
            let start_margin_ms = budget_margins(limit).map_or(0, |(m, _)| m.as_millis() as u64);
            let total_ms = limit.saturating_mul(1000);
            let reserve_ms = (total_ms / 3).clamp(start_margin_ms + 500, 45_000);
            Duration::from_millis(total_ms.saturating_sub(reserve_ms))
        }
    }
}

fn next_pass_budget(limit: u64, remaining: Duration, rem_passes: usize) -> Option<Duration> {
    let (start_margin, safety_buffer) = budget_margins(limit)?;
    if rem_passes == 0 || remaining < start_margin {
        return None;
    }
    let usable = remaining.saturating_sub(safety_buffer);
    if usable.is_zero() {
        return None;
    }
    let divisor = u32::try_from(rem_passes).unwrap_or(u32::MAX);
    let fair = usable / divisor;
    Some(
        fair.clamp(safety_buffer, Duration::from_secs(30))
            .min(usable),
    )
}

fn route_with_deadline(
    board: &mut BoardIr,
    cfg: &RouteOptions,
    deadline: Option<Instant>,
) -> Result<(RouteSummary, HashSet<String>, bool), String> {
    let watchdog = deadline.map(DeadlineWatchdog::spawn);
    let stop_flag = watchdog.as_ref().map(DeadlineWatchdog::flag);
    let res = route_board_ir_inner(board, cfg, stop_flag.clone());
    drop(watchdog);
    let timed_out = stop_flag.is_some_and(|f| f.load(Ordering::Acquire));
    let (summary, unconnected) = res?;
    Ok((summary, unconnected, timed_out))
}

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
            | ViolationKind::MaxVias
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

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct RepairScore {
    unconnected_pairs: usize,
    geom_viols: usize,
    pour_opens: usize,
    unconnected_pour_terminals: usize,
    zone_chokes: usize,
    max_length_viols: usize,
    max_vias_viols: usize,
    total_excess_nm: Nm,
    total_excess_vias: Nm,
}

impl RepairScore {
    fn is_better_than(&self, cur: &Self) -> bool {
        let geom_self = (
            self.unconnected_pairs,
            self.geom_viols,
            self.pour_opens,
            self.unconnected_pour_terminals,
            self.zone_chokes,
        );
        let geom_cur = (
            cur.unconnected_pairs,
            cur.geom_viols,
            cur.pour_opens,
            cur.unconnected_pour_terminals,
            cur.zone_chokes,
        );
        if geom_self != geom_cur {
            return geom_self < geom_cur;
        }
        let cnt_self = self.max_length_viols + self.max_vias_viols;
        let cnt_cur = cur.max_length_viols + cur.max_vias_viols;
        if cnt_self != cnt_cur {
            return cnt_self < cnt_cur;
        }
        self.max_length_viols == cur.max_length_viols
            && self.total_excess_nm <= cur.total_excess_nm
            && self.total_excess_vias <= cur.total_excess_vias
            && (self.total_excess_nm < cur.total_excess_nm
                || self.total_excess_vias < cur.total_excess_vias)
    }
}

struct RepairEval {
    repairable: Vec<Violation>,
    pour_opens: Vec<Violation>,
    constraint_excess: HashMap<String, (Nm, Nm)>,
    score: RepairScore,
}

fn eval_repair_violations(
    board: &BoardIr,
    violations: Vec<Violation>,
    ignored: &HashSet<String>,
    unconnected_pairs: usize,
    zone_report: Option<&ZoneReport>,
) -> RepairEval {
    let zones_active = zone_report.is_some();
    let (unconnected_pour_terminals, zone_chokes) = zone_report.map_or((0, 0), |rep| {
        (
            rep.pours.iter().map(|p| p.unconnected_terminals).sum(),
            rep.chokes.len(),
        )
    });
    let mut repairable = Vec::new();
    let mut pour_opens = Vec::new();
    let mut constraint_excess: HashMap<String, (Nm, Nm)> = HashMap::new();
    let mut max_length_viols = 0usize;
    let mut max_vias_viols = 0usize;
    let mut total_excess_nm: Nm = 0;
    let mut total_excess_vias: Nm = 0;
    for v in violations {
        if v.severity != Severity::Error || !is_repairable_kind(v.kind) {
            continue;
        }
        let net_ignored = v.net_a.is_empty() || board.is_net_ignored(&v.net_a, ignored);
        if v.kind == ViolationKind::MaxLength {
            if !net_ignored && net_can_reroute(board, &v.net_a) {
                let excess = (v.measured_nm - v.required_nm).max(0);
                max_length_viols += 1;
                total_excess_nm = total_excess_nm.saturating_add(excess);
                let e = constraint_excess.entry(v.net_a).or_insert((0, 0));
                e.0 = e.0.max(excess);
            }
            continue;
        }
        if v.kind == ViolationKind::MaxVias {
            if !net_ignored {
                let excess = (v.measured_nm - v.required_nm).max(0);
                max_vias_viols += 1;
                total_excess_vias = total_excess_vias.saturating_add(excess);
                // Only target nets with at least one unlocked via; locked-via violations stay in score to prevent regressions.
                if board
                    .vias
                    .iter()
                    .any(|via| !via.locked && via.net == v.net_a)
                {
                    let e = constraint_excess.entry(v.net_a).or_insert((0, 0));
                    e.1 = e.1.max(excess);
                }
            }
            continue;
        }
        let is_pour_net = !v.net_a.is_empty() && board.pours.iter().any(|p| p.net == v.net_a);
        if v.kind == ViolationKind::OpenNet {
            if zones_active && is_pour_net {
                pour_opens.push(v);
            } else if !net_ignored && !is_pour_net {
                repairable.push(v);
            }
            continue;
        }
        if matches!(
            v.kind,
            ViolationKind::TrackDangling | ViolationKind::ViaDangling
        ) && (net_ignored || (!zones_active && is_pour_net))
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
    let score = RepairScore {
        unconnected_pairs,
        geom_viols: repairable.len(),
        pour_opens: pour_opens.len(),
        unconnected_pour_terminals,
        zone_chokes,
        max_length_viols,
        max_vias_viols,
        total_excess_nm,
        total_excess_vias,
    };
    RepairEval {
        repairable,
        pour_opens,
        constraint_excess,
        score,
    }
}

fn eval_repair_state_with_static(
    board: &BoardIr,
    ignored: &HashSet<String>,
    unconnected_pairs: usize,
    zone_report: Option<&ZoneReport>,
    static_ctx: &RepairDrcStatic,
) -> RepairEval {
    let violations = check_repair_drc(board, static_ctx);
    eval_repair_violations(board, violations, ignored, unconnected_pairs, zone_report)
}

#[cfg(test)]
fn eval_repair_state(
    board: &BoardIr,
    ignored: &HashSet<String>,
    unconnected_pairs: usize,
    zone_report: Option<&ZoneReport>,
) -> RepairEval {
    let static_ctx = RepairDrcStatic::new(board);
    eval_repair_state_with_static(board, ignored, unconnected_pairs, zone_report, &static_ctx)
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
    fn matches_region(
        &self,
        centroid_nm: Pt,
        radius_nm: Nm,
        base_nets: Option<&HashSet<String>>,
    ) -> bool {
        let match_dist = DEFAULT_CLUSTER_RADIUS_NM.max(radius_nm).max(self.radius_nm) as f64;
        centroid_nm.dist(self.centroid_nm) <= match_dist
            || base_nets.is_some_and(|nets| !nets.is_empty() && &self.base_nets == nets)
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
    anchors.extend(
        resolve_pad_centers(board, &open_pads, &[])
            .into_iter()
            .map(|p| p.pos),
    );
    anchors
}

struct BoardSnapshot {
    tracks: Vec<Track>,
    vias: Vec<Via>,
    filled_pours: Option<Vec<FilledPour>>,
}

impl BoardSnapshot {
    fn take(board: &mut BoardIr, save_filled_pours: bool) -> Self {
        Self {
            tracks: board.tracks.clone(),
            vias: board.vias.clone(),
            filled_pours: save_filled_pours.then(|| std::mem::take(&mut board.filled_pours)),
        }
    }

    fn restore(self, board: &mut BoardIr) {
        board.tracks = self.tracks;
        board.vias = self.vias;
        if let Some(fp) = self.filled_pours {
            board.filled_pours = fp;
        }
    }
}

struct RepairState<'a> {
    cfg: &'a RouteOptions,
    pass_cfg: RouteOptions,
    hard_deadline: Option<Instant>,
    repair_passes: usize,
    repair_passes_run: usize,
    passes_run: usize,
    starts_run: usize,
    any_stopped_early: bool,
    summary: RouteSummary,
    repaired_nets: HashSet<String>,
    cur_eval: RepairEval,
    pour_keepouts: Vec<Keepout>,
    zone_state: Option<ZoneReport>,
    drc_static: RepairDrcStatic,
}

impl<'a> RepairState<'a> {
    fn has_start_budget(&self) -> bool {
        match (self.cfg.timeout_secs, self.hard_deadline) {
            (Some(limit), Some(dl)) => budget_margins(limit).is_some_and(|(start_margin, _)| {
                dl.saturating_duration_since(Instant::now()) >= start_margin
            }),
            _ => true,
        }
    }

    fn can_step(&self) -> bool {
        self.repair_passes_run < self.repair_passes
            && !viahop::signal::stop_requested(None)
            && self.has_start_budget()
    }

    fn has_actionable_work(
        &self,
        board: &BoardIr,
        eligible_nets: &HashSet<String>,
        scope_nets: Option<&HashSet<String>>,
    ) -> bool {
        if !self.cur_eval.repairable.is_empty() {
            let (_, candidates) = collect_geom_candidates(
                board,
                &self.cur_eval.repairable,
                eligible_nets,
                scope_nets,
                &[],
            );
            if !candidates.is_empty() {
                return true;
            }
        }
        if let Some(rep) = &self.zone_state
            && (!rep.chokes.is_empty() || !self.cur_eval.pour_opens.is_empty())
            && !collect_pour_hotspots(
                board,
                &rep.chokes,
                &self.cur_eval.pour_opens,
                eligible_nets,
                scope_nets,
            )
            .is_empty()
        {
            return true;
        }
        let allowed_nets = scope_nets.unwrap_or(eligible_nets);
        !ordered_constraint_violating_nets(&self.cur_eval, allowed_nets).is_empty()
    }

    fn apply_pass_options(&mut self, timed: Option<(u64, Duration, Duration)>) {
        let Some((limit, remaining, pass_dur)) = timed else {
            self.pass_cfg.max_passes = self.cfg.max_passes;
            self.pass_cfg.multi_start = self.cfg.multi_start;
            self.pass_cfg.optimize = self.cfg.optimize;
            return;
        };
        let allow_optimize = if limit >= 10 && remaining < Duration::from_secs(15) {
            false
        } else {
            pass_dur >= Duration::from_secs(10)
        };
        self.pass_cfg.optimize = self.cfg.optimize && allow_optimize;
        let max_starts = if allow_optimize {
            REPAIR_PASS_MAX_MULTI_START
        } else {
            1
        };
        self.pass_cfg.max_passes = self.cfg.max_passes.min(REPAIR_PASS_MAX_PASSES);
        self.pass_cfg.multi_start = self.cfg.multi_start.min(max_starts);
    }

    fn configure_pass_budget(&mut self) -> Option<Option<Instant>> {
        if self.repair_passes_run >= self.repair_passes || viahop::signal::stop_requested(None) {
            return None;
        }
        let (Some(limit), Some(hard_deadline)) = (self.cfg.timeout_secs, self.hard_deadline) else {
            self.apply_pass_options(None);
            return Some(None);
        };
        let now = Instant::now();
        let remaining = hard_deadline.saturating_duration_since(now);
        let rem_passes = self.repair_passes - self.repair_passes_run;
        let pass_dur = next_pass_budget(limit, remaining, rem_passes)?;
        self.apply_pass_options(Some((limit, remaining, pass_dur)));
        Some(Some(now + pass_dur))
    }

    fn prepare_stage2(&mut self, pass_deadline: Option<Instant>) -> bool {
        if viahop::signal::stop_requested(None) {
            return false;
        }
        let Some(pdl) = pass_deadline else {
            self.apply_pass_options(None);
            return true;
        };
        let now = Instant::now();
        let stage2_dur = pdl.saturating_duration_since(now);
        if stage2_dur < MIN_STAGE2_DURATION {
            return false;
        }
        if let (Some(limit), Some(hard_deadline)) = (self.cfg.timeout_secs, self.hard_deadline) {
            let remaining = hard_deadline.saturating_duration_since(now);
            self.apply_pass_options(Some((limit, remaining, stage2_dur)));
        }
        true
    }

    fn route_with_keepouts(
        &mut self,
        board: &mut BoardIr,
        extra_keepouts: &[Keepout],
        pass_deadline: Option<Instant>,
    ) -> Result<(RouteSummary, HashSet<String>, bool), String> {
        let orig_len = board.keepouts.len();
        board.keepouts.extend(self.pour_keepouts.iter().cloned());
        board.keepouts.extend(extra_keepouts.iter().cloned());
        let res = route_with_deadline(board, &self.pass_cfg, pass_deadline);
        board.keepouts.truncate(orig_len);
        let (summary, unconnected, timed_out) = res?;
        self.passes_run += summary.passes_run;
        self.starts_run += summary.starts_run;
        self.any_stopped_early |= summary.stopped_early || timed_out;
        Ok((summary, unconnected, timed_out))
    }

    fn finish_candidate(
        &mut self,
        board: &mut BoardIr,
        snap: BoardSnapshot,
        cand_summary: RouteSummary,
        targets: impl IntoIterator<Item = String>,
        extra_keepouts: Vec<Keepout>,
    ) -> Result<bool, String> {
        let cand_zone_rep = self.zone_state.is_some().then(|| fill_zones(board));
        let cand_eval = eval_repair_state_with_static(
            board,
            &self.cfg.ignored_nets,
            cand_summary.unconnected_pairs,
            cand_zone_rep.as_ref(),
            &self.drc_static,
        );
        if cand_eval.score.is_better_than(&self.cur_eval.score) {
            self.summary = cand_summary;
            self.repaired_nets.extend(targets);
            self.cur_eval = cand_eval;
            self.pour_keepouts.extend(extra_keepouts);
            if let Some(rep) = cand_zone_rep {
                self.zone_state = Some(rep);
            }
            Ok(true)
        } else {
            snap.restore(board);
            Ok(false)
        }
    }

    fn try_candidate(
        &mut self,
        board: &mut BoardIr,
        targets: HashSet<String>,
        pad_mm: f64,
    ) -> Result<bool, String> {
        self.try_candidate_with_keepouts(board, targets, pad_mm, Vec::new())
    }

    fn try_candidate_with_keepouts(
        &mut self,
        board: &mut BoardIr,
        targets: HashSet<String>,
        pad_mm: f64,
        extra_keepouts: Vec<Keepout>,
    ) -> Result<bool, String> {
        if targets.is_empty() {
            return Ok(false);
        }
        let Some(pass_deadline) = self.configure_pass_budget() else {
            return Ok(false);
        };
        self.repair_passes_run += 1;
        let snap = BoardSnapshot::take(board, self.zone_state.is_some());
        self.pass_cfg.reroute_nets = Some(targets.clone());
        self.pass_cfg.reroute_pad_mm = Some(pad_mm);

        let cand_summary = match self.route_with_keepouts(board, &extra_keepouts, pass_deadline) {
            Ok((s, _, _)) => s,
            Err(e) => {
                snap.restore(board);
                return Err(e);
            }
        };
        self.finish_candidate(board, snap, cand_summary, targets, extra_keepouts)
    }

    // Route a MaxVias net while neighbors are stripped, then route neighbors around it.
    fn try_prioritized_candidate(
        &mut self,
        board: &mut BoardIr,
        primary_net: &str,
        neighbors: &HashSet<String>,
        pad_mm: f64,
    ) -> Result<bool, String> {
        if neighbors.is_empty() {
            return Ok(false);
        }
        let Some(pass_deadline) = self.configure_pass_budget() else {
            return Ok(false);
        };
        let vias_before = board.vias.iter().filter(|v| v.net == primary_net).count();
        self.repair_passes_run += 1;
        let snap = BoardSnapshot::take(board, self.zone_state.is_some());

        board
            .tracks
            .retain(|t| t.locked || !neighbors.contains(&t.net));
        board
            .vias
            .retain(|v| v.locked || !neighbors.contains(&v.net));

        self.pass_cfg.reroute_nets = Some(HashSet::from([primary_net.to_string()]));
        self.pass_cfg.reroute_pad_mm = Some(pad_mm);
        let (net_summary, net_unconnected, net_timed_out) =
            match self.route_with_keepouts(board, &[], pass_deadline) {
                Ok(v) => v,
                Err(e) => {
                    snap.restore(board);
                    return Err(e);
                }
            };

        let vias_after = board.vias.iter().filter(|v| v.net == primary_net).count();
        let bad_unconnected = net_unconnected.contains(primary_net)
            || (self.cur_eval.score.unconnected_pairs == 0
                && net_unconnected.iter().any(|n| !neighbors.contains(n)));
        if bad_unconnected
            || net_summary.stopped_early
            || net_timed_out
            || viahop::signal::stop_requested(None)
            || vias_after >= vias_before
        {
            snap.restore(board);
            return Ok(false);
        }

        if !self.prepare_stage2(pass_deadline) {
            self.any_stopped_early = true;
            snap.restore(board);
            return Ok(false);
        }

        self.pass_cfg.reroute_nets = Some(neighbors.clone());
        self.pass_cfg.reroute_pad_mm = Some(pad_mm);
        let nbr_summary = match self.route_with_keepouts(board, &[], pass_deadline) {
            Ok((s, _, _)) => s,
            Err(e) => {
                snap.restore(board);
                return Err(e);
            }
        };

        let targets = std::iter::once(primary_net.to_string()).chain(neighbors.iter().cloned());
        self.finish_candidate(board, snap, nbr_summary, targets, Vec::new())
    }
}

struct GeomCandidate {
    hotspot_idx: usize,
    base_nets: HashSet<String>,
    prev_attempts: usize,
}

fn collect_geom_candidates(
    board: &BoardIr,
    repairable: &[Violation],
    eligible_nets: &HashSet<String>,
    scope_nets: Option<&HashSet<String>>,
    history: &[PriorCluster],
) -> (Vec<Hotspot>, Vec<GeomCandidate>) {
    let hotspots = cluster_hotspots(Some(board), repairable, DEFAULT_CLUSTER_RADIUS_NM);
    let open_nets: HashSet<&str> = repairable
        .iter()
        .filter(|v| v.kind == ViolationKind::OpenNet && !v.net_a.is_empty())
        .map(|v| v.net_a.as_str())
        .collect();
    let candidates = hotspots
        .iter()
        .enumerate()
        .filter_map(|(hotspot_idx, h)| {
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
                || scope_nets.is_some_and(|scope| !base_nets.iter().any(|n| scope.contains(n)))
            {
                return None;
            }
            let prev_attempts = history
                .iter()
                .filter(|p| p.matches_region(h.centroid_nm, h.radius_nm, Some(&base_nets)))
                .map(|p| p.attempts)
                .max()
                .unwrap_or(0);
            Some(GeomCandidate {
                hotspot_idx,
                base_nets,
                prev_attempts,
            })
        })
        .collect();
    (hotspots, candidates)
}

fn ordered_constraint_violating_nets(
    eval: &RepairEval,
    allowed_nets: &HashSet<String>,
) -> Vec<(String, (Nm, Nm))> {
    let mut ordered: Vec<(String, (Nm, Nm))> = eval
        .constraint_excess
        .iter()
        .filter(|(n, _)| allowed_nets.contains(n.as_str()))
        .map(|(n, &ex)| (n.clone(), ex))
        .collect();
    ordered.sort_by(|a, b| b.1.cmp(&a.1).then_with(|| a.0.cmp(&b.0)));
    ordered
}

fn repair_constraints(
    board: &mut BoardIr,
    state: &mut RepairState<'_>,
    eligible_nets: &HashSet<String>,
    scope_nets: Option<&HashSet<String>>,
    pad_mm: f64,
) -> Result<(), String> {
    let allowed_nets = scope_nets.unwrap_or(eligible_nets);
    let initial = ordered_constraint_violating_nets(&state.cur_eval, allowed_nets);
    if initial.is_empty() {
        return Ok(());
    }
    let batch_solo = if initial.len() > 1 || initial[0].1.1 == 0 {
        let solo = (initial.len() == 1).then(|| initial[0].0.clone());
        let all_set: HashSet<String> = initial.into_iter().map(|(n, _)| n).collect();
        state.try_candidate(board, all_set, pad_mm)?;
        solo
    } else {
        None
    };

    let mut tried = HashSet::new();
    while state.can_step() {
        let Some((net, (excess_nm, excess_vias))) =
            ordered_constraint_violating_nets(&state.cur_eval, allowed_nets)
                .into_iter()
                .find(|(n, _)| !tried.contains(n))
        else {
            break;
        };
        tried.insert(net.clone());
        let neighbors = collect_corridor_neighbors(board, &net, eligible_nets, scope_nets);
        if !neighbors.is_empty() {
            let mut improved = false;
            if excess_vias > 0 {
                improved = state.try_prioritized_candidate(board, &net, &neighbors, pad_mm)?;
            }
            if !improved && (excess_vias == 0 || excess_nm > 0) {
                let mut targets = neighbors;
                targets.insert(net.clone());
                improved = state.try_candidate(board, targets, pad_mm)?;
            }
            if improved && state.cur_eval.constraint_excess.contains_key(&net) {
                state.try_candidate(board, HashSet::from([net]), pad_mm)?;
            }
        } else if batch_solo.as_deref() != Some(net.as_str()) {
            state.try_candidate(board, HashSet::from([net]), pad_mm)?;
        }
    }
    Ok(())
}

fn record_cluster_attempt(
    history: &mut Vec<PriorCluster>,
    centroid_nm: Pt,
    radius_nm: Nm,
    base_nets: Option<&HashSet<String>>,
    attempt: usize,
) {
    let mut matched = false;
    for prior in history.iter_mut() {
        if prior.matches_region(centroid_nm, radius_nm, base_nets) {
            prior.attempts = prior.attempts.max(attempt);
            prior.centroid_nm = centroid_nm;
            prior.radius_nm = prior.radius_nm.max(radius_nm);
            if let Some(nets) = base_nets {
                prior.base_nets.extend(nets.iter().cloned());
            }
            matched = true;
        }
    }
    if !matched {
        history.push(PriorCluster {
            centroid_nm,
            radius_nm,
            base_nets: base_nets.cloned().unwrap_or_default(),
            attempts: attempt,
        });
    }
}

fn repair_pours(
    board: &mut BoardIr,
    state: &mut RepairState<'_>,
    eligible_nets: &HashSet<String>,
    scope_nets: Option<&HashSet<String>>,
    base_pad_mm: f64,
) -> Result<(), String> {
    let mut history: Vec<PriorCluster> = Vec::new();
    while state.can_step() {
        let chokes = state
            .zone_state
            .as_ref()
            .map(|rep| rep.chokes.as_slice())
            .unwrap_or(&[]);
        if chokes.is_empty() && state.cur_eval.pour_opens.is_empty() {
            break;
        }
        let hotspots = collect_pour_hotspots(
            board,
            chokes,
            &state.cur_eval.pour_opens,
            eligible_nets,
            scope_nets,
        );
        if hotspots.is_empty() {
            break;
        }
        let Some((h_idx, prev_attempts)) = hotspots
            .iter()
            .enumerate()
            .map(|(idx, h)| {
                let prev = history
                    .iter()
                    .filter(|p| p.matches_region(h.centroid_nm, h.radius_nm, None))
                    .map(|p| p.attempts)
                    .max()
                    .unwrap_or(0);
                (idx, prev)
            })
            .filter(|(_, prev)| *prev < 3)
            .min_by_key(|(idx, prev)| (*prev, *idx))
        else {
            break;
        };

        let hotspot = &hotspots[h_idx];
        let attempt = prev_attempts + 1;
        let neighbor_radius_nm = ((attempt - 1) as Nm) * NEIGHBOR_RADIUS_STEP_NM;
        let pad_mm = base_pad_mm + ((attempt - 1) as f64) * REROUTE_PAD_STEP_MM;
        let targets = collect_hotspot_targets(
            board,
            eligible_nets,
            &hotspot.base_nets,
            &hotspot.anchors,
            hotspot.radius_nm,
            neighbor_radius_nm,
            Some(hotspot.pour_net.as_str()),
        );
        let keepouts = hotspot.build_keepouts(board, attempt);
        record_cluster_attempt(
            &mut history,
            hotspot.centroid_nm,
            hotspot.radius_nm,
            None,
            attempt,
        );
        state.try_candidate_with_keepouts(board, targets, pad_mm, keepouts)?;
    }
    Ok(())
}

pub(crate) struct RouteRepairResult {
    pub(crate) summary: RouteSummary,
    pub(crate) zones: Option<(Vec<FilledPour>, ZoneReport)>,
}

/// Runs `route_board_ir`, optionally followed by bounded hotspot auto-repair and final `tune_match_groups`.
pub(crate) fn route_with_repair_and_zones(
    board: &mut BoardIr,
    cfg: &RouteOptions,
    repair_passes: usize,
) -> Result<RouteRepairResult, String> {
    if repair_passes == 0 {
        return Ok(RouteRepairResult {
            summary: route_board_ir(board, cfg)?,
            zones: None,
        });
    }

    let start = Instant::now();
    let hard_deadline = cfg.timeout_secs.map(|s| start + Duration::from_secs(s));
    if !board.pours.is_empty() {
        board.filled_pours.clear();
    }
    let drc_static = RepairDrcStatic::new(board);
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

    let mut init_cfg = cfg.clone();
    init_cfg.tune_match_groups = false;
    init_cfg.timeout_secs = None;
    let init_deadline = cfg
        .timeout_secs
        .map(|limit| start + initial_route_budget(limit));
    let (initial_summary, _, init_timed_out) =
        route_with_deadline(board, &init_cfg, init_deadline)?;

    let initial_eval = eval_repair_state_with_static(
        board,
        &cfg.ignored_nets,
        initial_summary.unconnected_pairs,
        None,
        &drc_static,
    );
    let mut history: Vec<PriorCluster> = Vec::new();
    let mut state = RepairState {
        cfg,
        pass_cfg: init_cfg,
        hard_deadline,
        repair_passes,
        repair_passes_run: 0,
        passes_run: initial_summary.passes_run,
        starts_run: initial_summary.starts_run,
        any_stopped_early: initial_summary.stopped_early || init_timed_out,
        summary: initial_summary,
        repaired_nets: HashSet::new(),
        cur_eval: initial_eval,
        pour_keepouts: Vec::new(),
        zone_state: None,
        drc_static,
    };

    while state.can_step() && !state.cur_eval.repairable.is_empty() {
        let (hotspots, candidates) = collect_geom_candidates(
            board,
            &state.cur_eval.repairable,
            &eligible_nets,
            scope_nets.as_ref(),
            &history,
        );

        let Some(cand) = candidates.iter().find(|c| c.prev_attempts < 2).or_else(|| {
            candidates
                .iter()
                .min_by_key(|c| (c.prev_attempts, c.hotspot_idx))
        }) else {
            break;
        };

        let hotspot = &hotspots[cand.hotspot_idx];
        let attempt = cand.prev_attempts + 1;
        let neighbor_radius_nm = ((attempt - 1) as Nm) * NEIGHBOR_RADIUS_STEP_NM;
        let pad_mm = base_pad_mm + ((attempt - 1) as f64) * REROUTE_PAD_STEP_MM;
        let anchors = collect_hotspot_anchors(board, hotspot);
        let targets = collect_hotspot_targets(
            board,
            &eligible_nets,
            &cand.base_nets,
            &anchors,
            hotspot.radius_nm,
            neighbor_radius_nm,
            None,
        );

        record_cluster_attempt(
            &mut history,
            hotspot.centroid_nm,
            hotspot.radius_nm,
            Some(&cand.base_nets),
            attempt,
        );

        state.try_candidate(board, targets, pad_mm)?;
    }

    if !board.pours.is_empty() && state.can_step() {
        let zone_rep = fill_zones(board);
        state.cur_eval = eval_repair_state_with_static(
            board,
            &cfg.ignored_nets,
            state.summary.unconnected_pairs,
            Some(&zone_rep),
            &state.drc_static,
        );
        state.zone_state = Some(zone_rep);
        repair_pours(
            board,
            &mut state,
            &eligible_nets,
            scope_nets.as_ref(),
            base_pad_mm,
        )?;
    }

    if state.can_step() && !state.cur_eval.constraint_excess.is_empty() {
        repair_constraints(
            board,
            &mut state,
            &eligible_nets,
            scope_nets.as_ref(),
            base_pad_mm,
        )?;
    }

    if !state.any_stopped_early
        && !state.can_step()
        && state.repair_passes_run < state.repair_passes
        && state.has_actionable_work(board, &eligible_nets, scope_nets.as_ref())
    {
        state.any_stopped_early = true;
    }

    let saved_filled_pours = if !board.pours.is_empty() {
        std::mem::take(&mut board.filled_pours)
    } else {
        Vec::new()
    };

    let mut summary = state.summary;
    if !state.pour_keepouts.is_empty() {
        summary.drc_violations = state.cur_eval.score.geom_viols;
    }
    let repaired_nets = state.repaired_nets;
    let can_tune = cfg.tune_match_groups && hard_deadline.is_none_or(|dl| Instant::now() < dl);
    let tuned_tracks = if can_tune {
        let only_nets = cfg.reroute_nets.clone().map(|mut s| {
            s.extend(repaired_nets.iter().cloned());
            for rule in &board.constraints.match_groups {
                if rule.nets.iter().any(|n| repaired_nets.contains(n)) {
                    s.extend(rule.nets.iter().cloned());
                }
            }
            s
        });
        apply_match_group_tuning(board, &mut summary, only_nets, &cfg.ignored_nets)
    } else {
        if cfg.tune_match_groups && !board.constraints.match_groups.is_empty() {
            state.any_stopped_early = true;
        }
        if !board.constraints.match_groups.is_empty() || !board.constraints.max_lengths.is_empty() {
            summary.length_violations = count_length_violations(board);
        }
        false
    };

    let precomputed_zones = if !tuned_tracks {
        state.zone_state.map(|rep| (saved_filled_pours, rep))
    } else {
        None
    };

    summary.passes_run = state.passes_run;
    summary.starts_run = state.starts_run;
    summary.stopped_early = state.any_stopped_early;
    summary.repair_passes_run = state.repair_passes_run;
    summary.elapsed_ms = start.elapsed().as_millis();
    Ok(RouteRepairResult {
        summary,
        zones: precomputed_zones,
    })
}

#[cfg(test)]
mod tests {
    use std::collections::HashSet;
    use std::sync::atomic::Ordering;
    use std::time::{Duration, Instant};

    use super::{
        DeadlineWatchdog, REPAIR_PASS_MAX_MULTI_START, REPAIR_PASS_MAX_PASSES, RepairDrcStatic,
        RepairScore, RepairState, budget_margins, eval_repair_state, eval_repair_violations,
        initial_route_budget, next_pass_budget, route_with_repair_and_zones,
    };
    use crate::drc::{ViolationKind, check_board_drc};
    use crate::ir::{
        BgaShadowRule, BoardIr, BoardSpec, CapProximityRule, FilledPour, FootprintDef, Keepout,
        KeepoutKind, MatchGroupRule, MaxLengthRule, MaxViasRule, PadDef, PadKind, PadShape,
        Placement, PourPoly, PourSpec, Pt, Side, Track, Via, mm,
    };
    use crate::route::{RouteOptions, RouteSummary};
    use crate::zones::fill_zones;

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
        let s_long = eval_repair_state(&board, &ignored, 0, None);
        assert_eq!(s_long.score.unconnected_pairs, 0);
        assert_eq!(s_long.score.geom_viols, 0);
        assert_eq!(s_long.score.max_length_viols, 1);
        assert!(s_long.score.total_excess_nm > mm(4.0));

        // Shorter excess length improves score when geom_viols and max_length_count are equal.
        board.tracks[0].b = Pt::new(mm(9.0), mm(13.0));
        board.tracks[1].a = Pt::new(mm(9.0), mm(13.0));
        let s_shorter = eval_repair_state(&board, &ignored, 0, None);
        assert_eq!(
            (
                s_shorter.score.unconnected_pairs,
                s_shorter.score.geom_viols,
                s_shorter.score.max_length_viols,
            ),
            (0, 0, 1)
        );
        assert!(s_shorter.score.is_better_than(&s_long.score));
        assert!(!s_long.score.is_better_than(&s_shorter.score));

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
        let s_geom_bad = eval_repair_state(&board, &ignored, 0, None);
        assert!(s_geom_bad.score.geom_viols > 0);
        assert_eq!(s_geom_bad.score.max_length_viols, 0);
        assert!(s_long.score.is_better_than(&s_geom_bad.score));
        assert!(!s_geom_bad.score.is_better_than(&s_long.score));

        // MaxVias excess scoring and no blind trade between MaxLength and MaxVias.
        board.constraints.max_vias.push(MaxViasRule {
            nets: vec!["SIG_A".into()],
            max_vias: 0,
        });
        board.tracks = vec![Track {
            net: "SIG_A".into(),
            layer: "F.Cu".into(),
            a: Pt::new(mm(2.0), mm(10.0)),
            b: Pt::new(mm(16.0), mm(10.0)),
            width: mm(0.2),
            locked: false,
        }];
        board.vias = vec![
            Via {
                net: "SIG_A".into(),
                pos: Pt::new(mm(5.0), mm(10.0)),
                diameter: mm(0.45),
                drill: mm(0.2),
                layers: ("F.Cu".into(), "B.Cu".into()),
                locked: false,
                tented: true,
            },
            Via {
                net: "SIG_A".into(),
                pos: Pt::new(mm(12.0), mm(10.0)),
                diameter: mm(0.45),
                drill: mm(0.2),
                layers: ("F.Cu".into(), "B.Cu".into()),
                locked: false,
                tented: true,
            },
        ];
        let s_two_vias = eval_repair_state(&board, &ignored, 0, None);
        assert_eq!(s_two_vias.score.max_vias_viols, 1);
        assert_eq!(s_two_vias.score.total_excess_vias, 2);

        board.vias.pop();
        let s_one_via = eval_repair_state(&board, &ignored, 0, None);
        assert_eq!(s_one_via.score.max_vias_viols, 1);
        assert_eq!(s_one_via.score.total_excess_vias, 1);
        assert!(s_one_via.score.is_better_than(&s_two_vias.score));

        // Never blindly trade a MaxLength violation for a MaxVias violation (or vice versa).
        let trade_len = RepairScore {
            unconnected_pairs: 0,
            geom_viols: 0,
            pour_opens: 0,
            unconnected_pour_terminals: 0,
            zone_chokes: 0,
            max_length_viols: 1,
            max_vias_viols: 0,
            total_excess_nm: mm(2.0),
            total_excess_vias: 0,
        };
        let trade_vias = RepairScore {
            unconnected_pairs: 0,
            geom_viols: 0,
            pour_opens: 0,
            unconnected_pour_terminals: 0,
            zone_chokes: 0,
            max_length_viols: 0,
            max_vias_viols: 1,
            total_excess_nm: 0,
            total_excess_vias: 1,
        };
        assert!(!trade_vias.is_better_than(&trade_len));
        assert!(!trade_len.is_better_than(&trade_vias));

        // Locked-via MaxVias stays counted in score (rejects extra unlocked via) without becoming an actionable target.
        board.vias = vec![Via {
            net: "SIG_A".into(),
            pos: Pt::new(mm(5.0), mm(10.0)),
            diameter: mm(0.45),
            drill: mm(0.2),
            layers: ("F.Cu".into(), "B.Cu".into()),
            locked: true,
            tented: true,
        }];
        let s_locked_only = eval_repair_state(&board, &ignored, 0, None);
        assert_eq!(s_locked_only.score.max_vias_viols, 1);
        assert_eq!(s_locked_only.score.total_excess_vias, 1);
        assert!(!s_locked_only.constraint_excess.contains_key("SIG_A"));

        board.vias.push(Via {
            net: "SIG_A".into(),
            pos: Pt::new(mm(12.0), mm(10.0)),
            diameter: mm(0.45),
            drill: mm(0.2),
            layers: ("F.Cu".into(), "B.Cu".into()),
            locked: false,
            tented: true,
        });
        let s_locked_plus_unlocked = eval_repair_state(&board, &ignored, 0, None);
        assert!(
            s_locked_only
                .score
                .is_better_than(&s_locked_plus_unlocked.score)
        );
    }

    #[test]
    fn repair_score_rejects_pour_open_and_island_destruction_trades() {
        let narrow_neck = RepairScore {
            unconnected_pairs: 0,
            geom_viols: 0,
            pour_opens: 0,
            unconnected_pour_terminals: 0,
            zone_chokes: 1,
            max_length_viols: 0,
            max_vias_viols: 0,
            total_excess_nm: 0,
            total_excess_vias: 0,
        };
        let severed_pour = RepairScore {
            pour_opens: 1,
            zone_chokes: 0,
            ..narrow_neck
        };
        assert!(!severed_pour.is_better_than(&narrow_neck));
        assert!(narrow_neck.is_better_than(&severed_pour));

        let split_pour = RepairScore {
            unconnected_pairs: 0,
            geom_viols: 0,
            pour_opens: 1,
            unconnected_pour_terminals: 0,
            zone_chokes: 1,
            max_length_viols: 0,
            max_vias_viols: 0,
            total_excess_nm: 0,
            total_excess_vias: 0,
        };
        let destroyed_island = RepairScore {
            unconnected_pour_terminals: 1,
            zone_chokes: 0,
            ..split_pour
        };
        assert!(!destroyed_island.is_better_than(&split_pour));
        assert!(split_pour.is_better_than(&destroyed_island));

        let healed = RepairScore {
            pour_opens: 0,
            unconnected_pour_terminals: 0,
            zone_chokes: 0,
            ..split_pour
        };
        assert!(healed.is_better_than(&split_pour));
    }

    #[test]
    fn eval_repair_state_keeps_prebaked_filled_pour_opens_repairable() {
        let spec: BoardSpec = toml::from_str(
            r#"
name = "prebaked_pour_open"
size_nm = [20000000, 12000000]
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
pos = { x = 4000000, y = 6000000 }
locked = true
pin_nets = { "1" = "GND" }
[[placements]]
refdes = "A2"
footprint = "PAD1"
pos = { x = 16000000, y = 6000000 }
locked = true
pin_nets = { "1" = "GND" }
"#,
        )
        .unwrap();
        let mut board = BoardIr::from_spec(&spec).unwrap();
        board.filled_pours.push(FilledPour {
            net: "GND".into(),
            layer: "F.Cu".into(),
            priority: 0,
            polys: vec![PourPoly {
                outer: vec![
                    Pt::new(mm(3.0), mm(5.0)),
                    Pt::new(mm(8.0), mm(5.0)),
                    Pt::new(mm(8.0), mm(7.0)),
                    Pt::new(mm(3.0), mm(7.0)),
                ],
                holes: Vec::new(),
            }],
        });
        let eval = eval_repair_state(&board, &HashSet::new(), 1, None);
        assert_eq!(eval.pour_opens.len(), 0);
        assert_eq!(eval.repairable.len(), 1);
    }

    #[test]
    fn budget_splitting_and_caps_edge_cases() {
        assert_eq!(budget_margins(0), None);
        assert_eq!(budget_margins(1), None);
        assert_eq!(initial_route_budget(0), Duration::ZERO);
        assert_eq!(initial_route_budget(1), Duration::from_secs(1));
        assert_eq!(initial_route_budget(2), Duration::from_millis(900));
        assert_eq!(initial_route_budget(180), Duration::from_secs(135));
        assert_eq!(
            budget_margins(180),
            Some((Duration::from_secs(5), Duration::from_secs(3)))
        );

        let mut prev_init = Duration::ZERO;
        let mut prev_reserve = Duration::ZERO;
        let mut prev_margin = Duration::ZERO;
        let mut prev_buffer = Duration::ZERO;
        for limit in 2..=200u64 {
            let (start_margin, buffer) = budget_margins(limit).unwrap();
            let total = Duration::from_secs(limit);
            let init = initial_route_budget(limit);
            assert!(init < total, "limit={limit}");
            let reserve = total - init;
            assert!(
                reserve > start_margin,
                "limit={limit}: reserve={reserve:?} <= start_margin={start_margin:?}"
            );
            assert!(start_margin > buffer, "limit={limit}");
            assert!(init >= prev_init, "init not monotonic at limit={limit}");
            assert!(
                reserve >= prev_reserve,
                "reserve not monotonic at limit={limit}"
            );
            assert!(
                start_margin >= prev_margin,
                "margin not monotonic at limit={limit}"
            );
            assert!(
                buffer >= prev_buffer,
                "buffer not monotonic at limit={limit}"
            );
            prev_init = init;
            prev_reserve = reserve;
            prev_margin = start_margin;
            prev_buffer = buffer;
        }

        assert_eq!(next_pass_budget(0, Duration::from_secs(10), 4), None);
        assert_eq!(next_pass_budget(1, Duration::from_secs(1), 4), None);
        assert_eq!(next_pass_budget(2, Duration::from_millis(749), 4), None);
        assert_eq!(
            next_pass_budget(2, Duration::from_millis(1100), 4),
            Some(Duration::from_millis(350))
        );
        assert_eq!(next_pass_budget(20, Duration::from_millis(4999), 4), None);
        assert_eq!(
            next_pass_budget(20, Duration::from_secs(5), 4),
            Some(Duration::from_secs(2))
        );
        assert_eq!(
            next_pass_budget(180, Duration::from_secs(45), 8),
            Some(Duration::from_millis(5250))
        );
        assert_eq!(
            next_pass_budget(180, Duration::from_secs(45), 1),
            Some(Duration::from_secs(30))
        );
        assert_eq!(next_pass_budget(180, Duration::from_secs(45), 0), None);
        assert_eq!(
            next_pass_budget(180, Duration::from_secs(45), usize::MAX),
            Some(Duration::from_secs(3))
        );
        assert_eq!(
            next_pass_budget(
                180,
                Duration::from_secs(45),
                (u32::MAX as usize).saturating_add(1)
            ),
            Some(Duration::from_secs(3))
        );
    }

    #[test]
    fn deadline_watchdog_trips_after_deadline_and_joins_cleanly_on_early_drop() {
        let wd_short = DeadlineWatchdog::spawn(Instant::now() + Duration::from_millis(50));
        let flag_short = wd_short.flag();
        std::thread::sleep(Duration::from_millis(120));
        assert!(flag_short.load(Ordering::Acquire));
        assert!(wd_short.tripped());
        drop(wd_short);

        let wd_long = DeadlineWatchdog::spawn(Instant::now() + Duration::from_secs(10));
        let flag_long = wd_long.flag();
        let t0 = Instant::now();
        drop(wd_long);
        assert!(t0.elapsed() < Duration::from_millis(200));
        assert!(!flag_long.load(Ordering::Acquire));
    }

    #[test]
    fn repair_state_budget_untimed_stage2_and_caps() {
        let spec: BoardSpec = toml::from_str(
            r#"
name = "budget_state_test"
size_nm = [20000000, 12000000]
[[layers]]
name = "F.Cu"
kind = "signal"
side = "top"
"#,
        )
        .unwrap();
        let board = BoardIr::from_spec(&spec).unwrap();
        let summary = RouteSummary {
            total_nets: 1,
            routable_nets: 1,
            routed_nets: 0,
            unconnected_pairs: 1,
            total_segments: 0,
            total_vias: 0,
            total_wirelength_um: 0.0,
            passes_run: 1,
            starts_run: 1,
            drc_violations: 0,
            length_violations: 0,
            dense_bga_pads: 0,
            pofv_capable_pads: 0,
            grid_step_um: 100.0,
            stopped_early: true,
            elapsed_ms: 0,
            repair_passes_run: 0,
        };

        // Untimed repair passes preserve user max_passes, multi_start, and optimize.
        let untimed_cfg = RouteOptions {
            max_passes: 90,
            multi_start: 6,
            optimize: true,
            timeout_secs: None,
            ..Default::default()
        };
        let mut untimed_pass_cfg = untimed_cfg.clone();
        untimed_pass_cfg.timeout_secs = None;
        let mut untimed_state = RepairState {
            cfg: &untimed_cfg,
            pass_cfg: untimed_pass_cfg,
            hard_deadline: None,
            repair_passes: 4,
            repair_passes_run: 0,
            passes_run: 1,
            starts_run: 1,
            any_stopped_early: false,
            summary: summary.clone(),
            repaired_nets: HashSet::new(),
            cur_eval: eval_repair_state(&board, &HashSet::new(), 1, None),
            pour_keepouts: Vec::new(),
            zone_state: None,
            drc_static: RepairDrcStatic::new(&board),
        };
        assert_eq!(untimed_state.configure_pass_budget(), Some(None));
        assert_eq!(untimed_state.pass_cfg.max_passes, 90);
        assert_eq!(untimed_state.pass_cfg.multi_start, 6);
        assert!(untimed_state.pass_cfg.optimize);
        assert_eq!(untimed_state.pass_cfg.timeout_secs, None);

        // Timed repair pass caps max_passes/multi_start, clears pass_cfg.timeout_secs, and allows Stage 2 below start_margin.
        let timed_cfg = RouteOptions {
            max_passes: 90,
            multi_start: 6,
            optimize: true,
            timeout_secs: Some(20),
            ..Default::default()
        };
        let mut timed_pass_cfg = timed_cfg.clone();
        timed_pass_cfg.timeout_secs = None;
        let mut state_ok = RepairState {
            cfg: &timed_cfg,
            pass_cfg: timed_pass_cfg.clone(),
            hard_deadline: Some(Instant::now() + Duration::from_secs(8)),
            repair_passes: 4,
            repair_passes_run: 0,
            passes_run: 1,
            starts_run: 1,
            any_stopped_early: summary.stopped_early,
            summary: summary.clone(),
            repaired_nets: HashSet::new(),
            cur_eval: eval_repair_state(&board, &HashSet::new(), 1, None),
            pour_keepouts: Vec::new(),
            zone_state: None,
            drc_static: RepairDrcStatic::new(&board),
        };
        assert!(state_ok.can_step());
        let dl = state_ok.configure_pass_budget().unwrap().unwrap();
        assert_eq!(state_ok.pass_cfg.max_passes, REPAIR_PASS_MAX_PASSES);
        assert_eq!(state_ok.pass_cfg.multi_start, 1);
        assert!(!state_ok.pass_cfg.optimize);
        assert_eq!(state_ok.pass_cfg.timeout_secs, None);
        assert!(dl <= state_ok.hard_deadline.unwrap() - Duration::from_secs(3));

        // Stage 2 with remaining pass time between safety_buffer + MIN_STAGE2_DURATION and start_margin (e.g. 4s < 5s) succeeds.
        let mut state_stage2 = RepairState {
            cfg: &timed_cfg,
            pass_cfg: timed_pass_cfg.clone(),
            hard_deadline: Some(Instant::now() + Duration::from_millis(7500)),
            repair_passes: 4,
            repair_passes_run: 1,
            passes_run: 1,
            starts_run: 1,
            any_stopped_early: false,
            summary: summary.clone(),
            repaired_nets: HashSet::new(),
            cur_eval: eval_repair_state(&board, &HashSet::new(), 1, None),
            pour_keepouts: Vec::new(),
            zone_state: None,
            drc_static: RepairDrcStatic::new(&board),
        };
        assert!(state_stage2.prepare_stage2(Some(Instant::now() + Duration::from_secs(4))));
        assert_eq!(state_stage2.pass_cfg.max_passes, REPAIR_PASS_MAX_PASSES);
        assert_eq!(state_stage2.pass_cfg.multi_start, 1);
        assert!(!state_stage2.pass_cfg.optimize);
        assert_eq!(state_stage2.pass_cfg.timeout_secs, None);
        assert!(!state_stage2.prepare_stage2(Some(Instant::now() + Duration::from_millis(200))));

        // Remaining < start_margin (5s) -> cannot start a new pass, and detects actionable work.
        let mut state_low = RepairState {
            cfg: &timed_cfg,
            pass_cfg: timed_pass_cfg,
            hard_deadline: Some(Instant::now() + Duration::from_secs(3)),
            repair_passes: 4,
            repair_passes_run: 0,
            passes_run: 1,
            starts_run: 1,
            any_stopped_early: false,
            summary,
            repaired_nets: HashSet::new(),
            cur_eval: eval_repair_state(&board, &HashSet::new(), 1, None),
            pour_keepouts: Vec::new(),
            zone_state: None,
            drc_static: RepairDrcStatic::new(&board),
        };
        assert!(!state_low.can_step());
        assert!(!state_low.has_start_budget());
        assert_eq!(state_low.configure_pass_budget(), None);

        // Generous budget (> 15s remaining, >= 10s pass_dur) keeps optimize=true and caps multi_start at REPAIR_PASS_MAX_MULTI_START.
        let mut state_gen = RepairState {
            cfg: &timed_cfg,
            pass_cfg: RouteOptions {
                timeout_secs: None,
                ..timed_cfg.clone()
            },
            hard_deadline: Some(Instant::now() + Duration::from_secs(45)),
            repair_passes: 2,
            repair_passes_run: 0,
            passes_run: 0,
            starts_run: 0,
            any_stopped_early: false,
            summary: state_low.summary,
            repaired_nets: HashSet::new(),
            cur_eval: eval_repair_state(&board, &HashSet::new(), 0, None),
            pour_keepouts: Vec::new(),
            zone_state: None,
            drc_static: RepairDrcStatic::new(&board),
        };
        assert!(state_gen.configure_pass_budget().is_some());
        assert_eq!(state_gen.pass_cfg.max_passes, REPAIR_PASS_MAX_PASSES);
        assert_eq!(state_gen.pass_cfg.multi_start, REPAIR_PASS_MAX_MULTI_START);
        assert!(state_gen.pass_cfg.optimize);
    }

    #[test]
    fn check_repair_drc_matches_board_drc_eval_across_violation_kinds() {
        let spec: BoardSpec = toml::from_str(
            r#"
name = "repair_drc_parity"
size_nm = [40000000, 30000000]
[[layers]]
name = "F.Cu"
kind = "signal"
side = "top"
[[layers]]
name = "B.Cu"
kind = "signal"
side = "bottom"
[footprints.PAD1]
kind = "custom"
[[footprints.PAD1.pads]]
name = "1"
kind = "smd"
shape = { shape = "circle", diameter = 600000 }
pos = { x = 0, y = 0 }
layers = ["F.Cu", "F.Mask"]
[[placements]]
refdes = "U1"
footprint = "PAD1"
pos = { x = 4000000, y = 10000000 }
locked = true
pin_nets = { "1" = "OPEN_NET" }
[[placements]]
refdes = "U2"
footprint = "PAD1"
pos = { x = 34000000, y = 10000000 }
locked = true
pin_nets = { "1" = "OPEN_NET" }
[[placements]]
refdes = "C1"
footprint = "PAD1"
pos = { x = 4000000, y = 10000000 }
side = "bottom"
locked = true
pin_nets = { "1" = "SIG_A" }
[[placements]]
refdes = "C2"
footprint = "PAD1"
pos = { x = 4000000, y = 10000000 }
side = "bottom"
locked = true
pin_nets = { "1" = "SIG_B" }
[rules]
clearance_nm = 150000
hole_to_hole_nm = 250000
edge_clearance_nm = 300000
mask_to_copper_nm = 200000
"#,
        )
        .unwrap();
        let mut board = BoardIr::from_spec(&spec).unwrap();
        board.constraints.max_lengths.push(MaxLengthRule {
            nets: vec!["SIG_A".into()],
            max_nm: mm(5.0),
        });
        board.constraints.match_groups.push(MatchGroupRule {
            name: "MG".into(),
            nets: vec!["SIG_A".into(), "SIG_B".into()],
            tolerance_nm: mm(0.1),
            target_nm: None,
        });
        board.constraints.max_vias.push(MaxViasRule {
            nets: vec!["SIG_A".into()],
            max_vias: 1,
        });
        board.constraints.cap_proximity.push(CapProximityRule {
            cap_refdes: "C1".into(),
            ic_refdes: "U2".into(),
            net: String::new(),
            max_dist_nm: mm(1.0),
        });
        board.constraints.bga_shadows.push(BgaShadowRule {
            bga_refdes: "U1".into(),
            min_clearance_nm: mm(0.5),
            allow_passives: false,
        });
        board.tracks = vec![
            // Long + dangling track on SIG_A, plus close to SIG_B track (Clearance) and U1 mask (SolderMaskToCopper).
            Track {
                net: "SIG_A".into(),
                layer: "F.Cu".into(),
                a: Pt::new(mm(4.35), mm(10.0)),
                b: Pt::new(mm(16.0), mm(10.0)),
                width: mm(0.2),
                locked: false,
            },
            Track {
                net: "SIG_B".into(),
                layer: "F.Cu".into(),
                a: Pt::new(mm(8.0), mm(10.22)),
                b: Pt::new(mm(14.0), mm(10.22)),
                width: mm(0.2),
                locked: false,
            },
            // Track passing too close to via drill on SIG_B (DrillToCopper + Clearance).
            Track {
                net: "SIG_A".into(),
                layer: "F.Cu".into(),
                a: Pt::new(mm(20.0), mm(20.0)),
                b: Pt::new(mm(25.0), mm(20.0)),
                width: mm(0.2),
                locked: false,
            },
        ];
        board.vias = vec![
            Via {
                net: "SIG_A".into(),
                pos: Pt::new(mm(16.0), mm(10.0)),
                diameter: mm(0.45),
                drill: mm(0.25),
                layers: ("F.Cu".into(), "B.Cu".into()),
                locked: false,
                tented: true,
            },
            // Close second via causing HoleToHole + MaxVias + DrillToCopper.
            Via {
                net: "SIG_A".into(),
                pos: Pt::new(mm(16.3), mm(10.0)),
                diameter: mm(0.45),
                drill: mm(0.25),
                layers: ("F.Cu".into(), "B.Cu".into()),
                locked: false,
                tented: true,
            },
            Via {
                net: "SIG_B".into(),
                pos: Pt::new(mm(22.0), mm(20.15)),
                diameter: mm(0.45),
                drill: mm(0.25),
                layers: ("F.Cu".into(), "B.Cu".into()),
                locked: false,
                tented: true,
            },
        ];

        board.footprints.insert(
            "PAD_B".into(),
            FootprintDef {
                name: "PAD_B".into(),
                pads: vec![PadDef {
                    name: "1".into(),
                    kind: PadKind::Smd,
                    shape: PadShape::Circle { diameter: 600_000 },
                    pos: Pt::new(0, 0),
                    rot_deg: 0.0,
                    drill_nm: None,
                    layers: vec!["B.Cu".into(), "B.Mask".into()],
                    mask_margin_nm: None,
                    paste_margin_nm: None,
                }],
                courtyard: Vec::new(),
                graphics: Vec::new(),
                keepouts: Vec::new(),
                models: Vec::new(),
                pin_types: Default::default(),
                kicad_raw: None,
            },
        );
        board.placements.push(Placement {
            refdes: "J1".into(),
            footprint: "PAD_B".into(),
            pos: Pt::new(mm(6.0), mm(25.0)),
            side: Side::Top,
            locked: true,
            pin_nets: [("1".into(), "GND".into())].into_iter().collect(),
            ..Default::default()
        });
        board.placements.push(Placement {
            refdes: "J2".into(),
            footprint: "PAD_B".into(),
            pos: Pt::new(mm(34.0), mm(25.0)),
            side: Side::Top,
            locked: true,
            pin_nets: [("1".into(), "GND".into())].into_iter().collect(),
            ..Default::default()
        });
        board.keepouts.push(Keepout {
            kind: KeepoutKind::Copper,
            layers: vec!["B.Cu".into()],
            poly: vec![
                Pt::new(mm(19.0), 0),
                Pt::new(mm(21.0), 0),
                Pt::new(mm(21.0), mm(30.0)),
                Pt::new(mm(19.0), mm(30.0)),
            ],
        });
        board.pours.push(PourSpec {
            name: "GND_B".into(),
            net: "GND".into(),
            layer: "B.Cu".into(),
            outline: board.outline.clone(),
            min_width_nm: 200_000,
            ..Default::default()
        });
        let zone_rep = fill_zones(&mut board);

        let ignored = HashSet::new();
        let fast = eval_repair_state(&board, &ignored, 1, Some(&zone_rep));
        let full_drc = check_board_drc(&board);
        let ref_eval =
            eval_repair_violations(&board, full_drc.violations, &ignored, 1, Some(&zone_rep));

        assert_eq!(fast.score, ref_eval.score);
        assert_eq!(fast.constraint_excess, ref_eval.constraint_excess);
        assert_eq!(fast.repairable, ref_eval.repairable);
        assert_eq!(fast.pour_opens, ref_eval.pour_opens);
        assert!(!fast.pour_opens.is_empty());
        assert!(
            fast.repairable
                .iter()
                .any(|v| v.kind == ViolationKind::SolderMaskToCopper)
        );
        assert!(
            fast.repairable
                .iter()
                .any(|v| v.kind == ViolationKind::HoleToHole)
        );
        assert!(
            fast.repairable
                .iter()
                .any(|v| v.kind == ViolationKind::DrillToCopper)
        );
        assert!(
            fast.repairable
                .iter()
                .any(|v| v.kind == ViolationKind::TrackDangling)
        );
        assert!(
            fast.repairable
                .iter()
                .any(|v| v.kind == ViolationKind::OpenNet)
        );
        assert_eq!(fast.score.max_length_viols, 1);
        assert_eq!(fast.score.max_vias_viols, 1);
    }

    #[test]
    fn route_with_repair_and_zones_handoff_reused_or_invalidated_by_tuning() {
        let mut spec: BoardSpec = toml::from_str(
            r#"
name = "zone_handoff"
size_nm = [30000000, 20000000]
[[layers]]
name = "F.Cu"
kind = "signal"
side = "top"
[[layers]]
name = "B.Cu"
kind = "plane"
side = "bottom"
[footprints.PAD]
kind = "custom"
[[footprints.PAD.pads]]
name = "1"
kind = "smd"
shape = { shape = "rect", width = 600000, height = 600000 }
pos = { x = 0, y = 0 }
layers = ["F.Cu"]
[[placements]]
refdes = "U1"
footprint = "PAD"
pos = { x = 4000000, y = 6000000 }
locked = true
pin_nets = { "1" = "SIG_A" }
[[placements]]
refdes = "U2"
footprint = "PAD"
pos = { x = 10000000, y = 6000000 }
locked = true
pin_nets = { "1" = "SIG_A" }
[[placements]]
refdes = "U3"
footprint = "PAD"
pos = { x = 4000000, y = 14000000 }
locked = true
pin_nets = { "1" = "SIG_B" }
[[placements]]
refdes = "U4"
footprint = "PAD"
pos = { x = 26000000, y = 14000000 }
locked = true
pin_nets = { "1" = "SIG_B" }
[rules]
clearance_nm = 150000
edge_clearance_nm = 300000
track_width_nm = 200000
"#,
        )
        .unwrap();
        spec.pours.push(PourSpec {
            name: "GND_B".into(),
            net: "GND".into(),
            layer: "B.Cu".into(),
            outline: vec![
                Pt::new(0, 0),
                Pt::new(mm(30.0), 0),
                Pt::new(mm(30.0), mm(20.0)),
                Pt::new(0, mm(20.0)),
            ],
            min_width_nm: 200_000,
            remove_islands: false,
            ..Default::default()
        });
        let base_board = BoardIr::from_spec(&spec).unwrap();

        let mut no_tune_board = base_board.clone();
        let no_tune_cfg = RouteOptions {
            tune_match_groups: false,
            ..Default::default()
        };
        let no_tune = route_with_repair_and_zones(&mut no_tune_board, &no_tune_cfg, 1).unwrap();
        let (filled, rep) = no_tune
            .zones
            .expect("zones should be handed off when tuning makes no edits");
        assert!(!filled.is_empty());
        assert_eq!(rep.pours.len(), 1);

        let mut tuned_board = base_board;
        tuned_board.constraints.match_groups.push(MatchGroupRule {
            name: "MG".into(),
            nets: vec!["SIG_A".into(), "SIG_B".into()],
            tolerance_nm: mm(0.1),
            target_nm: None,
        });
        let tune_cfg = RouteOptions {
            tune_match_groups: true,
            ..Default::default()
        };
        let after_tune = route_with_repair_and_zones(&mut tuned_board, &tune_cfg, 1).unwrap();
        assert!(
            after_tune.zones.is_none(),
            "precomputed zones must be invalidated when match-group tuning edits tracks"
        );
    }
}
