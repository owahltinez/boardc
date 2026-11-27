use std::collections::{BTreeMap, HashMap};

use serde_json::Value;

use crate::ir::{DesignRules, NetClass, Nm, mm};

fn json_mm(val: &Value, key: &str) -> Option<Nm> {
    val.get(key)
        .and_then(Value::as_f64)
        .filter(|&v| v > 0.0)
        .map(mm)
}

pub(crate) fn parse_kicad_pro(
    src: &str,
    rules: &mut DesignRules,
    net_classes: &mut BTreeMap<String, NetClass>,
    assignments: &mut HashMap<String, String>,
) -> Result<(), String> {
    let root: Value =
        serde_json::from_str(src).map_err(|e| format!("failed to parse .kicad_pro JSON: {e}"))?;
    let mut has_min_via_dia = false;
    let mut has_min_via_drill = false;
    if let Some(r) = root
        .get("board")
        .and_then(|b| b.get("design_settings"))
        .and_then(|d| d.get("rules"))
    {
        for (key, dst) in [
            ("min_track_width", &mut rules.track_width_nm),
            ("min_clearance", &mut rules.clearance_nm),
            ("min_via_annular_width", &mut rules.min_annular_ring_nm),
            ("min_hole_to_hole", &mut rules.hole_to_hole_nm),
            ("min_copper_edge_clearance", &mut rules.edge_clearance_nm),
            ("min_silk_clearance", &mut rules.silk_clearance_nm),
        ] {
            if let Some(v) = json_mm(r, key) {
                *dst = v;
            }
        }
        if let Some(v) = json_mm(r, "min_via_diameter") {
            rules.via_dia_nm = v;
            has_min_via_dia = true;
        }
        if let Some(v) = json_mm(r, "min_through_hole_diameter") {
            rules.via_drill_nm = v;
            has_min_via_drill = true;
        }
        if let Some(v) = json_mm(r, "min_hole_clearance") {
            rules.hole_clearance_nm = Some(v);
        }
        if let Some(v) = json_mm(r, "solder_mask_to_copper_clearance") {
            rules.mask_to_copper_nm = Some(v);
        }
    }
    if let Some(ns) = root.get("net_settings") {
        if let Some(classes) = ns.get("classes").and_then(Value::as_array) {
            for c in classes {
                let Some(name) = c.get("name").and_then(Value::as_str) else {
                    continue;
                };
                let nc = NetClass {
                    name: name.to_string(),
                    track_width_nm: json_mm(c, "track_width").unwrap_or(rules.track_width_nm),
                    clearance_nm: json_mm(c, "clearance").unwrap_or(rules.clearance_nm),
                    via_dia_nm: json_mm(c, "via_diameter").unwrap_or(rules.via_dia_nm),
                    via_drill_nm: json_mm(c, "via_drill").unwrap_or(rules.via_drill_nm),
                    diff_pair_width_nm: json_mm(c, "diff_pair_width"),
                    diff_pair_gap_nm: json_mm(c, "diff_pair_gap"),
                };
                if name == "Default" {
                    if !has_min_via_dia {
                        rules.via_dia_nm = nc.via_dia_nm;
                    }
                    if !has_min_via_drill {
                        rules.via_drill_nm = nc.via_drill_nm;
                    }
                }
                net_classes.insert(name.to_string(), nc);
            }
        }
        if let Some(map) = ns.get("netclass_assignments").and_then(Value::as_object) {
            for (net, cls) in map {
                if let Some(cls_name) = cls.as_str() {
                    assignments.insert(net.clone(), cls_name.to_string());
                } else if let Some(arr) = cls.as_array()
                    && let Some(first) = arr.first().and_then(Value::as_str)
                {
                    assignments.insert(net.clone(), first.to_string());
                }
            }
        }
    }
    Ok(())
}
