use serde::{Deserialize, Serialize};

use crate::drc_cluster;
use crate::drc_connect;
use crate::drc_constraints::{check_constraints, check_repair_constraints};
use crate::drc_copper::{
    check_pad_to_pad_clearances, check_pour_clearances, check_track_rules_and_clearances,
    check_via_to_via_and_pad,
};
use crate::drc_dangling;
use crate::drc_layout::{check_courtyards_and_component_edges, check_keepouts};
use crate::drc_mask::{
    MaskOpening, MaskOpeningSides, append_via_mask_openings, check_mask_to_copper,
    collect_mask_openings, collect_pad_mask_openings,
};
use crate::drc_rules::{check_drill_rules, check_edge_clearances};
use crate::drc_scene::{
    BoardEdgeIndex, DrcInputs, KeepoutItem, PadItem, collect_drc_grids, collect_drills,
    collect_keepouts, collect_pads, collect_pours, collect_tracks, collect_vias,
    compute_max_clearance,
};
use crate::drc_silk::check_silkscreen;
use crate::geom::SpatialGrid;
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
    let violations = run_drc(board, None, true);
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

/// Precomputed routing-invariant board state shared across repair-pass DRC evaluations.
pub(crate) struct RepairDrcStatic {
    cu_layers: Vec<String>,
    pads: Vec<PadItem>,
    keepouts: Vec<KeepoutItem>,
    edge_idx: Option<BoardEdgeIndex>,
    pad_grid: SpatialGrid,
    mask_openings: Option<(Vec<MaskOpening>, Vec<MaskOpening>)>,
}

impl RepairDrcStatic {
    pub(crate) fn new(board: &BoardIr) -> Self {
        let cu_layers = board.copper_layer_names();
        let pads = collect_pads(board, &cu_layers);
        let keepouts = collect_keepouts(board);
        let edge_idx = BoardEdgeIndex::new(board);
        let pad_grid = SpatialGrid::new(pads.iter().map(|p| p.bbox), 2_000_000);
        let mask_openings = board
            .rules
            .mask_to_copper_nm
            .is_some_and(|v| v >= 0)
            .then(|| {
                let top = if cu_layers.iter().any(|l| l == "F.Cu") {
                    collect_pad_mask_openings(board, &cu_layers, &pads, Side::Top)
                } else {
                    Vec::new()
                };
                let bot = if cu_layers.iter().any(|l| l == "B.Cu") {
                    collect_pad_mask_openings(board, &cu_layers, &pads, Side::Bottom)
                } else {
                    Vec::new()
                };
                (top, bot)
            });
        Self {
            cu_layers,
            pads,
            keepouts,
            edge_idx,
            pad_grid,
            mask_openings,
        }
    }
}

fn run_drc(board: &BoardIr, static_ctx: Option<&RepairDrcStatic>, full: bool) -> Vec<Violation> {
    let owned_cu;
    let owned_pads;
    let owned_keepouts;
    let owned_edge;
    let owned_pad_grid;
    let (cu_layers, pads, keepouts, edge_idx, pad_grid) = if let Some(ctx) = static_ctx {
        (
            ctx.cu_layers.as_slice(),
            ctx.pads.as_slice(),
            ctx.keepouts.as_slice(),
            ctx.edge_idx.as_ref(),
            &ctx.pad_grid,
        )
    } else {
        owned_cu = board.copper_layer_names();
        owned_pads = collect_pads(board, &owned_cu);
        owned_keepouts = collect_keepouts(board);
        owned_edge = BoardEdgeIndex::new(board);
        owned_pad_grid = SpatialGrid::new(owned_pads.iter().map(|p| p.bbox), 2_000_000);
        (
            owned_cu.as_slice(),
            owned_pads.as_slice(),
            owned_keepouts.as_slice(),
            owned_edge.as_ref(),
            &owned_pad_grid,
        )
    };

    let tracks = collect_tracks(board, cu_layers);
    let vias = collect_vias(board, cu_layers);
    let drills = collect_drills(board, pads, &vias, cu_layers);
    let pours = collect_pours(board);
    let grids = collect_drc_grids(cu_layers, pad_grid, &vias, &tracks);
    let max_clr = compute_max_clearance(board, &pours);

    let mask_needs_openings = board.rules.mask_to_copper_nm.is_some_and(|v| v >= 0);
    let silk_needs_openings = full && board.rules.silk_to_mask_nm.is_some_and(|v| v >= 0);
    let mut full_openings: Option<(Vec<MaskOpening>, Vec<MaskOpening>)> = None;
    let mut via_openings: Option<(Vec<MaskOpening>, Vec<MaskOpening>)> = None;
    let top_slices: [&[MaskOpening]; 2];
    let bot_slices: [&[MaskOpening]; 2];
    let (mask_openings_ref, silk_openings_ref) = if let Some(ctx) = static_ctx
        && !silk_needs_openings
    {
        if mask_needs_openings && let Some((top_s, bot_s)) = ctx.mask_openings.as_ref() {
            let mut vt = Vec::new();
            if cu_layers.iter().any(|l| l == "F.Cu") {
                append_via_mask_openings(board, &vias, Side::Top, &mut vt);
            }
            let mut vb = Vec::new();
            if cu_layers.iter().any(|l| l == "B.Cu") {
                append_via_mask_openings(board, &vias, Side::Bottom, &mut vb);
            }
            let (vt_ref, vb_ref) = via_openings.insert((vt, vb));
            top_slices = [top_s.as_slice(), vt_ref.as_slice()];
            bot_slices = [bot_s.as_slice(), vb_ref.as_slice()];
            (
                Some(MaskOpeningSides {
                    top: &top_slices,
                    bot: &bot_slices,
                }),
                None,
            )
        } else {
            (None, None)
        }
    } else if mask_needs_openings || silk_needs_openings {
        let top = if cu_layers.iter().any(|l| l == "F.Cu") || silk_needs_openings {
            collect_mask_openings(
                board,
                cu_layers,
                pads,
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
                cu_layers,
                pads,
                &vias,
                Side::Bottom,
                silk_needs_openings,
            )
        } else {
            Vec::new()
        };
        let (t_ref, b_ref) = full_openings.insert((top, bot));
        top_slices = [t_ref.as_slice(), &[]];
        bot_slices = [b_ref.as_slice(), &[]];
        (
            mask_needs_openings.then_some(MaskOpeningSides {
                top: &top_slices[..1],
                bot: &bot_slices[..1],
            }),
            silk_needs_openings.then_some((t_ref.as_slice(), b_ref.as_slice())),
        )
    } else {
        (None, None)
    };

    let inputs = DrcInputs {
        cu_layers,
        tracks: &tracks,
        pads,
        vias: &vias,
        pours: &pours,
        grids: &grids,
        max_clr,
    };

    let mut violations = std::thread::scope(|s| {
        let h_copper = s.spawn(|| {
            let mut v = Vec::new();
            check_track_rules_and_clearances(board, &inputs, &mut v);
            if full {
                check_pad_to_pad_clearances(board, &inputs, &mut v);
            }
            check_via_to_via_and_pad(board, &inputs, &mut v);
            check_mask_to_copper(board, &inputs, mask_openings_ref, &mut v);
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
            check_edge_clearances(board, &tracks, pads, &vias, &pours, edge_idx, &mut v);
            check_keepouts(board, &inputs, keepouts, &mut v);
            if full {
                check_courtyards_and_component_edges(board, keepouts, edge_idx, &mut v);
                check_constraints(board, pads, &mut v);
                check_silkscreen(board, edge_idx, silk_openings_ref, &mut v);
            } else {
                check_repair_constraints(board, pads, &mut v);
            }
            v
        });
        let h_conn = s.spawn(|| {
            let mut v =
                drc_connect::check_connectivity(board, cu_layers, &tracks, pads, &vias, &pours);
            v.extend(drc_dangling::check_dangling(
                board, cu_layers, &tracks, pads, &vias, &pours, &grids,
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
    violations
}

/// Fast repair-pass DRC returning atomic violations while skipping checks invariant during routing.
pub(crate) fn check_repair_drc(board: &BoardIr, static_ctx: &RepairDrcStatic) -> Vec<Violation> {
    run_drc(board, Some(static_ctx), false)
}

/// Groups atomic violations into spatial and shared-object root-cause hotspots.
pub fn cluster_hotspots(violations: &[Violation], cluster_radius_nm: Nm) -> Vec<Hotspot> {
    drc_cluster::cluster_hotspots(None, violations, cluster_radius_nm)
}
