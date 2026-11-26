use std::collections::{BTreeMap, HashMap};
use std::fs;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::footprints;
use crate::ir::{
    BoardIr, Constraints, DesignRules, FootprintDef, Graphic, Keepout, LayerDef, NetClass, NetDef,
    Nm, PadDef, Placement, PourSpec, Pt, Stackup, Track, Via, default_net_class,
    default_two_layer_stackup,
};
use crate::kicad_io::{load_kicad_mod, load_kicad_net};

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

fn resolve_spec_path(raw: &str, base_dir: Option<&Path>) -> PathBuf {
    let p = Path::new(raw);
    if p.is_absolute() {
        return p.to_path_buf();
    }
    if let Some(base) = base_dir {
        let joined = base.join(p);
        if joined.exists() || !p.exists() {
            return joined;
        }
    }
    p.to_path_buf()
}

fn is_safe_path_segment(seg: &str) -> bool {
    !seg.is_empty()
        && seg != "."
        && seg != ".."
        && !seg.contains('/')
        && !seg.contains('\\')
        && !seg.contains('\0')
}

fn bare_footprint_name(name: &str) -> &str {
    name.split_once(':').map_or(name, |(_, fp)| fp)
}

fn resolve_footprint_from_libs(
    name: &str,
    libs: &[PathBuf],
) -> Result<Option<FootprintDef>, String> {
    if libs.is_empty() {
        return Ok(None);
    }
    let (lib_opt, bare) = match name.split_once(':') {
        Some((lib, fp)) => {
            if !is_safe_path_segment(lib) || !is_safe_path_segment(fp) {
                return Err(format!("invalid footprint name '{name}'"));
            }
            (Some(lib), fp)
        }
        None => {
            if !is_safe_path_segment(name) {
                return Err(format!("invalid footprint name '{name}'"));
            }
            (None, name)
        }
    };
    let file_name = format!("{bare}.kicad_mod");

    // Pass 1: library-qualified match (<Lib>.pretty/<Fp>.kicad_mod or dir/<Lib>.pretty/<Fp>.kicad_mod).
    if let Some(lib) = lib_opt {
        let pretty = format!("{lib}.pretty");
        for dir in libs {
            if !dir.is_dir() {
                continue;
            }
            if dir.file_stem().and_then(|s| s.to_str()) == Some(lib) {
                let candidate = dir.join(&file_name);
                if candidate.is_file() {
                    let mut fp = load_kicad_mod(&candidate)?;
                    fp.name = name.to_string();
                    return Ok(Some(fp));
                }
            }
            let nested = dir.join(&pretty).join(&file_name);
            if nested.is_file() {
                let mut fp = load_kicad_mod(&nested)?;
                fp.name = name.to_string();
                return Ok(Some(fp));
            }
        }
    }

    // Pass 2: bare <Fp>.kicad_mod inside any listed directory, or a directly listed .kicad_mod file.
    for entry in libs {
        if entry.is_file() {
            let is_mod = entry
                .extension()
                .and_then(|s| s.to_str())
                .is_some_and(|e| e.eq_ignore_ascii_case("kicad_mod"));
            if is_mod && entry.file_stem().and_then(|s| s.to_str()) == Some(bare) {
                let mut fp = load_kicad_mod(entry)?;
                fp.name = name.to_string();
                return Ok(Some(fp));
            }
        } else if entry.is_dir() {
            let candidate = entry.join(&file_name);
            if candidate.is_file() {
                let mut fp = load_kicad_mod(&candidate)?;
                fp.name = name.to_string();
                return Ok(Some(fp));
            }
        }
    }
    Ok(None)
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

        let mut resolved_libs = Vec::with_capacity(spec.footprint_libs.len());
        for raw_lib in &spec.footprint_libs {
            let path = resolve_spec_path(raw_lib, base_dir);
            if !path.exists() {
                return Err(format!("footprint library '{}' not found", path.display()));
            }
            resolved_libs.push(path);
        }

        let netlist = match spec.netlist.as_deref() {
            Some(raw_net) => Some(load_kicad_net(&resolve_spec_path(raw_net, base_dir))?),
            None => None,
        };

        let mut footprints = BTreeMap::new();
        for (name, fp_spec) in &spec.footprints {
            footprints.insert(name.clone(), fp_spec.resolve(name)?);
        }
        let mut placements = spec.placements.clone();
        let mut nets = spec.nets.clone();

        if let Some(ref nl) = netlist {
            for p in &mut placements {
                let Some(comp) = nl.components.get(&p.refdes) else {
                    continue;
                };
                let spec_fp_empty = p.footprint.is_empty();
                if spec_fp_empty && !comp.footprint.is_empty() {
                    p.footprint = comp.footprint.clone();
                }
                if p.value.is_empty() && !comp.value.is_empty() {
                    p.value = comp.value.clone();
                }
                if p.mpn.is_none() {
                    p.mpn = comp.mpn.clone();
                }
                if p.lcsc.is_none() {
                    p.lcsc = comp.lcsc.clone();
                }
                p.dnp |= comp.dnp;
                p.exclude_from_bom |= comp.exclude_from_bom;

                let mut sch = p.schematic.clone().unwrap_or_default();
                if sch.path.is_none() {
                    sch.path = comp.path.clone();
                }
                if sch.sheetname.is_none() {
                    sch.sheetname = comp.sheetname.clone();
                }
                if sch.sheetfile.is_none() {
                    sch.sheetfile = comp.sheetfile.clone();
                }
                if sch.footprint.is_none()
                    && !comp.footprint.is_empty()
                    && (spec_fp_empty
                        || p.footprint == comp.footprint
                        || bare_footprint_name(&comp.footprint) == p.footprint)
                {
                    sch.footprint = Some(comp.footprint.clone());
                }
                for (k, v) in &comp.fields {
                    sch.fields.entry(k.clone()).or_insert_with(|| v.clone());
                }
                p.schematic = (!sch.is_empty()).then_some(sch);
            }
        }

        for p in &placements {
            if p.footprint.is_empty() {
                return Err(format!("placement '{}' has no footprint", p.refdes));
            }
            if !footprints.contains_key(&p.footprint) {
                if let Some(fp) = resolve_footprint_from_libs(&p.footprint, &resolved_libs)? {
                    footprints.insert(p.footprint.clone(), fp);
                } else {
                    return Err(format!(
                        "placement '{}' references unknown footprint '{}'",
                        p.refdes, p.footprint
                    ));
                }
            }
        }

        // Apply explicit spec.nets pins onto placements before netlist fallback so spec overrides win.
        for (net_name, net_def) in &nets {
            for (refdes, pin) in &net_def.pins {
                if let Some(p) = placements.iter_mut().find(|pl| &pl.refdes == refdes) {
                    p.pin_nets
                        .entry(pin.clone())
                        .or_insert_with(|| net_name.clone());
                }
            }
        }

        let mut netlist_classes: HashMap<String, String> = HashMap::new();
        if let Some(ref nl) = netlist {
            let mut by_ref: HashMap<String, Vec<usize>> = HashMap::new();
            for (idx, p) in placements.iter().enumerate() {
                by_ref.entry(p.refdes.clone()).or_default().push(idx);
            }
            for nl_net in &nl.nets {
                if !nl_net.class.is_empty() {
                    netlist_classes
                        .entry(nl_net.name.clone())
                        .or_insert_with(|| nl_net.class.clone());
                }
                for (refdes, pin) in &nl_net.nodes {
                    if let Some(indices) = by_ref.get(refdes.as_str()) {
                        for &idx in indices {
                            placements[idx]
                                .pin_nets
                                .entry(pin.clone())
                                .or_insert_with(|| nl_net.name.clone());
                        }
                    }
                }
            }
        }

        // Keep Placement::pin_nets and NetDef::pins mutually consistent.
        for p in &placements {
            for (pin, net_name) in &p.pin_nets {
                let nl_class = netlist_classes.get(net_name).map(String::as_str);
                let entry = nets.entry(net_name.clone()).or_insert_with(|| NetDef {
                    name: net_name.clone(),
                    class: nl_class
                        .map(str::to_string)
                        .unwrap_or_else(default_net_class),
                    pins: Vec::new(),
                    ignore_route: false,
                });
                if entry.class == "Default"
                    && let Some(c) = nl_class
                    && c != "Default"
                {
                    entry.class = c.to_string();
                }
                let pair = (p.refdes.clone(), pin.clone());
                if !entry.pins.contains(&pair) {
                    entry.pins.push(pair);
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
    serde_json::from_str(&text)
        .map_err(|e| format!("failed to parse board IR '{}': {e}", path.display()))
}
