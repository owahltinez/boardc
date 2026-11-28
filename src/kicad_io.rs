use std::collections::{BTreeMap, HashMap};
use std::f64::consts::TAU;
use std::fs;
use std::path::Path;

use clipper2_rust::{FillRule, union_subjects_64};
use viahop::sexp::SExp;

use crate::geom::{board_to_local, ensure_ccw, polygon_area};
use crate::ir::{
    FootprintDef, Keepout, KeepoutKind, Model3d, Nm, PadDef, PadKind, PadShape, Pt, Side, mm,
};
use crate::kicad_geom::{
    atom_at, chain_segments, child_atom, collect_poly_or_segment, f64_at, parse_at, parse_fill,
    parse_graphic_node, parse_kicad_sexp, parse_layers_list, parse_net_ref, parse_pt, parse_pts,
    parse_stroke_width,
};
pub use crate::kicad_net::{
    KicadNetlist, NetlistComponent, NetlistNet, load_kicad_net, parse_kicad_net,
};
pub use crate::kicad_pcb::{import_kicad_pcb, parse_kicad_pcb};
use crate::shapes::{capsule_poly, circle_poly, paths64_to_polys, polys_to_paths64, rect_poly};

/// Loads and parses a `.kicad_mod` footprint file into canonical Y-up local coordinates.
pub fn load_kicad_mod(path: &Path) -> Result<FootprintDef, String> {
    let src = fs::read_to_string(path)
        .map_err(|e| format!("failed to read footprint '{}': {e}", path.display()))?;
    parse_kicad_mod(&src)
}

/// Parses `.kicad_mod` S-expression text into canonical Y-up `FootprintDef`.
pub fn parse_kicad_mod(src: &str) -> Result<FootprintDef, String> {
    let root = parse_kicad_sexp(src)?;
    let tag = root
        .tag()
        .ok_or("expected S-expression list in .kicad_mod")?;
    if tag != "footprint" && tag != "module" {
        return Err(format!("expected '(footprint ...)', got '({tag} ...)'"));
    }
    let empty_nets = HashMap::new();
    let (fp, _) = parse_footprint_node(&root, false, &empty_nets)?;
    Ok(fp)
}

pub(crate) fn parse_footprint_node(
    node: &SExp,
    is_placed_pcb: bool,
    net_id_map: &HashMap<i64, String>,
) -> Result<(FootprintDef, Vec<(String, String)>), String> {
    let fp_name = atom_at(node, 1).unwrap_or("unnamed").to_string();
    let side = if child_atom(node, "layer") == Some("B.Cu") {
        Side::Bottom
    } else {
        Side::Top
    };
    let (fp_pos, fp_rot) = node
        .find_child("at")
        .and_then(|at| parse_at(at, -1))
        .unwrap_or((Pt::new(0, 0), 0.0));
    // In .kicad_pcb, B.Cu footprints already store Y-flipped local coords and B.* layers.
    let unflip_bottom = is_placed_pcb && side == Side::Bottom;
    let y_sign: i64 = if unflip_bottom { 1 } else { -1 };
    let layer_norm = unflip_bottom.then_some(Side::Bottom);

    let fp_mask_margin = node
        .find_child("solder_mask_margin")
        .and_then(|n| f64_at(n, 1))
        .map(mm);
    let mut pads = Vec::new();
    let mut pad_nets = Vec::new();
    let mut graphics = Vec::new();
    let mut models = Vec::new();
    let mut keepouts = Vec::new();
    let mut crtyd_open = Vec::new();
    let mut crtyd_closed = Vec::new();

    for child in node.as_list().into_iter().flatten().skip(1) {
        match child.tag() {
            Some("pad") => {
                let (mut pad, net) = parse_pad(
                    child,
                    y_sign,
                    fp_rot,
                    unflip_bottom,
                    is_placed_pcb,
                    net_id_map,
                )?;
                pad.mask_margin_nm = pad.mask_margin_nm.or(fp_mask_margin);
                if !pad.name.is_empty() && !net.is_empty() {
                    pad_nets.push((pad.name.clone(), net));
                }
                pads.push(pad);
            }
            Some("fp_line") | Some("fp_arc") | Some("fp_rect") | Some("fp_circle")
            | Some("fp_poly") | Some("fp_text") => {
                let raw_layer = child_atom(child, "layer").unwrap_or("");
                if raw_layer == "F.CrtYd" || raw_layer == "B.CrtYd" {
                    collect_poly_or_segment(child, y_sign, &mut crtyd_open, &mut crtyd_closed);
                } else if raw_layer != "Edge.Cuts" {
                    graphics.extend(parse_graphic_node(
                        child,
                        y_sign,
                        layer_norm,
                        is_placed_pcb.then_some(fp_rot),
                    ));
                }
            }
            Some("zone") => {
                if let Some(mut ko) = parse_keepout_zone(child, layer_norm) {
                    if is_placed_pcb {
                        for p in &mut ko.poly {
                            *p = board_to_local(*p, fp_pos.x, fp_pos.y, fp_rot, side);
                        }
                        ensure_ccw(&mut ko.poly);
                    }
                    keepouts.push(ko);
                }
            }
            Some("model") => {
                if let Some(m) = parse_model_3d(child) {
                    models.push(m);
                }
            }
            _ => {}
        }
    }

    chain_segments(crtyd_open, &mut crtyd_closed);
    let courtyard = crtyd_closed
        .into_iter()
        .max_by(|a, b| polygon_area(a).total_cmp(&polygon_area(b)))
        .unwrap_or_default();

    Ok((
        FootprintDef {
            name: fp_name,
            pads,
            courtyard,
            graphics,
            keepouts,
            models,
            pin_types: BTreeMap::new(),
        },
        pad_nets,
    ))
}

fn parse_xyz(parent: &SExp, child_tag: &str, default: [f64; 3]) -> [f64; 3] {
    let Some(sub) = parent.find_child(child_tag) else {
        return default;
    };
    let Some(xyz) = sub.find_child("xyz") else {
        return default;
    };
    [
        f64_at(xyz, 1).unwrap_or(default[0]),
        f64_at(xyz, 2).unwrap_or(default[1]),
        f64_at(xyz, 3).unwrap_or(default[2]),
    ]
}

fn parse_model_3d(node: &SExp) -> Option<Model3d> {
    let path = atom_at(node, 1)?.trim();
    if path.is_empty() {
        return None;
    }
    Some(Model3d {
        path: path.to_string(),
        offset_mm: parse_xyz(node, "offset", [0.0, 0.0, 0.0]),
        scale: parse_xyz(node, "scale", [1.0, 1.0, 1.0]),
        rotate_deg: parse_xyz(node, "rotate", [0.0, 0.0, 0.0]),
    })
}

fn parse_pad(
    node: &SExp,
    y_sign: i64,
    fp_rot: f64,
    unflip_bottom: bool,
    is_placed_pcb: bool,
    net_id_map: &HashMap<i64, String>,
) -> Result<(PadDef, String), String> {
    let name = atom_at(node, 1).unwrap_or("").to_string();
    let kind = match atom_at(node, 2) {
        Some("smd") | Some("connect") => PadKind::Smd,
        Some("thru_hole") => PadKind::Tht,
        Some("np_thru_hole") => PadKind::Npth,
        other => return Err(format!("unsupported pad kind '{other:?}'")),
    };
    let shape_str = atom_at(node, 3).unwrap_or("rect");
    let (pos, raw_rot) = node
        .find_child("at")
        .and_then(|at| parse_at(at, y_sign))
        .unwrap_or((Pt::new(0, 0), 0.0));
    let rot_deg = if !is_placed_pcb {
        raw_rot.rem_euclid(360.0)
    } else if unflip_bottom {
        (fp_rot - raw_rot).rem_euclid(360.0)
    } else {
        (raw_rot - fp_rot).rem_euclid(360.0)
    };
    let (w_mm, h_mm) = node
        .find_child("size")
        .map(|s| (f64_at(s, 1).unwrap_or(0.0), f64_at(s, 2).unwrap_or(0.0)))
        .unwrap_or((0.0, 0.0));

    let shape = match shape_str {
        "circle" => PadShape::Circle { diameter: mm(w_mm) },
        "rect" | "trapezoid" => PadShape::Rect {
            width: mm(w_mm),
            height: mm(h_mm),
        },
        "roundrect" => {
            let rratio = node
                .find_child("roundrect_rratio")
                .and_then(|n| f64_at(n, 1))
                .unwrap_or(0.25);
            PadShape::RoundRect {
                width: mm(w_mm),
                height: mm(h_mm),
                corner_radius: mm(w_mm.min(h_mm) * rratio),
            }
        }
        "oval" => PadShape::Oval {
            width: mm(w_mm),
            height: mm(h_mm),
        },
        "custom" => parse_custom_pad_shape(node, y_sign, mm(w_mm), mm(h_mm)),
        other => return Err(format!("unsupported pad shape '{other}'")),
    };

    let drill_nm = node.find_child("drill").and_then(|d| {
        let nums: Vec<f64> = d
            .as_list()?
            .iter()
            .skip(1)
            .filter_map(SExp::as_f64)
            .collect();
        match nums.as_slice() {
            [dia] => Some(mm(*dia)),
            [d1, d2, ..] => Some(mm(d1.min(*d2))),
            _ => None,
        }
    });
    let layers = parse_layers_list(node, unflip_bottom.then_some(Side::Bottom));
    let net = parse_net_ref(node, net_id_map);
    let mask_margin_nm = node
        .find_child("solder_mask_margin")
        .and_then(|n| f64_at(n, 1))
        .map(mm);
    let paste_margin_nm = node
        .find_child("solder_paste_margin")
        .and_then(|n| f64_at(n, 1))
        .map(mm);
    Ok((
        PadDef {
            name,
            kind,
            shape,
            pos,
            rot_deg,
            drill_nm,
            layers,
            mask_margin_nm,
            paste_margin_nm,
        },
        net,
    ))
}

fn parse_custom_pad_shape(node: &SExp, y_sign: i64, fallback_w: Nm, fallback_h: Nm) -> PadShape {
    let Some(prims) = node.find_child("primitives").and_then(SExp::as_list) else {
        return PadShape::Rect {
            width: fallback_w,
            height: fallback_h,
        };
    };
    let mut polys: Vec<Vec<Pt>> = Vec::new();
    if let Some(anchor) = node
        .find_child("options")
        .and_then(|opt| child_atom(opt, "anchor"))
        && fallback_w > 0
        && fallback_h > 0
    {
        match anchor {
            "circle" => polys.push(circle_poly(
                Pt::new(0, 0),
                fallback_w.min(fallback_h) / 2,
                16,
            )),
            "rect" => polys.push(rect_poly(fallback_w, fallback_h)),
            _ => {}
        }
    }
    for prim in prims.iter().skip(1) {
        match prim.tag() {
            Some("gr_poly") => {
                let mut pts = parse_pts(prim, y_sign);
                if pts.len() >= 3 {
                    ensure_ccw(&mut pts);
                    polys.push(pts);
                }
            }
            Some("gr_rect") => {
                if let (Some(s), Some(e)) = (
                    prim.find_child("start").and_then(|n| parse_pt(n, y_sign)),
                    prim.find_child("end").and_then(|n| parse_pt(n, y_sign)),
                ) {
                    polys.push(vec![
                        Pt::new(s.x.min(e.x), s.y.min(e.y)),
                        Pt::new(s.x.max(e.x), s.y.min(e.y)),
                        Pt::new(s.x.max(e.x), s.y.max(e.y)),
                        Pt::new(s.x.min(e.x), s.y.max(e.y)),
                    ]);
                }
            }
            Some("gr_circle") => {
                if let (Some(c), Some(e)) = (
                    prim.find_child("center").and_then(|n| parse_pt(n, y_sign)),
                    prim.find_child("end").and_then(|n| parse_pt(n, y_sign)),
                ) {
                    let r = c.dist(e).round() as Nm;
                    let w = parse_stroke_width(prim);
                    let fill = parse_fill(prim);
                    if !fill && w > 0 && r > w / 2 {
                        polys.push(annulus_slit_poly(c, r - w / 2, r + w / 2, 32));
                    } else {
                        polys.push(circle_poly(c, r + if fill { 0 } else { w / 2 }, 32));
                    }
                }
            }
            Some("gr_line") => {
                if let (Some(a), Some(b)) = (
                    prim.find_child("start").and_then(|n| parse_pt(n, y_sign)),
                    prim.find_child("end").and_then(|n| parse_pt(n, y_sign)),
                ) {
                    polys.push(capsule_poly(a, b, (parse_stroke_width(prim) / 2).max(1), 8));
                }
            }
            _ => {}
        }
    }
    if polys.len() > 1 {
        let unioned = union_subjects_64(&polys_to_paths64(&polys), FillRule::NonZero);
        if let Some(verts) = paths64_to_polys(&unioned)
            .into_iter()
            .max_by(|a, b| polygon_area(a).total_cmp(&polygon_area(b)))
        {
            return PadShape::Poly { verts };
        }
    }
    match polys.into_iter().next() {
        Some(verts) => PadShape::Poly { verts },
        None => PadShape::Rect {
            width: fallback_w,
            height: fallback_h,
        },
    }
}

fn annulus_slit_poly(center: Pt, r_in: Nm, r_out: Nm, segs: usize) -> Vec<Pt> {
    let eps = 1e-3;
    let mut verts = Vec::with_capacity(2 * (segs + 1));
    for i in 0..=segs {
        let a = eps + (TAU - 2.0 * eps) * (i as f64 / segs as f64);
        verts.push(Pt::new(
            center.x + (r_out as f64 * a.cos()).round() as Nm,
            center.y + (r_out as f64 * a.sin()).round() as Nm,
        ));
    }
    for i in (0..=segs).rev() {
        let a = eps + (TAU - 2.0 * eps) * (i as f64 / segs as f64);
        verts.push(Pt::new(
            center.x + (r_in as f64 * a.cos()).round() as Nm,
            center.y + (r_in as f64 * a.sin()).round() as Nm,
        ));
    }
    verts
}

pub(crate) fn parse_keepout_zone(node: &SExp, layer_norm: Option<Side>) -> Option<Keepout> {
    let ko = node.find_child("keepout")?;
    let tracks_no = child_atom(ko, "tracks") == Some("not_allowed");
    let vias_no = child_atom(ko, "vias") == Some("not_allowed");
    let pour_no = child_atom(ko, "copperpour") == Some("not_allowed");
    let fp_no = child_atom(ko, "footprints") == Some("not_allowed");
    let kind = match (tracks_no, vias_no, pour_no, fp_no) {
        (true, false, false, false) => KeepoutKind::Track,
        (false, true, false, false) => KeepoutKind::Via,
        (false, false, true, false) => KeepoutKind::Copper,
        (false, false, false, true) => KeepoutKind::Placement,
        (true, true, true, true) | (false, false, false, false) => KeepoutKind::All,
        (tracks, vias, copper, placement) => KeepoutKind::Custom {
            tracks,
            vias,
            copper,
            placement,
        },
    };
    let mut poly = node
        .find_child("polygon")
        .map(|p| parse_pts(p, -1))
        .unwrap_or_default();
    if poly.len() < 3 {
        return None;
    }
    ensure_ccw(&mut poly);
    Some(Keepout {
        kind,
        layers: parse_layers_list(node, layer_norm),
        poly,
    })
}
