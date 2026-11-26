use std::collections::BTreeSet;

use clipper2_rust::{
    EndType, FillRule, JoinType, Paths64, difference_64, inflate_paths_64, intersect_64,
    simplify_paths, union_subjects_64,
};

use crate::geom::{
    ensure_ccw, path64_to_pts, point_to_polygon_dist, polygon_to_polygon_dist, pts_to_path64,
    segment_to_polygon_dist,
};
use crate::ir::{BBoxNm, BoardIr, FilledPour, Nm, PourPoly, PourSpec, Pt};
use crate::poly_index::{IndexedPour, IndexedRing};
use crate::zone_scene::{
    ARC_TOLERANCE, LayerScene, Obstacle, add_cutout_obstacles, build_board_copper_region,
    collect_layer_scene, indexed_pour_intersects_terminal, match_pour_specs, paths_to_pour_polys,
    pour_poly_area, pour_poly_to_paths64,
};
use crate::zones::{BlockerRef, ChokeKind, ZoneChoke};

const CHOKE_SIMPLIFY_EPS: f64 = 2_000.0;

fn blocker_ref(obs: &Obstacle, ref_pt: Pt) -> BlockerRef {
    BlockerRef {
        kind: obs.kind,
        net: obs.net.clone(),
        label: obs.label.clone(),
        pos: obs.pos,
        dist_nm: obs.pos.dist(ref_pt).round() as Nm,
    }
}

fn flood_reachable(seeds: &[bool], adj: &[Vec<usize>]) -> Vec<bool> {
    let mut reach = seeds.to_vec();
    let mut stack: Vec<usize> = (0..seeds.len()).filter(|&i| reach[i]).collect();
    while let Some(u) = stack.pop() {
        for &v in &adj[u] {
            if !reach[v] {
                reach[v] = true;
                stack.push(v);
            }
        }
    }
    reach
}

fn closest_points_between_rings(a: &IndexedRing, b: &IndexedRing) -> (Pt, Pt, f64) {
    let (small, big, swapped) = if a.pts.len() <= b.pts.len() {
        (a, b, false)
    } else {
        (b, a, true)
    };
    let mut best = (Pt::new(0, 0), Pt::new(0, 0), f64::INFINITY);
    // Stride-sample large rings first to get a tight upper bound, then refine nearby vertices.
    let stride_s = (small.pts.len() / 128).max(1);
    let stride_b = (big.pts.len() / 128).max(1);
    for &ps in small.pts.iter().step_by(stride_s) {
        for &pb in big.pts.iter().step_by(stride_b) {
            let d = ps.dist(pb);
            if d < best.2 {
                best = (ps, pb, d);
            }
        }
    }
    let rad = (best.2.ceil() as Nm).saturating_add(500_000);
    let win_s = BBoxNm::from_pt(best.0, rad);
    let win_b = BBoxNm::from_pt(best.1, rad);
    let near_s: Vec<Pt> = small
        .pts
        .iter()
        .copied()
        .filter(|&p| win_s.contains_pt(p))
        .collect();
    let near_b: Vec<Pt> = big
        .pts
        .iter()
        .copied()
        .filter(|&p| win_b.contains_pt(p))
        .collect();
    for &ps in &near_s {
        for &pb in &near_b {
            let d = ps.dist(pb);
            if d < best.2 {
                best = (ps, pb, d);
            }
        }
    }
    if swapped {
        (best.1, best.0, best.2)
    } else {
        best
    }
}

fn obstacle_touches_ring(obs: &Obstacle, obs_pts: &[Pt], ring: &IndexedRing, margin: Nm) -> bool {
    if !ring.bbox.expand(margin).intersects(obs.cleared_bbox) {
        return false;
    }
    ring.min_dist_to_poly_edges_bounded(obs_pts, obs.cleared_bbox, margin) <= margin as f64
}

fn find_split_blockers(
    ring_a: &IndexedRing,
    ring_b: &IndexedRing,
    pa: Pt,
    pb: Pt,
    obstacles: &[Obstacle],
) -> Vec<BlockerRef> {
    let mid = Pt::new((pa.x + pb.x) / 2, (pa.y + pb.y) / 2);
    let margin = 120_000;
    let obs_polys: Vec<Vec<Pt>> = obstacles
        .iter()
        .map(|o| path64_to_pts(&o.cleared_poly))
        .collect();

    let touches_a: Vec<bool> = obstacles
        .iter()
        .enumerate()
        .map(|(i, o)| obstacle_touches_ring(o, &obs_polys[i], ring_a, margin))
        .collect();
    let touches_b: Vec<bool> = obstacles
        .iter()
        .enumerate()
        .map(|(i, o)| obstacle_touches_ring(o, &obs_polys[i], ring_b, margin))
        .collect();

    let mut adj = vec![Vec::new(); obstacles.len()];
    for i in 0..obstacles.len() {
        let bi = obstacles[i].cleared_bbox.expand(margin);
        for j in (i + 1)..obstacles.len() {
            if bi.intersects(obstacles[j].cleared_bbox)
                && polygon_to_polygon_dist(&obs_polys[i], &obs_polys[j]) <= margin as f64
            {
                adj[i].push(j);
                adj[j].push(i);
            }
        }
    }

    let reach_a = flood_reachable(&touches_a, &adj);
    let reach_b = flood_reachable(&touches_b, &adj);

    let mut refs: Vec<BlockerRef> = (0..obstacles.len())
        .filter(|&i| (touches_a[i] && touches_b[i]) || (reach_a[i] && reach_b[i]))
        .map(|i| blocker_ref(&obstacles[i], mid))
        .collect();

    if refs.is_empty() {
        let gap_limit = pa.dist(pb) + 500_000.0;
        let seg_box = BBoxNm::from_pts(&[pa, pb]).expand(gap_limit.ceil() as Nm);
        for (i, obs) in obstacles.iter().enumerate() {
            if !obs.cleared_bbox.intersects(seg_box) {
                continue;
            }
            let d = segment_to_polygon_dist(pa, pb, &obs_polys[i]);
            if d <= gap_limit {
                refs.push(blocker_ref(obs, mid));
            }
        }
    }
    refs.sort_by_key(|r| r.dist_nm);
    refs.dedup_by(|a, b| a.label == b.label && a.pos == b.pos);
    refs
}

fn bridge_connected_thermal_moats(
    island_paths: &Paths64,
    spec: Option<&PourSpec>,
    board_region: &Paths64,
    scene: &LayerScene,
    term_indices: &[usize],
) -> Paths64 {
    let Some(sp) = spec else {
        return island_paths.clone();
    };
    if !sp.thermal_relief || sp.thermal_gap_nm <= 0 || term_indices.is_empty() {
        return island_paths.clone();
    }
    let mut moats = Paths64::new();
    let mut moat_bbox: Option<BBoxNm> = None;
    for &t_idx in term_indices {
        let term = &scene.terminals[t_idx];
        if !term.thermal_relief {
            continue;
        }
        let tb = term.bbox.expand(sp.thermal_gap_nm + 10_000);
        moat_bbox = Some(moat_bbox.map_or(tb, |prev| prev.union(tb)));
        moats.extend(inflate_paths_64(
            &vec![term.poly.clone()],
            sp.thermal_gap_nm as f64 + 5_000.0,
            JoinType::Round,
            EndType::Polygon,
            2.0,
            ARC_TOLERANCE,
        ));
    }
    let Some(mb) = moat_bbox else {
        return island_paths.clone();
    };
    let clipped_moats = if sp.outline.len() >= 3 && !board_region.is_empty() {
        let mut out = sp.outline.clone();
        ensure_ccw(&mut out);
        let pour_bounds = intersect_64(&vec![pts_to_path64(&out)], board_region, FillRule::NonZero);
        if pour_bounds.is_empty() {
            return island_paths.clone();
        }
        intersect_64(&moats, &pour_bounds, FillRule::NonZero)
    } else if !board_region.is_empty() {
        intersect_64(&moats, board_region, FillRule::NonZero)
    } else {
        moats
    };
    let obs_clips: Paths64 = scene
        .obstacles
        .iter()
        .filter(|o| o.cleared_bbox.intersects(mb))
        .map(|o| o.cleared_poly.clone())
        .collect();
    let safe_moats = if obs_clips.is_empty() {
        clipped_moats
    } else {
        difference_64(&clipped_moats, &obs_clips, FillRule::NonZero)
    };
    if safe_moats.is_empty() {
        return island_paths.clone();
    }
    let mut merged = island_paths.clone();
    merged.extend(safe_moats);
    union_subjects_64(&merged, FillRule::NonZero)
}

fn measure_neck_width(
    test_paths: &Paths64,
    max_width: Nm,
    scene: &LayerScene,
    term_indices: &[usize],
) -> Nm {
    let mut lo: Nm = 0;
    let mut hi: Nm = max_width;
    for _ in 0..8 {
        if hi - lo <= 2_000 {
            break;
        }
        let mid = (lo + hi) / 2;
        let eroded = inflate_paths_64(
            test_paths,
            -(mid as f64 / 2.0),
            JoinType::Miter,
            EndType::Polygon,
            2.0,
            ARC_TOLERANCE,
        );
        let comps = paths_to_pour_polys(&eroded);
        let intact = if comps.len() == 1 {
            if term_indices.len() < 2 {
                true
            } else {
                let idx0 = IndexedPour::new(&comps[0]);
                term_indices
                    .iter()
                    .all(|&ti| indexed_pour_intersects_terminal(&idx0, &scene.terminals[ti]))
            }
        } else {
            false
        };
        if intact {
            lo = mid;
        } else {
            hi = mid;
        }
    }
    lo
}

fn find_neck_blockers(
    bridge_pts: &[Pt],
    choke_pos: Pt,
    min_neck_width: Nm,
    obstacles: &[Obstacle],
) -> Vec<BlockerRef> {
    let margin_nm: Nm = 100_000;
    let margin = margin_nm as f64;
    let bridge_bbox = if bridge_pts.len() >= 3 {
        Some(BBoxNm::from_pts(bridge_pts).expand(margin_nm))
    } else {
        None
    };
    let bridge_ring = (bridge_pts.len() > 16).then(|| IndexedRing::new(bridge_pts));
    let mut hits = Vec::new();
    for obs in obstacles {
        if let Some(bb) = bridge_bbox {
            if !obs.cleared_bbox.intersects(bb) {
                continue;
            }
        } else if !obs.cleared_bbox.expand(margin_nm).contains_pt(choke_pos) {
            continue;
        }
        let obs_pts = path64_to_pts(&obs.cleared_poly);
        let d = if let Some(ref br) = bridge_ring {
            br.min_dist_to_poly_edges_bounded(&obs_pts, obs.cleared_bbox, margin_nm)
        } else if bridge_pts.len() >= 3 {
            polygon_to_polygon_dist(bridge_pts, &obs_pts)
        } else {
            point_to_polygon_dist(choke_pos, &obs_pts)
        };
        if d <= margin {
            hits.push(blocker_ref(obs, choke_pos));
        }
    }
    if hits.is_empty() {
        let wider_nm = min_neck_width + 150_000;
        let wider = wider_nm as f64;
        for obs in obstacles {
            if !obs.cleared_bbox.expand(wider_nm).contains_pt(choke_pos) {
                continue;
            }
            let obs_pts = path64_to_pts(&obs.cleared_poly);
            let d = point_to_polygon_dist(choke_pos, &obs_pts);
            if d <= wider {
                hits.push(blocker_ref(obs, choke_pos));
            }
        }
    }
    hits.sort_by_key(|r| r.dist_nm);
    hits.dedup_by(|a, b| a.label == b.label && a.pos == b.pos);
    hits
}

struct PourAnalysisCtx<'a> {
    spec: Option<&'a PourSpec>,
    board_region: &'a Paths64,
    pour_name: &'a str,
    filled: &'a FilledPour,
    min_neck_width: Nm,
    scene: &'a LayerScene,
}

fn check_island_narrow_neck(
    island: &PourPoly,
    ctx: &PourAnalysisCtx<'_>,
    term_indices: &[usize],
) -> Option<ZoneChoke> {
    if ctx.min_neck_width <= 0 {
        return None;
    }
    let raw_paths = pour_poly_to_paths64(island);
    let island_paths = simplify_paths(&raw_paths, CHOKE_SIMPLIFY_EPS, true);
    let test_paths = bridge_connected_thermal_moats(
        &island_paths,
        ctx.spec,
        ctx.board_region,
        ctx.scene,
        term_indices,
    );
    let delta = ctx.min_neck_width as f64 / 2.0;
    let eroded = inflate_paths_64(
        &test_paths,
        -delta,
        JoinType::Miter,
        EndType::Polygon,
        2.0,
        ARC_TOLERANCE,
    );
    let eroded_polys = paths_to_pour_polys(&eroded);
    let dilated = if eroded.is_empty() {
        Paths64::new()
    } else {
        inflate_paths_64(
            &eroded,
            delta,
            JoinType::Miter,
            EndType::Polygon,
            2.0,
            ARC_TOLERANCE,
        )
    };
    let opened = if dilated.is_empty() {
        Paths64::new()
    } else {
        intersect_64(&dilated, &test_paths, FillRule::NonZero)
    };
    let opened_polys = paths_to_pour_polys(&opened);
    let opened_idx: Vec<IndexedPour> = opened_polys.iter().map(IndexedPour::new).collect();

    let min_comp_area = (ctx.min_neck_width as f64 * 0.25).powi(2);
    let sig_eroded = eroded_polys
        .iter()
        .filter(|p| pour_poly_area(p) >= min_comp_area)
        .count();
    let sig_opened = opened_polys
        .iter()
        .filter(|p| pour_poly_area(p) >= min_comp_area)
        .count();
    let (split_components, dropped_terminal) = if term_indices.len() >= 2 {
        let mut touched_opened = 0usize;
        let mut total_assigned = 0usize;
        for op in &opened_idx {
            let count = term_indices
                .iter()
                .filter(|&&ti| indexed_pour_intersects_terminal(op, &ctx.scene.terminals[ti]))
                .count();
            if count > 0 {
                touched_opened += 1;
                total_assigned += count;
            }
        }
        (
            touched_opened >= 2,
            opened_idx.is_empty() || total_assigned < term_indices.len(),
        )
    } else {
        (sig_eroded >= 2 || sig_opened >= 2, false)
    };
    if !split_components && !dropped_terminal {
        return None;
    }

    let dropped_paths = difference_64(&test_paths, &opened, FillRule::NonZero);
    let dropped_polys = paths_to_pour_polys(&dropped_paths);
    let mut best_bridge: Option<&PourPoly> = None;
    if opened_idx.len() >= 2 {
        for dp in &dropped_polys {
            let dp_bbox = BBoxNm::from_pts(&dp.outer);
            let touch_count = opened_idx
                .iter()
                .filter(|op| {
                    op.outer
                        .min_dist_to_poly_edges_bounded(&dp.outer, dp_bbox, 5_000)
                        <= 5_000.0
                })
                .count();
            if touch_count >= 2 {
                best_bridge = Some(dp);
                break;
            }
        }
    }

    let bridge_pts = best_bridge
        .map(|b| b.outer.clone())
        .or_else(|| dropped_polys.first().map(|d| d.outer.clone()))
        .unwrap_or_default();
    let choke_pos = if !bridge_pts.is_empty() {
        let bb = BBoxNm::from_pts(&bridge_pts);
        Pt::new((bb.min_x + bb.max_x) / 2, (bb.min_y + bb.max_y) / 2)
    } else {
        let bb = BBoxNm::from_pts(&island.outer);
        Pt::new((bb.min_x + bb.max_x) / 2, (bb.min_y + bb.max_y) / 2)
    };

    let actual_width = measure_neck_width(&test_paths, ctx.min_neck_width, ctx.scene, term_indices);
    let blockers = find_neck_blockers(
        &bridge_pts,
        choke_pos,
        ctx.min_neck_width,
        &ctx.scene.obstacles,
    );

    let mut separated_groups: Vec<Vec<String>> = Vec::new();
    let mut assigned = BTreeSet::new();
    for op in &opened_idx {
        let mut grp = Vec::new();
        for &ti in term_indices {
            if indexed_pour_intersects_terminal(op, &ctx.scene.terminals[ti]) {
                grp.push(ctx.scene.terminals[ti].label.clone());
                assigned.insert(ti);
            }
        }
        if !grp.is_empty() {
            separated_groups.push(grp);
        }
    }
    let leftover: Vec<String> = term_indices
        .iter()
        .filter(|ti| !assigned.contains(ti))
        .map(|&ti| ctx.scene.terminals[ti].label.clone())
        .collect();
    if !leftover.is_empty() {
        separated_groups.push(leftover);
    }

    let blocker_names: Vec<String> = blockers.iter().map(|b| b.label.clone()).collect();
    let message = format!(
        "narrow copper neck ({:.3} mm < {:.3} mm) in pour '{}' ({} on {}) caused by [{}]",
        actual_width as f64 / 1e6,
        ctx.min_neck_width as f64 / 1e6,
        ctx.pour_name,
        ctx.filled.net,
        ctx.filled.layer,
        blocker_names.join(", ")
    );

    Some(ZoneChoke {
        kind: ChokeKind::NarrowNeck,
        pour_name: ctx.pour_name.to_string(),
        net: ctx.filled.net.clone(),
        layer: ctx.filled.layer.clone(),
        pos: choke_pos,
        min_neck_width_nm: actual_width,
        required_width_nm: ctx.min_neck_width,
        island_count: opened_polys.len().max(1),
        separated_groups,
        blockers,
        message,
    })
}

pub(crate) fn analyze_single_pour_chokes(
    board: &BoardIr,
    filled: &FilledPour,
    spec: Option<&PourSpec>,
    board_region: &Paths64,
    scene: &LayerScene,
    indexed_islands: &[IndexedPour],
    min_neck_override: Option<Nm>,
) -> Vec<ZoneChoke> {
    let mut chokes = Vec::new();
    let pour_name = spec
        .filter(|s| !s.name.is_empty())
        .map(|s| s.name.clone())
        .unwrap_or_else(|| format!("{}_{}", filled.net, filled.layer));
    let base_min_width = spec.map(|s| s.min_width_nm).unwrap_or(200_000);
    let min_neck_width = min_neck_override
        .unwrap_or_else(|| (base_min_width * 2).max(board.net_track_width(&filled.net) * 2));

    let island_terms: Vec<Vec<usize>> = indexed_islands
        .iter()
        .map(|ip| {
            scene
                .terminals
                .iter()
                .enumerate()
                .filter(|(_, t)| indexed_pour_intersects_terminal(ip, t))
                .map(|(i, _)| i)
                .collect()
        })
        .collect();

    if filled.polys.len() >= 2 {
        let main_idx = (0..filled.polys.len())
            .max_by(|&a, &b| {
                island_terms[a]
                    .len()
                    .cmp(&island_terms[b].len())
                    .then_with(|| {
                        pour_poly_area(&filled.polys[a])
                            .partial_cmp(&pour_poly_area(&filled.polys[b]))
                            .unwrap_or(std::cmp::Ordering::Equal)
                    })
            })
            .unwrap_or(0);

        let mut best_pair = (0usize, 1usize, Pt::new(0, 0), Pt::new(0, 0), f64::INFINITY);
        for other_idx in 0..filled.polys.len() {
            if other_idx == main_idx {
                continue;
            }
            let (pa, pb, d) = closest_points_between_rings(
                &indexed_islands[main_idx].outer,
                &indexed_islands[other_idx].outer,
            );
            if d < best_pair.4 {
                best_pair = (main_idx, other_idx, pa, pb, d);
            }
        }

        let separated_groups: Vec<Vec<String>> = island_terms
            .iter()
            .map(|terms| {
                terms
                    .iter()
                    .map(|&ti| scene.terminals[ti].label.clone())
                    .collect()
            })
            .collect();
        let mid = Pt::new(
            (best_pair.2.x + best_pair.3.x) / 2,
            (best_pair.2.y + best_pair.3.y) / 2,
        );
        let blockers = find_split_blockers(
            &indexed_islands[best_pair.0].outer,
            &indexed_islands[best_pair.1].outer,
            best_pair.2,
            best_pair.3,
            &scene.obstacles,
        );
        let blocker_names: Vec<String> = blockers.iter().map(|b| b.label.clone()).collect();
        let message = format!(
            "pour '{}' ({} on {}) split into {} disconnected islands by [{}]",
            pour_name,
            filled.net,
            filled.layer,
            filled.polys.len(),
            blocker_names.join(", ")
        );
        chokes.push(ZoneChoke {
            kind: ChokeKind::SplitPour,
            pour_name: pour_name.clone(),
            net: filled.net.clone(),
            layer: filled.layer.clone(),
            pos: mid,
            min_neck_width_nm: 0,
            required_width_nm: min_neck_width,
            island_count: filled.polys.len(),
            separated_groups,
            blockers,
            message,
        });
    }

    let ctx = PourAnalysisCtx {
        spec,
        board_region,
        pour_name: &pour_name,
        filled,
        min_neck_width,
        scene,
    };
    for (i_idx, poly) in filled.polys.iter().enumerate() {
        if let Some(neck) = check_island_narrow_neck(poly, &ctx, &island_terms[i_idx]) {
            chokes.push(neck);
        }
    }
    chokes
}

pub(crate) fn analyze_zone_chokes_with_min_neck(
    board: &BoardIr,
    min_neck_override: Option<Nm>,
) -> Vec<ZoneChoke> {
    if board.filled_pours.is_empty() {
        return Vec::new();
    }
    let matched_specs = match_pour_specs(board, &board.filled_pours);
    let board_region = build_board_copper_region(board);

    let results: Vec<Vec<ZoneChoke>> = std::thread::scope(|s| {
        let handles: Vec<_> = board
            .filled_pours
            .iter()
            .enumerate()
            .map(|(fp_idx, filled)| {
                let spec = matched_specs[fp_idx];
                let region_ref = &board_region;
                s.spawn(move || {
                    let pour_clr_opt = spec.and_then(|s| s.clearance_nm);
                    let mut scene =
                        collect_layer_scene(board, &filled.layer, &filled.net, pour_clr_opt);
                    add_cutout_obstacles(board, &mut scene.obstacles);
                    let indexed: Vec<IndexedPour> =
                        filled.polys.iter().map(IndexedPour::new).collect();
                    analyze_single_pour_chokes(
                        board,
                        filled,
                        spec,
                        region_ref,
                        &scene,
                        &indexed,
                        min_neck_override,
                    )
                })
            })
            .collect();
        handles
            .into_iter()
            .map(|h| h.join().expect("zone choke thread panicked"))
            .collect()
    });

    results.into_iter().flatten().collect()
}
