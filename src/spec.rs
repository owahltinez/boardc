use std::collections::{BTreeMap, HashMap, HashSet};
use std::fs;
use std::path::Path;

use serde::{Deserialize, Serialize};

use crate::footprints;
use crate::ir::{
    BoardIr, Constraints, DesignRules, FabConfig, FootprintDef, Graphic, Keepout, LayerDef,
    Model3d, NetClass, NetDef, Nm, PadDef, PinType, Placement, PourSpec, Pt, Stackup, Track, Via,
    default_two_layer_stackup, trim_nonempty,
};
use crate::kicad_io::load_kicad_net;
use crate::spec_expand::expand_subcircuits;
pub use crate::spec_expand::{SubcircuitInstance, SubcircuitSpec};
use crate::spec_kicad::{
    apply_netlist_components, apply_netlist_pin_types, bare_footprint_name,
    resolve_footprint_from_libs, resolve_footprint_libs, resolve_spec_path,
    sync_spec_and_netlist_nets,
};
use crate::spec_serde::{deserialize_footprints, deserialize_nets, deserialize_pin_names};
pub use crate::spec_validate::validate_board_ir_pins;
use crate::spec_validate::{
    backfill_net_names, check_subcircuit_pin_net_conflicts, finalize_footprint_pins,
    lower_nets_pin_aliases, lower_pin_types, lower_placement_pin_nets, validate_courtyard,
    validate_graphics, validate_models, validate_optional_rule_clearances,
    validate_pin_types_for_footprint,
};

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum FootprintSpec {
    Chip2Pin {
        package: String,
        #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
        pin_types: BTreeMap<String, PinType>,
        #[serde(
            default,
            deserialize_with = "deserialize_pin_names",
            skip_serializing_if = "BTreeMap::is_empty"
        )]
        pin_names: BTreeMap<String, Vec<String>>,
    },
    Bga {
        rows: usize,
        cols: usize,
        pitch_nm: Nm,
        ball_dia_nm: Nm,
        #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
        pin_types: BTreeMap<String, PinType>,
        #[serde(
            default,
            deserialize_with = "deserialize_pin_names",
            skip_serializing_if = "BTreeMap::is_empty"
        )]
        pin_names: BTreeMap<String, Vec<String>>,
    },
    Qfn {
        pins_per_side: usize,
        pitch_nm: Nm,
        body_nm: Nm,
        #[serde(default)]
        ep_nm: Nm,
        #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
        pin_types: BTreeMap<String, PinType>,
        #[serde(
            default,
            deserialize_with = "deserialize_pin_names",
            skip_serializing_if = "BTreeMap::is_empty"
        )]
        pin_names: BTreeMap<String, Vec<String>>,
    },
    Sot23 {
        pin_count: usize,
        #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
        pin_types: BTreeMap<String, PinType>,
        #[serde(
            default,
            deserialize_with = "deserialize_pin_names",
            skip_serializing_if = "BTreeMap::is_empty"
        )]
        pin_names: BTreeMap<String, Vec<String>>,
    },
    PinHeader {
        pins: usize,
        pitch_nm: Nm,
        #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
        pin_types: BTreeMap<String, PinType>,
        #[serde(
            default,
            deserialize_with = "deserialize_pin_names",
            skip_serializing_if = "BTreeMap::is_empty"
        )]
        pin_names: BTreeMap<String, Vec<String>>,
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
        #[serde(
            default,
            deserialize_with = "deserialize_pin_names",
            skip_serializing_if = "BTreeMap::is_empty"
        )]
        pin_names: BTreeMap<String, Vec<String>>,
    },
    /// Pin-type, pin-alias, and/or courtyard overlay for a `.kicad_mod` footprint loaded from `footprint_libs`.
    Lib {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        courtyard: Option<Vec<Pt>>,
        #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
        pin_types: BTreeMap<String, PinType>,
        #[serde(
            default,
            deserialize_with = "deserialize_pin_names",
            skip_serializing_if = "BTreeMap::is_empty"
        )]
        pin_names: BTreeMap<String, Vec<String>>,
    },
}

impl FootprintSpec {
    pub fn pin_names(&self) -> &BTreeMap<String, Vec<String>> {
        match self {
            Self::Chip2Pin { pin_names, .. }
            | Self::Bga { pin_names, .. }
            | Self::Qfn { pin_names, .. }
            | Self::Sot23 { pin_names, .. }
            | Self::PinHeader { pin_names, .. }
            | Self::Custom { pin_names, .. }
            | Self::Lib { pin_names, .. } => pin_names,
        }
    }

    pub fn resolve(&self, name: &str) -> Result<FootprintDef, String> {
        let (mut fp, pin_types) = match self {
            Self::Chip2Pin {
                package, pin_types, ..
            } => (footprints::chip_2pin(package)?, pin_types),
            Self::Bga {
                rows,
                cols,
                pitch_nm,
                ball_dia_nm,
                pin_types,
                ..
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
                ..
            } => (
                footprints::qfn(*pins_per_side, *pitch_nm, *body_nm, *ep_nm)?,
                pin_types,
            ),
            Self::Sot23 {
                pin_count,
                pin_types,
                ..
            } => (footprints::sot23(*pin_count)?, pin_types),
            Self::PinHeader {
                pins,
                pitch_nm,
                pin_types,
                ..
            } => (footprints::pin_header(*pins, *pitch_nm)?, pin_types),
            Self::Custom {
                pads,
                courtyard,
                graphics,
                keepouts,
                models,
                pin_types,
                ..
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
                        kicad_raw: None,
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
        finalize_footprint_pins(name, &mut fp, self.pin_names())?;
        Ok(fp)
    }
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
    #[serde(default, deserialize_with = "deserialize_nets")]
    pub nets: BTreeMap<String, NetDef>,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub subcircuits: BTreeMap<String, SubcircuitSpec>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub instances: Vec<SubcircuitInstance>,
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

        let expanded = expand_subcircuits(spec)?;
        let resolved_libs = resolve_footprint_libs(&spec.footprint_libs, base_dir)?;
        let netlist = match spec.netlist.as_deref() {
            Some(raw_net) => Some(load_kicad_net(&resolve_spec_path(
                raw_net, base_dir, "netlist",
            )?)?),
            None => None,
        };

        let mut footprints = BTreeMap::new();
        let mut fp_pin_names: HashMap<String, &BTreeMap<String, Vec<String>>> = HashMap::new();
        for (name, fp_spec) in &spec.footprints {
            if !matches!(fp_spec, FootprintSpec::Lib { .. }) {
                let fp = fp_spec.resolve(name)?;
                if !fp_spec.pin_names().is_empty() {
                    fp_pin_names.insert(name.clone(), fp_spec.pin_names());
                }
                footprints.insert(name.clone(), fp);
            }
        }
        let mut matched_overlays: HashSet<&str> = HashSet::new();
        let mut placements = expanded.placements;
        let mut nets = expanded.nets;

        if let Some(ref nl) = netlist {
            apply_netlist_components(&mut placements, nl)?;
        }

        for p in &mut placements {
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
                    if let Some((key, courtyard, pin_types, pin_names)) = [
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
                                pin_names,
                            },
                        )) => Some((key.as_str(), courtyard, pin_types, pin_names)),
                        _ => None,
                    }) {
                        matched_overlays.insert(key);
                        if let Some(cy) = courtyard {
                            fp.courtyard = cy.clone();
                        }
                        fp.pin_types = pin_types.clone();
                        finalize_footprint_pins(key, &mut fp, pin_names)?;
                        if !pin_names.is_empty() {
                            fp_pin_names.insert(p.footprint.clone(), pin_names);
                        }
                    }
                    footprints.insert(p.footprint.clone(), fp);
                } else {
                    return Err(format!(
                        "placement '{}' references unknown footprint '{}'",
                        p.refdes, p.footprint
                    ));
                }
            }
            let fp = &footprints[&p.footprint];
            if let Some(pin_names) = fp_pin_names.get(&p.footprint) {
                lower_pin_types(
                    "placement",
                    &p.refdes,
                    &fp.name,
                    &mut p.pin_types,
                    pin_names,
                )?;
                lower_placement_pin_nets(&p.refdes, &mut p.pin_nets, pin_names)?;
            }
            if !p.pin_types.is_empty() {
                validate_pin_types_for_footprint("placement", &p.refdes, fp, &p.pin_types)?;
            }
            if let Some(comp) = netlist.as_ref().and_then(|nl| nl.components.get(&p.refdes))
                && !comp.pin_types.is_empty()
            {
                apply_netlist_pin_types(p, fp, &comp.pin_types);
            }
        }
        for (name, fp_spec) in &spec.footprints {
            if let FootprintSpec::Lib {
                courtyard,
                pin_types,
                pin_names,
            } = fp_spec
            {
                if courtyard.is_none() && pin_types.is_empty() && pin_names.is_empty() {
                    return Err(format!(
                        "footprint '{name}' library overlay must specify courtyard, pin_types, and/or pin_names"
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

        check_subcircuit_pin_net_conflicts(&placements, &fp_pin_names, &expanded.sub_net_pins)?;
        lower_nets_pin_aliases(&placements, &fp_pin_names, &mut nets)?;
        sync_spec_and_netlist_nets(&mut placements, &mut nets, netlist.as_ref());
        if spec.rules.track_width_nm <= 0
            || spec.rules.via_dia_nm <= 0
            || spec.rules.via_drill_nm <= 0
        {
            return Err(
                "design rules track_width_nm, via_dia_nm, and via_drill_nm must be > 0".into(),
            );
        }
        validate_optional_rule_clearances(&spec.rules)?;
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
            constraints: expanded.constraints,
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
