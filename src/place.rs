use serde::{Deserialize, Serialize};

use crate::ir::{BoardIr, Nm, Pt};
use crate::place_legalize;
use crate::place_vias;

/// Configuration for `legalize_placement`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct PlaceOptions {
    pub grid_step_nm: Nm,
    pub max_search_radius_nm: Nm,
    pub allow_rotation: bool,
}

impl Default for PlaceOptions {
    fn default() -> Self {
        Self {
            grid_step_nm: 200_000,
            max_search_radius_nm: 20_000_000,
            allow_rotation: true,
        }
    }
}

/// Summary of a moved component during placement legalization.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct MovedPart {
    pub refdes: String,
    pub from_pos: Pt,
    pub to_pos: Pt,
    pub from_rot_deg: f64,
    pub to_rot_deg: f64,
    pub displacement_nm: Nm,
}

/// Final `cap_proximity` evaluation after placement legalization.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CapProximityStatus {
    pub cap_refdes: String,
    pub ic_refdes: String,
    pub net: String,
    pub final_dist_nm: Nm,
    pub max_dist_nm: Nm,
    pub satisfied: bool,
}

/// Placement that could not satisfy all geometric or constraint rules.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct UnresolvablePlacement {
    pub refdes: String,
    pub reason: String,
}

/// Deterministic placement legalizer report.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct PlaceReport {
    pub total_placements: usize,
    pub fixed_count: usize,
    pub moved_parts: Vec<MovedPart>,
    pub cap_proximity: Vec<CapProximityStatus>,
    pub unresolvable: Vec<UnresolvablePlacement>,
    pub ok: bool,
    pub elapsed_ms: u128,
}

/// Configuration for `solve_power_vias`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct PowerViaOptions {
    pub plane_nets: Vec<String>,
    pub allow_pofv: Option<bool>,
    pub thermal_pad_min_dim_nm: Nm,
    pub thermal_pitch_nm: Nm,
    pub thermal_via_dia_nm: Option<Nm>,
    pub thermal_via_drill_nm: Option<Nm>,
    pub plane_edge_margin_nm: Nm,
    pub stub_width_nm: Option<Nm>,
    pub max_stub_len_nm: Nm,
}

impl Default for PowerViaOptions {
    fn default() -> Self {
        Self {
            plane_nets: Vec::new(),
            allow_pofv: None,
            thermal_pad_min_dim_nm: 1_500_000,
            thermal_pitch_nm: 900_000,
            thermal_via_dia_nm: None,
            thermal_via_drill_nm: None,
            plane_edge_margin_nm: 50_000,
            stub_width_nm: None,
            max_stub_len_nm: 2_500_000,
        }
    }
}

/// Record of a power or thermal via added by `solve_power_vias`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SolvedViaDrop {
    pub pad_label: String,
    pub net: String,
    pub pos: Pt,
    pub is_thermal: bool,
    pub is_pofv: bool,
    pub stub_len_nm: Nm,
}

/// Plane-net SMD pad that could not receive a legal via drop.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct UnresolvedPowerPad {
    pub pad_label: String,
    pub net: String,
    pub reason: String,
}

/// Summary report emitted by `solve_power_vias`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct PowerViaReport {
    pub plane_nets: Vec<String>,
    pub thermal_vias_added: usize,
    pub plane_vias_added: usize,
    pub stubs_added: usize,
    pub pofv_count: usize,
    pub drops: Vec<SolvedViaDrop>,
    pub unresolved: Vec<UnresolvedPowerPad>,
    pub chokes_after: usize,
    pub ok: bool,
    pub elapsed_ms: u128,
}

/// Legalizes movable component placements against outline, keepouts, BGA shadows, courtyards, and `cap_proximity`.
pub fn legalize_placement(board: &mut BoardIr, cfg: &PlaceOptions) -> Result<PlaceReport, String> {
    place_legalize::legalize_placement(board, cfg)
}

/// Solves thermal via arrays for large exposed pads and plane-drop vias with short stubs for power SMD pads.
pub fn solve_power_vias(
    board: &mut BoardIr,
    cfg: &PowerViaOptions,
) -> Result<PowerViaReport, String> {
    place_vias::solve_power_vias(board, cfg)
}
