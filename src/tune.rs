use std::collections::{BTreeMap, BTreeSet, HashSet};
use std::time::Instant;

use serde::{Deserialize, Serialize};

use crate::drc_constraints::collect_net_lengths;
use crate::geom::EPS_NM;
use crate::ir::{BoardIr, Nm, Track};
use crate::tune_checks::{SpanContext, TuneScene};
use crate::tune_geom::{find_best_meander_for_span, split_subspans};

/// Options controlling `match_groups` trombone length-matching.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct TuneOptions {
    #[serde(default)]
    pub only_nets: Option<HashSet<String>>,
    #[serde(default)]
    pub ignored_nets: HashSet<String>,
}

/// Per-net tuning summary within a `MatchGroupRule`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TuneNetReport {
    pub net: String,
    pub before_nm: Nm,
    pub after_nm: Nm,
    pub target_nm: Nm,
    pub added_meanders: usize,
    pub added_segments: usize,
    pub met: bool,
}

/// Per-group tuning summary for a `MatchGroupRule`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TuneGroupReport {
    pub name: String,
    pub target_nm: Nm,
    pub tolerance_nm: Nm,
    pub skew_before_nm: Nm,
    pub skew_after_nm: Nm,
    pub nets: Vec<TuneNetReport>,
    pub unmet_nets: Vec<String>,
    pub met: bool,
}

/// Complete report returned by `tune_match_groups`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TuneReport {
    pub ok: bool,
    pub groups: Vec<TuneGroupReport>,
    pub tuned_nets: usize,
    pub added_meanders: usize,
    pub added_segments: usize,
    pub unmet_nets: Vec<String>,
    pub elapsed_ms: u128,
}

#[derive(Clone, Copy)]
struct NetTarget {
    min_required: f64,
    desired_target: f64,
    upper_bound: f64,
}

/// Rejects `--only` targets that match neither a board net nor a `NetClass`.
pub fn validate_tune_targets(board: &BoardIr, targets: &HashSet<String>) -> Result<(), String> {
    if targets.is_empty() {
        return Ok(());
    }
    let nets: HashSet<String> = board.all_net_names().into_iter().collect();
    for t in targets {
        let is_net = nets.contains(t);
        let is_class = t == "Default"
            || board.net_classes.contains_key(t)
            || board.nets.values().any(|n| n.class == *t);
        if !is_net && !is_class {
            return Err(format!("unknown --only target '{t}'"));
        }
    }
    Ok(())
}

fn net_allowed(board: &BoardIr, net: &str, cfg: &TuneOptions) -> bool {
    if board.is_net_ignored(net, &cfg.ignored_nets) {
        return false;
    }
    let class_name = board.nets.get(net).map_or("Default", |n| n.class.as_str());
    cfg.only_nets
        .as_ref()
        .is_none_or(|only| only.contains(net) || only.contains(class_name))
}

fn relative_group_hi(
    nets: &[String],
    lengths: &BTreeMap<String, f64>,
    desireds: &BTreeMap<String, f64>,
) -> Option<f64> {
    nets.iter()
        .filter_map(|n| {
            let cur = lengths.get(n).copied().unwrap_or(0.0);
            (cur > EPS_NM).then(|| cur.max(desireds.get(n).copied().unwrap_or(0.0)))
        })
        .reduce(f64::max)
}

fn compute_net_targets(
    board: &BoardIr,
    lengths: &BTreeMap<String, f64>,
) -> BTreeMap<String, NetTarget> {
    let mut upper_bounds: BTreeMap<String, f64> = BTreeMap::new();
    for rule in &board.constraints.max_lengths {
        for net in &rule.nets {
            upper_bounds
                .entry(net.clone())
                .and_modify(|v| *v = v.min(rule.max_nm as f64))
                .or_insert(rule.max_nm as f64);
        }
    }
    for rule in &board.constraints.match_groups {
        if let Some(target) = rule.target_nm {
            let upper = (target + rule.tolerance_nm.max(0)) as f64;
            for net in &rule.nets {
                upper_bounds
                    .entry(net.clone())
                    .and_modify(|v| *v = v.min(upper))
                    .or_insert(upper);
            }
        }
    }

    let mut min_reqs: BTreeMap<String, f64> = BTreeMap::new();
    let mut desireds: BTreeMap<String, f64> = BTreeMap::new();
    for _ in 0..=board.constraints.match_groups.len() {
        let mut changed = false;
        for rule in &board.constraints.match_groups {
            let tol = rule.tolerance_nm.max(0) as f64;
            let (req, desired_val) = if let Some(target) = rule.target_nm {
                let t = target as f64;
                (t - tol, t)
            } else if rule.nets.len() >= 2 {
                let Some(group_hi) = relative_group_hi(&rule.nets, lengths, &desireds) else {
                    continue;
                };
                (group_hi - tol, group_hi)
            } else {
                continue;
            };
            for net in &rule.nets {
                let cur = lengths.get(net).copied().unwrap_or(0.0);
                if cur > EPS_NM && cur < req - EPS_NM {
                    let prev_req = min_reqs.get(net).copied().unwrap_or(0.0);
                    if req > prev_req + EPS_NM {
                        min_reqs.insert(net.clone(), req);
                        changed = true;
                    }
                    let d = upper_bounds
                        .get(net)
                        .copied()
                        .map_or(desired_val, |u| desired_val.min(u));
                    let prev_d = desireds.get(net).copied().unwrap_or(0.0);
                    if d > prev_d + EPS_NM {
                        desireds.insert(net.clone(), d);
                        changed = true;
                    }
                }
            }
        }
        if !changed {
            break;
        }
    }

    for rule in &board.constraints.match_groups {
        if rule.target_nm.is_none()
            && rule.nets.len() >= 2
            && let Some(group_hi) = relative_group_hi(&rule.nets, lengths, &desireds)
        {
            for net in &rule.nets {
                upper_bounds
                    .entry(net.clone())
                    .and_modify(|v| *v = v.min(group_hi))
                    .or_insert(group_hi);
            }
        }
    }

    let mut targets = BTreeMap::new();
    for (net, min_req) in min_reqs {
        let upper = upper_bounds.get(&net).copied().unwrap_or(f64::INFINITY);
        let desired = desireds.get(&net).copied().unwrap_or(min_req).min(upper);
        let cur = lengths.get(&net).copied().unwrap_or(0.0);
        if upper > cur + EPS_NM && desired > cur + EPS_NM {
            targets.insert(
                net,
                NetTarget {
                    min_required: min_req,
                    desired_target: desired,
                    upper_bound: upper,
                },
            );
        }
    }
    targets
}

fn tune_single_net(
    scene: &mut TuneScene<'_>,
    net: &str,
    cur_len: &mut f64,
    target: NetTarget,
) -> (usize, usize) {
    let mut meanders_added = 0usize;
    let mut segs_delta = 0usize;
    let same_net_clr = scene.board.net_clearance(net).max(1);

    for _pass in 0..3 {
        if *cur_len >= target.min_required - EPS_NM {
            break;
        }
        let mut candidates: Vec<(usize, f64)> = scene
            .tracks_by_net
            .get(net)
            .into_iter()
            .flat_map(|idxs| idxs.iter().copied())
            .filter_map(|i| {
                let t = scene.tracks[i].as_ref()?;
                (!t.locked && t.a != t.b && t.width > 0).then(|| (i, t.a.dist(t.b)))
            })
            .collect();
        candidates.sort_by(|a, b| b.1.total_cmp(&a.1).then_with(|| a.0.cmp(&b.0)));

        let mut progress = 0.0_f64;
        for (orig_idx, _) in candidates {
            if *cur_len >= target.min_required - EPS_NM {
                break;
            }
            let min_needed = (target.min_required - *cur_len).max(0.0);
            let want = (target.desired_target - *cur_len).max(0.0);
            let cap = (target.upper_bound - *cur_len).max(0.0);
            if cap <= 1_000.0 || want <= EPS_NM {
                break;
            }
            let Some(t) = scene.tracks[orig_idx].as_ref() else {
                continue;
            };
            let (layer, a, b, width) = (t.layer.clone(), t.a, t.b, t.width);
            let step = (width + same_net_clr).max(10_000) as f64;
            let protected = scene.protected_intervals(orig_idx, net, &layer, a, b, width);
            let subspans = split_subspans(a, b, &protected);

            for (si, &(span_a, span_b, meanderable)) in subspans.iter().enumerate() {
                if !meanderable {
                    continue;
                }
                let span_len = span_a.dist(span_b);
                if span_len < 3.0 * step {
                    continue;
                }

                let pad_lead_a =
                    scene.endpoint_pad_clear_lead(net, &layer, span_a, width, same_net_clr, step);
                let pad_lead_b =
                    scene.endpoint_pad_clear_lead(net, &layer, span_b, width, same_net_clr, step);
                let (lead_a, lead_b) = if pad_lead_a + pad_lead_b + step <= span_len {
                    (pad_lead_a, pad_lead_b)
                } else {
                    (step, step)
                };

                let ctx = SpanContext {
                    orig_idx,
                    net,
                    layer: &layer,
                    width,
                    span_a,
                    span_b,
                    step,
                };
                let Some(cand) =
                    find_best_meander_for_span(scene, &ctx, lead_a, lead_b, min_needed, want, cap)
                else {
                    continue;
                };

                let mut replacement = Vec::new();
                let mut push_seg = |pa, pb| {
                    replacement.push(Track {
                        net: net.to_string(),
                        layer: layer.clone(),
                        a: pa,
                        b: pb,
                        width,
                        locked: false,
                    });
                };
                for &(pa, pb, _) in &subspans[..si] {
                    push_seg(pa, pb);
                }
                for w in cand.pts.windows(2) {
                    push_seg(w[0], w[1]);
                }
                for &(pa, pb, _) in &subspans[si + 1..] {
                    push_seg(pa, pb);
                }

                let delta = replacement.iter().map(|t| t.a.dist(t.b)).sum::<f64>() - a.dist(b);
                if delta <= 0.0 || *cur_len + delta > target.upper_bound + EPS_NM {
                    continue;
                }
                let added_segs = replacement.len().saturating_sub(1);
                scene.commit_replacement(orig_idx, replacement);
                *cur_len += delta;
                progress += delta;
                meanders_added += 1;
                segs_delta += added_segs;
                break;
            }
        }
        if progress <= 1_000.0 {
            break;
        }
    }
    (meanders_added, segs_delta)
}

fn group_skew_nm(rule_nets: &[String], lengths: &BTreeMap<String, f64>, target: Option<Nm>) -> Nm {
    if let Some(t) = target {
        rule_nets
            .iter()
            .map(|n| (lengths.get(n).copied().unwrap_or(0.0) - t as f64).abs())
            .fold(0.0_f64, f64::max)
            .round() as Nm
    } else if rule_nets.len() >= 2 {
        let mut min_l = f64::INFINITY;
        let mut max_l = f64::NEG_INFINITY;
        for n in rule_nets {
            let l = lengths.get(n).copied().unwrap_or(0.0);
            min_l = min_l.min(l);
            max_l = max_l.max(l);
        }
        (max_l - min_l).max(0.0).round() as Nm
    } else {
        0
    }
}

/// Lengthens short nets in `constraints.match_groups` using DRC-clean trombone meanders.
pub fn tune_match_groups(board: &mut BoardIr, cfg: &TuneOptions) -> TuneReport {
    let t0 = Instant::now();
    let initial_lengths: BTreeMap<String, f64> = collect_net_lengths(board)
        .into_iter()
        .map(|(k, v)| (k.to_string(), v))
        .collect();
    let mut ordered: Vec<(String, NetTarget, f64)> = compute_net_targets(board, &initial_lengths)
        .into_iter()
        .filter(|(net, _)| net_allowed(board, net, cfg))
        .map(|(net, tgt)| {
            let cur = initial_lengths.get(&net).copied().unwrap_or(0.0);
            let deficit = (tgt.desired_target - cur).max(0.0);
            (net, tgt, deficit)
        })
        .collect();
    ordered.sort_by(|a, b| b.2.total_cmp(&a.2).then_with(|| a.0.cmp(&b.0)));

    let mut per_net_stats: BTreeMap<String, (usize, usize)> = BTreeMap::new();
    if !ordered.is_empty() {
        let mut scene = TuneScene::new(board);
        for (net, tgt, _) in ordered {
            let mut len = initial_lengths.get(&net).copied().unwrap_or(0.0);
            let (m, s) = tune_single_net(&mut scene, &net, &mut len, tgt);
            if m > 0 {
                per_net_stats.insert(net, (m, s));
            }
        }
        board.tracks = scene.into_tracks();
    }

    let final_lengths: BTreeMap<String, f64> = collect_net_lengths(board)
        .into_iter()
        .map(|(k, v)| (k.to_string(), v))
        .collect();

    let mut groups = Vec::with_capacity(board.constraints.match_groups.len());
    let mut all_unmet: BTreeSet<String> = BTreeSet::new();
    let mut all_ok = true;

    for rule in &board.constraints.match_groups {
        let skew_before = group_skew_nm(&rule.nets, &initial_lengths, rule.target_nm);
        let skew_after = group_skew_nm(&rule.nets, &final_lengths, rule.target_nm);
        let target_nm = if let Some(t) = rule.target_nm {
            t
        } else {
            rule.nets
                .iter()
                .filter_map(|n| final_lengths.get(n).copied().filter(|&l| l > EPS_NM))
                .reduce(f64::max)
                .unwrap_or(0.0)
                .round() as Nm
        };

        let mut net_reports = Vec::with_capacity(rule.nets.len());
        let mut group_unmet = Vec::new();
        for net in &rule.nets {
            let before = initial_lengths.get(net).copied().unwrap_or(0.0);
            let after = final_lengths.get(net).copied().unwrap_or(0.0);
            let met = if let Some(t) = rule.target_nm {
                (after - t as f64).abs() <= rule.tolerance_nm as f64 + EPS_NM
            } else if rule.nets.len() >= 2 {
                after > EPS_NM && (target_nm as f64 - after) <= rule.tolerance_nm as f64 + EPS_NM
            } else {
                true
            };
            if !met {
                group_unmet.push(net.clone());
                all_unmet.insert(net.clone());
            }
            let (added_meanders, added_segments) =
                per_net_stats.get(net).copied().unwrap_or((0, 0));
            net_reports.push(TuneNetReport {
                net: net.clone(),
                before_nm: before.round() as Nm,
                after_nm: after.round() as Nm,
                target_nm,
                added_meanders,
                added_segments,
                met,
            });
        }
        let group_met =
            skew_after as f64 <= rule.tolerance_nm as f64 + EPS_NM && group_unmet.is_empty();
        if !group_met {
            all_ok = false;
        }
        groups.push(TuneGroupReport {
            name: rule.name.clone(),
            target_nm,
            tolerance_nm: rule.tolerance_nm,
            skew_before_nm: skew_before,
            skew_after_nm: skew_after,
            nets: net_reports,
            unmet_nets: group_unmet,
            met: group_met,
        });
    }

    TuneReport {
        ok: all_ok,
        groups,
        tuned_nets: per_net_stats.len(),
        added_meanders: per_net_stats.values().map(|v| v.0).sum(),
        added_segments: per_net_stats.values().map(|v| v.1).sum(),
        unmet_nets: all_unmet.into_iter().collect(),
        elapsed_ms: t0.elapsed().as_millis(),
    }
}
