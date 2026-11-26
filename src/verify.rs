use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::ir::{BBoxNm, BoardIr};
use crate::verify_gerber::verify_fab_outputs_impl;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct GerberFileCheck {
    pub file: PathBuf,
    pub parse_errors: usize,
    pub command_count: usize,
    pub within_bounds: bool,
    pub bbox_nm: Option<BBoxNm>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DrillCheck {
    pub passed: bool,
    pub pth_expected: usize,
    pub pth_parsed: usize,
    pub pth_matched: usize,
    pub npth_expected: usize,
    pub npth_parsed: usize,
    pub npth_matched: usize,
    pub mismatches: usize,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ConnectivityCheck {
    pub passed: bool,
    pub island_count: usize,
    pub checked_nets: usize,
    pub shorts: Vec<String>,
    pub opens: Vec<String>,
    pub missing_pad_copper: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct VerifyReport {
    pub passed: bool,
    pub gerber_files: Vec<GerberFileCheck>,
    pub drills: DrillCheck,
    pub connectivity: ConnectivityCheck,
    pub errors: Vec<String>,
}

/// Independently verifies emitted Gerbers, Excellon drills, and copper connectivity against `BoardIr`.
pub fn verify_fab_outputs(board: &BoardIr, fab_dir: &Path) -> Result<VerifyReport, String> {
    let (gerber_files, drills, connectivity) = verify_fab_outputs_impl(board, fab_dir)?;
    let mut errors = Vec::new();
    for g in &gerber_files {
        if let Some(e) = &g.error {
            errors.push(e.clone());
        }
    }
    if !drills.passed {
        errors.push(format!(
            "Excellon drill mismatch: {} mismatches (PTH {}/{}, NPTH {}/{})",
            drills.mismatches,
            drills.pth_matched,
            drills.pth_expected,
            drills.npth_matched,
            drills.npth_expected
        ));
    }
    errors.extend(connectivity.shorts.iter().cloned());
    errors.extend(connectivity.opens.iter().cloned());
    for m in &connectivity.missing_pad_copper {
        errors.push(format!("missing pad copper in Gerbers: {m}"));
    }
    Ok(VerifyReport {
        passed: errors.is_empty(),
        gerber_files,
        drills,
        connectivity,
        errors,
    })
}
