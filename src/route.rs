use std::collections::{BTreeSet, HashMap, HashSet};

use serde::{Deserialize, Serialize, Serializer};
use viahop::geometry::Point;
use viahop::{
    Board as ViahopBoard, LengthConstraints, MaxLengthConstraint, NetRoute, RoutedSegment,
    RoutedVia, RouterConfig, route_board, validate_reroute_nets,
};

use crate::drc_constraints::count_length_violations;
use crate::drc_dangling::CopperContext;
use crate::drc_scene::{PourItem, collect_pads, collect_pours, collect_tracks, collect_vias};
use crate::geom::EPS_NM;
use crate::ir::{BoardIr, Nm, PourPoly, Pt, Track, Via, nm_to_um, um};
use crate::poly_index::IndexedPour;
pub use crate::route_bridge::to_viahop_board;
use crate::route_bridge::{pt_from_viahop, pt_to_viahop, via_spanned_indices};
use crate::tune::{TuneOptions, tune_match_groups};

fn serialize_sorted_set<S: Serializer>(set: &HashSet<String>, ser: S) -> Result<S::Ok, S::Error> {
    let sorted: BTreeSet<&String> = set.iter().collect();
    sorted.serialize(ser)
}

fn serialize_opt_sorted_set<S: Serializer>(
    opt: &Option<HashSet<String>>,
    ser: S,
) -> Result<S::Ok, S::Error> {
    let sorted: Option<BTreeSet<&String>> = opt.as_ref().map(|s| s.iter().collect());
    sorted.serialize(ser)
}

/// Options controlling in-memory `viahop` autorouting on a `BoardIr`.
#[derive(Debug, Clone, Serialize)]
pub struct RouteOptions {
    pub max_passes: usize,
    pub multi_start: usize,
    pub threads: usize,
    pub timeout_secs: Option<u64>,
    pub optimize: bool,
    pub via_at_smd: Option<bool>,
    #[serde(serialize_with = "serialize_sorted_set")]
    pub ignored_nets: HashSet<String>,
    #[serde(serialize_with = "serialize_opt_sorted_set")]
    pub reroute_nets: Option<HashSet<String>>,
    pub reroute_pad_mm: Option<f64>,
    pub resume: bool,
    pub tune_match_groups: bool,
}

impl Default for RouteOptions {
    fn default() -> Self {
        Self {
            max_passes: 20,
            multi_start: 4,
            threads: 1,
            timeout_secs: None,
            optimize: true,
            via_at_smd: None,
            ignored_nets: HashSet::new(),
            reroute_nets: None,
            reroute_pad_mm: None,
            resume: false,
            tune_match_groups: true,
        }
    }
}

/// Summary statistics from an in-memory `viahop` routing run.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct RouteSummary {
    pub total_nets: usize,
    pub routable_nets: usize,
    pub routed_nets: usize,
    pub unconnected_pairs: usize,
    pub total_segments: usize,
    pub total_vias: usize,
    pub total_wirelength_um: f64,
    pub passes_run: usize,
    pub starts_run: usize,
    pub drc_violations: usize,
    pub length_violations: usize,
    pub dense_bga_pads: usize,
    pub pofv_capable_pads: usize,
    pub grid_step_um: f64,
    pub stopped_early: bool,
    pub elapsed_ms: u128,
    #[serde(default)]
    pub repair_passes_run: usize,
}

pub(crate) fn point_from_viahop(p: Point) -> Pt {
    pt_from_viahop(p)
}

fn build_length_constraints(board: &BoardIr) -> LengthConstraints {
    LengthConstraints {
        max_lengths: board
            .constraints
            .max_lengths
            .iter()
            .map(|r| MaxLengthConstraint {
                nets: r.nets.clone(),
                max_um: nm_to_um(r.max_nm),
            })
            .collect(),
        // Match-group skew is closed cleanly by `tune_match_groups` without pre-fragmenting traces.
        match_groups: Vec::new(),
    }
}

fn build_initial_routes(board: &BoardIr, vboard: &ViahopBoard) -> Vec<NetRoute> {
    let mut routes: Vec<NetRoute> = (0..vboard.nets.len())
        .map(|net_id| NetRoute {
            net_id,
            ..Default::default()
        })
        .collect();
    for t in &board.tracks {
        if t.locked {
            continue;
        }
        let Some(&nid) = vboard.net_name_to_id.get(&t.net) else {
            continue;
        };
        if vboard.nets[nid].ignored {
            continue;
        }
        let Some(&layer) = vboard.layer_name_to_idx.get(&t.layer) else {
            continue;
        };
        routes[nid].segments.push(RoutedSegment {
            layer,
            a: pt_to_viahop(t.a),
            b: pt_to_viahop(t.b),
            width: nm_to_um(t.width),
        });
    }
    let num_layers = vboard.layer_names.len();
    for v in &board.vias {
        if v.locked {
            continue;
        }
        let Some(&nid) = vboard.net_name_to_id.get(&v.net) else {
            continue;
        };
        if vboard.nets[nid].ignored {
            continue;
        }
        let vlayers = via_spanned_indices(&v.layers, &vboard.layer_name_to_idx, num_layers);
        routes[nid].vias.push(RoutedVia {
            pos: pt_to_viahop(v.pos),
            padstack: vboard.nets[nid].rules.via_padstack.clone(),
            radius: nm_to_um(v.diameter) * 0.5,
            drill: nm_to_um(v.drill),
            layers: vlayers,
        });
    }
    routes
}

fn is_net_ignored(vboard: &ViahopBoard, net: &str) -> bool {
    vboard
        .net_name_to_id
        .get(net)
        .is_some_and(|&nid| vboard.nets[nid].ignored)
}

fn is_target_routable_net(
    vboard: &ViahopBoard,
    targets: Option<&HashSet<String>>,
    net: &str,
) -> bool {
    let Some(&nid) = vboard.net_name_to_id.get(net) else {
        return false;
    };
    let info = &vboard.nets[nid];
    if info.ignored {
        return false;
    }
    targets.is_none_or(|t| t.contains(net) || t.contains(info.class_name.as_str()))
}

fn write_routes_to_board(board: &mut BoardIr, vboard: &ViahopBoard, routes: &[NetRoute]) {
    // Always preserve locked, no-net, and ignored-net copper (passed as fixed obstacles to viahop).
    board
        .tracks
        .retain(|t| t.locked || t.net.is_empty() || is_net_ignored(vboard, &t.net));
    board
        .vias
        .retain(|v| v.locked || v.net.is_empty() || is_net_ignored(vboard, &v.net));

    for nr in routes {
        let net_name = &vboard.nets[nr.net_id].name;
        if vboard.nets[nr.net_id].ignored {
            continue;
        }
        for seg in &nr.segments {
            let a = pt_from_viahop(seg.a);
            let b = pt_from_viahop(seg.b);
            if a == b {
                continue;
            }
            let Some(layer) = vboard.layer_names.get(seg.layer) else {
                continue;
            };
            board.tracks.push(Track {
                net: net_name.clone(),
                layer: layer.clone(),
                a,
                b,
                width: um(seg.width),
                locked: false,
            });
        }
        for via in &nr.vias {
            let lo = via.layers.iter().copied().min().unwrap_or(0);
            let hi = via
                .layers
                .iter()
                .copied()
                .max()
                .unwrap_or(vboard.layer_names.len().saturating_sub(1));
            let l0 = vboard
                .layer_names
                .get(lo)
                .cloned()
                .unwrap_or_else(|| "F.Cu".into());
            let l1 = vboard
                .layer_names
                .get(hi)
                .cloned()
                .unwrap_or_else(|| "B.Cu".into());
            board.vias.push(Via {
                net: net_name.clone(),
                pos: pt_from_viahop(via.pos),
                diameter: um(via.radius * 2.0),
                drill: um(via.drill),
                layers: (l0, l1),
                locked: false,
                tented: true,
            });
        }
    }
}

const MAX_STUB_LEN_NM: Nm = 80_000;

fn prune_dead_end_stubs(
    board: &mut BoardIr,
    vboard: &ViahopBoard,
    cfg: &RouteOptions,
) -> (usize, f64, usize) {
    let targets = cfg.reroute_nets.as_ref();
    let eligible_tracks: Vec<usize> = board
        .tracks
        .iter()
        .enumerate()
        .filter_map(|(i, t)| {
            (!t.locked
                && !t.net.is_empty()
                && is_target_routable_net(vboard, targets, &t.net)
                && t.a.dist(t.b) <= MAX_STUB_LEN_NM as f64 + EPS_NM)
                .then_some(i)
        })
        .collect();
    let eligible_vias: Vec<usize> = board
        .vias
        .iter()
        .enumerate()
        .filter_map(|(i, v)| {
            (!v.locked && !v.net.is_empty() && is_target_routable_net(vboard, targets, &v.net))
                .then_some(i)
        })
        .collect();
    if eligible_tracks.is_empty() && eligible_vias.is_empty() {
        return (0, 0.0, 0);
    }

    let (alive_tracks, alive_vias) = {
        let cu_layers = board.copper_layer_names();
        let pads = collect_pads(board, &cu_layers);
        let vias = collect_vias(board);
        let unfilled_polys: Vec<(&str, &str, PourPoly)> = if board.filled_pours.is_empty() {
            board
                .pours
                .iter()
                .filter(|p| p.outline.len() >= 3)
                .map(|p| {
                    (
                        p.net.as_str(),
                        p.layer.as_str(),
                        PourPoly {
                            outer: p.outline.clone(),
                            holes: Vec::new(),
                        },
                    )
                })
                .collect()
        } else {
            Vec::new()
        };
        let pours: Vec<PourItem<'_>> = if board.filled_pours.is_empty() {
            unfilled_polys
                .iter()
                .enumerate()
                .map(|(id, (net, layer, poly))| {
                    let indexed = IndexedPour::new(poly);
                    let bbox = indexed.outer.bbox;
                    PourItem {
                        label: format!("unfilled_pour#{id}"),
                        net,
                        layer,
                        poly,
                        bbox,
                        indexed,
                        clearance_override: None,
                    }
                })
                .collect()
        } else {
            collect_pours(board)
        };
        let tracks = collect_tracks(board);
        let mut ctx = CopperContext::new(board, &cu_layers, &pads, &vias, &pours, &tracks);
        let mut pruned_at: HashMap<(&str, &str, Pt), f64> = HashMap::new();

        loop {
            loop {
                let mut changed = false;
                for &i in &eligible_tracks {
                    if !ctx.alive_tracks[i] {
                        continue;
                    }
                    let tr = &tracks[i];
                    let a_anch = ctx.is_track_endpoint_anchored(i, tr.a);
                    let b_anch = ctx.is_track_endpoint_anchored(i, tr.b);
                    if a_anch && b_anch {
                        continue;
                    }
                    let anchor = a_anch.then_some(tr.a).or_else(|| b_anch.then_some(tr.b));
                    let prior = match (a_anch, b_anch) {
                        (true, false) => pruned_at
                            .get(&(tr.layer, tr.net, tr.b))
                            .copied()
                            .unwrap_or(0.0),
                        (false, true) => pruned_at
                            .get(&(tr.layer, tr.net, tr.a))
                            .copied()
                            .unwrap_or(0.0),
                        _ => {
                            pruned_at
                                .get(&(tr.layer, tr.net, tr.a))
                                .copied()
                                .unwrap_or(0.0)
                                + pruned_at
                                    .get(&(tr.layer, tr.net, tr.b))
                                    .copied()
                                    .unwrap_or(0.0)
                        }
                    };
                    let total_chain_len = prior + tr.a.dist(tr.b);
                    if total_chain_len > MAX_STUB_LEN_NM as f64 + EPS_NM {
                        continue;
                    }
                    if ctx.track_has_external_touch(i, anchor) {
                        continue;
                    }
                    ctx.alive_tracks[i] = false;
                    if let Some(anc) = anchor {
                        *pruned_at.entry((tr.layer, tr.net, anc)).or_insert(0.0) += total_chain_len;
                    }
                    changed = true;
                }
                if !changed {
                    break;
                }
            }

            let mut via_pruned = false;
            if !eligible_vias.is_empty() {
                let via_counts = ctx.compute_via_connected_counts();
                for &vi in &eligible_vias {
                    if ctx.alive_vias[vi] && via_counts[vi] < 2 {
                        ctx.alive_vias[vi] = false;
                        via_pruned = true;
                    }
                }
            }
            if !via_pruned {
                break;
            }
        }
        (ctx.alive_tracks, ctx.alive_vias)
    };

    let mut pruned_tracks = 0usize;
    let mut pruned_len_nm = 0.0_f64;
    let mut idx = 0usize;
    board.tracks.retain(|t| {
        let keep = alive_tracks[idx];
        idx += 1;
        if !keep {
            pruned_tracks += 1;
            pruned_len_nm += t.a.dist(t.b);
        }
        keep
    });
    let mut pruned_vias = 0usize;
    let mut vidx = 0usize;
    board.vias.retain(|_| {
        let keep = alive_vias[vidx];
        vidx += 1;
        if !keep {
            pruned_vias += 1;
        }
        keep
    });
    (pruned_tracks, pruned_len_nm, pruned_vias)
}

pub(crate) fn apply_match_group_tuning(
    board: &mut BoardIr,
    summary: &mut RouteSummary,
    only_nets: Option<HashSet<String>>,
    ignored_nets: &HashSet<String>,
) {
    if !board.constraints.match_groups.is_empty() {
        let len_before: f64 = board.tracks.iter().map(|t| t.a.dist(t.b)).sum();
        let tune = tune_match_groups(
            board,
            &TuneOptions {
                only_nets,
                ignored_nets: ignored_nets.clone(),
            },
        );
        let len_after: f64 = board.tracks.iter().map(|t| t.a.dist(t.b)).sum();
        summary.total_segments += tune.added_segments;
        summary.total_wirelength_um =
            (summary.total_wirelength_um + (len_after - len_before) / 1000.0).max(0.0);
        summary.elapsed_ms += tune.elapsed_ms;
    }
    if !board.constraints.match_groups.is_empty() || !board.constraints.max_lengths.is_empty() {
        summary.length_violations = count_length_violations(board);
    }
}

/// Routes all non-ignored (or `--reroute-nets` targeted) nets in memory using `viahop`.
pub fn route_board_ir(board: &mut BoardIr, cfg: &RouteOptions) -> Result<RouteSummary, String> {
    route_board_ir_with_unconnected(board, cfg).map(|(summary, _)| summary)
}

pub(crate) fn route_board_ir_with_unconnected(
    board: &mut BoardIr,
    cfg: &RouteOptions,
) -> Result<(RouteSummary, HashSet<String>), String> {
    if let Some(pad_mm) = cfg.reroute_pad_mm
        && (!pad_mm.is_finite() || pad_mm <= 0.0)
    {
        return Err(format!("--reroute-pad-mm must be positive, got {pad_mm}"));
    }
    let vboard = to_viahop_board(board, &cfg.ignored_nets, cfg.via_at_smd)?;
    if let Some(ref targets) = cfg.reroute_nets {
        validate_reroute_nets(&vboard, targets)?;
    }
    let length_constraints = build_length_constraints(board);
    length_constraints.validate(&vboard)?;
    for rule in &board.constraints.match_groups {
        for net in &rule.nets {
            if !vboard.net_name_to_id.contains_key(net) {
                return Err(format!(
                    "match_group '{}' references unknown net '{net}'",
                    rule.name
                ));
            }
        }
    }
    for rule in &board.constraints.max_vias {
        for net in &rule.nets {
            if !vboard.net_name_to_id.contains_key(net) {
                return Err(format!(
                    "max_vias constraint references unknown net '{net}'"
                ));
            }
        }
    }

    let use_initial = cfg.resume || cfg.reroute_nets.is_some();
    let initial_routes = use_initial.then(|| build_initial_routes(board, &vboard));

    let rcfg = RouterConfig {
        max_passes: cfg.max_passes,
        ignored_nets: cfg.ignored_nets.clone(),
        via_at_smd: cfg.via_at_smd,
        hole_to_hole_um: Some(vboard.hole_to_hole_um),
        edge_clearance_um: vboard.edge_clearance_um,
        threads: cfg.threads.max(1),
        timeout_secs: cfg.timeout_secs,
        optimize: cfg.optimize,
        multi_start: cfg.multi_start.max(1),
        initial_routes,
        length_constraints,
        reroute_nets: cfg.reroute_nets.clone(),
        reroute_pad_um: cfg.reroute_pad_mm.map(|m| m * 1000.0),
        stop_flag: None,
    };

    let result = route_board(&vboard, &rcfg);
    let unconnected_nets: HashSet<String> = result
        .routes
        .iter()
        .filter(|nr| !vboard.nets[nr.net_id].ignored && nr.unconnected_pairs > 0)
        .map(|nr| vboard.nets[nr.net_id].name.clone())
        .collect();
    write_routes_to_board(board, &vboard, &result.routes);
    let (pruned_tracks, pruned_len_nm, pruned_vias) = prune_dead_end_stubs(board, &vboard, cfg);

    let s = result.stats;
    let mut summary = RouteSummary {
        total_nets: s.total_nets,
        routable_nets: s.routable_nets,
        routed_nets: s.routed_nets,
        unconnected_pairs: s.unconnected_pairs,
        total_segments: s.total_segments.saturating_sub(pruned_tracks),
        total_vias: s.total_vias.saturating_sub(pruned_vias),
        total_wirelength_um: (s.total_wirelength_um - pruned_len_nm / 1000.0).max(0.0),
        passes_run: s.passes_run,
        starts_run: s.starts_run,
        drc_violations: s.drc_violations,
        length_violations: s.length_violations,
        dense_bga_pads: s.dense_bga_pads,
        pofv_capable_pads: s.pofv_capable_pads,
        grid_step_um: s.grid_step_um,
        stopped_early: s.stopped_early,
        elapsed_ms: s.elapsed_ms,
        repair_passes_run: 0,
    };
    if cfg.tune_match_groups {
        apply_match_group_tuning(
            board,
            &mut summary,
            cfg.reroute_nets.clone(),
            &cfg.ignored_nets,
        );
    } else if !board.constraints.match_groups.is_empty()
        || !board.constraints.max_lengths.is_empty()
    {
        summary.length_violations = count_length_violations(board);
    }
    Ok((summary, unconnected_nets))
}
