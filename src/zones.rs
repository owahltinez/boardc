use std::cmp::Reverse;
use std::collections::BTreeMap;

use clipper2_rust::{
    EndType, FillRule, JoinType, Path64, Paths64, difference_64, inflate_paths_64, intersect_64,
    union_subjects_64,
};
use serde::{Deserialize, Serialize};

use crate::geom::{ensure_ccw, point_in_polygon, pts_to_path64};
use crate::ir::{BBoxNm, BoardIr, FilledPour, Nm, PourSpec, Pt};
use crate::poly_index::IndexedPour;
use crate::zone_chokes;
use crate::zone_scene::{
    ARC_TOLERANCE, LayerScene, Terminal, add_cutout_obstacles, build_board_copper_region,
    collect_layer_scene, indexed_pour_intersects_terminal, inflate_round_outer, morphological_open,
    paths_to_pour_polys, pour_poly_area, pour_poly_to_paths64,
};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ChokeKind {
    SplitPour,
    NarrowNeck,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum BlockerKind {
    Via,
    Pad,
    Track,
    Keepout,
    Cutout,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct BlockerRef {
    pub kind: BlockerKind,
    pub net: String,
    pub label: String,
    pub pos: Pt,
    pub dist_nm: Nm,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ZoneChoke {
    pub kind: ChokeKind,
    pub pour_name: String,
    pub net: String,
    pub layer: String,
    pub pos: Pt,
    pub min_neck_width_nm: Nm,
    pub required_width_nm: Nm,
    pub island_count: usize,
    pub separated_groups: Vec<Vec<String>>,
    pub blockers: Vec<BlockerRef>,
    pub message: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ZonePourSummary {
    pub name: String,
    pub net: String,
    pub layer: String,
    pub priority: i32,
    pub island_count: usize,
    pub removed_dead_islands: usize,
    pub total_area_nm2: f64,
    pub connected_terminals: usize,
    pub unconnected_terminals: usize,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ZoneReport {
    pub pours: Vec<ZonePourSummary>,
    pub chokes: Vec<ZoneChoke>,
    pub total_islands: usize,
    pub total_removed_dead_islands: usize,
}

fn build_spoke_rects(term: &Terminal, spec: &PourSpec) -> Vec<Path64> {
    let (ux, uy) = term.axes;
    let (vx, vy) = (-uy, ux);
    let (mut min_u, mut max_u) = (f64::INFINITY, f64::NEG_INFINITY);
    let (mut min_v, mut max_v) = (f64::INFINITY, f64::NEG_INFINITY);
    for pt in &term.pts {
        let (dx, dy) = ((pt.x - term.center.x) as f64, (pt.y - term.center.y) as f64);
        let (pu, pv) = (dx * ux + dy * uy, dx * vx + dy * vy);
        min_u = min_u.min(pu);
        max_u = max_u.max(pu);
        min_v = min_v.min(pv);
        max_v = max_v.max(pv);
    }
    let extend = spec.thermal_gap_nm + spec.min_width_nm.max(100_000);
    let hw = spec.thermal_spoke_nm as f64 / 2.0;
    let make_rect = |u0: f64, u1: f64, v0: f64, v1: f64| -> Path64 {
        let pts: Vec<Pt> = [(u0, v0), (u1, v0), (u1, v1), (u0, v1)]
            .into_iter()
            .map(|(cu, cv)| {
                let x = term.center.x + (cu * ux + cv * vx).round() as Nm;
                let y = term.center.y + (cu * uy + cv * vy).round() as Nm;
                Pt::new(x, y)
            })
            .collect();
        pts_to_path64(&pts)
    };
    vec![
        make_rect(0.0, max_u + extend as f64, -hw, hw),
        make_rect(min_u - extend as f64, 0.0, -hw, hw),
        make_rect(-hw, hw, 0.0, max_v + extend as f64),
        make_rect(-hw, hw, min_v - extend as f64, 0.0),
    ]
}

fn build_raw_pour(
    spec: &PourSpec,
    board_region: &Paths64,
    higher_priority_on_layer: &[(Paths64, String, Nm)],
    scene: &LayerScene,
    pour_clr: Nm,
) -> Paths64 {
    if spec.outline.len() < 3 || board_region.is_empty() {
        return Paths64::new();
    }
    let outline_bbox = BBoxNm::from_pts(&spec.outline);
    let mut outline = spec.outline.clone();
    ensure_ccw(&mut outline);
    let clipped = intersect_64(
        &vec![pts_to_path64(&outline)],
        board_region,
        FillRule::NonZero,
    );
    if clipped.is_empty() {
        return clipped;
    }

    let mut clips = Paths64::new();
    for (hp_paths, hp_net, hp_clr) in higher_priority_on_layer {
        if hp_paths.is_empty() {
            continue;
        }
        if !spec.net.is_empty() && hp_net == &spec.net {
            clips.extend(hp_paths.iter().cloned());
        } else {
            clips.extend(inflate_round_outer(hp_paths, pour_clr.max(*hp_clr)));
        }
    }
    for obs in &scene.obstacles {
        if obs.cleared_bbox.intersects(outline_bbox) {
            clips.push(obs.cleared_poly.clone());
        }
    }
    if clips.is_empty() {
        clipped
    } else {
        difference_64(&clipped, &clips, FillRule::NonZero)
    }
}

fn apply_thermal_or_solid(p_raw: &Paths64, spec: &PourSpec, scene: &LayerScene) -> Paths64 {
    if p_raw.is_empty() {
        return Paths64::new();
    }
    if !spec.thermal_relief
        || spec.thermal_gap_nm <= 0
        || spec.thermal_spoke_nm <= 0
        || scene.terminals.is_empty()
    {
        return morphological_open(p_raw, spec.min_width_nm);
    }
    let outline_bbox = BBoxNm::from_pts(&spec.outline);
    let mut moats = Paths64::new();
    let mut active_terms = Vec::new();
    for term in &scene.terminals {
        if !term.thermal_relief
            || !term
                .bbox
                .expand(spec.thermal_gap_nm)
                .intersects(outline_bbox)
        {
            continue;
        }
        active_terms.push(term);
        moats.extend(inflate_paths_64(
            &vec![term.poly.clone()],
            spec.thermal_gap_nm as f64,
            JoinType::Round,
            EndType::Polygon,
            2.0,
            ARC_TOLERANCE,
        ));
    }
    if active_terms.is_empty() {
        return morphological_open(p_raw, spec.min_width_nm);
    }
    let p_moat = difference_64(p_raw, &moats, FillRule::NonZero);
    let p_bulk = morphological_open(&p_moat, spec.min_width_nm);
    let mut all_parts = p_bulk.clone();
    for term in active_terms {
        for spoke in build_spoke_rects(term, spec) {
            let clipped = intersect_64(&vec![spoke], p_raw, FillRule::NonZero);
            if !clipped.is_empty()
                && !p_bulk.is_empty()
                && !intersect_64(&clipped, &p_bulk, FillRule::NonZero).is_empty()
            {
                all_parts.extend(clipped);
            }
        }
    }
    if all_parts.is_empty() {
        Paths64::new()
    } else {
        union_subjects_64(&all_parts, FillRule::NonZero)
    }
}

struct FilledPourTaskOut {
    order_pos: usize,
    filled: FilledPour,
    summary: ZonePourSummary,
    removed_dead: usize,
    chokes: Vec<ZoneChoke>,
}

/// Fills all copper pours in descending priority order (parallel across layers) and analyzes chokes.
pub fn fill_zones(board: &mut BoardIr) -> ZoneReport {
    board.filled_pours.clear();
    if board.pours.is_empty() {
        return ZoneReport {
            pours: Vec::new(),
            chokes: Vec::new(),
            total_islands: 0,
            total_removed_dead_islands: 0,
        };
    }

    let board_region = build_board_copper_region(board);
    let mut order: Vec<usize> = (0..board.pours.len()).collect();
    order.sort_by_key(|&i| (Reverse(board.pours[i].priority), i));

    let mut by_layer: BTreeMap<String, Vec<(usize, usize)>> = BTreeMap::new();
    for (order_pos, &pour_idx) in order.iter().enumerate() {
        by_layer
            .entry(board.pours[pour_idx].layer.clone())
            .or_default()
            .push((order_pos, pour_idx));
    }

    let board_ref: &BoardIr = board;
    let region_ref = &board_region;
    let layer_groups: Vec<Vec<(usize, usize)>> = by_layer.into_values().collect();

    let mut task_results: Vec<FilledPourTaskOut> = std::thread::scope(|s| {
        let handles: Vec<_> = layer_groups
            .into_iter()
            .map(|group| {
                s.spawn(move || {
                    let mut higher: Vec<(Paths64, String, Nm)> = Vec::new();
                    let mut out = Vec::with_capacity(group.len());
                    for (order_pos, idx) in group {
                        let spec = &board_ref.pours[idx];
                        let pour_clr = spec
                            .clearance_nm
                            .unwrap_or_else(|| board_ref.net_clearance(&spec.net));
                        let mut scene = collect_layer_scene(
                            board_ref,
                            &spec.layer,
                            &spec.net,
                            spec.clearance_nm,
                        );
                        let p_raw = build_raw_pour(spec, region_ref, &higher, &scene, pour_clr);
                        let filled_paths = apply_thermal_or_solid(&p_raw, spec, &scene);
                        let all_polys = paths_to_pour_polys(&filled_paths);

                        let mut kept_polys = Vec::with_capacity(all_polys.len());
                        let mut kept_indexed = Vec::with_capacity(all_polys.len());
                        let mut removed = 0usize;
                        for poly in all_polys {
                            let idx_pour = IndexedPour::new(&poly);
                            if spec.remove_islands
                                && !scene
                                    .terminals
                                    .iter()
                                    .any(|t| indexed_pour_intersects_terminal(&idx_pour, t))
                            {
                                removed += 1;
                                continue;
                            }
                            kept_polys.push(poly);
                            kept_indexed.push(idx_pour);
                        }

                        let mut connected_terminals = 0usize;
                        let mut unconnected_terminals = 0usize;
                        let mut outline_ccw = spec.outline.clone();
                        ensure_ccw(&mut outline_ccw);
                        for term in &scene.terminals {
                            if kept_indexed
                                .iter()
                                .any(|p| indexed_pour_intersects_terminal(p, term))
                            {
                                connected_terminals += 1;
                            } else if point_in_polygon(term.center, &outline_ccw) {
                                unconnected_terminals += 1;
                            }
                        }

                        let total_area_nm2: f64 = kept_polys.iter().map(pour_poly_area).sum();
                        let island_count = kept_polys.len();

                        let mut layer_paths = Paths64::new();
                        for p in &kept_polys {
                            layer_paths.extend(pour_poly_to_paths64(p));
                        }
                        higher.push((layer_paths, spec.net.clone(), pour_clr));

                        let filled = FilledPour {
                            net: spec.net.clone(),
                            layer: spec.layer.clone(),
                            priority: spec.priority,
                            polys: kept_polys,
                        };
                        add_cutout_obstacles(board_ref, &mut scene.obstacles);
                        let chokes = zone_chokes::analyze_single_pour_chokes(
                            board_ref,
                            &filled,
                            Some(spec),
                            region_ref,
                            &scene,
                            &kept_indexed,
                            None,
                        );
                        let summary = ZonePourSummary {
                            name: if spec.name.is_empty() {
                                format!("{}_{}", spec.net, spec.layer)
                            } else {
                                spec.name.clone()
                            },
                            net: spec.net.clone(),
                            layer: spec.layer.clone(),
                            priority: spec.priority,
                            island_count,
                            removed_dead_islands: removed,
                            total_area_nm2,
                            connected_terminals,
                            unconnected_terminals,
                        };
                        out.push(FilledPourTaskOut {
                            order_pos,
                            filled,
                            summary,
                            removed_dead: removed,
                            chokes,
                        });
                    }
                    out
                })
            })
            .collect();
        let mut all = Vec::with_capacity(order.len());
        for h in handles {
            all.extend(h.join().expect("zone fill thread panicked"));
        }
        all
    });

    task_results.sort_by_key(|r| r.order_pos);
    let mut summaries = Vec::with_capacity(task_results.len());
    let mut chokes = Vec::new();
    let mut total_islands = 0usize;
    let mut total_removed_dead_islands = 0usize;
    for res in task_results {
        total_islands += res.summary.island_count;
        total_removed_dead_islands += res.removed_dead;
        board.filled_pours.push(res.filled);
        summaries.push(res.summary);
        chokes.extend(res.chokes);
    }

    ZoneReport {
        pours: summaries,
        chokes,
        total_islands,
        total_removed_dead_islands,
    }
}

/// Detects split pours and narrow copper necks below threshold, naming blocking obstacles.
pub fn analyze_zone_chokes(board: &BoardIr) -> Vec<ZoneChoke> {
    zone_chokes::analyze_zone_chokes_with_min_neck(board, None)
}
