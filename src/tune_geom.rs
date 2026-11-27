use crate::geom::EPS_NM;
use crate::ir::{Nm, Pt};
use crate::tune_checks::{SpanContext, TuneScene};

const MIN_MEANDER_HEIGHT_NM: f64 = 5_000.0;
const BINARY_SEARCH_STEPS: usize = 22;

pub(crate) struct MeanderCandidate {
    pub(crate) pts: Vec<Pt>,
    pub(crate) segs: Vec<(Pt, Pt, bool)>,
    pub(crate) added_len: f64,
    pub(crate) lobes: usize,
    pub(crate) height: f64,
}

#[derive(Clone, Copy)]
struct SearchBudget {
    want: f64,
    cap: f64,
    h_max: f64,
}

fn polyline_len(pts: &[Pt]) -> f64 {
    pts.windows(2).map(|w| w[0].dist(w[1])).sum()
}

pub(crate) fn split_subspans(a: Pt, b: Pt, splits: &[f64]) -> Vec<(Pt, Pt)> {
    let len = a.dist(b);
    let ux = (b.x - a.x) as f64 / len;
    let uy = (b.y - a.y) as f64 / len;
    let mut pts = Vec::with_capacity(splits.len() + 2);
    pts.push(a);
    for &s in splits {
        pts.push(Pt::new(
            a.x + (s * ux).round() as Nm,
            a.y + (s * uy).round() as Nm,
        ));
    }
    pts.push(b);
    pts.dedup();
    pts.windows(2).map(|w| (w[0], w[1])).collect()
}

fn build_wave_candidate(
    ctx: &SpanContext<'_>,
    s0: f64,
    s1: f64,
    side: f64,
    lobes: usize,
    height: f64,
) -> Option<MeanderCandidate> {
    let span_len = ctx.span_a.dist(ctx.span_b);
    let wave_span = s1 - s0;
    if lobes == 0 || wave_span < ctx.step || height < MIN_MEANDER_HEIGHT_NM {
        return None;
    }
    let gap = if lobes > 1 { ctx.step } else { 0.0 };
    let lobe_w = (wave_span - (lobes - 1) as f64 * gap) / lobes as f64;
    if lobe_w < ctx.step - 1.0 {
        return None;
    }

    let ux = (ctx.span_b.x - ctx.span_a.x) as f64 / span_len;
    let uy = (ctx.span_b.y - ctx.span_a.y) as f64 / span_len;
    let vx = -uy * side;
    let vy = ux * side;
    let pt_at = |s: f64, h: f64| -> Pt {
        Pt::new(
            ctx.span_a.x + (s * ux + h * vx).round() as Nm,
            ctx.span_a.y + (s * uy + h * vy).round() as Nm,
        )
    };

    let m_limit = ((lobe_w - ctx.step) * 0.25).max(0.0).min(ctx.step * 0.75);
    let mut pts = Vec::with_capacity(lobes * 8 + 2);
    pts.push(ctx.span_a);
    let mut s = s0;
    for i in 0..lobes {
        pts.push(pt_at(s, 0.0));
        if m_limit <= 1_000.0 {
            pts.push(pt_at(s, height));
            pts.push(pt_at(s + lobe_w, height));
        } else if height <= 2.0 * m_limit {
            let top_flat = (lobe_w - 2.0 * height).max(ctx.step);
            pts.push(pt_at(s + height, height));
            pts.push(pt_at(s + height + top_flat, height));
        } else {
            let m = m_limit;
            let top_flat = (lobe_w - 4.0 * m).max(ctx.step);
            pts.push(pt_at(s + m, m));
            pts.push(pt_at(s + m, height - m));
            pts.push(pt_at(s + 2.0 * m, height));
            pts.push(pt_at(s + 2.0 * m + top_flat, height));
            pts.push(pt_at(s + 3.0 * m + top_flat, height - m));
            pts.push(pt_at(s + 3.0 * m + top_flat, m));
        }
        pts.push(pt_at(s + lobe_w, 0.0));
        s += lobe_w + if i + 1 < lobes { gap } else { 0.0 };
    }
    pts.push(ctx.span_b);

    let mut cleaned: Vec<Pt> = Vec::with_capacity(pts.len());
    for p in pts {
        if cleaned.last() != Some(&p) {
            cleaned.push(p);
        }
    }
    if cleaned.len() < 4 || cleaned[0] != ctx.span_a || cleaned[cleaned.len() - 1] != ctx.span_b {
        return None;
    }
    let added_len = polyline_len(&cleaned) - span_len;
    if added_len <= 0.0 {
        return None;
    }
    let n_segs = cleaned.len() - 1;
    let segs: Vec<(Pt, Pt, bool)> = (0..n_segs)
        .map(|i| (cleaned[i], cleaned[i + 1], i > 0 && i + 1 < n_segs))
        .collect();
    Some(MeanderCandidate {
        pts: cleaned,
        segs,
        added_len,
        lobes,
        height,
    })
}

fn better_candidate(a: &MeanderCandidate, b: &MeanderCandidate, want: f64) -> bool {
    let a_meets = a.added_len >= want - EPS_NM;
    let b_meets = b.added_len >= want - EPS_NM;
    match (a_meets, b_meets) {
        (true, false) => true,
        (false, true) => false,
        (true, true) => {
            (a.lobes, a.height, a.added_len).partial_cmp(&(b.lobes, b.height, b.added_len))
                == Some(std::cmp::Ordering::Less)
        }
        (false, false) => a.added_len > b.added_len,
    }
}

fn search_wave_amplitude(
    scene: &mut TuneScene<'_>,
    ctx: &SpanContext<'_>,
    s0: f64,
    s1: f64,
    side: f64,
    lobes: usize,
    budget: SearchBudget,
) -> Option<MeanderCandidate> {
    let mut lo = 0.0_f64;
    let mut hi = budget.h_max;
    let mut best: Option<MeanderCandidate> = None;

    for _ in 0..BINARY_SEARCH_STEPS {
        let mid = 0.5 * (lo + hi);
        if mid < MIN_MEANDER_HEIGHT_NM {
            lo = mid;
            continue;
        }
        let Some(cand) = build_wave_candidate(ctx, s0, s1, side, lobes, mid) else {
            lo = mid;
            continue;
        };
        if cand.added_len > budget.cap || !scene.segments_are_clean(ctx, &cand.segs) {
            hi = mid;
            continue;
        }
        let meets_want = cand.added_len >= budget.want - EPS_NM;
        if best
            .as_ref()
            .is_none_or(|b| better_candidate(&cand, b, budget.want))
        {
            best = Some(cand);
        }
        if meets_want {
            hi = mid;
        } else {
            lo = mid;
        }
    }
    best
}

fn solve_span_run(
    scene: &mut TuneScene<'_>,
    ctx: &SpanContext<'_>,
    s0: f64,
    s1: f64,
    side: f64,
    budget: SearchBudget,
) -> Option<MeanderCandidate> {
    let wave_span = s1 - s0;
    if wave_span < ctx.step {
        return None;
    }
    let c1 = search_wave_amplitude(scene, ctx, s0, s1, side, 1, budget)?;
    if c1.added_len >= budget.want - EPS_NM {
        return Some(c1);
    }

    let k_max = ((wave_span + ctx.step) / (2.0 * ctx.step)).floor().max(1.0) as usize;
    if k_max < 2 {
        return Some(c1);
    }
    let mut best = c1;
    let est = (budget.want / best.added_len.max(1.0)).ceil() as usize;
    let mut tried = Vec::new();
    for k in [
        est.clamp(2, k_max),
        (est + 1).clamp(2, k_max),
        ((est + k_max) / 2).clamp(2, k_max),
        k_max,
    ] {
        if tried.contains(&k) {
            continue;
        }
        tried.push(k);
        if let Some(ck) = search_wave_amplitude(scene, ctx, s0, s1, side, k, budget) {
            let done = ck.added_len >= budget.want - EPS_NM;
            if better_candidate(&ck, &best, budget.want) {
                best = ck;
            }
            if done {
                break;
            }
        }
    }
    Some(best)
}

fn probe_clear_runs(
    scene: &mut TuneScene<'_>,
    ctx: &SpanContext<'_>,
    s_lo: f64,
    s_hi: f64,
    side: f64,
    h_probe: f64,
) -> Vec<(f64, f64)> {
    let total = s_hi - s_lo;
    if total < ctx.step {
        return Vec::new();
    }
    let slice_w = (total / 24.0).clamp(ctx.step, 1_000_000.0);
    let mut runs = Vec::new();
    let mut cur_start: Option<f64> = None;
    let mut s = s_lo;
    while s + ctx.step <= s_hi + 1.0 {
        let s_end = (s + slice_w).min(s_hi);
        let ok = build_wave_candidate(ctx, s, s_end, side, 1, h_probe)
            .is_some_and(|c| scene.segments_are_clean(ctx, &c.segs));
        if ok {
            if cur_start.is_none() {
                cur_start = Some(s);
            }
        } else if let Some(rs) = cur_start.take()
            && s - rs >= ctx.step
        {
            runs.push((rs, s));
        }
        s += slice_w;
    }
    if let Some(rs) = cur_start
        && s_hi - rs >= ctx.step
    {
        runs.push((rs, s_hi));
    }
    runs.sort_by(|a, b| (b.1 - b.0).total_cmp(&(a.1 - a.0)));
    runs
}

pub(crate) fn find_best_meander_for_span(
    scene: &mut TuneScene<'_>,
    ctx: &SpanContext<'_>,
    lead_a: f64,
    lead_b: f64,
    want: f64,
    cap: f64,
) -> Option<MeanderCandidate> {
    let span_len = ctx.span_a.dist(ctx.span_b);
    let s_lo = lead_a;
    let s_hi = span_len - lead_b;
    if s_hi - s_lo < ctx.step {
        return None;
    }
    let budget = SearchBudget {
        want,
        cap,
        h_max: (want + 2.0 * ctx.step).min(scene.board_max_extent()),
    };
    let mut best: Option<MeanderCandidate> = None;
    let probe_heights = [
        ctx.step.clamp(MIN_MEANDER_HEIGHT_NM, 100_000.0),
        (want * 0.2).clamp(ctx.step, 1_500_000.0),
        (want * 0.45).clamp(ctx.step, 3_500_000.0),
    ];

    for side in [1.0, -1.0] {
        if let Some(cand) = solve_span_run(scene, ctx, s_lo, s_hi, side, budget) {
            if best
                .as_ref()
                .is_none_or(|b| better_candidate(&cand, b, want))
            {
                best = Some(cand);
            }
            if best.as_ref().is_some_and(|b| b.added_len >= want - EPS_NM) {
                continue;
            }
        }
        // Full span clipped or blocked; probe contiguous clear sub-intervals at shallow then deeper heights.
        for &h_probe in &probe_heights {
            if best.as_ref().is_some_and(|b| b.added_len >= want - EPS_NM) {
                break;
            }
            for (rs, re) in probe_clear_runs(scene, ctx, s_lo, s_hi, side, h_probe) {
                if let Some(cand) = solve_span_run(scene, ctx, rs, re, side, budget)
                    && best
                        .as_ref()
                        .is_none_or(|b| better_candidate(&cand, b, want))
                {
                    best = Some(cand);
                }
                if best.as_ref().is_some_and(|b| b.added_len >= want - EPS_NM) {
                    break;
                }
            }
        }
    }
    best
}
