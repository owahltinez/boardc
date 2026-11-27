use std::collections::{BTreeMap, HashSet};
use std::fs;
use std::path::Path;

use serde::{Deserialize, Serialize};

use crate::footprints;
use crate::geom::polygon_signed_area_2;
use crate::ir::{
    BoardIr, Constraints, DesignRules, FabConfig, FootprintDef, Graphic, Keepout, LayerDef,
    Model3d, NetClass, NetDef, Nm, PadDef, PadKind, PinType, Placement, PourSpec, Pt, Stackup,
    Track, Via, default_two_layer_stackup, trim_nonempty,
};
use crate::kicad_io::load_kicad_net;
use crate::spec_kicad::{
    apply_netlist_components, bare_footprint_name, resolve_footprint_from_libs,
    resolve_footprint_libs, resolve_spec_path, sync_spec_and_netlist_nets,
};

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum FootprintSpec {
    Chip2Pin {
        package: String,
        #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
        pin_types: BTreeMap<String, PinType>,
    },
    Bga {
        rows: usize,
        cols: usize,
        pitch_nm: Nm,
        ball_dia_nm: Nm,
        #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
        pin_types: BTreeMap<String, PinType>,
    },
    Qfn {
        pins_per_side: usize,
        pitch_nm: Nm,
        body_nm: Nm,
        #[serde(default)]
        ep_nm: Nm,
        #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
        pin_types: BTreeMap<String, PinType>,
    },
    Sot23 {
        pin_count: usize,
        #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
        pin_types: BTreeMap<String, PinType>,
    },
    PinHeader {
        pins: usize,
        pitch_nm: Nm,
        #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
        pin_types: BTreeMap<String, PinType>,
    },
    Custom {
        pads: Vec<PadDef>,
        #[serde(default)]
        courtyard: Vec<Pt>,
        #[serde(default)]
        graphics: Vec<Graphic>,
        #[serde(default)]
        keepouts: Vec<Keepout>,
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        models: Vec<Model3d>,
        #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
        pin_types: BTreeMap<String, PinType>,
    },
    /// Pin-type and/or courtyard overlay for a `.kicad_mod` footprint loaded from `footprint_libs`.
    Lib {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        courtyard: Option<Vec<Pt>>,
        #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
        pin_types: BTreeMap<String, PinType>,
    },
}

fn deserialize_footprints<'de, D: serde::Deserializer<'de>>(
    de: D,
) -> Result<BTreeMap<String, FootprintSpec>, D::Error> {
    let raw = BTreeMap::<String, serde_json::Value>::deserialize(de)?;
    let mut out = BTreeMap::new();
    for (name, mut val) in raw {
        if let Some(map) = val.as_object_mut()
            && !map.contains_key("kind")
            && !map.is_empty()
            && map.keys().all(|k| k == "pin_types" || k == "courtyard")
        {
            map.insert("kind".into(), serde_json::Value::String("lib".into()));
        }
        let fp: FootprintSpec = serde_json::from_value(val)
            .map_err(|e| serde::de::Error::custom(format!("footprint '{name}': {e}")))?;
        out.insert(name, fp);
    }
    Ok(out)
}

fn validate_courtyard(owner: &str, pts: &[Pt]) -> Result<(), String> {
    if pts.len() < 3 {
        return Err(format!("{owner} courtyard must have at least 3 vertices"));
    }
    if polygon_signed_area_2(pts) == 0 {
        return Err(format!("{owner} courtyard polygon must have non-zero area"));
    }
    Ok(())
}

fn validate_models(owner: &str, models: &[Model3d]) -> Result<(), String> {
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

impl FootprintSpec {
    pub fn resolve(&self, name: &str) -> Result<FootprintDef, String> {
        let (mut fp, pin_types) = match self {
            Self::Chip2Pin { package, pin_types } => (footprints::chip_2pin(package)?, pin_types),
            Self::Bga {
                rows,
                cols,
                pitch_nm,
                ball_dia_nm,
                pin_types,
            } => (
                footprints::bga(*rows, *cols, *pitch_nm, *ball_dia_nm)?,
                pin_types,
            ),
            Self::Qfn {
                pins_per_side,
                pitch_nm,
                body_nm,
                ep_nm,
                pin_types,
            } => (
                footprints::qfn(*pins_per_side, *pitch_nm, *body_nm, *ep_nm)?,
                pin_types,
            ),
            Self::Sot23 {
                pin_count,
                pin_types,
            } => (footprints::sot23(*pin_count)?, pin_types),
            Self::PinHeader {
                pins,
                pitch_nm,
                pin_types,
            } => (footprints::pin_header(*pins, *pitch_nm)?, pin_types),
            Self::Custom {
                pads,
                courtyard,
                graphics,
                keepouts,
                models,
                pin_types,
            } => {
                let owner = format!("footprint '{name}'");
                if !courtyard.is_empty() {
                    validate_courtyard(&owner, courtyard)?;
                }
                validate_models(&owner, models)?;
                (
                    FootprintDef {
                        name: String::new(),
                        pads: pads.clone(),
                        courtyard: courtyard.clone(),
                        graphics: graphics.clone(),
                        keepouts: keepouts.clone(),
                        models: models.clone(),
                        pin_types: BTreeMap::new(),
                    },
                    pin_types,
                )
            }
            Self::Lib { .. } => {
                return Err(format!(
                    "footprint '{name}' is a library overlay and requires footprint_libs"
                ));
            }
        };
        fp.name = name.to_string();
        fp.pin_types = pin_types.clone();
        Ok(fp)
    }
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
    let known: HashSet<&str> = fp
        .pads
        .iter()
        .filter(|p| p.kind != PadKind::Npth && !p.name.is_empty())
        .map(|p| p.name.as_str())
        .collect();
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

fn validate_graphics(owner: &str, graphics: &[Graphic]) -> Result<(), String> {
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

pub fn validate_board_ir_pins(ir: &BoardIr, path: &Path) -> Result<(), String> {
    if ir.rules.mask_to_copper_nm.is_some_and(|v| v < 0) {
        return Err(format!(
            "board IR '{}': design rules mask_to_copper_nm must be >= 0",
            path.display()
        ));
    }
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

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct BoardSpec {
    pub name: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub title: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub revision: Option<String>,
    #[serde(default)]
    pub outline: Vec<Pt>,
    #[serde(default)]
    pub size_nm: Option<[Nm; 2]>,
    #[serde(default)]
    pub cutouts: Vec<Vec<Pt>>,
    #[serde(default)]
    pub layers: Vec<LayerDef>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub footprint_libs: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub netlist: Option<String>,
    #[serde(default, deserialize_with = "deserialize_footprints")]
    pub footprints: BTreeMap<String, FootprintSpec>,
    #[serde(default)]
    pub placements: Vec<Placement>,
    #[serde(default)]
    pub keepouts: Vec<Keepout>,
    #[serde(default)]
    pub pours: Vec<PourSpec>,
    #[serde(default)]
    pub tracks: Vec<Track>,
    #[serde(default)]
    pub vias: Vec<Via>,
    #[serde(default)]
    pub net_classes: BTreeMap<String, NetClass>,
    #[serde(default)]
    pub nets: BTreeMap<String, NetDef>,
    #[serde(default)]
    pub rules: DesignRules,
    #[serde(default)]
    pub constraints: Constraints,
    #[serde(default)]
    pub graphics: Vec<Graphic>,
    #[serde(default)]
    pub stackup: Stackup,
    #[serde(default, skip_serializing_if = "FabConfig::is_default")]
    pub fab: FabConfig,
}

impl BoardIr {
    pub fn from_spec(spec: &BoardSpec) -> Result<Self, String> {
        Self::from_spec_with_base(spec, None)
    }

    /// Resolves `BoardSpec` into `BoardIr`, resolving relative `footprint_libs` and `netlist` paths against `base_dir`.
    pub fn from_spec_with_base(spec: &BoardSpec, base_dir: Option<&Path>) -> Result<Self, String> {
        let outline = if !spec.outline.is_empty() {
            spec.outline.clone()
        } else if let Some([w, h]) = spec.size_nm {
            if w <= 0 || h <= 0 {
                return Err("size_nm dimensions must be > 0".into());
            }
            vec![Pt::new(0, 0), Pt::new(w, 0), Pt::new(w, h), Pt::new(0, h)]
        } else {
            return Err("BoardSpec requires either non-empty outline or size_nm".into());
        };
        if outline.len() < 3 {
            return Err("board outline must have at least 3 vertices".into());
        }
        let layers = if spec.layers.is_empty() {
            default_two_layer_stackup()
        } else {
            spec.layers.clone()
        };

        let resolved_libs = resolve_footprint_libs(&spec.footprint_libs, base_dir)?;
        let netlist = match spec.netlist.as_deref() {
            Some(raw_net) => Some(load_kicad_net(&resolve_spec_path(
                raw_net, base_dir, "netlist",
            )?)?),
            None => None,
        };

        let mut footprints = BTreeMap::new();
        for (name, fp_spec) in &spec.footprints {
            if !matches!(fp_spec, FootprintSpec::Lib { .. }) {
                let fp = fp_spec.resolve(name)?;
                validate_pin_types_for_footprint("footprint", name, &fp, &fp.pin_types)?;
                footprints.insert(name.clone(), fp);
            }
        }
        let mut matched_overlays: HashSet<&str> = HashSet::new();
        let mut placements = spec.placements.clone();
        let mut seen_refs = HashSet::with_capacity(placements.len());
        for p in &placements {
            let r = p.refdes.trim();
            if !r.is_empty() && !seen_refs.insert(r) {
                return Err(format!("duplicate placement refdes '{}'", p.refdes));
            }
        }
        validate_net_names(&spec.nets)?;
        let mut nets = spec.nets.clone();

        if let Some(ref nl) = netlist {
            apply_netlist_components(&mut placements, nl)?;
        }

        for p in &placements {
            if p.footprint.trim().is_empty() {
                return Err(format!("placement '{}' has no footprint", p.refdes));
            }
            if !footprints.contains_key(&p.footprint) {
                let qual_fallback = p
                    .schematic
                    .as_ref()
                    .and_then(|s| s.footprint.as_deref())
                    .filter(|sf| bare_footprint_name(sf) == p.footprint);
                if let Some(mut fp) =
                    resolve_footprint_from_libs(&p.footprint, qual_fallback, &resolved_libs)?
                {
                    if let Some((key, courtyard, pin_types)) = [
                        Some(p.footprint.as_str()),
                        qual_fallback,
                        Some(bare_footprint_name(&p.footprint)),
                    ]
                    .into_iter()
                    .flatten()
                    .find_map(|k| match spec.footprints.get_key_value(k) {
                        Some((
                            key,
                            FootprintSpec::Lib {
                                courtyard,
                                pin_types,
                            },
                        )) => Some((key.as_str(), courtyard, pin_types)),
                        _ => None,
                    }) {
                        matched_overlays.insert(key);
                        if let Some(cy) = courtyard {
                            fp.courtyard = cy.clone();
                        }
                        fp.pin_types = pin_types.clone();
                        validate_pin_types_for_footprint(
                            "footprint",
                            &p.footprint,
                            &fp,
                            &fp.pin_types,
                        )?;
                    }
                    footprints.insert(p.footprint.clone(), fp);
                } else {
                    return Err(format!(
                        "placement '{}' references unknown footprint '{}'",
                        p.refdes, p.footprint
                    ));
                }
            }
            if !p.pin_types.is_empty() {
                let fp = &footprints[&p.footprint];
                validate_pin_types_for_footprint("placement", &p.refdes, fp, &p.pin_types)?;
            }
        }
        for (name, fp_spec) in &spec.footprints {
            if let FootprintSpec::Lib {
                courtyard,
                pin_types,
            } = fp_spec
            {
                if courtyard.is_none() && pin_types.is_empty() {
                    return Err(format!(
                        "footprint '{name}' library overlay must specify courtyard and/or pin_types"
                    ));
                }
                if let Some(cy) = courtyard {
                    validate_courtyard(&format!("footprint '{name}'"), cy)?;
                }
                if !matched_overlays.contains(name.as_str()) {
                    return Err(format!(
                        "footprint '{name}' library overlay did not match any library footprint placement"
                    ));
                }
            }
        }

        for fp in footprints.values() {
            validate_graphics(&format!("footprint '{}'", fp.name), &fp.graphics)?;
        }
        validate_graphics("board", &spec.graphics)?;

        sync_spec_and_netlist_nets(&mut placements, &mut nets, netlist.as_ref());
        if spec.rules.track_width_nm <= 0
            || spec.rules.via_dia_nm <= 0
            || spec.rules.via_drill_nm <= 0
        {
            return Err(
                "design rules track_width_nm, via_dia_nm, and via_drill_nm must be > 0".into(),
            );
        }
        if spec.rules.mask_to_copper_nm.is_some_and(|v| v < 0) {
            return Err("design rules mask_to_copper_nm must be >= 0".into());
        }
        if spec.stackup.thickness_nm <= 0 {
            return Err("stackup.thickness_nm must be > 0".into());
        }
        if spec.stackup.dielectric_nm.iter().any(|&d| d <= 0) {
            return Err("stackup.dielectric_nm entries must be > 0".into());
        }
        let expected_dielectrics = layers.len().saturating_sub(1).max(1);
        if !spec.stackup.dielectric_nm.is_empty()
            && spec.stackup.dielectric_nm.len() != expected_dielectrics
        {
            return Err(format!(
                "stackup.dielectric_nm length ({}) must equal {expected_dielectrics}",
                spec.stackup.dielectric_nm.len()
            ));
        }
        spec.fab.validate()?;
        let mut rules = spec.rules.clone();
        let mut stackup = spec.stackup.clone();
        // Keep stackup POFV and via-in-pad DRC/routing flags consistent across the pipeline and fab exports.
        if stackup.pofv || rules.via_at_smd {
            rules.via_at_smd = true;
            stackup.pofv = true;
        }
        Ok(Self {
            name: spec.name.clone(),
            title: trim_nonempty(spec.title.as_deref()).map(str::to_string),
            revision: trim_nonempty(spec.revision.as_deref()).map(str::to_string),
            outline,
            cutouts: spec.cutouts.clone(),
            layers,
            footprints,
            placements,
            keepouts: spec.keepouts.clone(),
            pours: spec.pours.clone(),
            filled_pours: Vec::new(),
            tracks: spec.tracks.clone(),
            vias: spec.vias.clone(),
            net_classes: spec.net_classes.clone(),
            nets,
            rules,
            constraints: spec.constraints.clone(),
            graphics: spec.graphics.clone(),
            stackup,
            fab: spec.fab,
        })
    }
}

pub fn load_spec_file(path: &Path) -> Result<BoardSpec, String> {
    let text = fs::read_to_string(path)
        .map_err(|e| format!("failed to read spec '{}': {e}", path.display()))?;
    let ext = path
        .extension()
        .and_then(|s| s.to_str())
        .map(str::to_ascii_lowercase);
    match ext.as_deref() {
        Some("toml") => toml::from_str(&text)
            .map_err(|e| format!("failed to parse TOML spec '{}': {e}", path.display())),
        Some("json") => serde_json::from_str(&text)
            .map_err(|e| format!("failed to parse JSON spec '{}': {e}", path.display())),
        _ => serde_json::from_str(&text)
            .or_else(|_| toml::from_str(&text))
            .map_err(|e| format!("failed to parse spec '{}': {e}", path.display())),
    }
}

pub fn save_board_json(board: &BoardIr, path: &Path) -> Result<(), String> {
    let json = serde_json::to_string_pretty(board)
        .map_err(|e| format!("failed to serialize board IR: {e}"))?;
    fs::write(path, json).map_err(|e| format!("failed to write '{}': {e}", path.display()))
}

pub fn load_board_json(path: &Path) -> Result<BoardIr, String> {
    let text = fs::read_to_string(path)
        .map_err(|e| format!("failed to read board IR '{}': {e}", path.display()))?;
    let mut ir: BoardIr = serde_json::from_str(&text)
        .map_err(|e| format!("failed to parse board IR '{}': {e}", path.display()))?;
    validate_board_ir_pins(&ir, path)?;
    backfill_net_names(&mut ir.nets);
    Ok(ir)
}
