use std::fs;
use std::path::Path;

use serde_json::json;

use crate::fab_gerber::copper_gerber_filename;
use crate::ir::{BBoxNm, BoardIr, SOLDER_MASK_THICKNESS_NM, fnv1a_128, nm_to_mm};

fn project_guid(name: &str) -> String {
    let hex = format!("{:032x}", fnv1a_128(name.as_bytes()));
    format!(
        "{}-{}-4{}-8{}-{}",
        &hex[0..8],
        &hex[8..12],
        &hex[13..16],
        &hex[17..20],
        &hex[20..32]
    )
}

pub(crate) fn emit_gbrjob(
    board: &BoardIr,
    cu_layers: &[String],
    out_path: &Path,
) -> Result<(), String> {
    let bbox = BBoxNm::from_pts(&board.outline);
    let (copper, diel_nm) = board.resolved_stackup_thicknesses();
    let mask_mm = nm_to_mm(SOLDER_MASK_THICKNESS_NM);
    let r = &board.rules;

    let mut general = json!({
        "ProjectId": {
            "Name": board.name,
            "GUID": project_guid(&board.name),
            "Revision": board.effective_revision(),
        },
        "Size": {
            "X": nm_to_mm(bbox.width()),
            "Y": nm_to_mm(bbox.height()),
        },
        "LayerNumber": cu_layers.len(),
        "BoardThickness": nm_to_mm(board.effective_board_thickness_nm()),
        "Finish": board.stackup.finish_trimmed().unwrap_or("None"),
    });
    if board.pofv_enabled() {
        general["ViaProtection"] = json!(["VII"]);
    }

    let mut design_rules = vec![json!({
        "Layers": "Outer",
        "PadToPad": nm_to_mm(r.smd_to_smd_nm.unwrap_or(r.clearance_nm)),
        "PadToTrack": nm_to_mm(r.clearance_nm),
        "TrackToTrack": nm_to_mm(r.clearance_nm),
        "MinLineWidth": nm_to_mm(r.track_width_nm),
    })];
    if cu_layers.len() > 2 {
        design_rules.push(json!({
            "Layers": "Inner",
            "PadToPad": nm_to_mm(r.clearance_nm),
            "PadToTrack": nm_to_mm(r.clearance_nm),
            "TrackToTrack": nm_to_mm(r.clearance_nm),
            "MinLineWidth": nm_to_mm(r.track_width_nm),
        }));
    }

    let mut files_attrs = Vec::with_capacity(cu_layers.len() + 7);
    for (idx, layer_name) in cu_layers.iter().enumerate() {
        let side = if idx == 0 {
            "Top"
        } else if idx + 1 == cu_layers.len() {
            "Bot"
        } else {
            "Inr"
        };
        files_attrs.push(json!({
            "Path": copper_gerber_filename(layer_name, idx, cu_layers),
            "FileFunction": format!("Copper,L{},{side}", idx + 1),
            "FilePolarity": "Positive",
        }));
    }
    for (path, func, polarity) in [
        ("F_Mask.gts", "SolderMask,Top", "Negative"),
        ("F_Paste.gtp", "SolderPaste,Top", "Positive"),
        ("F_Silkscreen.gto", "Legend,Top", "Positive"),
        ("B_Mask.gbs", "SolderMask,Bot", "Negative"),
        ("B_Paste.gbp", "SolderPaste,Bot", "Positive"),
        ("B_Silkscreen.gbo", "Legend,Bot", "Positive"),
        ("Edge_Cuts.gm1", "Profile", "Positive"),
    ] {
        files_attrs.push(json!({
            "Path": path,
            "FileFunction": func,
            "FilePolarity": polarity,
        }));
    }

    let mut stackup = vec![
        json!({ "Type": "Legend", "Name": "Top Silk Screen" }),
        json!({ "Type": "SolderPaste", "Name": "Top Solder Paste" }),
        json!({ "Type": "SolderMask", "Thickness": mask_mm, "Name": "Top Solder Mask" }),
    ];
    for (idx, &(layer_name, cu_thick)) in copper.iter().enumerate() {
        stackup.push(json!({
            "Type": "Copper",
            "Thickness": nm_to_mm(cu_thick),
            "Name": layer_name,
        }));
        if let Some(&d_nm) = diel_nm.get(idx) {
            let (diel_name, notes) = if let Some(&(next_name, _)) = copper.get(idx + 1) {
                (
                    format!("{layer_name}/{next_name}"),
                    format!(
                        "Type: dielectric layer {} (from {layer_name} to {next_name})",
                        idx + 1
                    ),
                )
            } else {
                (
                    format!("{layer_name}/B.Mask"),
                    format!(
                        "Type: dielectric layer {} (substrate below {layer_name})",
                        idx + 1
                    ),
                )
            };
            stackup.push(json!({
                "Type": "Dielectric",
                "Thickness": nm_to_mm(d_nm),
                "Material": "FR4",
                "Name": diel_name,
                "Notes": notes,
            }));
        }
    }
    stackup
        .push(json!({ "Type": "SolderMask", "Thickness": mask_mm, "Name": "Bottom Solder Mask" }));
    stackup.push(json!({ "Type": "SolderPaste", "Name": "Bottom Solder Paste" }));
    stackup.push(json!({ "Type": "Legend", "Name": "Bottom Silk Screen" }));

    let doc = json!({
        "Header": {
            "GenerationSoftware": {
                "Vendor": "boardc",
                "Application": "boardc",
                "Version": env!("CARGO_PKG_VERSION"),
            }
        },
        "GeneralSpecs": general,
        "DesignRules": design_rules,
        "FilesAttributes": files_attrs,
        "MaterialStackup": stackup,
    });
    let text = serde_json::to_string_pretty(&doc)
        .map_err(|e| format!("failed to serialize Gerber job file: {e}"))?;
    fs::write(out_path, format!("{text}\n"))
        .map_err(|e| format!("failed to write Gerber job '{}': {e}", out_path.display()))
}

pub(crate) fn verify_gbrjob(
    board: &BoardIr,
    cu_layers: &[String],
    fab_dir: &Path,
    job_path: &Path,
) -> Result<(), String> {
    let job_text = fs::read_to_string(job_path).map_err(|e| {
        format!(
            "missing or unreadable Gerber job file '{}': {e}",
            job_path.display()
        )
    })?;
    let job_json: serde_json::Value = serde_json::from_str(&job_text)
        .map_err(|e| format!("invalid Gerber job JSON '{}': {e}", job_path.display()))?;
    let specs = &job_json["GeneralSpecs"];
    if specs["LayerNumber"].as_u64() != Some(cu_layers.len() as u64) {
        return Err(format!(
            "Gerber job '{}' LayerNumber mismatch (expected {})",
            job_path.display(),
            cu_layers.len()
        ));
    }
    let exp_thick = nm_to_mm(board.effective_board_thickness_nm());
    if !specs["BoardThickness"]
        .as_f64()
        .is_some_and(|v| (v - exp_thick).abs() <= 1e-4)
    {
        return Err(format!(
            "Gerber job '{}' BoardThickness mismatch (expected {exp_thick})",
            job_path.display()
        ));
    }
    let exp_rev = board.effective_revision();
    if specs["ProjectId"]["Revision"].as_str() != Some(exp_rev) {
        return Err(format!(
            "Gerber job '{}' ProjectId.Revision mismatch (expected '{exp_rev}')",
            job_path.display()
        ));
    }
    let exp_finish = board.stackup.finish_trimmed().unwrap_or("None");
    if specs["Finish"].as_str() != Some(exp_finish) {
        return Err(format!(
            "Gerber job '{}' Finish mismatch (expected '{exp_finish}')",
            job_path.display()
        ));
    }
    let has_vii = specs
        .get("ViaProtection")
        .and_then(|v| v.as_array())
        .is_some_and(|arr| arr.iter().any(|x| x.as_str() == Some("VII")));
    if has_vii != board.pofv_enabled() {
        return Err(format!(
            "Gerber job '{}' ViaProtection mismatch (expected pofv={})",
            job_path.display(),
            board.pofv_enabled()
        ));
    }
    let files_attr = job_json["FilesAttributes"].as_array().ok_or_else(|| {
        format!(
            "Gerber job '{}' missing FilesAttributes",
            job_path.display()
        )
    })?;
    if files_attr.len() != cu_layers.len() + 7 {
        return Err(format!(
            "Gerber job '{}' FilesAttributes count mismatch (expected {})",
            job_path.display(),
            cu_layers.len() + 7
        ));
    }
    for entry in files_attr {
        let Some(rel) = entry["Path"].as_str() else {
            return Err(format!(
                "Gerber job '{}' FilesAttributes entry missing Path",
                job_path.display()
            ));
        };
        if !fab_dir.join(rel).exists() {
            return Err(format!(
                "Gerber job '{}' references missing file '{rel}'",
                job_path.display()
            ));
        }
    }
    let (_, exp_diel) = board.resolved_stackup_thicknesses();
    let got_diel: Vec<f64> = job_json["MaterialStackup"]
        .as_array()
        .ok_or_else(|| {
            format!(
                "Gerber job '{}' missing MaterialStackup",
                job_path.display()
            )
        })?
        .iter()
        .filter(|e| e["Type"] == "Dielectric")
        .filter_map(|e| e["Thickness"].as_f64())
        .collect();
    if got_diel.len() != exp_diel.len()
        || got_diel
            .iter()
            .zip(&exp_diel)
            .any(|(g, &e)| (g - nm_to_mm(e)).abs() > 1e-4)
    {
        return Err(format!(
            "Gerber job '{}' MaterialStackup dielectric mismatch",
            job_path.display()
        ));
    }
    Ok(())
}
