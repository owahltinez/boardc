use std::collections::BTreeMap;
use std::fs;
use std::path::Path;

use serde::{Deserialize, Serialize};

use crate::footprints;
use crate::ir::{
    BoardIr, Constraints, DesignRules, FootprintDef, Graphic, Keepout, LayerDef, NetClass, NetDef,
    Nm, PadDef, Placement, PourSpec, Pt, Track, Via, default_net_class, default_two_layer_stackup,
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
}

impl BoardIr {
    pub fn from_spec(spec: &BoardSpec) -> Result<Self, String> {
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
        let mut footprints = BTreeMap::new();
        for (name, fp_spec) in &spec.footprints {
            footprints.insert(name.clone(), fp_spec.resolve(name)?);
        }
        let mut placements = spec.placements.clone();
        let mut nets = spec.nets.clone();
        for p in &placements {
            if !footprints.contains_key(&p.footprint) {
                return Err(format!(
                    "placement '{}' references unknown footprint '{}'",
                    p.refdes, p.footprint
                ));
            }
        }
        // Keep Placement::pin_nets and NetDef::pins mutually consistent.
        for p in &placements {
            for (pin, net_name) in &p.pin_nets {
                let entry = nets.entry(net_name.clone()).or_insert_with(|| NetDef {
                    name: net_name.clone(),
                    class: default_net_class(),
                    pins: Vec::new(),
                    ignore_route: false,
                });
                let pair = (p.refdes.clone(), pin.clone());
                if !entry.pins.contains(&pair) {
                    entry.pins.push(pair);
                }
            }
        }
        for (net_name, net_def) in &nets {
            for (refdes, pin) in &net_def.pins {
                if let Some(p) = placements.iter_mut().find(|pl| &pl.refdes == refdes) {
                    p.pin_nets
                        .entry(pin.clone())
                        .or_insert_with(|| net_name.clone());
                }
            }
        }
        if spec.rules.track_width_nm <= 0
            || spec.rules.via_dia_nm <= 0
            || spec.rules.via_drill_nm <= 0
        {
            return Err(
                "design rules track_width_nm, via_dia_nm, and via_drill_nm must be > 0".into(),
            );
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
            rules: spec.rules.clone(),
            constraints: spec.constraints.clone(),
            graphics: spec.graphics.clone(),
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
    serde_json::from_str(&text)
        .map_err(|e| format!("failed to parse board IR '{}': {e}", path.display()))
}
