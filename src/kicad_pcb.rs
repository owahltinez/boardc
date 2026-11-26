use std::collections::{BTreeMap, HashMap};
use std::fs;
use std::path::Path;

use viahop::sexp::{self, SExp};

use crate::geom::{ensure_ccw, local_to_board, polygon_area};
use crate::ir::{
    BoardIr, Constraints, DesignRules, FilledPour, FootprintDef, Keepout, LayerDef, LayerKind,
    NetDef, Nm, Placement, PourPoly, PourSpec, Pt, Side, Track, Via, default_net_class,
    default_two_layer_stackup, mm,
};
use crate::kicad_geom::{
    atom_at, chain_segments, child_atom, collect_poly_or_segment, discretize_arc, f64_at, has_flag,
    parse_at, parse_graphic_node, parse_layers_list, parse_net_ref, parse_pt, parse_pts,
};
use crate::kicad_io::{parse_footprint_node, parse_keepout_zone};
use crate::kicad_pro::parse_kicad_pro;

pub(crate) fn import_kicad_pcb(
    pcb_path: &Path,
    pro_path_opt: Option<&Path>,
) -> Result<BoardIr, String> {
    let src = fs::read_to_string(pcb_path)
        .map_err(|e| format!("failed to read '{}': {e}", pcb_path.display()))?;
    let pro_json = if let Some(pro_path) = pro_path_opt {
        Some(
            fs::read_to_string(pro_path)
                .map_err(|e| format!("failed to read '{}': {e}", pro_path.display()))?,
        )
    } else {
        let sibling = pcb_path.with_extension("kicad_pro");
        if sibling.exists() {
            Some(
                fs::read_to_string(&sibling)
                    .map_err(|e| format!("failed to read '{}': {e}", sibling.display()))?,
            )
        } else {
            None
        }
    };
    let mut ir = parse_kicad_pcb(&src, pro_json.as_deref())?;
    if ir.name == "kicad_board"
        && let Some(stem) = pcb_path.file_stem().and_then(|s| s.to_str())
    {
        ir.name = stem.to_string();
    }
    Ok(ir)
}

pub(crate) fn parse_kicad_pcb(src: &str, pro_json_opt: Option<&str>) -> Result<BoardIr, String> {
    let root = sexp::parse(src)?;
    if root.tag() != Some("kicad_pcb") {
        return Err("expected top-level '(kicad_pcb ...)'".into());
    }
    let items = root.as_list().ok_or("invalid kicad_pcb list")?;

    let mut name = String::new();
    let mut layers = Vec::new();
    let mut net_id_map: HashMap<i64, String> = HashMap::new();
    let mut footprints: BTreeMap<String, FootprintDef> = BTreeMap::new();
    let mut placements = Vec::new();
    let mut keepouts = Vec::new();
    let mut pours = Vec::new();
    let mut filled_pours = Vec::new();
    let mut tracks = Vec::new();
    let mut vias = Vec::new();
    let mut graphics = Vec::new();
    let mut edge_open: Vec<Vec<Pt>> = Vec::new();
    let mut edge_closed: Vec<Vec<Pt>> = Vec::new();
    let mut layer_thickness: HashMap<String, Nm> = HashMap::new();
    let mut setup_pofv = false;

    for node in items.iter().skip(1) {
        match node.tag() {
            Some("title_block") => {
                if let Some(t) = child_atom(node, "title")
                    && !t.is_empty()
                {
                    name = t.to_string();
                }
            }
            Some("layers") => layers = parse_layers_block(node),
            Some("setup") => parse_setup(node, &mut layer_thickness, &mut setup_pofv),
            Some("net") => {
                if let (Some(id), Some(n)) = (f64_at(node, 1).map(|v| v as i64), atom_at(node, 2))
                    && !n.is_empty()
                {
                    net_id_map.insert(id, n.to_string());
                }
            }
            Some("footprint") | Some("module") => parse_placed_footprint(
                node,
                &net_id_map,
                &mut footprints,
                &mut placements,
                &mut edge_open,
                &mut edge_closed,
            )?,
            Some("segment") => tracks.push(parse_segment(node, &net_id_map)?),
            Some("arc") => tracks.extend(parse_track_arc(node, &net_id_map)?),
            Some("via") => vias.push(parse_via(node, &net_id_map)?),
            Some("zone") => parse_board_zone(
                node,
                &net_id_map,
                &mut keepouts,
                &mut pours,
                &mut filled_pours,
            ),
            Some("gr_line") | Some("gr_arc") | Some("gr_rect") | Some("gr_circle")
            | Some("gr_poly") => {
                if child_atom(node, "layer") == Some("Edge.Cuts") {
                    collect_poly_or_segment(node, -1, &mut edge_open, &mut edge_closed);
                } else {
                    graphics.extend(parse_graphic_node(node, -1, None));
                }
            }
            _ => {}
        }
    }

    for layer in &mut layers {
        if let Some(&t) = layer_thickness.get(&layer.name) {
            layer.thickness_nm = Some(t);
        }
    }
    if layers.is_empty() {
        layers = default_two_layer_stackup();
    }

    chain_segments(edge_open, &mut edge_closed);
    if edge_closed.is_empty() {
        return Err("no closed Edge.Cuts board outline found".into());
    }
    edge_closed.sort_by(|a, b| polygon_area(b).total_cmp(&polygon_area(a)));
    let outline = edge_closed.remove(0);
    let cutouts = edge_closed;

    let mut net_classes = BTreeMap::new();
    let mut rules = DesignRules::default();
    if setup_pofv {
        rules.via_at_smd = true;
    }
    let mut net_class_assignments: HashMap<String, String> = HashMap::new();
    if let Some(pro_json) = pro_json_opt {
        parse_kicad_pro(
            pro_json,
            &mut rules,
            &mut net_classes,
            &mut net_class_assignments,
        )?;
    }

    let mut nets: BTreeMap<String, NetDef> = BTreeMap::new();
    for pl in &placements {
        for (pin, net_name) in &pl.pin_nets {
            let entry = nets.entry(net_name.clone()).or_insert_with(|| NetDef {
                name: net_name.clone(),
                class: net_class_assignments
                    .get(net_name)
                    .cloned()
                    .unwrap_or_else(default_net_class),
                pins: Vec::new(),
                ignore_route: false,
            });
            let pair = (pl.refdes.clone(), pin.clone());
            if !entry.pins.contains(&pair) {
                entry.pins.push(pair);
            }
        }
    }
    for net_name in tracks
        .iter()
        .map(|t| &t.net)
        .chain(vias.iter().map(|v| &v.net))
        .chain(pours.iter().map(|p| &p.net))
    {
        if !net_name.is_empty() {
            nets.entry(net_name.clone()).or_insert_with(|| NetDef {
                name: net_name.clone(),
                class: net_class_assignments
                    .get(net_name)
                    .cloned()
                    .unwrap_or_else(default_net_class),
                pins: Vec::new(),
                ignore_route: false,
            });
        }
    }

    Ok(BoardIr {
        name: if name.is_empty() {
            "kicad_board".into()
        } else {
            name
        },
        outline,
        cutouts,
        layers,
        footprints,
        placements,
        keepouts,
        pours,
        filled_pours,
        tracks,
        vias,
        net_classes,
        nets,
        rules,
        constraints: Constraints::default(),
        graphics,
    })
}

fn parse_board_zone(
    node: &SExp,
    net_id_map: &HashMap<i64, String>,
    keepouts: &mut Vec<Keepout>,
    pours: &mut Vec<PourSpec>,
    filled_pours: &mut Vec<FilledPour>,
) {
    if let Some(ko) = parse_keepout_zone(node, -1, None) {
        keepouts.push(ko);
        return;
    }
    let net = parse_net_ref(node, net_id_map);
    let layers = parse_layers_list(node, None);
    let mut outline = node
        .find_child("polygon")
        .map(|p| parse_pts(p, -1))
        .unwrap_or_default();
    if outline.len() < 3 {
        return;
    }
    ensure_ccw(&mut outline);
    let priority = node
        .find_child("priority")
        .and_then(|n| n.as_list()?.get(1)?.as_i64())
        .unwrap_or(0) as i32;
    let clearance_nm = node
        .find_child("connect_pads")
        .and_then(|cp| cp.find_child("clearance"))
        .and_then(|c| f64_at(c, 1))
        .map(mm);
    let thermal_relief = node
        .find_child("connect_pads")
        .and_then(|cp| atom_at(cp, 1))
        != Some("yes");
    let min_width_nm = node
        .find_child("min_thickness")
        .and_then(|n| f64_at(n, 1))
        .map(mm)
        .unwrap_or(200_000);
    let fill = node.find_child("fill");
    let thermal_gap_nm = fill
        .and_then(|f| f.find_child("thermal_gap"))
        .and_then(|n| f64_at(n, 1))
        .map(mm)
        .unwrap_or(250_000);
    let thermal_spoke_nm = fill
        .and_then(|f| f.find_child("thermal_bridge_width"))
        .and_then(|n| f64_at(n, 1))
        .map(mm)
        .unwrap_or(300_000);
    let remove_islands = fill
        .and_then(|f| f.find_child("island_removal_mode"))
        .and_then(|n| n.as_list()?.get(1)?.as_i64())
        != Some(1);
    let zone_name = child_atom(node, "name").unwrap_or("").to_string();

    for layer in &layers {
        pours.push(PourSpec {
            name: if zone_name.is_empty() {
                format!("{net}_{layer}")
            } else {
                zone_name.clone()
            },
            net: net.clone(),
            layer: layer.clone(),
            outline: outline.clone(),
            priority,
            clearance_nm,
            min_width_nm,
            thermal_relief,
            thermal_gap_nm,
            thermal_spoke_nm,
            remove_islands,
        });
    }

    let mut by_layer: BTreeMap<String, Vec<PourPoly>> = BTreeMap::new();
    for fp in node.children_with_tag("filled_polygon") {
        let Some(layer) = child_atom(fp, "layer") else {
            continue;
        };
        let mut outer = parse_pts(fp, -1);
        if outer.len() >= 3 {
            ensure_ccw(&mut outer);
            by_layer
                .entry(layer.to_string())
                .or_default()
                .push(PourPoly {
                    outer,
                    holes: Vec::new(),
                });
        }
    }
    for (layer, polys) in by_layer {
        filled_pours.push(FilledPour {
            net: net.clone(),
            layer,
            priority,
            polys,
        });
    }
}

fn parse_layers_block(node: &SExp) -> Vec<LayerDef> {
    let mut out = Vec::new();
    for item in node.as_list().into_iter().flatten().skip(1) {
        let Some(name) = atom_at(item, 1) else {
            continue;
        };
        let kind_str = atom_at(item, 2).unwrap_or("");
        if !name.ends_with(".Cu") && !matches!(kind_str, "signal" | "power" | "mixed") {
            continue;
        }
        out.push(LayerDef {
            name: name.to_string(),
            kind: match kind_str {
                "power" => LayerKind::Plane,
                "mixed" => LayerKind::Mixed,
                _ => LayerKind::Signal,
            },
            side: match name {
                "F.Cu" => Some(Side::Top),
                "B.Cu" => Some(Side::Bottom),
                _ => None,
            },
            thickness_nm: None,
        });
    }
    out
}

fn parse_setup(node: &SExp, thickness: &mut HashMap<String, Nm>, pofv: &mut bool) {
    if child_atom(node, "filling") == Some("yes") || child_atom(node, "capping") == Some("yes") {
        *pofv = true;
    }
    let Some(stackup) = node.find_child("stackup") else {
        return;
    };
    for child in stackup.children_with_tag("layer") {
        if let Some(name) = atom_at(child, 1)
            && child_atom(child, "type") == Some("copper")
            && let Some(t) = child.find_child("thickness").and_then(|n| f64_at(n, 1))
        {
            thickness.insert(name.to_string(), mm(t));
        }
    }
}

fn parse_footprint_prop(node: &SExp, key: &str, legacy_type: &str) -> Option<String> {
    for prop in node.children_with_tag("property") {
        if atom_at(prop, 1) == Some(key) {
            return atom_at(prop, 2).map(str::to_string);
        }
    }
    for txt in node.children_with_tag("fp_text") {
        if atom_at(txt, 1) == Some(legacy_type) {
            return atom_at(txt, 2).map(str::to_string);
        }
    }
    None
}

fn parse_placed_footprint(
    node: &SExp,
    net_id_map: &HashMap<i64, String>,
    footprints: &mut BTreeMap<String, FootprintDef>,
    placements: &mut Vec<Placement>,
    edge_open: &mut Vec<Vec<Pt>>,
    edge_closed: &mut Vec<Vec<Pt>>,
) -> Result<(), String> {
    let (fp_def, pad_nets) = parse_footprint_node(node, true, net_id_map)?;
    let side = if child_atom(node, "layer") == Some("B.Cu") {
        Side::Bottom
    } else {
        Side::Top
    };
    let (pos, rot_deg) = node
        .find_child("at")
        .and_then(|at| parse_at(at, -1))
        .unwrap_or((Pt::new(0, 0), 0.0));
    let mut refdes = parse_footprint_prop(node, "Reference", "reference").unwrap_or_default();
    if refdes.is_empty() || placements.iter().any(|p| p.refdes == refdes) {
        let base = if refdes.is_empty() {
            "FP".to_string()
        } else {
            refdes
        };
        refdes = format!("{base}#{}", placements.len() + 1);
    }
    let value = parse_footprint_prop(node, "Value", "value").unwrap_or_default();

    for child in node.as_list().into_iter().flatten().skip(1) {
        if child_atom(child, "layer") == Some("Edge.Cuts") {
            let mut local_open = Vec::new();
            let mut local_closed = Vec::new();
            let y_sign = if side == Side::Bottom { 1 } else { -1 };
            collect_poly_or_segment(child, y_sign, &mut local_open, &mut local_closed);
            for pts in local_open {
                edge_open.push(
                    pts.into_iter()
                        .map(|p| local_to_board(p, pos.x, pos.y, rot_deg, side))
                        .collect(),
                );
            }
            for pts in local_closed {
                edge_closed.push(
                    pts.into_iter()
                        .map(|p| local_to_board(p, pos.x, pos.y, rot_deg, side))
                        .collect(),
                );
            }
        }
    }

    let fp_key = match footprints.get(&fp_def.name) {
        Some(existing)
            if existing.pads == fp_def.pads
                && existing.courtyard == fp_def.courtyard
                && existing.graphics == fp_def.graphics
                && existing.keepouts == fp_def.keepouts
                && existing.models == fp_def.models =>
        {
            fp_def.name.clone()
        }
        Some(_) => {
            let unique = format!("{}#{refdes}", fp_def.name);
            let mut cloned = fp_def;
            cloned.name = unique.clone();
            footprints.insert(unique.clone(), cloned);
            unique
        }
        None => {
            let key = fp_def.name.clone();
            footprints.insert(key.clone(), fp_def);
            key
        }
    };

    let mut dnp = false;
    let mut exclude_from_bom = false;
    let mut exclude_from_pos = false;
    for attr in node.children_with_tag("attr") {
        for tok in attr.as_list().into_iter().flatten().skip(1) {
            match tok.as_atom() {
                Some("dnp") => dnp = true,
                Some("exclude_from_bom") => exclude_from_bom = true,
                Some("exclude_from_pos_files") => exclude_from_pos = true,
                Some("virtual") => {
                    exclude_from_bom = true;
                    exclude_from_pos = true;
                }
                _ => {}
            }
        }
    }

    let mut pin_nets = BTreeMap::new();
    for (pin, net) in pad_nets {
        pin_nets.insert(pin, net);
    }
    placements.push(Placement {
        refdes,
        footprint: fp_key,
        value,
        pos,
        rot_deg: rot_deg.rem_euclid(360.0),
        side,
        locked: has_flag(node, "locked"),
        pin_nets,
        mpn: None,
        lcsc: None,
        dnp,
        exclude_from_bom,
        exclude_from_pos,
        cpl_rotation_offset_deg: 0.0,
    });
    Ok(())
}

fn parse_segment(node: &SExp, net_id_map: &HashMap<i64, String>) -> Result<Track, String> {
    Ok(Track {
        net: parse_net_ref(node, net_id_map),
        layer: child_atom(node, "layer").unwrap_or("F.Cu").to_string(),
        a: node
            .find_child("start")
            .and_then(|n| parse_pt(n, -1))
            .ok_or("segment missing start")?,
        b: node
            .find_child("end")
            .and_then(|n| parse_pt(n, -1))
            .ok_or("segment missing end")?,
        width: node
            .find_child("width")
            .and_then(|n| f64_at(n, 1))
            .map(mm)
            .unwrap_or(0),
        locked: has_flag(node, "locked"),
    })
}

fn parse_track_arc(node: &SExp, net_id_map: &HashMap<i64, String>) -> Result<Vec<Track>, String> {
    let s = node
        .find_child("start")
        .and_then(|n| parse_pt(n, -1))
        .ok_or("arc missing start")?;
    let m = node
        .find_child("mid")
        .and_then(|n| parse_pt(n, -1))
        .ok_or("arc missing mid")?;
    let e = node
        .find_child("end")
        .and_then(|n| parse_pt(n, -1))
        .ok_or("arc missing end")?;
    let width = node
        .find_child("width")
        .and_then(|n| f64_at(n, 1))
        .map(mm)
        .unwrap_or(0);
    let layer = child_atom(node, "layer").unwrap_or("F.Cu").to_string();
    let net = parse_net_ref(node, net_id_map);
    let locked = has_flag(node, "locked");
    Ok(discretize_arc(s, m, e)
        .windows(2)
        .map(|w| Track {
            net: net.clone(),
            layer: layer.clone(),
            a: w[0],
            b: w[1],
            width,
            locked,
        })
        .collect())
}

fn parse_via(node: &SExp, net_id_map: &HashMap<i64, String>) -> Result<Via, String> {
    let layers_vec = parse_layers_list(node, None);
    let layers = match layers_vec.as_slice() {
        [a, b, ..] => (a.clone(), b.clone()),
        [a] => (a.clone(), a.clone()),
        _ => ("F.Cu".into(), "B.Cu".into()),
    };
    Ok(Via {
        net: parse_net_ref(node, net_id_map),
        pos: node
            .find_child("at")
            .and_then(|n| parse_pt(n, -1))
            .ok_or("via missing at")?,
        diameter: node
            .find_child("size")
            .and_then(|n| f64_at(n, 1))
            .map(mm)
            .unwrap_or(0),
        drill: node
            .find_child("drill")
            .and_then(|n| f64_at(n, 1))
            .map(mm)
            .unwrap_or(0),
        layers,
        locked: has_flag(node, "locked"),
        tented: true,
    })
}
