use std::collections::{BTreeMap, HashMap, HashSet};
use std::fs;
use std::path::Path;

use viahop::sexp::SExp;

use crate::ir::{PinType, default_net_class};
use crate::kicad_geom::{atom_at, child_atom, parse_kicad_sexp};

/// Parsed component entry from a KiCad S-expression netlist (`.net` / `.kicad_net`).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct NetlistComponent {
    pub refdes: String,
    pub value: String,
    pub footprint: String,
    pub mpn: Option<String>,
    pub lcsc: Option<String>,
    pub dnp: bool,
    pub exclude_from_bom: bool,
    pub exclude_from_board: bool,
    pub pin_types: BTreeMap<String, PinType>,
}

/// Parsed net entry from a KiCad S-expression netlist.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct NetlistNet {
    pub name: String,
    pub class: String,
    pub nodes: Vec<(String, String)>,
}

/// Parsed KiCad S-expression netlist (`.net` / `.kicad_net`).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct KicadNetlist {
    pub components: BTreeMap<String, NetlistComponent>,
    pub nets: Vec<NetlistNet>,
}

/// Loads and parses a KiCad S-expression netlist file (`.net` / `.kicad_net`).
pub fn load_kicad_net(path: &Path) -> Result<KicadNetlist, String> {
    let src = fs::read_to_string(path)
        .map_err(|e| format!("failed to read netlist '{}': {e}", path.display()))?;
    parse_kicad_net(&src)
}

/// Parses KiCad S-expression netlist text (version D or E) into structured components and nets.
pub fn parse_kicad_net(src: &str) -> Result<KicadNetlist, String> {
    let root = parse_kicad_sexp(src)?;
    if root.tag() != Some("export") {
        return Err(format!(
            "expected top-level '(export ...)' in KiCad netlist, got '{:?}'",
            root.tag()
        ));
    }

    let libparts = parse_libparts(&root);

    let mut components = BTreeMap::new();
    if let Some(comps_node) = root.find_child("components") {
        for comp in comps_node.children_with_tag("comp") {
            let parsed = parse_comp(comp, &libparts)?;
            if components.contains_key(&parsed.refdes) {
                return Err(format!(
                    "duplicate component reference '{}' in KiCad netlist",
                    parsed.refdes
                ));
            }
            components.insert(parsed.refdes.clone(), parsed);
        }
    }

    let mut nets = Vec::new();
    if let Some(nets_node) = root.find_child("nets") {
        for net_node in nets_node.children_with_tag("net") {
            if let Some(net) = parse_net(net_node, &mut components) {
                nets.push(net);
            }
        }
    }

    Ok(KicadNetlist { components, nets })
}

fn parse_bool_flag_prop(node: &SExp) -> bool {
    match child_atom(node, "value").map(str::trim) {
        None | Some("") => true,
        Some(v) => !matches!(v.to_ascii_lowercase().as_str(), "no" | "false" | "0"),
    }
}

fn parse_kicad_pin_type(raw: &str) -> Option<PinType> {
    match raw.trim().to_ascii_lowercase().as_str() {
        "input" | "in" => Some(PinType::In),
        "output" | "out" => Some(PinType::Out),
        "bidirectional" | "bidi" | "bidir" => Some(PinType::Bidir),
        "tri_state" | "tristate" | "3state" | "tri" => Some(PinType::Tri),
        "passive" => Some(PinType::Passive),
        "power_in" => Some(PinType::PowerIn),
        "power_out" => Some(PinType::PowerOut),
        "open_collector" | "open_drain" | "opencol" => Some(PinType::OpenCollector),
        "open_emitter" | "open_source" | "openem" | "openemitter" => Some(PinType::OpenEmitter),
        "free" => Some(PinType::Free),
        "no_connect" | "not_connected" | "notconnected" | "unconnected" | "nc" => Some(PinType::Nc),
        _ => None,
    }
}

fn is_unspecified_kicad_pin_type(raw: &str) -> bool {
    let t = raw.trim();
    t.eq_ignore_ascii_case("unspecified") || t.eq_ignore_ascii_case("unspc")
}

fn pin_type_rank(pt: PinType) -> u8 {
    match pt {
        PinType::PowerOut => 8,
        PinType::Out => 7,
        PinType::Tri | PinType::OpenCollector | PinType::OpenEmitter => 6,
        PinType::Bidir => 5,
        PinType::PowerIn => 4,
        PinType::In => 3,
        PinType::Passive => 2,
        PinType::Free => 1,
        PinType::Nc => 0,
    }
}

type LibPartKey<'a> = (&'a str, &'a str);

fn lib_part_key(node: &SExp) -> Option<LibPartKey<'_>> {
    let lib = child_atom(node, "lib").map(str::trim).unwrap_or("");
    let part = child_atom(node, "part")
        .map(str::trim)
        .filter(|s| !s.is_empty())?;
    Some((lib, part))
}

fn parse_libparts<'a>(root: &'a SExp) -> HashMap<LibPartKey<'a>, BTreeMap<String, PinType>> {
    let mut out: HashMap<LibPartKey<'a>, BTreeMap<String, PinType>> = HashMap::new();
    let Some(libparts_node) = root.find_child("libparts") else {
        return out;
    };
    let mut alias_links: Vec<(LibPartKey<'a>, LibPartKey<'a>)> = Vec::new();
    for libpart in libparts_node.children_with_tag("libpart") {
        let Some((lib, part)) = lib_part_key(libpart) else {
            continue;
        };
        let Some(pins_node) = libpart.find_child("pins") else {
            continue;
        };
        let mut pins: BTreeMap<String, PinType> = BTreeMap::new();
        for pin in pins_node.children_with_tag("pin") {
            let Some(num) = child_atom(pin, "num")
                .map(str::trim)
                .filter(|s| !s.is_empty())
            else {
                continue;
            };
            if let Some(pt) = child_atom(pin, "type").and_then(parse_kicad_pin_type) {
                pins.entry(num.to_string())
                    .and_modify(|cur| {
                        if pin_type_rank(pt) > pin_type_rank(*cur) {
                            *cur = pt;
                        }
                    })
                    .or_insert(pt);
            }
        }
        if pins.is_empty() {
            continue;
        }
        out.insert((lib, part), pins);
        if let Some(aliases_node) = libpart.find_child("aliases") {
            for alias_node in aliases_node.children_with_tag("alias") {
                if let Some(alias) = atom_at(alias_node, 1)
                    .map(str::trim)
                    .filter(|s| !s.is_empty())
                {
                    alias_links.push(((lib, alias), (lib, part)));
                }
            }
        }
    }
    for (alias_key, primary_key) in alias_links {
        if !out.contains_key(&alias_key)
            && let Some(pins) = out.get(&primary_key).cloned()
        {
            out.insert(alias_key, pins);
        }
    }
    out
}

fn parse_comp(
    node: &SExp,
    libparts: &HashMap<(&str, &str), BTreeMap<String, PinType>>,
) -> Result<NetlistComponent, String> {
    let refdes = child_atom(node, "ref")
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .ok_or("KiCad netlist component missing non-empty '(ref ...)'")?
        .to_string();
    let value = child_atom(node, "value").unwrap_or("").to_string();
    let footprint = child_atom(node, "footprint")
        .map(str::trim)
        .unwrap_or("")
        .to_string();

    let mut mpn = None;
    let mut lcsc = None;
    if let Some(fields_node) = node.find_child("fields") {
        for field in fields_node.children_with_tag("field") {
            if let (Some(k), Some(v)) = (child_atom(field, "name"), atom_at(field, 2))
                && !v.is_empty()
            {
                if k.eq_ignore_ascii_case("mpn") {
                    mpn = Some(v.to_string());
                } else if k.eq_ignore_ascii_case("lcsc") {
                    lcsc = Some(v.to_string());
                }
            }
        }
    }

    let mut dnp = false;
    let mut exclude_from_bom = false;
    let mut exclude_from_board = false;

    for prop in node.children_with_tag("property") {
        let Some(k) = child_atom(prop, "name").filter(|s| !s.is_empty()) else {
            continue;
        };
        if k.eq_ignore_ascii_case("dnp") {
            dnp = parse_bool_flag_prop(prop);
        } else if k.eq_ignore_ascii_case("exclude_from_bom") {
            exclude_from_bom = parse_bool_flag_prop(prop);
        } else if k.eq_ignore_ascii_case("exclude_from_board") {
            exclude_from_board = parse_bool_flag_prop(prop);
        } else if let Some(v) = child_atom(prop, "value").filter(|s| !s.is_empty()) {
            if k.eq_ignore_ascii_case("mpn") {
                mpn = Some(v.to_string());
            } else if k.eq_ignore_ascii_case("lcsc") {
                lcsc = Some(v.to_string());
            }
        }
    }

    let pin_types = node
        .find_child("libsource")
        .and_then(lib_part_key)
        .and_then(|key| libparts.get(&key).cloned())
        .unwrap_or_default();

    Ok(NetlistComponent {
        refdes,
        value,
        footprint,
        mpn,
        lcsc,
        dnp,
        exclude_from_bom,
        exclude_from_board,
        pin_types,
    })
}

fn parse_net_class(raw: Option<&str>) -> String {
    let Some(s) = raw else {
        return default_net_class();
    };
    let mut first = None;
    for part in s.split(',').map(str::trim).filter(|p| !p.is_empty()) {
        if first.is_none() {
            first = Some(part);
        }
        if part != "Default" {
            return part.to_string();
        }
    }
    first.map(str::to_string).unwrap_or_else(default_net_class)
}

fn parse_net(
    node: &SExp,
    components: &mut BTreeMap<String, NetlistComponent>,
) -> Option<NetlistNet> {
    let name = child_atom(node, "name")
        .map(str::trim)
        .filter(|s| !s.is_empty())?
        .to_string();
    let class = parse_net_class(child_atom(node, "class"));
    let mut nodes = Vec::new();
    let mut seen: HashSet<(&str, &str)> = HashSet::new();
    for n in node.children_with_tag("node") {
        let Some(refdes) = child_atom(n, "ref")
            .map(str::trim)
            .filter(|s| !s.is_empty())
        else {
            continue;
        };
        let Some(pin) = child_atom(n, "pin")
            .map(str::trim)
            .filter(|s| !s.is_empty())
        else {
            continue;
        };
        if let Some(raw_pt) = child_atom(n, "pintype")
            && let Some(comp) = components.get_mut(refdes)
        {
            if let Some(pt) = parse_kicad_pin_type(raw_pt) {
                comp.pin_types.insert(pin.to_string(), pt);
            } else if is_unspecified_kicad_pin_type(raw_pt) {
                comp.pin_types.remove(pin);
            }
        }
        if seen.insert((refdes, pin)) {
            nodes.push((refdes.to_string(), pin.to_string()));
        }
    }
    Some(NetlistNet { name, class, nodes })
}
