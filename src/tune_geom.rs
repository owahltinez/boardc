use crate::geom::{EPS_NM, point_to_segment_dist, segment_to_segment_dist};
use crate::ir::{Nm, Pt};
use crate::shapes::ResolvedShape;
use crate::tune_checks::{SpanContext, TuneScene};

const MIN_MEANDER_HEIGHT_NM: f64 = 5_000.0;
const BINARY_SEARCH_STEPS: usize = 22;
const DONE_SLACK_NM: f64 = 2_000.0;
pub(crate) const TOUCH_TOL_NM: f64 = 1.0;

pub(crate) fn push_contact(
    out: &mut Vec<(f64, f64)>,
    len: f64,
    touches_a: bool,
    touches_b: bool,
    lo: f64,
    hi: f64,
) {
    if touches_a && touches_b {
        out.push((0.0, len));
    } else if !touches_a && !touches_b {
        let (c0, c1) = (lo.clamp(0.0, len), hi.clamp(0.0, len));
        if c1 > c0 + TOUCH_TOL_NM {
            out.push((c0, c1));
        }
    }
}

pub(crate) fn push_circle_contact(
    out: &mut Vec<(f64, f64)>,
    (a, b): (Pt, Pt),
    (ux, uy, len): (f64, f64, f64),
    c: Pt,
    reach: f64,
) {
    if point_to_segment_dist(c, a, b) <= reach {
        let s = (c.x - a.x) as f64 * ux + (c.y - a.y) as f64 * uy;
        push_contact(
            out,
            len,
            c.dist(a) <= reach,
            c.dist(b) <= reach,
            s - reach,
            s + reach,
        );
    }
}

pub(crate) fn track_contact_interval(
    a: Pt,
    ux: f64,
    uy: f64,
    len: f64,
    ta: Pt,
    tb: Pt,
    reach: f64,
) -> (f64, f64) {
    let to_uv = |p: Pt| {
        let (dx, dy) = ((p.x - a.x) as f64, (p.y - a.y) as f64);
        (dx * ux + dy * uy, -dx * uy + dy * ux)
    };
    let ((u0, v0), (u1, v1)) = (to_uv(ta), to_uv(tb));
    let (mut lo, mut hi) = (len, 0.0_f64);
    for &(uc, vc) in &[(u0, v0), (u1, v1)] {
        if vc.abs() <= reach {
            let dx = (reach * reach - vc * vc).max(0.0).sqrt();
            lo = lo.min(uc - dx);
            hi = hi.max(uc + dx);
        }
    }
    let (du, dv) = (u1 - u0, v1 - v0);
    let seg_len = du.hypot(dv);
    if seg_len > 0.0 && dv.abs() > 1e-9 {
        for sign in [-1.0_f64, 1.0_f64] {
            let tau = -(v0 + sign * reach * (du / seg_len)) / dv;
            if (0.0..=1.0).contains(&tau) {
                let u = u0 + tau * du + sign * reach * (-dv / seg_len);
                lo = lo.min(u);
                hi = hi.max(u);
            }
        }
    }
    (lo.clamp(0.0, len), hi.clamp(0.0, len))
}

pub(crate) fn project_shape_interval(
    shape: &ResolvedShape,
    a: Pt,
    ux: f64,
    uy: f64,
    r: Nm,
) -> (f64, f64) {
    let proj = |p: Pt| (p.x - a.x) as f64 * ux + (p.y - a.y) as f64 * uy;
    let m_base = r as f64 + TOUCH_TOL_NM;
    match shape {
        ResolvedShape::Circle { center, radius } => {
            let (s, m) = (proj(*center), *radius as f64 + m_base);
            (s - m, s + m)
        }
        ResolvedShape::Capsule {
            a: pa,
            b: pb,
            radius,
        } => {
            let (s0, s1, m) = (proj(*pa), proj(*pb), *radius as f64 + m_base);
            (s0.min(s1) - m, s0.max(s1) + m)
        }
        ResolvedShape::Poly { verts, .. } => {
            let (mut lo, mut hi) = (f64::INFINITY, f64::NEG_INFINITY);
            for &v in verts {
                let s = proj(v);
                lo = lo.min(s);
                hi = hi.max(s);
            }
            (lo - m_base, hi + m_base)
        }
    }
}

pub(crate) fn polyline_self_clear(pts: &[Pt], step: f64) -> bool {
    let n_segs = pts.len().saturating_sub(1);
    if n_segs == 0 {
        return false;
    }
    // Forbid centerline crossings and opposing-leg crowding; allow obtuse/parallel corner steps.
    for i in 0..n_segs {
        let (a1, a2) = (pts[i], pts[i + 1]);
        let (dx1, dy1) = ((a2.x - a1.x) as f64, (a2.y - a1.y) as f64);
        let len1 = dx1.hypot(dy1);
        if len1 <= 0.0 {
            continue;
        }
        for j in (i + 2)..n_segs {
            let (b1, b2) = (pts[j], pts[j + 1]);
            let d = segment_to_segment_dist(a1, a2, b1, b2);
            if d == 0.0 {
                return false;
            }
            let (dx2, dy2) = ((b2.x - b1.x) as f64, (b2.y - b1.y) as f64);
            let len2 = dx2.hypot(dy2);
            if len2 > 0.0 && (dx1 * dx2 + dy1 * dy2) / (len1 * len2) < -0.5 && d < step - EPS_NM {
                return false;
            }
        }
    }
    true
}

pub(crate) struct MeanderCandidate {
    pub(crate) pts: Vec<Pt>,
    pub(crate) added_len: f64,
    pub(crate) lobes: usize,
    pub(crate) height: f64,
}

#[derive(Clone, Copy)]
pub(crate) struct SearchBudget {
    pub(crate) want: f64,
    pub(crate) cap: f64,
    pub(crate) done_threshold: f64,
    pub(crate) h_max: f64,
}

impl SearchBudget {
    pub(crate) fn new(min_needed: f64, want: f64, cap: f64, h_max: f64) -> Self {
        let slack = DONE_SLACK_NM.min((want - min_needed).max(0.0)).max(EPS_NM);
        Self {
            want,
            cap,
            done_threshold: (want - slack).max(min_needed - EPS_NM),
            h_max,
        }
    }

    fn meets_want(self, cand: &MeanderCandidate) -> bool {
        cand.added_len >= self.done_threshold
    }
}

fn polyline_len(pts: &[Pt]) -> f64 {
    pts.windows(2).map(|w| w[0].dist(w[1])).sum()
}

/// Splits `(a, b)` around sorted `protected` intervals into `(pa, pb, meanderable)` subspans.
pub(crate) fn split_subspans(a: Pt, b: Pt, protected: &[(f64, f64)]) -> Vec<(Pt, Pt, bool)> {
    let len = a.dist(b);
    if len <= 0.0 {
        return Vec::new();
    }
    let ux = (b.x - a.x) as f64 / len;
    let uy = (b.y - a.y) as f64 / len;
    let pt_at = |s: f64| Pt::new(a.x + (s * ux).round() as Nm, a.y + (s * uy).round() as Nm);
    let mut out = Vec::with_capacity(protected.len() * 2 + 1);
    let mut cur = a;
    for &(s0, s1) in protected {
        let p0 = pt_at(s0.clamp(0.0, len));
        if p0 != cur {
            out.push((cur, p0, true));
            cur = p0;
        }
        let p1 = pt_at(s1.clamp(0.0, len));
        if p1 != cur {
            out.push((cur, p1, false));
            cur = p1;
        }
    }
    if b != cur {
        out.push((cur, b, true));
    }
    out
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
            let top_flat = lobe_w - 2.0 * height;
            pts.push(pt_at(s + height, height));
            pts.push(pt_at(s + height + top_flat, height));
        } else {
            let m = m_limit;
            let top_flat = lobe_w - 4.0 * m;
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

    pts.dedup();
    if pts.len() < 4 {
        return None;
    }
    let added_len = polyline_len(&pts) - span_len;
    if added_len <= 0.0 {
        return None;
    }
    Some(MeanderCandidate {
        pts,
        added_len,
        lobes,
        height,
    })
}

fn better_candidate(a: &MeanderCandidate, b: &MeanderCandidate, budget: SearchBudget) -> bool {
    match (budget.meets_want(a), budget.meets_want(b)) {
        (true, false) => true,
        (false, true) => false,
        (true, true) => {
            let a_err = (a.added_len - budget.want).max(0.0);
            let b_err = (b.added_len - budget.want).max(0.0);
            (a.lobes, a.height, a_err).partial_cmp(&(b.lobes, b.height, b_err))
                == Some(std::cmp::Ordering::Less)
        }
        (false, false) => a.added_len > b.added_len,
    }
}

fn update_best(
    best: &mut Option<MeanderCandidate>,
    cand: MeanderCandidate,
    budget: SearchBudget,
) -> bool {
    if best
        .as_ref()
        .is_none_or(|b| better_candidate(&cand, b, budget))
    {
        *best = Some(cand);
    }
    best.as_ref().is_some_and(|b| budget.meets_want(b))
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
        if cand.added_len > budget.cap || !scene.segments_are_clean(ctx, &cand.pts) {
            hi = mid;
            continue;
        }
        let meets_want = cand.added_len >= budget.want - EPS_NM;
        update_best(&mut best, cand, budget);
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
    let mut best = search_wave_amplitude(scene, ctx, s0, s1, side, 1, budget)?;
    if budget.meets_want(&best) {
        return Some(best);
    }

    let k_max = ((wave_span + ctx.step) / (2.0 * ctx.step)).floor().max(1.0) as usize;
    if k_max < 2 {
        return Some(best);
    }
    let est = (budget.want / best.added_len.max(1.0)).ceil() as usize;
    let ks = [
        est.clamp(2, k_max),
        (est + 1).clamp(2, k_max),
        ((est + k_max) / 2).clamp(2, k_max),
        k_max,
    ];
    for (idx, &k) in ks.iter().enumerate() {
        if ks[..idx].contains(&k) {
            continue;
        }
        if let Some(ck) = search_wave_amplitude(scene, ctx, s0, s1, side, k, budget) {
            if better_candidate(&ck, &best, budget) {
                best = ck;
            }
            if budget.meets_want(&best) {
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
            .is_some_and(|c| scene.segments_are_clean(ctx, &c.pts));
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
    min_needed: f64,
    want: f64,
    cap: f64,
) -> Option<MeanderCandidate> {
    let span_len = ctx.span_a.dist(ctx.span_b);
    let s_lo = lead_a;
    let s_hi = span_len - lead_b;
    if s_hi - s_lo < ctx.step {
        return None;
    }
    let budget = SearchBudget::new(
        min_needed,
        want,
        cap,
        (want + 2.0 * ctx.step).min(scene.board_max_extent()),
    );
    let mut best: Option<MeanderCandidate> = None;
    let probe_heights = [
        ctx.step.clamp(MIN_MEANDER_HEIGHT_NM, 100_000.0),
        (want * 0.2).clamp(ctx.step, 1_500_000.0),
        (want * 0.45).clamp(ctx.step, 3_500_000.0),
    ];

    for side in [1.0, -1.0] {
        if let Some(cand) = solve_span_run(scene, ctx, s_lo, s_hi, side, budget)
            && update_best(&mut best, cand, budget)
        {
            return best;
        }
        // Full span clipped or blocked; probe contiguous clear sub-intervals at shallow then deeper heights.
        for &h_probe in &probe_heights {
            for (rs, re) in probe_clear_runs(scene, ctx, s_lo, s_hi, side, h_probe) {
                if let Some(cand) = solve_span_run(scene, ctx, rs, re, side, budget)
                    && update_best(&mut best, cand, budget)
                {
                    return best;
                }
            }
        }
    }
    best
}
