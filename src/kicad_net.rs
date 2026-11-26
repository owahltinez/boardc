use std::collections::{BTreeMap, HashMap, HashSet};
use std::fs;
use std::path::Path;

use viahop::sexp::{self, SExp};

use crate::ir::default_net_class;
use crate::kicad_geom::{atom_at, child_atom};

/// Parsed component entry from a KiCad S-expression netlist (`.net` / `.kicad_net`).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct NetlistComponent {
    pub refdes: String,
    pub value: String,
    pub footprint: String,
    pub path: Option<String>,
    pub sheetname: Option<String>,
    pub sheetfile: Option<String>,
    pub mpn: Option<String>,
    pub lcsc: Option<String>,
    pub dnp: bool,
    pub exclude_from_bom: bool,
    pub exclude_from_board: bool,
    pub fields: BTreeMap<String, String>,
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
    let root = sexp::parse(src)?;
    if root.tag() != Some("export") {
        return Err(format!(
            "expected top-level '(export ...)' in KiCad netlist, got '{:?}'",
            root.tag()
        ));
    }

    let mut sheet_info: HashMap<String, (Option<String>, Option<String>)> = HashMap::new();
    if let Some(design) = root.find_child("design") {
        for sheet in design.children_with_tag("sheet") {
            let Some(ts) = child_atom(sheet, "tstamps").filter(|s| !s.is_empty()) else {
                continue;
            };
            let name = child_atom(sheet, "name")
                .filter(|s| !s.is_empty())
                .map(str::to_string);
            let source = sheet
                .find_child("title_block")
                .and_then(|tb| child_atom(tb, "source"))
                .filter(|s| !s.is_empty())
                .map(str::to_string);
            sheet_info.insert(ts.to_string(), (name, source));
        }
    }

    let mut components = BTreeMap::new();
    if let Some(comps_node) = root.find_child("components") {
        for comp in comps_node.children_with_tag("comp") {
            let parsed = parse_comp(comp, &sheet_info)?;
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
            if let Some(net) = parse_net(net_node) {
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

fn is_ignored_field(name: &str) -> bool {
    matches!(
        name,
        "Reference" | "Value" | "Footprint" | "Sheetname" | "Sheetfile"
    ) || name.starts_with("ki_")
}

fn compose_schematic_path(sheet_ts: Option<&str>, comp_ts: Option<&str>) -> Option<String> {
    let comp = comp_ts?.trim().trim_start_matches('/');
    if comp.is_empty() {
        return None;
    }
    let sheet = sheet_ts.unwrap_or("/").trim();
    if sheet.is_empty() || sheet == "/" {
        Some(format!("/{comp}"))
    } else {
        let mid = sheet.trim_matches('/');
        Some(format!("/{mid}/{comp}"))
    }
}

fn parse_comp(
    node: &SExp,
    sheet_info: &HashMap<String, (Option<String>, Option<String>)>,
) -> Result<NetlistComponent, String> {
    let refdes = child_atom(node, "ref")
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .ok_or("KiCad netlist component missing non-empty '(ref ...)'")?
        .to_string();
    let value = child_atom(node, "value").unwrap_or("").to_string();
    let footprint = child_atom(node, "footprint").unwrap_or("").to_string();

    let mut fields = BTreeMap::new();
    if let Some(ds) = child_atom(node, "datasheet").filter(|s| !s.is_empty() && *s != "~") {
        fields.insert("Datasheet".into(), ds.to_string());
    }
    if let Some(desc) = child_atom(node, "description").filter(|s| !s.is_empty()) {
        fields.insert("Description".into(), desc.to_string());
    }
    if let Some(fields_node) = node.find_child("fields") {
        for field in fields_node.children_with_tag("field") {
            let Some(k) = child_atom(field, "name").filter(|s| !s.is_empty()) else {
                continue;
            };
            if is_ignored_field(k) {
                continue;
            }
            let val = atom_at(field, 2).unwrap_or("");
            if val.is_empty() || (k == "Datasheet" && val == "~") {
                continue;
            }
            fields.insert(k.to_string(), val.to_string());
        }
    }

    let mut prop_sheetname = None;
    let mut sheetfile = None;
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
        } else if k == "Sheetname" {
            if let Some(v) = child_atom(prop, "value").filter(|s| !s.is_empty()) {
                prop_sheetname = Some(v.to_string());
            }
        } else if k == "Sheetfile" {
            if let Some(v) = child_atom(prop, "value").filter(|s| !s.is_empty()) {
                sheetfile = Some(v.to_string());
            }
        } else if !is_ignored_field(k)
            && let Some(v) = child_atom(prop, "value").filter(|s| !s.is_empty())
        {
            if k == "Datasheet" && v == "~" {
                continue;
            }
            fields.insert(k.to_string(), v.to_string());
        }
    }

    let sheetpath = node.find_child("sheetpath");
    let sheet_ts = sheetpath
        .and_then(|sp| child_atom(sp, "tstamps"))
        .filter(|s| !s.is_empty());
    let sp_name = sheetpath
        .and_then(|sp| child_atom(sp, "names"))
        .filter(|s| !s.is_empty())
        .map(str::to_string);
    let design_entry = sheet_ts.and_then(|ts| sheet_info.get(ts));
    let sheetname = sp_name
        .or_else(|| design_entry.and_then(|(n, _)| n.clone()))
        .or(prop_sheetname);
    if sheetfile.is_none() {
        sheetfile = design_entry.and_then(|(_, src)| src.clone());
    }

    // Multi-unit symbols list one UUID per unit; KiCad PCB footprints store the first unit's UUID.
    let comp_ts = child_atom(node, "tstamps").or_else(|| child_atom(node, "tstamp"));
    let path = compose_schematic_path(sheet_ts, comp_ts);

    let mpn = fields
        .iter()
        .find(|(k, _)| k.eq_ignore_ascii_case("mpn"))
        .map(|(_, v)| v.clone());
    let lcsc = fields
        .iter()
        .find(|(k, _)| k.eq_ignore_ascii_case("lcsc"))
        .map(|(_, v)| v.clone());

    Ok(NetlistComponent {
        refdes,
        value,
        footprint,
        path,
        sheetname,
        sheetfile,
        mpn,
        lcsc,
        dnp,
        exclude_from_bom,
        exclude_from_board,
        fields,
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

fn parse_net(node: &SExp) -> Option<NetlistNet> {
    let name = child_atom(node, "name")
        .filter(|s| !s.is_empty())?
        .to_string();
    let class = parse_net_class(child_atom(node, "class"));
    let mut nodes = Vec::new();
    let mut seen = HashSet::new();
    for n in node.children_with_tag("node") {
        let Some(refdes) = child_atom(n, "ref").filter(|s| !s.is_empty()) else {
            continue;
        };
        let Some(pin) = child_atom(n, "pin").filter(|s| !s.is_empty()) else {
            continue;
        };
        let pair = (refdes.to_string(), pin.to_string());
        if seen.insert(pair.clone()) {
            nodes.push(pair);
        }
    }
    Some(NetlistNet { name, class, nodes })
}
