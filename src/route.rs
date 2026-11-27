use std::collections::{BTreeSet, HashSet};

use serde::{Deserialize, Serialize, Serializer};
use viahop::geometry::Point;
use viahop::{
    Board as ViahopBoard, LengthConstraints, MaxLengthConstraint, NetRoute, RoutedSegment,
    RoutedVia, RouterConfig, route_board, validate_reroute_nets,
};

use crate::drc_constraints::count_length_violations;
use crate::ir::{BoardIr, Pt, Track, Via, nm_to_um, um};
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

fn write_routes_to_board(board: &mut BoardIr, vboard: &ViahopBoard, routes: &[NetRoute]) {
    let ignored_names: HashSet<&str> = vboard
        .nets
        .iter()
        .filter(|n| n.ignored)
        .map(|n| n.name.as_str())
        .collect();
    // Always preserve locked, no-net, and ignored-net copper (passed as fixed obstacles to viahop).
    board
        .tracks
        .retain(|t| t.locked || t.net.is_empty() || ignored_names.contains(t.net.as_str()));
    board
        .vias
        .retain(|v| v.locked || v.net.is_empty() || ignored_names.contains(v.net.as_str()));

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

/// Routes all non-ignored (or `--reroute-nets` targeted) nets in memory using `viahop`.
pub fn route_board_ir(board: &mut BoardIr, cfg: &RouteOptions) -> Result<RouteSummary, String> {
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
    write_routes_to_board(board, &vboard, &result.routes);

    let s = result.stats;
    let mut total_segments = s.total_segments;
    let mut total_wirelength_um = s.total_wirelength_um;
    let mut length_violations = s.length_violations;
    let mut elapsed_ms = s.elapsed_ms;
    if cfg.tune_match_groups && !board.constraints.match_groups.is_empty() {
        let len_before: f64 = board.tracks.iter().map(|t| t.a.dist(t.b)).sum();
        let tune = tune_match_groups(
            board,
            &TuneOptions {
                only_nets: cfg.reroute_nets.clone(),
                ignored_nets: cfg.ignored_nets.clone(),
            },
        );
        let len_after: f64 = board.tracks.iter().map(|t| t.a.dist(t.b)).sum();
        total_segments += tune.added_segments;
        total_wirelength_um = (total_wirelength_um + (len_after - len_before) / 1000.0).max(0.0);
        elapsed_ms += tune.elapsed_ms;
    }
    if !board.constraints.match_groups.is_empty() || !board.constraints.max_lengths.is_empty() {
        length_violations = count_length_violations(board);
    }

    Ok(RouteSummary {
        total_nets: s.total_nets,
        routable_nets: s.routable_nets,
        routed_nets: s.routed_nets,
        unconnected_pairs: s.unconnected_pairs,
        total_segments,
        total_vias: s.total_vias,
        total_wirelength_um,
        passes_run: s.passes_run,
        starts_run: s.starts_run,
        drc_violations: s.drc_violations,
        length_violations,
        dense_bga_pads: s.dense_bga_pads,
        pofv_capable_pads: s.pofv_capable_pads,
        grid_step_um: s.grid_step_um,
        stopped_early: s.stopped_early,
        elapsed_ms,
    })
}
