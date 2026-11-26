use serde::{Deserialize, Serialize};

use crate::drc_cluster;
use crate::drc_connect;
use crate::drc_constraints::check_constraints;
use crate::drc_copper::{
    check_pad_to_pad_clearances, check_pour_clearances, check_track_rules_and_clearances,
    check_via_to_via_and_pad,
};
use crate::drc_layout::{check_courtyards_and_component_edges, check_keepouts};
use crate::drc_rules::{check_drill_rules, check_edge_clearances};
use crate::drc_scene::{
    collect_drills, collect_keepouts, collect_pads, collect_pours, collect_tracks, collect_vias,
    compute_max_clearance,
};
use crate::ir::{BoardIr, Nm, Pt};

pub const DEFAULT_CLUSTER_RADIUS_NM: Nm = 2_000_000;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ViolationKind {
    Clearance,
    HoleToHole,
    DrillToCopper,
    MinAnnularRing,
    MinTraceWidth,
    EdgeClearance,
    Keepout,
    ViaAtSmd,
    CourtyardOverlap,
    ComponentToEdge,
    OpenNet,
    MaxLength,
    MatchGroup,
    CapProximity,
    MinVias,
    BgaShadow,
    Other,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Severity {
    Error,
    Warning,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Violation {
    pub kind: ViolationKind,
    pub severity: Severity,
    pub layer: String,
    pub pos_nm: Pt,
    pub measured_nm: Nm,
    pub required_nm: Nm,
    pub item_a: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub item_b: Option<String>,
    #[serde(default)]
    pub net_a: String,
    #[serde(default)]
    pub net_b: String,
    #[serde(default)]
    pub message: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Hotspot {
    pub centroid_nm: Pt,
    pub radius_nm: Nm,
    pub violation_count: usize,
    pub error_count: usize,
    pub warning_count: usize,
    pub root_cause_item: String,
    pub root_cause_net: String,
    pub affected_nets: Vec<String>,
    pub layers: Vec<String>,
    pub kinds: Vec<ViolationKind>,
    pub action_hint: String,
    pub violations: Vec<Violation>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DrcReport {
    pub violations: Vec<Violation>,
    pub hotspots: Vec<Hotspot>,
    pub error_count: usize,
    pub warning_count: usize,
    pub passed: bool,
}

/// Runs exact continuous DRC across clearances, drills, edges, keepouts, courtyards, connectivity, and constraints.
pub fn check_board_drc(board: &BoardIr) -> DrcReport {
    let cu_layers = board.copper_layer_names();
    let pads = collect_pads(board, &cu_layers);
    let tracks = collect_tracks(board);
    let vias = collect_vias(board);
    let drills = collect_drills(board, &pads, &vias, &cu_layers);
    let pours = collect_pours(board);
    let keepouts = collect_keepouts(board);
    let max_clr = compute_max_clearance(board, &pours);

    let mut violations = std::thread::scope(|s| {
        let h_copper = s.spawn(|| {
            let mut v = Vec::new();
            check_track_rules_and_clearances(board, &tracks, &pads, &vias, max_clr, &mut v);
            check_pad_to_pad_clearances(board, &pads, max_clr, &mut v);
            check_via_to_via_and_pad(board, &pads, &vias, &cu_layers, max_clr, &mut v);
            v
        });
        let h_pours = s.spawn(|| {
            let mut v = Vec::new();
            check_pour_clearances(board, &tracks, &pads, &vias, &pours, max_clr, &mut v);
            v
        });
        let h_drills =
            s.spawn(|| check_drill_rules(board, &tracks, &pads, &vias, &drills, &pours, max_clr));
        let h_layout = s.spawn(|| {
            let mut v = Vec::new();
            check_edge_clearances(board, &tracks, &pads, &vias, &pours, &mut v);
            check_keepouts(board, &tracks, &pads, &vias, &pours, &keepouts, &mut v);
            check_courtyards_and_component_edges(board, &keepouts, &mut v);
            check_constraints(board, &pads, &mut v);
            v
        });
        let h_conn = s.spawn(|| {
            drc_connect::check_connectivity(board, &cu_layers, &tracks, &pads, &vias, &pours)
        });

        let mut all = Vec::new();
        for h in [h_copper, h_pours, h_drills, h_layout, h_conn] {
            all.extend(h.join().expect("drc worker thread panicked"));
        }
        all
    });

    violations.sort_by(|a, b| {
        (a.severity, a.kind, a.layer.as_str(), a.pos_nm.x, a.pos_nm.y)
            .cmp(&(b.severity, b.kind, b.layer.as_str(), b.pos_nm.x, b.pos_nm.y))
            .then_with(|| a.item_a.cmp(&b.item_a))
            .then_with(|| a.item_b.cmp(&b.item_b))
    });
    let error_count = violations
        .iter()
        .filter(|v| v.severity == Severity::Error)
        .count();
    let warning_count = violations.len() - error_count;
    let hotspots = cluster_hotspots(&violations, DEFAULT_CLUSTER_RADIUS_NM);
    DrcReport {
        violations,
        hotspots,
        error_count,
        warning_count,
        passed: error_count == 0,
    }
}

/// Groups atomic violations into spatial and shared-object root-cause hotspots.
pub fn cluster_hotspots(violations: &[Violation], cluster_radius_nm: Nm) -> Vec<Hotspot> {
    drc_cluster::cluster_hotspots(violations, cluster_radius_nm)
}

/// Parses `kicad-cli pcb drc --format json` output, maps coordinates onto `BoardIr` objects, and clusters hotspots.
pub fn cluster_kicad_drc_json(
    board: &BoardIr,
    kicad_drc_json: &str,
) -> Result<Vec<Hotspot>, String> {
    drc_cluster::cluster_kicad_drc_json(board, kicad_drc_json)
}
