use std::collections::hash_map::Entry;
use std::collections::{BTreeMap, HashMap, HashSet};
use std::path::Path;

use crate::geom::polygon_signed_area_2;
use crate::ir::{
    BoardIr, DesignRules, FootprintDef, Graphic, Model3d, NetDef, PadKind, PinType, Placement, Pt,
};

pub(crate) fn validate_courtyard(owner: &str, pts: &[Pt]) -> Result<(), String> {
    if pts.len() < 3 {
        return Err(format!("{owner} courtyard must have at least 3 vertices"));
    }
    if polygon_signed_area_2(pts) == 0 {
        return Err(format!("{owner} courtyard polygon must have non-zero area"));
    }
    Ok(())
}

pub(crate) fn validate_models(owner: &str, models: &[Model3d]) -> Result<(), String> {
    for (i, m) in models.iter().enumerate() {
        if m.path.trim().is_empty() {
            return Err(format!("{owner} model#{i} has empty path"));
        }
        if m.offset_mm.iter().any(|v| !v.is_finite())
            || m.scale.iter().any(|v| !v.is_finite())
            || m.rotate_deg.iter().any(|v| !v.is_finite())
        {
            return Err(format!("{owner} model#{i} has non-finite transform value"));
        }
        if m.scale.contains(&0.0) {
            return Err(format!(
                "{owner} model#{i} scale components must be non-zero"
            ));
        }
    }
    Ok(())
}

fn electrical_pad_names(fp: &FootprintDef) -> HashSet<&str> {
    fp.pads
        .iter()
        .filter(|p| p.kind != PadKind::Npth && !p.name.is_empty())
        .map(|p| p.name.as_str())
        .collect()
}

pub(crate) fn validate_pin_names(
    fp_name: &str,
    fp: &FootprintDef,
    pin_names: &BTreeMap<String, Vec<String>>,
) -> Result<(), String> {
    if pin_names.is_empty() {
        return Ok(());
    }
    let known = electrical_pad_names(fp);
    for (alias, targets) in pin_names {
        if alias.trim().is_empty() || alias != alias.trim() {
            return Err(format!(
                "footprint '{fp_name}' pin_names has empty or untrimmed alias '{alias}'"
            ));
        }
        if targets.is_empty() {
            return Err(format!(
                "footprint '{fp_name}' pin_names alias '{alias}' must map to at least one pad"
            ));
        }
        if known.contains(alias.as_str()) && !(targets.len() == 1 && targets[0] == *alias) {
            return Err(format!(
                "footprint '{fp_name}' pin_names alias '{alias}' collides with physical pad name"
            ));
        }
        let mut seen_targets = HashSet::with_capacity(targets.len());
        for t in targets {
            if t.trim().is_empty() || t != t.trim() {
                return Err(format!(
                    "footprint '{fp_name}' pin_names alias '{alias}' has empty or untrimmed target pad '{t}'"
                ));
            }
            if !known.contains(t.as_str()) {
                return Err(format!(
                    "footprint '{fp_name}' pin_names alias '{alias}' references unknown electrical pad '{t}'"
                ));
            }
            if !seen_targets.insert(t.as_str()) {
                return Err(format!(
                    "footprint '{fp_name}' pin_names alias '{alias}' lists duplicate pad '{t}'"
                ));
            }
        }
    }
    Ok(())
}

pub(crate) fn validate_pin_types_for_footprint(
    owner_kind: &str,
    owner_name: &str,
    fp: &FootprintDef,
    pin_types: &BTreeMap<String, PinType>,
) -> Result<(), String> {
    if pin_types.is_empty() {
        return Ok(());
    }
    let known = electrical_pad_names(fp);
    for pin in pin_types.keys() {
        if !known.contains(pin.as_str()) {
            return Err(format!(
                "{owner_kind} '{owner_name}' pin_types references unknown electrical pin '{pin}' on footprint '{}'",
                fp.name
            ));
        }
    }
    Ok(())
}

pub(crate) fn finalize_footprint_pins(
    alias_owner: &str,
    fp: &mut FootprintDef,
    pin_names: &BTreeMap<String, Vec<String>>,
) -> Result<(), String> {
    validate_pin_names(alias_owner, fp, pin_names)?;
    lower_pin_types(
        "footprint",
        &fp.name,
        &fp.name,
        &mut fp.pin_types,
        pin_names,
    )?;
    validate_pin_types_for_footprint("footprint", &fp.name, fp, &fp.pin_types)
}

fn expand_pin<'a>(
    pin: &'a String,
    pin_names: Option<&'a BTreeMap<String, Vec<String>>>,
) -> &'a [String] {
    if let Some(map) = pin_names
        && let Some(targets) = map.get(pin.as_str())
    {
        return targets.as_slice();
    }
    std::slice::from_ref(pin)
}

pub(crate) fn lower_pin_types(
    owner_kind: &str,
    owner_name: &str,
    fp_name: &str,
    pin_types: &mut BTreeMap<String, PinType>,
    pin_names: &BTreeMap<String, Vec<String>>,
) -> Result<(), String> {
    if pin_names.is_empty() || pin_types.is_empty() {
        return Ok(());
    }
    let mut out = BTreeMap::new();
    for (key, &pt) in pin_types.iter() {
        for pad in expand_pin(key, Some(pin_names)) {
            if let Some(&prev) = out.get(pad.as_str())
                && prev != pt
            {
                return Err(format!(
                    "{owner_kind} '{owner_name}' pin_types assigns conflicting types to pad '{pad}' on footprint '{fp_name}'"
                ));
            }
            out.insert(pad.clone(), pt);
        }
    }
    *pin_types = out;
    Ok(())
}

pub(crate) fn lower_placement_pin_nets(
    refdes: &str,
    pin_nets: &mut BTreeMap<String, String>,
    pin_names: &BTreeMap<String, Vec<String>>,
) -> Result<(), String> {
    if pin_names.is_empty() || pin_nets.is_empty() {
        return Ok(());
    }
    let mut out: BTreeMap<String, String> = BTreeMap::new();
    for (key, net) in pin_nets.iter() {
        for pad in expand_pin(key, Some(pin_names)) {
            if let Some(prev) = out.get(pad.as_str()) {
                if prev.trim() != net.trim() {
                    return Err(format!(
                        "placement '{refdes}' pin_nets assigns conflicting nets '{prev}' and '{net}' to pad '{pad}'"
                    ));
                }
            } else {
                out.insert(pad.clone(), net.clone());
            }
        }
    }
    *pin_nets = out;
    Ok(())
}

pub(crate) fn check_subcircuit_pin_net_conflicts(
    placements: &[Placement],
    fp_pin_names: &HashMap<String, &BTreeMap<String, Vec<String>>>,
    sub_net_pins: &[(String, String, String)],
) -> Result<(), String> {
    if sub_net_pins.is_empty() {
        return Ok(());
    }
    let by_ref: HashMap<&str, &Placement> = placements
        .iter()
        .filter(|p| !p.refdes.trim().is_empty())
        .map(|p| (p.refdes.as_str(), p))
        .collect();
    for (sub_net, refdes, raw_pin) in sub_net_pins {
        let Some(&pl) = by_ref.get(refdes.as_str()) else {
            continue;
        };
        let pin_names = fp_pin_names.get(&pl.footprint).copied();
        for pad in expand_pin(raw_pin, pin_names) {
            if let Some(pl_net) = pl.pin_nets.get(pad.as_str())
                && pl_net.trim() != sub_net
            {
                return Err(format!(
                    "subcircuit placement '{refdes}' pin_nets assigns '{pl_net}' to pad '{pad}', conflicting with subcircuit net '{sub_net}'"
                ));
            }
        }
    }
    Ok(())
}

pub(crate) fn lower_nets_pin_aliases(
    placements: &[Placement],
    fp_pin_names: &HashMap<String, &BTreeMap<String, Vec<String>>>,
    nets: &mut BTreeMap<String, NetDef>,
) -> Result<(), String> {
    if nets.is_empty() {
        return Ok(());
    }
    let by_ref: HashMap<&str, &Placement> = placements
        .iter()
        .filter(|p| !p.refdes.trim().is_empty())
        .map(|p| (p.refdes.as_str(), p))
        .collect();
    let mut claimed: HashMap<(String, String), &str> = HashMap::new();
    for (net_name, nd) in nets.iter_mut() {
        if net_name.trim().is_empty() {
            continue;
        }
        let mut expanded = Vec::with_capacity(nd.pins.len());
        for (refdes, pin) in &nd.pins {
            let pl = by_ref.get(refdes.as_str()).copied();
            let pin_names = pl.and_then(|p| fp_pin_names.get(&p.footprint).copied());
            let pads = expand_pin(pin, pin_names);
            let multi_alias = pads.len() > 1;
            for pad in pads {
                if multi_alias
                    && let Some(pl_net) = pl.and_then(|p| p.pin_nets.get(pad.as_str()))
                    && pl_net.trim() != net_name
                {
                    return Err(format!(
                        "placement '{refdes}' pin_nets assigns '{pl_net}' to pad '{pad}', conflicting with multi-pad alias '{refdes}.{pin}' on net '{net_name}'"
                    ));
                }
                let key = (refdes.clone(), pad.clone());
                match claimed.entry(key.clone()) {
                    Entry::Occupied(e) => {
                        let prev_net = *e.get();
                        if prev_net != net_name.as_str() {
                            return Err(format!(
                                "conflicting net assignment for '{refdes}.{pad}': claimed by both '{prev_net}' and '{net_name}'"
                            ));
                        }
                    }
                    Entry::Vacant(e) => {
                        e.insert(net_name.as_str());
                        expanded.push(key);
                    }
                }
            }
        }
        nd.pins = expanded;
    }
    Ok(())
}

pub(crate) fn validate_graphics(owner: &str, graphics: &[Graphic]) -> Result<(), String> {
    for g in graphics {
        if let Graphic::Text {
            height_nm,
            stroke_nm,
            rot_deg,
            ..
        } = g
        {
            if *height_nm <= 0 {
                return Err(format!("{owner} text graphic height_nm must be > 0"));
            }
            if *stroke_nm <= 0 {
                return Err(format!("{owner} text graphic stroke_nm must be > 0"));
            }
            if !rot_deg.is_finite() {
                return Err(format!("{owner} text graphic rot_deg must be finite"));
            }
        }
    }
    Ok(())
}

pub(crate) fn validate_net_names(nets: &BTreeMap<String, NetDef>) -> Result<(), String> {
    for (key, nd) in nets {
        if !nd.name.trim().is_empty() && nd.name != *key {
            return Err(format!("net '{key}' has mismatched name '{}'", nd.name));
        }
    }
    Ok(())
}

pub(crate) fn backfill_net_names(nets: &mut BTreeMap<String, NetDef>) {
    for (key, nd) in nets {
        if nd.name.trim().is_empty() {
            nd.name = key.clone();
        }
    }
}

pub(crate) fn validate_optional_rule_clearances(rules: &DesignRules) -> Result<(), String> {
    for (name, val) in [
        ("mask_to_copper_nm", rules.mask_to_copper_nm),
        ("silk_to_mask_nm", rules.silk_to_mask_nm),
        ("silk_to_edge_nm", rules.silk_to_edge_nm),
    ] {
        if val.is_some_and(|v| v < 0) {
            return Err(format!("design rules {name} must be >= 0"));
        }
    }
    Ok(())
}

pub fn validate_board_ir_pins(ir: &BoardIr, path: &Path) -> Result<(), String> {
    validate_optional_rule_clearances(&ir.rules)
        .map_err(|e| format!("board IR '{}': {e}", path.display()))?;
    validate_net_names(&ir.nets).map_err(|e| format!("board IR '{}': {e}", path.display()))?;
    ir.fab
        .validate()
        .map_err(|e| format!("board IR '{}': {e}", path.display()))?;
    validate_graphics("board", &ir.graphics)
        .map_err(|e| format!("board IR '{}': {e}", path.display()))?;
    for (name, fp) in &ir.footprints {
        let owner = format!("footprint '{name}'");
        if !fp.courtyard.is_empty() {
            validate_courtyard(&owner, &fp.courtyard)
                .map_err(|e| format!("board IR '{}': {e}", path.display()))?;
        }
        validate_models(&owner, &fp.models)
            .map_err(|e| format!("board IR '{}': {e}", path.display()))?;
        validate_pin_types_for_footprint("footprint", name, fp, &fp.pin_types)
            .map_err(|e| format!("board IR '{}': {e}", path.display()))?;
        validate_graphics(&format!("footprint '{name}'"), &fp.graphics)
            .map_err(|e| format!("board IR '{}': {e}", path.display()))?;
    }
    for p in &ir.placements {
        if p.footprint.trim().is_empty() {
            return Err(format!(
                "board IR '{}' placement '{}' has empty footprint",
                path.display(),
                p.refdes
            ));
        }
        if !p.pin_types.is_empty()
            && let Some(fp) = ir.footprints.get(&p.footprint)
        {
            validate_pin_types_for_footprint("placement", &p.refdes, fp, &p.pin_types)
                .map_err(|e| format!("board IR '{}': {e}", path.display()))?;
        }
    }
    Ok(())
}
