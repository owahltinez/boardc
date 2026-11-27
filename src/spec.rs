use std::collections::{BTreeMap, HashSet};
use std::fs;
use std::path::Path;

use serde::{Deserialize, Serialize};

use crate::footprints;
use crate::ir::{
    BoardIr, Constraints, DesignRules, FootprintDef, Graphic, Keepout, LayerDef, NetClass, NetDef,
    Nm, PadDef, Placement, PourSpec, Pt, Stackup, Track, Via, default_two_layer_stackup,
};
use crate::kicad_io::load_kicad_net;
use crate::spec_kicad::{
    apply_netlist_components, bare_footprint_name, resolve_footprint_from_libs,
    resolve_footprint_libs, resolve_spec_path, sync_spec_and_netlist_nets,
};

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum FootprintSpec {
    Chip2Pin {
        package: String,
    },
    Bga {
        rows: usize,
        cols: usize,
        pitch_nm: Nm,
        ball_dia_nm: Nm,
    },
    Qfn {
        pins_per_side: usize,
        pitch_nm: Nm,
        body_nm: Nm,
        #[serde(default)]
        ep_nm: Nm,
    },
    Sot23 {
        pin_count: usize,
    },
    PinHeader {
        pins: usize,
        pitch_nm: Nm,
    },
    Custom {
        pads: Vec<PadDef>,
        #[serde(default)]
        courtyard: Vec<Pt>,
        #[serde(default)]
        graphics: Vec<Graphic>,
        #[serde(default)]
        keepouts: Vec<Keepout>,
    },
}

impl FootprintSpec {
    pub fn resolve(&self, name: &str) -> Result<FootprintDef, String> {
        let mut fp = match self {
            Self::Chip2Pin { package } => footprints::chip_2pin(package)?,
            Self::Bga {
                rows,
                cols,
                pitch_nm,
                ball_dia_nm,
            } => footprints::bga(*rows, *cols, *pitch_nm, *ball_dia_nm)?,
            Self::Qfn {
                pins_per_side,
                pitch_nm,
                body_nm,
                ep_nm,
            } => footprints::qfn(*pins_per_side, *pitch_nm, *body_nm, *ep_nm)?,
            Self::Sot23 { pin_count } => footprints::sot23(*pin_count)?,
            Self::PinHeader { pins, pitch_nm } => footprints::pin_header(*pins, *pitch_nm)?,
            Self::Custom {
                pads,
                courtyard,
                graphics,
                keepouts,
            } => FootprintDef {
                name: name.to_string(),
                pads: pads.clone(),
                courtyard: courtyard.clone(),
                graphics: graphics.clone(),
                keepouts: keepouts.clone(),
                models: Vec::new(),
            },
        };
        fp.name = name.to_string();
        Ok(fp)
    }
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct BoardSpec {
    pub name: String,
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
    #[serde(default)]
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
            footprints.insert(name.clone(), fp_spec.resolve(name)?);
        }
        let mut placements = spec.placements.clone();
        let mut seen_refs = HashSet::with_capacity(placements.len());
        for p in &placements {
            let r = p.refdes.trim();
            if !r.is_empty() && !seen_refs.insert(r) {
                return Err(format!("duplicate placement refdes '{}'", p.refdes));
            }
        }
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
                if let Some(fp) =
                    resolve_footprint_from_libs(&p.footprint, qual_fallback, &resolved_libs)?
                {
                    footprints.insert(p.footprint.clone(), fp);
                } else {
                    return Err(format!(
                        "placement '{}' references unknown footprint '{}'",
                        p.refdes, p.footprint
                    ));
                }
            }
        }

        sync_spec_and_netlist_nets(&mut placements, &mut nets, netlist.as_ref());
        if spec.rules.track_width_nm <= 0
            || spec.rules.via_dia_nm <= 0
            || spec.rules.via_drill_nm <= 0
        {
            return Err(
                "design rules track_width_nm, via_dia_nm, and via_drill_nm must be > 0".into(),
            );
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
        let mut rules = spec.rules.clone();
        let mut stackup = spec.stackup.clone();
        // Keep stackup POFV and via-in-pad DRC/routing flags consistent across the pipeline and fab exports.
        if stackup.pofv || rules.via_at_smd {
            rules.via_at_smd = true;
            stackup.pofv = true;
        }
        Ok(Self {
            name: spec.name.clone(),
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
    let ir: BoardIr = serde_json::from_str(&text)
        .map_err(|e| format!("failed to parse board IR '{}': {e}", path.display()))?;
    for p in &ir.placements {
        if p.footprint.trim().is_empty() {
            return Err(format!(
                "board IR '{}' placement '{}' has empty footprint",
                path.display(),
                p.refdes
            ));
        }
    }
    Ok(ir)
}
