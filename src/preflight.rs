use std::collections::HashSet;
use std::time::Instant;

use serde::{Deserialize, Serialize};
use viahop::run_preflight as viahop_run_preflight;

use crate::drc::Violation;
use crate::drc_constraints::check_cap_proximity;
use crate::drc_layout::{check_courtyards_and_component_edges, check_keepouts};
use crate::drc_rules::check_edge_clearances;
use crate::drc_scene::{
    BoardEdgeIndex, DrcInputs, collect_drc_grids, collect_keepouts, collect_pads, collect_pours,
    collect_tracks, collect_vias,
};
use crate::erc::{ErcViolation, check_board_erc};
use crate::geom::SpatialGrid;
use crate::ir::{BoardIr, LayerKind, Pt};
use crate::route::{point_from_viahop as pt_from_viahop, to_viahop_board};
use crate::zones::{ZoneChoke, analyze_zone_chokes, fill_zones};

/// Dense BGA cluster summary emitted by `run_preflight`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct PreflightBgaCluster {
    pub refdes: String,
    pub total_pads: usize,
    pub dense_pads: usize,
    pub max_ring: usize,
    pub pitch_um: f64,
    pub sealed_count: usize,
}

/// Sealed BGA pad that has no legal escape channel.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PreflightSealedPad {
    pub refdes: String,
    pub pin: String,
    pub net: String,
    pub pos_nm: Pt,
    pub ring: usize,
    pub blocker: String,
}

/// Routable net pin-pair blocked statically on an empty grid.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PreflightBlockedNet {
    pub net: String,
    pub class_name: String,
    pub from_pin: String,
    pub from_nm: Pt,
    pub to_pin: String,
    pub to_nm: Pt,
    pub reason: String,
}

/// Static DRC violation on pre-existing fixed copper.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct PreflightFixedViolation {
    pub kind: String,
    pub net_a: Option<String>,
    pub net_b: Option<String>,
    pub layer: Option<String>,
    pub pos_nm: Pt,
    pub gap_um: f64,
    pub required_um: f64,
    pub blocker: String,
}

/// Combined static feasibility report from `run_preflight`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct PreflightSummary {
    pub ok: bool,
    pub stopped_early: bool,
    pub total_nets: usize,
    pub routable_nets: usize,
    pub grid_step_um: f64,
    pub via_at_smd: bool,
    pub layout_violations: Vec<Violation>,
    pub zone_chokes: Vec<ZoneChoke>,
    pub bga_clusters: Vec<PreflightBgaCluster>,
    pub sealed_pads: Vec<PreflightSealedPad>,
    pub blocked_nets: Vec<PreflightBlockedNet>,
    pub fixed_violations: Vec<PreflightFixedViolation>,
    #[serde(default)]
    pub erc_violations: Vec<ErcViolation>,
    pub elapsed_ms: u128,
}

/// Runs sub-second static feasibility gate combining layout/DRC, zone-choke, and `viahop` preflight checks.
pub fn run_preflight(
    board: &BoardIr,
    ignored_nets: &HashSet<String>,
) -> Result<PreflightSummary, String> {
    let start = Instant::now();
    let cu_layers = board.copper_layer_names();

    let pads = collect_pads(board, &cu_layers);
    let tracks = collect_tracks(board, &cu_layers);
    let vias = collect_vias(board, &cu_layers);
    let pours = collect_pours(board);
    let keepouts = collect_keepouts(board);
    let edge_idx = BoardEdgeIndex::new(board);

    let mut layout_violations = Vec::new();
    check_courtyards_and_component_edges(
        board,
        &keepouts,
        edge_idx.as_ref(),
        &mut layout_violations,
    );
    if !keepouts.is_empty() {
        let pad_grid = SpatialGrid::new(pads.iter().map(|p| p.bbox), 2_000_000);
        let grids = collect_drc_grids(&cu_layers, &pad_grid, &vias, &tracks);
        let drc_inputs = DrcInputs {
            cu_layers: &cu_layers,
            tracks: &tracks,
            pads: &pads,
            vias: &vias,
            pours: &pours,
            grids: &grids,
            max_clr: 0,
        };
        check_keepouts(board, &drc_inputs, &keepouts, &mut layout_violations);
    }
    check_edge_clearances(
        board,
        &tracks,
        &pads,
        &vias,
        &pours,
        edge_idx.as_ref(),
        &mut layout_violations,
    );
    check_cap_proximity(board, &pads, &mut layout_violations);

    let vboard = to_viahop_board(board, ignored_nets, None)?;
    let threads = std::thread::available_parallelism()
        .map_or(1, |n| n.get())
        .clamp(1, 8);

    let (zone_chokes, vreport) = std::thread::scope(|s| {
        let z_handle = s.spawn(|| {
            if !board.pours.is_empty() && board.filled_pours.is_empty() {
                let mut tmp = board.clone();
                fill_zones(&mut tmp).chokes
            } else {
                analyze_zone_chokes(board)
            }
        });
        let vreport = viahop_run_preflight(&vboard, threads);
        let zone_chokes = z_handle
            .join()
            .expect("preflight zone choke thread panicked");
        (zone_chokes, vreport)
    });

    let bga_clusters = vreport
        .bga_clusters
        .into_iter()
        .map(|c| PreflightBgaCluster {
            refdes: c.refdes,
            total_pads: c.total_pads,
            dense_pads: c.dense_pads,
            max_ring: c.max_ring,
            pitch_um: c.pitch_um,
            sealed_count: c.sealed_count,
        })
        .collect();
    let sealed_pads: Vec<PreflightSealedPad> = vreport
        .sealed_pads
        .into_iter()
        .map(|p| PreflightSealedPad {
            refdes: p.refdes,
            pin: p.pin,
            net: p.net,
            pos_nm: pt_from_viahop(p.pos),
            ring: p.ring,
            blocker: p.blocker,
        })
        .collect();
    let blocked_nets: Vec<PreflightBlockedNet> = vreport
        .blocked_nets
        .into_iter()
        .map(|b| PreflightBlockedNet {
            net: b.net,
            class_name: b.class_name,
            from_pin: b.from_pin,
            from_nm: pt_from_viahop(b.from_pos),
            to_pin: b.to_pin,
            to_nm: pt_from_viahop(b.to_pos),
            reason: b.reason,
        })
        .collect();
    let mask_active = board.rules.mask_to_copper_nm.is_some_and(|v| v >= 0);
    let eff_layers = board.effective_layers();
    let has_signal_layer = eff_layers.iter().any(|l| l.kind != LayerKind::Plane);
    let fixed_violations: Vec<PreflightFixedViolation> = vreport
        .fixed_violations
        .into_iter()
        .filter(|v| {
            // Synthetic mask-guard FixedPlanes live only on non-Plane outer layers; ignore phantom hits against them.
            if !mask_active || !v.blocker.starts_with("plane (") {
                return true;
            }
            match v.layer.as_deref() {
                Some(name) => !eff_layers
                    .iter()
                    .find(|l| l.name == name)
                    .is_some_and(|l| l.kind != LayerKind::Plane),
                None => !has_signal_layer,
            }
        })
        .map(|v| PreflightFixedViolation {
            kind: v.kind.to_string(),
            net_a: v.net_a,
            net_b: v.net_b,
            layer: v.layer,
            pos_nm: pt_from_viahop(v.pos),
            gap_um: v.gap_um,
            required_um: v.required_um,
            blocker: v.blocker,
        })
        .collect();

    let erc = check_board_erc(board);

    let ok = !vreport.stopped_early
        && layout_violations.is_empty()
        && zone_chokes.is_empty()
        && sealed_pads.is_empty()
        && blocked_nets.is_empty()
        && fixed_violations.is_empty()
        && erc.passed;

    Ok(PreflightSummary {
        ok,
        stopped_early: vreport.stopped_early,
        total_nets: vreport.total_nets,
        routable_nets: vreport.routable_nets,
        grid_step_um: vreport.grid_step_um,
        via_at_smd: vreport.via_at_smd,
        layout_violations,
        zone_chokes,
        bga_clusters,
        sealed_pads,
        blocked_nets,
        fixed_violations,
        erc_violations: erc.violations,
        elapsed_ms: start.elapsed().as_millis(),
    })
}
