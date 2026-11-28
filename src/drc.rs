use serde::{Deserialize, Serialize};

use crate::drc_cluster;
use crate::drc_connect;
use crate::drc_constraints::check_constraints;
use crate::drc_copper::{
    check_pad_to_pad_clearances, check_pour_clearances, check_track_rules_and_clearances,
    check_via_to_via_and_pad,
};
use crate::drc_dangling;
use crate::drc_layout::{check_courtyards_and_component_edges, check_keepouts};
use crate::drc_mask::{MaskOpening, check_mask_to_copper, collect_mask_openings};
use crate::drc_rules::{check_drill_rules, check_edge_clearances};
use crate::drc_scene::{
    BoardEdgeIndex, DrcInputs, collect_drc_grids, collect_drills, collect_keepouts, collect_pads,
    collect_pours, collect_tracks, collect_vias, compute_max_clearance,
};
use crate::drc_silk::check_silkscreen;
use crate::ir::{BoardIr, Nm, Pt, Side};

pub const DEFAULT_CLUSTER_RADIUS_NM: Nm = 2_000_000;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ViolationKind {
    Clearance,
    SolderMaskToCopper,
    HoleToHole,
    DrillToCopper,
    MinAnnularRing,
    MinTraceWidth,
    EdgeClearance,
    SilkOverCopper,
    SilkEdgeClearance,
    Keepout,
    ViaAtSmd,
    CourtyardOverlap,
    ComponentToEdge,
    OpenNet,
    MaxLength,
    MatchGroup,
    CapProximity,
    MinVias,
    MaxVias,
    BgaShadow,
    TrackDangling,
    ViaDangling,
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
    let tracks = collect_tracks(board, &cu_layers);
    let vias = collect_vias(board, &cu_layers);
    let drills = collect_drills(board, &pads, &vias, &cu_layers);
    let pours = collect_pours(board);
    let keepouts = collect_keepouts(board);
    let grids = collect_drc_grids(&cu_layers, &pads, &vias, &tracks);
    let edge_idx = BoardEdgeIndex::new(board);
    let max_clr = compute_max_clearance(board, &pours);

    let mask_needs_openings = board.rules.mask_to_copper_nm.is_some_and(|v| v >= 0);
    let silk_needs_openings = board.rules.silk_to_mask_nm.is_some_and(|v| v >= 0);
    let shared_openings: Option<(Vec<MaskOpening<'_>>, Vec<MaskOpening<'_>>)> =
        (mask_needs_openings || silk_needs_openings).then(|| {
            let top = if cu_layers.iter().any(|l| l == "F.Cu") || silk_needs_openings {
                collect_mask_openings(
                    board,
                    &cu_layers,
                    &pads,
                    &vias,
                    Side::Top,
                    silk_needs_openings,
                )
            } else {
                Vec::new()
            };
            let bot = if cu_layers.iter().any(|l| l == "B.Cu") || silk_needs_openings {
                collect_mask_openings(
                    board,
                    &cu_layers,
                    &pads,
                    &vias,
                    Side::Bottom,
                    silk_needs_openings,
                )
            } else {
                Vec::new()
            };
            (top, bot)
        });
    let openings_ref = shared_openings
        .as_ref()
        .map(|(t, b)| (t.as_slice(), b.as_slice()));
    let inputs = DrcInputs {
        cu_layers: &cu_layers,
        tracks: &tracks,
        pads: &pads,
        vias: &vias,
        pours: &pours,
        grids: &grids,
        max_clr,
    };

    let mut violations = std::thread::scope(|s| {
        let h_copper = s.spawn(|| {
            let mut v = Vec::new();
            check_track_rules_and_clearances(board, &inputs, &mut v);
            check_pad_to_pad_clearances(board, &inputs, &mut v);
            check_via_to_via_and_pad(board, &inputs, &mut v);
            check_mask_to_copper(board, &inputs, openings_ref, &mut v);
            v
        });
        let h_pours = s.spawn(|| {
            let mut v = Vec::new();
            check_pour_clearances(board, &inputs, &mut v);
            v
        });
        let h_drills = s.spawn(|| check_drill_rules(board, &inputs, &drills));
        let h_layout = s.spawn(|| {
            let mut v = Vec::new();
            check_edge_clearances(
                board,
                &tracks,
                &pads,
                &vias,
                &pours,
                edge_idx.as_ref(),
                &mut v,
            );
            check_keepouts(board, &inputs, &keepouts, &mut v);
            check_courtyards_and_component_edges(board, &keepouts, edge_idx.as_ref(), &mut v);
            check_constraints(board, &pads, &mut v);
            check_silkscreen(board, edge_idx.as_ref(), openings_ref, &mut v);
            v
        });
        let h_conn = s.spawn(|| {
            let mut v =
                drc_connect::check_connectivity(board, &cu_layers, &tracks, &pads, &vias, &pours);
            v.extend(drc_dangling::check_dangling(
                board, &cu_layers, &tracks, &pads, &vias, &pours, &grids,
            ));
            v
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
    let hotspots =
        drc_cluster::cluster_hotspots(Some(board), &violations, DEFAULT_CLUSTER_RADIUS_NM);
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
    drc_cluster::cluster_hotspots(None, violations, cluster_radius_nm)
}

/// Parses `kicad-cli pcb drc --format json` output, maps coordinates onto `BoardIr` objects, and clusters hotspots.
pub fn cluster_kicad_drc_json(
    board: &BoardIr,
    kicad_drc_json: &str,
) -> Result<Vec<Hotspot>, String> {
    drc_cluster::cluster_kicad_drc_json(board, kicad_drc_json)
}
