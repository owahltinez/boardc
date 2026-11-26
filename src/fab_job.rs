use std::fs;
use std::path::Path;

use serde_json::{Map, Value, json};

use crate::fab_gerber::copper_gerber_filename;
use crate::ir::{BBoxNm, BoardIr, SOLDER_MASK_THICKNESS_NM, nm_to_mm};

fn project_guid(name: &str) -> String {
    let mut hash: u128 = 0x6c62_272e_07bb_0142_62b8_2175_6295_c58d;
    for b in name.as_bytes() {
        hash ^= *b as u128;
        hash = hash.wrapping_mul(0x0000_0000_0100_0000_0000_0000_0000_013b);
    }
    let hex = format!("{hash:032x}");
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
    let layers = board.effective_layers();
    let (cu_nm, diel_nm) = board.resolved_stackup_thicknesses();
    let mask_mm = nm_to_mm(SOLDER_MASK_THICKNESS_NM);
    let r = &board.rules;

    let mut general = Map::new();
    general.insert(
        "ProjectId".into(),
        json!({
            "Name": board.name,
            "GUID": project_guid(&board.name),
            "Revision": "1",
        }),
    );
    general.insert(
        "Size".into(),
        json!({
            "X": nm_to_mm(bbox.width()),
            "Y": nm_to_mm(bbox.height()),
        }),
    );
    general.insert("LayerNumber".into(), json!(cu_layers.len()));
    general.insert(
        "BoardThickness".into(),
        json!(nm_to_mm(board.effective_board_thickness_nm())),
    );
    general.insert(
        "Finish".into(),
        json!(board.stackup.finish_trimmed().unwrap_or("None")),
    );
    if board.stackup.pofv {
        general.insert("ViaProtection".into(), json!(["VII"]));
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
    for (idx, layer) in layers.iter().enumerate() {
        stackup.push(json!({
            "Type": "Copper",
            "Thickness": nm_to_mm(cu_nm[idx]),
            "Name": layer.name,
        }));
        if let Some(&d_nm) = diel_nm.get(idx) {
            let next_name = layers
                .get(idx + 1)
                .map(|l| l.name.as_str())
                .unwrap_or("B.Cu");
            stackup.push(json!({
                "Type": "Dielectric",
                "Thickness": nm_to_mm(d_nm),
                "Material": "FR4",
                "Name": format!("{}/{next_name}", layer.name),
                "Notes": format!(
                    "Type: dielectric layer {} (from {} to {next_name})",
                    idx + 1,
                    layer.name
                ),
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
        "GeneralSpecs": Value::Object(general),
        "DesignRules": design_rules,
        "FilesAttributes": files_attrs,
        "MaterialStackup": stackup,
    });
    let text = serde_json::to_string_pretty(&doc)
        .map_err(|e| format!("failed to serialize Gerber job file: {e}"))?;
    fs::write(out_path, format!("{text}\n"))
        .map_err(|e| format!("failed to write Gerber job '{}': {e}", out_path.display()))
}
