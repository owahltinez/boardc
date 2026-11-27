use std::collections::{BTreeMap, BTreeSet, HashSet};

use serde::{Deserialize, Serialize};

pub use crate::spec::{BoardSpec, FootprintSpec, load_board_json, load_spec_file, save_board_json};

pub fn same_named_net(a: &str, b: &str) -> bool {
    !a.is_empty() && a == b
}

pub fn safe_slug(name: &str) -> String {
    let s: String = name
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || c == '-' || c == '_' {
                c
            } else {
                '_'
            }
        })
        .collect();
    if s.is_empty() { "board".into() } else { s }
}

pub(crate) fn trim_nonempty(raw: Option<&str>) -> Option<&str> {
    raw.map(str::trim).filter(|s| !s.is_empty())
}

pub(crate) fn fnv1a_128(bytes: &[u8]) -> u128 {
    let mut hash: u128 = 0x6c62_272e_07bb_0142_62b8_2175_6295_c58d;
    for &b in bytes {
        hash ^= b as u128;
        hash = hash.wrapping_mul(0x0000_0000_0100_0000_0000_0000_0000_013b);
    }
    hash
}

pub fn default_two_layer_stackup() -> Vec<LayerDef> {
    vec![
        LayerDef {
            name: "F.Cu".into(),
            kind: LayerKind::Signal,
            side: Some(Side::Top),
            thickness_nm: None,
        },
        LayerDef {
            name: "B.Cu".into(),
            kind: LayerKind::Signal,
            side: Some(Side::Bottom),
            thickness_nm: None,
        },
    ]
}

/// Canonical internal length unit: 1 nanometer (`1 mm = 1_000_000 nm`).
pub type Nm = i64;

pub const UM: Nm = 1_000;
pub const MM: Nm = 1_000_000;
pub const DEFAULT_BOARD_THICKNESS_NM: Nm = 1_600_000;
pub const SOLDER_MASK_THICKNESS_NM: Nm = 10_000;
pub const OUTER_COPPER_THICKNESS_NM: Nm = 35_000;
pub const INNER_COPPER_THICKNESS_NM: Nm = 17_500;

pub fn mm(v: f64) -> Nm {
    (v * MM as f64).round() as Nm
}

pub fn um(v: f64) -> Nm {
    (v * UM as f64).round() as Nm
}

pub fn nm_to_mm(v: Nm) -> f64 {
    v as f64 / MM as f64
}

pub fn nm_to_um(v: Nm) -> f64 {
    v as f64 / UM as f64
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default, Serialize, Deserialize)]
pub struct Pt {
    pub x: Nm,
    pub y: Nm,
}

impl Pt {
    pub const fn new(x: Nm, y: Nm) -> Self {
        Self { x, y }
    }

    pub fn dist_sq(self, other: Self) -> f64 {
        let dx = (self.x - other.x) as f64;
        let dy = (self.y - other.y) as f64;
        dx * dx + dy * dy
    }

    pub fn dist(self, other: Self) -> f64 {
        self.dist_sq(other).sqrt()
    }

    pub fn midpoint(self, other: Self) -> Self {
        Self::new((self.x + other.x) / 2, (self.y + other.y) / 2)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct BBoxNm {
    pub min_x: Nm,
    pub min_y: Nm,
    pub max_x: Nm,
    pub max_y: Nm,
}

impl BBoxNm {
    pub const fn new(min_x: Nm, min_y: Nm, max_x: Nm, max_y: Nm) -> Self {
        Self {
            min_x,
            min_y,
            max_x,
            max_y,
        }
    }

    pub fn from_pt(p: Pt, margin: Nm) -> Self {
        Self::new(p.x - margin, p.y - margin, p.x + margin, p.y + margin)
    }

    pub fn from_pts(pts: &[Pt]) -> Self {
        if pts.is_empty() {
            return Self::new(0, 0, 0, 0);
        }
        let mut b = Self::new(pts[0].x, pts[0].y, pts[0].x, pts[0].y);
        for &p in &pts[1..] {
            b.min_x = b.min_x.min(p.x);
            b.min_y = b.min_y.min(p.y);
            b.max_x = b.max_x.max(p.x);
            b.max_y = b.max_y.max(p.y);
        }
        b
    }

    pub fn expand(self, margin: Nm) -> Self {
        Self::new(
            self.min_x - margin,
            self.min_y - margin,
            self.max_x + margin,
            self.max_y + margin,
        )
    }

    pub fn union(self, other: Self) -> Self {
        Self::new(
            self.min_x.min(other.min_x),
            self.min_y.min(other.min_y),
            self.max_x.max(other.max_x),
            self.max_y.max(other.max_y),
        )
    }

    pub fn intersects(self, other: Self) -> bool {
        self.min_x <= other.max_x
            && self.max_x >= other.min_x
            && self.min_y <= other.max_y
            && self.max_y >= other.min_y
    }

    pub fn contains_pt(self, p: Pt) -> bool {
        p.x >= self.min_x && p.x <= self.max_x && p.y >= self.min_y && p.y <= self.max_y
    }

    pub fn width(self) -> Nm {
        self.max_x - self.min_x
    }

    pub fn height(self) -> Nm {
        self.max_y - self.min_y
    }

    pub fn center(self) -> Pt {
        Pt::new((self.min_x + self.max_x) / 2, (self.min_y + self.max_y) / 2)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Side {
    #[default]
    Top,
    Bottom,
}

impl Side {
    pub fn flip(self) -> Self {
        match self {
            Self::Top => Self::Bottom,
            Self::Bottom => Self::Top,
        }
    }

    pub fn cu_layer(self) -> &'static str {
        match self {
            Self::Top => "F.Cu",
            Self::Bottom => "B.Cu",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum LayerKind {
    #[default]
    Signal,
    Plane,
    Mixed,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct LayerDef {
    pub name: String,
    #[serde(default)]
    pub kind: LayerKind,
    #[serde(default)]
    pub side: Option<Side>,
    #[serde(default)]
    pub thickness_nm: Option<Nm>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct Stackup {
    pub thickness_nm: Nm,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub finish: Option<String>,
    pub pofv: bool,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub dielectric_nm: Vec<Nm>,
}

impl Default for Stackup {
    fn default() -> Self {
        Self {
            thickness_nm: DEFAULT_BOARD_THICKNESS_NM,
            finish: None,
            pofv: false,
            dielectric_nm: Vec::new(),
        }
    }
}

impl Stackup {
    pub fn normalize_finish(raw: &str) -> Option<&str> {
        let trimmed = raw.trim();
        (!trimmed.is_empty() && !trimmed.eq_ignore_ascii_case("none")).then_some(trimmed)
    }

    pub fn finish_trimmed(&self) -> Option<&str> {
        self.finish.as_deref().and_then(Self::normalize_finish)
    }
}

pub const MIN_REFDES_STROKE_NM: Nm = 10_000;
pub const DEFAULT_REFDES_STROKE_NM: Nm = 120_000;

/// Mode controlling auto-generated component refdes text in silkscreen output.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum AutoRefdes {
    #[default]
    Rotate,
    Upright,
    Off,
}

impl<'de> Deserialize<'de> for AutoRefdes {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        struct AutoRefdesVisitor;

        impl<'de> serde::de::Visitor<'de> for AutoRefdesVisitor {
            type Value = AutoRefdes;

            fn expecting(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
                f.write_str("a boolean or one of \"rotate\", \"upright\", \"off\"")
            }

            fn visit_bool<E: serde::de::Error>(self, v: bool) -> Result<Self::Value, E> {
                Ok(if v {
                    AutoRefdes::Rotate
                } else {
                    AutoRefdes::Off
                })
            }

            fn visit_str<E: serde::de::Error>(self, v: &str) -> Result<Self::Value, E> {
                match v.trim().to_ascii_lowercase().as_str() {
                    "rotate" => Ok(AutoRefdes::Rotate),
                    "upright" => Ok(AutoRefdes::Upright),
                    "off" => Ok(AutoRefdes::Off),
                    _ => Err(E::invalid_value(serde::de::Unexpected::Str(v), &self)),
                }
            }
        }

        deserializer.deserialize_any(AutoRefdesVisitor)
    }
}

/// Fabrication silkscreen settings (`[fab]` in `BoardSpec` and `BoardIr`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct FabConfig {
    pub auto_refdes: AutoRefdes,
    pub refdes_stroke_nm: Nm,
}

impl Default for FabConfig {
    fn default() -> Self {
        Self {
            auto_refdes: AutoRefdes::default(),
            refdes_stroke_nm: DEFAULT_REFDES_STROKE_NM,
        }
    }
}

impl FabConfig {
    pub fn is_default(&self) -> bool {
        *self == Self::default()
    }

    pub fn validate(&self) -> Result<(), String> {
        if self.refdes_stroke_nm < MIN_REFDES_STROKE_NM {
            return Err(format!(
                "fab.refdes_stroke_nm ({}) must be >= {MIN_REFDES_STROKE_NM}",
                self.refdes_stroke_nm
            ));
        }
        Ok(())
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PadKind {
    Smd,
    Tht,
    Npth,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "shape", rename_all = "snake_case")]
pub enum PadShape {
    Circle {
        diameter: Nm,
    },
    Rect {
        width: Nm,
        height: Nm,
    },
    RoundRect {
        width: Nm,
        height: Nm,
        corner_radius: Nm,
    },
    Oval {
        width: Nm,
        height: Nm,
    },
    Poly {
        verts: Vec<Pt>,
    },
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct PadDef {
    pub name: String,
    pub kind: PadKind,
    pub shape: PadShape,
    pub pos: Pt,
    #[serde(default)]
    pub rot_deg: f64,
    #[serde(default)]
    pub drill_nm: Option<Nm>,
    pub layers: Vec<String>,
    #[serde(default)]
    pub mask_margin_nm: Option<Nm>,
    #[serde(default)]
    pub paste_margin_nm: Option<Nm>,
}

pub const DEFAULT_TEXT_HEIGHT_NM: Nm = 1_000_000;
pub const DEFAULT_TEXT_STROKE_NM: Nm = 150_000;

fn default_text_height_nm() -> Nm {
    DEFAULT_TEXT_HEIGHT_NM
}

fn default_text_stroke_nm() -> Nm {
    DEFAULT_TEXT_STROKE_NM
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum Graphic {
    Line {
        layer: String,
        a: Pt,
        b: Pt,
        width: Nm,
    },
    Rect {
        layer: String,
        min: Pt,
        max: Pt,
        width: Nm,
        #[serde(default)]
        fill: bool,
    },
    Circle {
        layer: String,
        center: Pt,
        radius: Nm,
        width: Nm,
        #[serde(default)]
        fill: bool,
    },
    Poly {
        layer: String,
        verts: Vec<Pt>,
        width: Nm,
        #[serde(default)]
        fill: bool,
    },
    Text {
        layer: String,
        text: String,
        pos: Pt,
        #[serde(default = "default_text_height_nm")]
        height_nm: Nm,
        #[serde(default = "default_text_stroke_nm")]
        stroke_nm: Nm,
        #[serde(default)]
        rot_deg: f64,
    },
}

impl Graphic {
    pub fn layer(&self) -> &str {
        match self {
            Self::Line { layer, .. }
            | Self::Rect { layer, .. }
            | Self::Circle { layer, .. }
            | Self::Poly { layer, .. }
            | Self::Text { layer, .. } => layer,
        }
    }
}

fn default_model_scale() -> [f64; 3] {
    [1.0, 1.0, 1.0]
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Model3d {
    pub path: String,
    #[serde(default)]
    pub offset_mm: [f64; 3],
    #[serde(default = "default_model_scale")]
    pub scale: [f64; 3],
    #[serde(default)]
    pub rotate_deg: [f64; 3],
}

/// Electrical pin type used by native ERC checks.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PinType {
    In,
    Out,
    Bidir,
    Tri,
    #[serde(alias = "open_drain")]
    OpenCollector,
    PowerIn,
    PowerOut,
    Passive,
    Nc,
}

impl PinType {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::In => "in",
            Self::Out => "out",
            Self::Bidir => "bidir",
            Self::Tri => "tri",
            Self::OpenCollector => "open_collector",
            Self::PowerIn => "power_in",
            Self::PowerOut => "power_out",
            Self::Passive => "passive",
            Self::Nc => "nc",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct FootprintDef {
    pub name: String,
    pub pads: Vec<PadDef>,
    #[serde(default)]
    pub courtyard: Vec<Pt>,
    #[serde(default)]
    pub graphics: Vec<Graphic>,
    #[serde(default)]
    pub keepouts: Vec<Keepout>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub models: Vec<Model3d>,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub pin_types: BTreeMap<String, PinType>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub kicad_raw: Option<String>,
}

/// KiCad schematic parity metadata preserved across `.kicad_net`/`.kicad_pcb` and `export-kicad`.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct SchematicMeta {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub path: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub sheetname: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub sheetfile: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub footprint: Option<String>,
    #[serde(skip_serializing_if = "BTreeMap::is_empty")]
    pub fields: BTreeMap<String, String>,
}

impl SchematicMeta {
    pub fn is_empty(&self) -> bool {
        self.path.as_deref().is_none_or(|s| s.trim().is_empty())
            && self
                .sheetname
                .as_deref()
                .is_none_or(|s| s.trim().is_empty())
            && self
                .sheetfile
                .as_deref()
                .is_none_or(|s| s.trim().is_empty())
            && self
                .footprint
                .as_deref()
                .is_none_or(|s| s.trim().is_empty())
            && self.fields.is_empty()
    }
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct Placement {
    pub refdes: String,
    #[serde(default)]
    pub footprint: String,
    #[serde(default)]
    pub value: String,
    pub pos: Pt,
    #[serde(default)]
    pub rot_deg: f64,
    #[serde(default)]
    pub side: Side,
    #[serde(default)]
    pub locked: bool,
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub allow_edge_overhang: bool,
    #[serde(default)]
    pub pin_nets: BTreeMap<String, String>,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub pin_types: BTreeMap<String, PinType>,
    #[serde(default)]
    pub mpn: Option<String>,
    #[serde(default)]
    pub lcsc: Option<String>,
    #[serde(default)]
    pub dnp: bool,
    #[serde(default)]
    pub exclude_from_bom: bool,
    #[serde(default)]
    pub exclude_from_pos: bool,
    #[serde(default)]
    pub cpl_rotation_offset_deg: f64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub schematic: Option<SchematicMeta>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum KeepoutKind {
    #[default]
    All,
    Copper,
    Via,
    Track,
    Placement,
    Custom {
        tracks: bool,
        vias: bool,
        copper: bool,
        placement: bool,
    },
}

impl KeepoutKind {
    pub const fn blocks_tracks(self) -> bool {
        matches!(
            self,
            Self::All | Self::Track | Self::Custom { tracks: true, .. }
        )
    }

    pub const fn blocks_vias(self) -> bool {
        matches!(
            self,
            Self::All | Self::Via | Self::Custom { vias: true, .. }
        )
    }

    pub const fn blocks_copper(self) -> bool {
        matches!(
            self,
            Self::All | Self::Copper | Self::Custom { copper: true, .. }
        )
    }

    pub const fn blocks_placement(self) -> bool {
        matches!(
            self,
            Self::All
                | Self::Placement
                | Self::Custom {
                    placement: true,
                    ..
                }
        )
    }

    pub const fn blocks_pads(self) -> bool {
        matches!(self, Self::All)
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Keepout {
    #[serde(default)]
    pub kind: KeepoutKind,
    #[serde(default)]
    pub layers: Vec<String>,
    pub poly: Vec<Pt>,
}

fn default_true() -> bool {
    true
}

fn default_pour_min_width() -> Nm {
    200_000
}

fn default_thermal_gap() -> Nm {
    250_000
}

fn default_thermal_spoke() -> Nm {
    300_000
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PourSpec {
    #[serde(default)]
    pub name: String,
    pub net: String,
    pub layer: String,
    pub outline: Vec<Pt>,
    #[serde(default)]
    pub priority: i32,
    #[serde(default)]
    pub clearance_nm: Option<Nm>,
    #[serde(default = "default_pour_min_width")]
    pub min_width_nm: Nm,
    #[serde(default = "default_true")]
    pub thermal_relief: bool,
    #[serde(default = "default_thermal_gap")]
    pub thermal_gap_nm: Nm,
    #[serde(default = "default_thermal_spoke")]
    pub thermal_spoke_nm: Nm,
    #[serde(default = "default_true")]
    pub remove_islands: bool,
}

impl Default for PourSpec {
    fn default() -> Self {
        Self {
            name: String::new(),
            net: String::new(),
            layer: String::new(),
            outline: Vec::new(),
            priority: 0,
            clearance_nm: None,
            min_width_nm: default_pour_min_width(),
            thermal_relief: true,
            thermal_gap_nm: default_thermal_gap(),
            thermal_spoke_nm: default_thermal_spoke(),
            remove_islands: true,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PourPoly {
    pub outer: Vec<Pt>,
    #[serde(default)]
    pub holes: Vec<Vec<Pt>>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FilledPour {
    pub net: String,
    pub layer: String,
    pub priority: i32,
    pub polys: Vec<PourPoly>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Track {
    pub net: String,
    pub layer: String,
    pub a: Pt,
    pub b: Pt,
    pub width: Nm,
    #[serde(default)]
    pub locked: bool,
}

fn default_via_layers() -> (String, String) {
    ("F.Cu".into(), "B.Cu".into())
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Via {
    pub net: String,
    pub pos: Pt,
    pub diameter: Nm,
    pub drill: Nm,
    #[serde(default = "default_via_layers")]
    pub layers: (String, String),
    #[serde(default)]
    pub locked: bool,
    #[serde(default = "default_true")]
    pub tented: bool,
}

impl Via {
    pub fn touches_layer(&self, layer: &str, layers: &[LayerDef]) -> bool {
        if let Some(idx) = layers.iter().position(|l| l.name == layer) {
            let i0 = layers.iter().position(|l| l.name == self.layers.0);
            let i1 = layers.iter().position(|l| l.name == self.layers.1);
            return match (i0, i1) {
                (Some(a), Some(b)) => idx >= a.min(b) && idx <= a.max(b),
                _ if (self.layers.0 == "F.Cu" && self.layers.1 == "B.Cu")
                    || (self.layers.0 == "B.Cu" && self.layers.1 == "F.Cu")
                    || self.layers.0 == "*.Cu"
                    || self.layers.1 == "*.Cu" =>
                {
                    true
                }
                _ => layer == self.layers.0 || layer == self.layers.1,
            };
        }
        layer == self.layers.0
            || layer == self.layers.1
            || self.layers.0 == "*.Cu"
            || self.layers.1 == "*.Cu"
    }

    pub fn spanned_layers(&self, cu_layers: &[String], layers: &[LayerDef]) -> Vec<String> {
        let mut out = Vec::new();
        for l in cu_layers {
            if self.touches_layer(l, layers) {
                out.push(l.clone());
            }
        }
        if out.is_empty() {
            out.push(self.layers.0.clone());
            if self.layers.1 != self.layers.0 {
                out.push(self.layers.1.clone());
            }
        }
        out
    }

    /// Canonical `(top_layer, bottom_layer)` endpoints ordered by stackup index.
    pub fn span_layer_pair(&self, cu_layers: &[String], layers: &[LayerDef]) -> (String, String) {
        let spanned = self.spanned_layers(cu_layers, layers);
        match (spanned.first(), spanned.last()) {
            (Some(a), Some(b)) => (a.clone(), b.clone()),
            _ => self.layers.clone(),
        }
    }

    pub fn is_through_hole(&self, cu_layers: &[String], layers: &[LayerDef]) -> bool {
        if cu_layers.len() <= 2 {
            return true;
        }
        let spanned = self.spanned_layers(cu_layers, layers);
        spanned.len() >= cu_layers.len()
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct NetClass {
    pub name: String,
    pub track_width_nm: Nm,
    pub clearance_nm: Nm,
    pub via_dia_nm: Nm,
    pub via_drill_nm: Nm,
    #[serde(default)]
    pub diff_pair_width_nm: Option<Nm>,
    #[serde(default)]
    pub diff_pair_gap_nm: Option<Nm>,
}

pub(crate) fn default_net_class() -> String {
    "Default".into()
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct NetDef {
    pub name: String,
    pub class: String,
    pub pins: Vec<(String, String)>,
    pub ignore_route: bool,
    #[serde(skip_serializing_if = "std::ops::Not::not")]
    pub power_driven: bool,
}

impl Default for NetDef {
    fn default() -> Self {
        Self {
            name: String::new(),
            class: default_net_class(),
            pins: Vec::new(),
            ignore_route: false,
            power_driven: false,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct DesignRules {
    pub track_width_nm: Nm,
    pub clearance_nm: Nm,
    pub via_dia_nm: Nm,
    pub via_drill_nm: Nm,
    pub min_annular_ring_nm: Nm,
    pub hole_to_hole_nm: Nm,
    pub hole_clearance_nm: Option<Nm>,
    pub edge_clearance_nm: Nm,
    pub silk_clearance_nm: Nm,
    pub courtyard_clearance_nm: Nm,
    pub smd_to_smd_nm: Option<Nm>,
    pub via_to_smd_nm: Option<Nm>,
    pub via_to_via_nm: Option<Nm>,
    pub via_to_track_nm: Option<Nm>,
    pub via_at_smd: bool,
    pub mask_expansion_nm: Nm,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub mask_to_copper_nm: Option<Nm>,
    pub paste_margin_nm: Nm,
}

impl Default for DesignRules {
    fn default() -> Self {
        Self {
            track_width_nm: 150_000,
            clearance_nm: 150_000,
            via_dia_nm: 450_000,
            via_drill_nm: 200_000,
            min_annular_ring_nm: 100_000,
            hole_to_hole_nm: 200_000,
            hole_clearance_nm: None,
            edge_clearance_nm: 300_000,
            silk_clearance_nm: 150_000,
            courtyard_clearance_nm: 0,
            smd_to_smd_nm: None,
            via_to_smd_nm: None,
            via_to_via_nm: None,
            via_to_track_nm: None,
            via_at_smd: false,
            mask_expansion_nm: 50_000,
            mask_to_copper_nm: None,
            paste_margin_nm: 0,
        }
    }
}

impl DesignRules {
    pub fn effective_hole_clearance_nm(&self) -> Nm {
        self.hole_clearance_nm.unwrap_or(self.clearance_nm)
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct MaxLengthRule {
    pub nets: Vec<String>,
    pub max_nm: Nm,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct MatchGroupRule {
    pub name: String,
    pub nets: Vec<String>,
    pub tolerance_nm: Nm,
    #[serde(default)]
    pub target_nm: Option<Nm>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CapProximityRule {
    pub cap_refdes: String,
    pub ic_refdes: String,
    #[serde(default)]
    pub net: String,
    pub max_dist_nm: Nm,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct MinViasRule {
    pub net: String,
    #[serde(default)]
    pub refdes: Option<String>,
    pub min_vias: usize,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct BgaShadowRule {
    pub bga_refdes: String,
    #[serde(default)]
    pub min_clearance_nm: Nm,
    #[serde(default)]
    pub allow_passives: bool,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct Constraints {
    pub max_lengths: Vec<MaxLengthRule>,
    pub match_groups: Vec<MatchGroupRule>,
    pub cap_proximity: Vec<CapProximityRule>,
    pub min_vias: Vec<MinViasRule>,
    pub bga_shadows: Vec<BgaShadowRule>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct BoardIr {
    pub name: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub title: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub revision: Option<String>,
    pub outline: Vec<Pt>,
    #[serde(default)]
    pub cutouts: Vec<Vec<Pt>>,
    pub layers: Vec<LayerDef>,
    pub footprints: BTreeMap<String, FootprintDef>,
    pub placements: Vec<Placement>,
    #[serde(default)]
    pub keepouts: Vec<Keepout>,
    #[serde(default)]
    pub pours: Vec<PourSpec>,
    #[serde(default)]
    pub filled_pours: Vec<FilledPour>,
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
    pub fn effective_title(&self) -> &str {
        trim_nonempty(self.title.as_deref()).unwrap_or(&self.name)
    }

    pub fn revision_trimmed(&self) -> Option<&str> {
        trim_nonempty(self.revision.as_deref())
    }

    pub fn effective_revision(&self) -> &str {
        self.revision_trimmed().unwrap_or("1")
    }

    pub fn effective_layers(&self) -> Vec<LayerDef> {
        if self.layers.is_empty() {
            default_two_layer_stackup()
        } else {
            self.layers.clone()
        }
    }

    pub fn effective_board_thickness_nm(&self) -> Nm {
        if self.stackup.thickness_nm > 0 {
            self.stackup.thickness_nm
        } else {
            DEFAULT_BOARD_THICKNESS_NM
        }
    }

    pub fn pofv_enabled(&self) -> bool {
        self.stackup.pofv || self.rules.via_at_smd
    }

    /// Resolves `(copper_layer_name, thickness_nm)` and dielectric thicknesses (in nm) top-to-bottom.
    pub fn resolved_stackup_thicknesses(&self) -> (Vec<(&str, Nm)>, Vec<Nm>) {
        let cu: Vec<(&str, Nm)> = if self.layers.is_empty() {
            vec![
                ("F.Cu", OUTER_COPPER_THICKNESS_NM),
                ("B.Cu", OUTER_COPPER_THICKNESS_NM),
            ]
        } else {
            let n = self.layers.len();
            self.layers
                .iter()
                .enumerate()
                .map(|(i, l)| {
                    let thick =
                        l.thickness_nm
                            .filter(|&t| t > 0)
                            .unwrap_or(if i == 0 || i + 1 == n {
                                OUTER_COPPER_THICKNESS_NM
                            } else {
                                INNER_COPPER_THICKNESS_NM
                            });
                    (l.name.as_str(), thick)
                })
                .collect()
        };
        let gap_count = cu.len().saturating_sub(1).max(1);
        if self.stackup.dielectric_nm.len() == gap_count
            && self.stackup.dielectric_nm.iter().all(|&d| d > 0)
        {
            return (cu, self.stackup.dielectric_nm.clone());
        }
        let total_cu: Nm = cu.iter().map(|(_, t)| *t).sum();
        let rem = (self.effective_board_thickness_nm() - total_cu - 2 * SOLDER_MASK_THICKNESS_NM)
            .max(gap_count as Nm * 1_000);
        let base = rem / gap_count as Nm;
        let mut diel_nm = vec![base; gap_count];
        diel_nm[gap_count / 2] += rem % gap_count as Nm;
        (cu, diel_nm)
    }

    pub fn copper_layer_names(&self) -> Vec<String> {
        if self.layers.is_empty() {
            vec!["F.Cu".into(), "B.Cu".into()]
        } else {
            self.layers.iter().map(|l| l.name.clone()).collect()
        }
    }

    pub fn net_class(&self, net: &str) -> Option<&NetClass> {
        self.nets
            .get(net)
            .and_then(|nd| self.net_classes.get(&nd.class))
    }

    pub fn explicit_net_class_clearance(&self, net: &str) -> Option<Nm> {
        self.net_class(net).map(|nc| nc.clearance_nm)
    }

    pub fn non_default_net_class_clearance(&self, net: &str) -> Option<Nm> {
        let nd = self.nets.get(net)?;
        if nd.class == "Default" {
            return None;
        }
        self.net_classes.get(&nd.class).map(|nc| nc.clearance_nm)
    }

    pub fn net_clearance(&self, net: &str) -> Nm {
        self.explicit_net_class_clearance(net)
            .unwrap_or(self.rules.clearance_nm)
    }

    pub fn net_track_width(&self, net: &str) -> Nm {
        self.net_class(net)
            .map_or(self.rules.track_width_nm, |nc| nc.track_width_nm)
    }

    pub fn net_via_dims(&self, net: &str) -> (Nm, Nm) {
        let nc = self.net_class(net);
        let drill = nc.map_or(self.rules.via_drill_nm, |c| c.via_drill_nm);
        let dia = nc
            .map_or(self.rules.via_dia_nm, |c| c.via_dia_nm)
            .max(drill + 2 * self.rules.min_annular_ring_nm);
        (dia, drill)
    }

    pub fn pair_clearance(&self, net_a: &str, net_b: &str, rule_override: Option<Nm>) -> Nm {
        if let Some(ov) = rule_override {
            let ca = self.non_default_net_class_clearance(net_a).unwrap_or(ov);
            let cb = self.non_default_net_class_clearance(net_b).unwrap_or(ov);
            ov.max(ca).max(cb)
        } else {
            let base = self.rules.clearance_nm;
            let ca = self.explicit_net_class_clearance(net_a).unwrap_or(base);
            let cb = self.explicit_net_class_clearance(net_b).unwrap_or(base);
            base.max(ca).max(cb)
        }
    }

    pub fn pour_pair_clearance(
        &self,
        pour_net: &str,
        pour_clr_override: Option<Nm>,
        other_net: &str,
    ) -> Nm {
        let base = pour_clr_override.unwrap_or_else(|| self.net_clearance(pour_net));
        if pour_clr_override.is_some() {
            self.non_default_net_class_clearance(other_net)
                .map_or(base, |c| base.max(c))
        } else {
            base.max(self.net_clearance(other_net))
        }
    }

    pub(crate) fn is_net_ignored(&self, net: &str, ignored: &HashSet<String>) -> bool {
        if net.is_empty() {
            return false;
        }
        let nd = self.nets.get(net);
        let class_name = nd.map_or("Default", |d| d.class.as_str());
        ignored.contains(net) || ignored.contains(class_name) || nd.is_some_and(|d| d.ignore_route)
    }

    pub fn all_net_names(&self) -> Vec<String> {
        let mut names = BTreeSet::new();
        let iter = self
            .nets
            .keys()
            .map(String::as_str)
            .chain(
                self.placements
                    .iter()
                    .flat_map(|pl| pl.pin_nets.values().map(String::as_str)),
            )
            .chain(self.tracks.iter().map(|t| t.net.as_str()))
            .chain(self.vias.iter().map(|v| v.net.as_str()))
            .chain(self.pours.iter().map(|p| p.net.as_str()))
            .chain(self.filled_pours.iter().map(|fp| fp.net.as_str()));
        for name in iter {
            if !name.is_empty() {
                names.insert(name.to_string());
            }
        }
        names.into_iter().collect()
    }
}
