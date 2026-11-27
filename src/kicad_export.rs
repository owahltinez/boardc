use std::collections::{BTreeMap, HashSet};
use std::fmt::Write as _;
use std::fs;
use std::path::Path;

use serde_json::json;

use crate::geom::{ensure_ccw, polygon_signed_area_2};
use crate::ir::{BoardIr, LayerKind, Nm, PourPoly, Pt, SOLDER_MASK_THICKNESS_NM, Side, nm_to_mm};
use crate::kicad_export_items::{
    UuidGen, esc, write_closed_poly_lines, write_footprint, write_graphic, write_keepout, write_pts,
};
use crate::zone_scene::match_pour_specs;

fn collect_net_ids(board: &BoardIr) -> BTreeMap<String, usize> {
    board
        .all_net_names()
        .into_iter()
        .enumerate()
        .map(|(i, n)| (n, i + 1))
        .collect()
}

fn kicad_copper_layer_id(
    name: &str,
    used_ids: &mut HashSet<usize>,
    next_inner: &mut usize,
) -> usize {
    let fixed = if name == "F.Cu" {
        Some(0)
    } else if name == "B.Cu" {
        Some(2)
    } else if let Some(rest) = name.strip_prefix("In").and_then(|s| s.strip_suffix(".Cu"))
        && let Ok(n) = rest.parse::<usize>()
        && n >= 1
    {
        Some(2 + 2 * n)
    } else {
        None
    };
    if let Some(id) = fixed {
        used_ids.insert(id);
        return id;
    }
    loop {
        let id = 4 + 2 * *next_inner;
        *next_inner += 1;
        if used_ids.insert(id) {
            return id;
        }
    }
}

fn write_layers(out: &mut String, board: &BoardIr) {
    out.push_str("\t(layers\n");
    let layers = board.effective_layers();
    // Pre-reserve standard InN.Cu IDs so non-standard custom inner layer names never collide with ID 4.
    let mut used_ids: HashSet<usize> = HashSet::new();
    for l in &layers {
        if l.name == "F.Cu" {
            used_ids.insert(0);
        } else if l.name == "B.Cu" {
            used_ids.insert(2);
        } else if let Some(rest) = l
            .name
            .strip_prefix("In")
            .and_then(|s| s.strip_suffix(".Cu"))
            && let Ok(n) = rest.parse::<usize>()
            && n >= 1
        {
            used_ids.insert(2 + 2 * n);
        }
    }
    let mut next_inner = 0usize;
    for l in &layers {
        let id = kicad_copper_layer_id(&l.name, &mut used_ids, &mut next_inner);
        let kind = match l.kind {
            LayerKind::Signal => "signal",
            LayerKind::Plane => "power",
            LayerKind::Mixed => "mixed",
        };
        let _ = writeln!(out, "\t\t({id} \"{}\" {kind})", esc(&l.name));
    }
    for (id, name, extra) in [
        (9, "F.Adhes", " \"F.Adhesive\""),
        (11, "B.Adhes", " \"B.Adhesive\""),
        (13, "F.Paste", ""),
        (15, "B.Paste", ""),
        (5, "F.SilkS", " \"F.Silkscreen\""),
        (7, "B.SilkS", " \"B.Silkscreen\""),
        (1, "F.Mask", ""),
        (3, "B.Mask", ""),
        (17, "Dwgs.User", ""),
        (19, "Cmts.User", ""),
        (25, "Edge.Cuts", ""),
        (27, "Margin", ""),
        (31, "F.CrtYd", " \"F.Courtyard\""),
        (29, "B.CrtYd", " \"B.Courtyard\""),
        (35, "F.Fab", ""),
        (33, "B.Fab", ""),
    ] {
        let _ = writeln!(out, "\t\t({id} \"{name}\" user{extra})");
    }
    out.push_str("\t)\n");
}

fn write_setup(out: &mut String, board: &BoardIr) {
    let (copper, diel_nm) = board.resolved_stackup_thicknesses();
    let mask_mm = nm_to_mm(SOLDER_MASK_THICKNESS_NM);
    out.push_str("\t(setup\n");
    out.push_str("\t\t(stackup\n");
    out.push_str("\t\t\t(layer \"F.SilkS\" (type \"Top Silk Screen\"))\n");
    out.push_str("\t\t\t(layer \"F.Paste\" (type \"Top Solder Paste\"))\n");
    let _ = writeln!(
        out,
        "\t\t\t(layer \"F.Mask\" (type \"Top Solder Mask\") (thickness {mask_mm}))"
    );
    for (idx, &(layer_name, cu_thick)) in copper.iter().enumerate() {
        let _ = writeln!(
            out,
            "\t\t\t(layer \"{}\" (type \"copper\") (thickness {}))",
            esc(layer_name),
            nm_to_mm(cu_thick)
        );
        if let Some(&d_nm) = diel_nm.get(idx) {
            let kind = if diel_nm.len() == 1 || idx % 2 == 1 {
                "core"
            } else {
                "prepreg"
            };
            let _ = writeln!(
                out,
                "\t\t\t(layer \"dielectric {}\" (type \"{kind}\") (thickness {}) (material \"FR4\") (epsilon_r 4.5) (loss_tangent 0.02))",
                idx + 1,
                nm_to_mm(d_nm)
            );
        }
    }
    let _ = writeln!(
        out,
        "\t\t\t(layer \"B.Mask\" (type \"Bottom Solder Mask\") (thickness {mask_mm}))"
    );
    out.push_str("\t\t\t(layer \"B.Paste\" (type \"Bottom Solder Paste\"))\n");
    out.push_str("\t\t\t(layer \"B.SilkS\" (type \"Bottom Silk Screen\"))\n");
    if let Some(finish) = board.stackup.finish_trimmed() {
        let _ = writeln!(out, "\t\t\t(copper_finish \"{}\")", esc(finish));
    }
    out.push_str("\t\t\t(dielectric_constraints no)\n");
    out.push_str("\t\t)\n");
    let _ = writeln!(
        out,
        "\t\t(pad_to_mask_clearance {:.6})",
        nm_to_mm(board.rules.mask_expansion_nm)
    );
    out.push_str("\t\t(allow_soldermask_bridges_in_footprints no)\n");
    let has_tented = board.vias.iter().any(|v| v.tented);
    let has_untented = board.vias.iter().any(|v| !v.tented);
    if board.pofv_enabled() || (has_tented && has_untented) {
        let yn = if board.pofv_enabled() || has_tented {
            "yes"
        } else {
            "no"
        };
        let _ = writeln!(out, "\t\t(tenting (front {yn}) (back {yn}))");
    } else if has_tented {
        out.push_str("\t\t(tenting front back)\n");
    } else if has_untented {
        out.push_str("\t\t(tenting none)\n");
    }
    if board.pofv_enabled() {
        out.push_str("\t\t(plugging (front yes) (back yes))\n");
        out.push_str("\t\t(capping yes)\n");
        out.push_str("\t\t(filling yes)\n");
    }
    out.push_str("\t)\n");
}

// Fractures a `PourPoly` (outer + holes) into a single KiCad keyhole-slit polygon.
fn fracture_pour_poly(poly: &PourPoly) -> Vec<Pt> {
    let mut ring = poly.outer.clone();
    if ring.len() < 3 || poly.holes.is_empty() {
        return ring;
    }
    ensure_ccw(&mut ring);
    let mut holes: Vec<(Vec<Pt>, usize, Pt)> = poly
        .holes
        .iter()
        .filter(|h| h.len() >= 3)
        .map(|h| {
            let mut cw = h.clone();
            if polygon_signed_area_2(&cw) > 0 {
                cw.reverse();
            }
            let (m_idx, &m_pt) = cw
                .iter()
                .enumerate()
                .max_by_key(|(_, p)| (p.x, p.y))
                .unwrap();
            (cw, m_idx, m_pt)
        })
        .collect();
    // Process rightmost holes first so no unprocessed hole can block the +X bridge ray.
    holes.sort_by_key(|h| std::cmp::Reverse((h.2.x, h.2.y)));

    for (h, m_idx, m) in holes {
        let n = ring.len();
        let mut best: Option<(f64, bool, usize, Option<Pt>)> = None;
        for i in 0..n {
            let a = ring[i];
            let b = ring[(i + 1) % n];
            if a.y == m.y && a.x >= m.x {
                let cand = (a.x as f64, false, i, None);
                if best.is_none_or(|cur| cand.0 < cur.0 || (cand.0 == cur.0 && !cand.1 && cur.1)) {
                    best = Some(cand);
                }
            }
            if (a.y > m.y) != (b.y > m.y) {
                let ix = a.x as f64 + (m.y - a.y) as f64 * (b.x - a.x) as f64 / (b.y - a.y) as f64;
                if ix >= m.x as f64 - 1e-6 {
                    let rx = ix.max(m.x as f64);
                    let cand = (rx, true, i, Some(Pt::new(rx.round() as Nm, m.y)));
                    if best.is_none_or(|cur| cand.0 < cur.0) {
                        best = Some(cand);
                    }
                }
            }
        }
        let Some((_, _, edge_idx, split_pt)) = best else {
            continue;
        };
        let hl = h.len();
        let mut splice = Vec::with_capacity(hl + 3);
        if let Some(ip) = split_pt {
            splice.push(ip);
        }
        for k in 0..=hl {
            splice.push(h[(m_idx + k) % hl]);
        }
        splice.push(split_pt.unwrap_or(ring[edge_idx]));

        let mut next = Vec::with_capacity(n + splice.len());
        next.extend_from_slice(&ring[..=edge_idx]);
        next.extend(splice);
        next.extend_from_slice(&ring[(edge_idx + 1)..]);
        next.dedup();
        if next.len() > 1 && next.first() == next.last() {
            next.pop();
        }
        ring = next;
    }
    ring
}

/// Renders `BoardIr` into a KiCad `.kicad_pcb` S-expression string (Y-up -> Y-down).
pub fn render_kicad_pcb(board: &BoardIr) -> String {
    let mut out = String::with_capacity(16 * 1024);
    let mut uuids = UuidGen(0);
    let net_ids = collect_net_ids(board);

    let has_mixed_vias =
        board.vias.iter().any(|v| v.tented) && board.vias.iter().any(|v| !v.tented);
    let use_kicad10 = board.pofv_enabled() || has_mixed_vias;
    let kicad_version = if use_kicad10 { "20260206" } else { "20241229" };
    out.push_str("(kicad_pcb\n");
    let _ = writeln!(out, "\t(version {kicad_version})");
    out.push_str("\t(generator \"boardc\")\n");
    out.push_str(concat!(
        "\t(generator_version \"",
        env!("CARGO_PKG_VERSION"),
        "\")\n"
    ));
    let _ = writeln!(
        out,
        "\t(general\n\t\t(thickness {})\n\t)",
        nm_to_mm(board.effective_board_thickness_nm())
    );
    out.push_str("\t(paper \"A4\")\n");
    let _ = writeln!(
        out,
        "\t(title_block\n\t\t(title \"{}\")\n\t)",
        esc(&board.name)
    );
    write_layers(&mut out, board);
    write_setup(&mut out, board);
    out.push_str("\t(net 0 \"\")\n");
    for (name, id) in &net_ids {
        let _ = writeln!(out, "\t(net {id} \"{}\")", esc(name));
    }

    write_closed_poly_lines(
        &mut out,
        "gr_line",
        "Edge.Cuts",
        &board.outline,
        50_000,
        -1,
        &mut uuids,
    );
    for cut in &board.cutouts {
        write_closed_poly_lines(
            &mut out,
            "gr_line",
            "Edge.Cuts",
            cut,
            50_000,
            -1,
            &mut uuids,
        );
    }
    for g in &board.graphics {
        write_graphic(&mut out, "gr", g, -1, Side::Top, "\t", &mut uuids);
    }

    for pl in &board.placements {
        write_footprint(&mut out, pl, board, &net_ids, &mut uuids);
    }

    for tr in &board.tracks {
        let net_id = net_ids.get(&tr.net).copied().unwrap_or(0);
        let _ = writeln!(
            out,
            "\t(segment (start {:.6} {:.6}) (end {:.6} {:.6}) (width {:.6}) (layer \"{}\") (net {net_id}) (uuid \"{}\"))",
            nm_to_mm(tr.a.x),
            nm_to_mm(-tr.a.y),
            nm_to_mm(tr.b.x),
            nm_to_mm(-tr.b.y),
            nm_to_mm(tr.width),
            esc(&tr.layer),
            uuids.next(),
        );
    }

    let cu_layers = board.copper_layer_names();
    for via in &board.vias {
        let net_id = net_ids.get(&via.net).copied().unwrap_or(0);
        let kind = if via.is_through_hole(&cu_layers, &board.layers) {
            ""
        } else {
            "blind "
        };
        let tent = if use_kicad10 {
            if via.tented {
                " (tenting (front yes) (back yes))"
            } else {
                " (tenting (front no) (back no))"
            }
        } else {
            ""
        };
        let _ = writeln!(
            out,
            "\t(via {kind}(at {:.6} {:.6}) (size {:.6}) (drill {:.6}) (layers \"{}\" \"{}\"){tent} (net {net_id}) (uuid \"{}\"))",
            nm_to_mm(via.pos.x),
            nm_to_mm(-via.pos.y),
            nm_to_mm(via.diameter),
            nm_to_mm(via.drill),
            esc(&via.layers.0),
            esc(&via.layers.1),
            uuids.next(),
        );
    }

    for ko in &board.keepouts {
        write_keepout(&mut out, ko, -1, Side::Top, "\t", &mut uuids);
    }

    let matched = match_pour_specs(board, &board.filled_pours);
    let mut filled_for_spec: BTreeMap<usize, Vec<&PourPoly>> = BTreeMap::new();
    for (fp, spec_opt) in board.filled_pours.iter().zip(matched) {
        if let Some(sp) = spec_opt
            && let Some(idx) = board.pours.iter().position(|p| std::ptr::eq(p, sp))
        {
            filled_for_spec.entry(idx).or_default().extend(&fp.polys);
        }
    }

    for (pour_idx, pour) in board.pours.iter().enumerate() {
        if pour.outline.len() < 3 {
            continue;
        }
        let net_id = net_ids.get(&pour.net).copied().unwrap_or(0);
        let clr = pour.clearance_nm.unwrap_or(board.rules.clearance_nm);
        let connect = if pour.thermal_relief { "" } else { "yes " };
        let irm = if pour.remove_islands { 0 } else { 1 };
        let _ = writeln!(out, "\t(zone");
        let _ = writeln!(out, "\t\t(net {net_id})");
        let _ = writeln!(out, "\t\t(net_name \"{}\")", esc(&pour.net));
        let _ = writeln!(out, "\t\t(layer \"{}\")", esc(&pour.layer));
        let _ = writeln!(out, "\t\t(uuid \"{}\")", uuids.next());
        let _ = writeln!(out, "\t\t(hatch edge 0.5)");
        if pour.priority > 0 {
            let _ = writeln!(out, "\t\t(priority {})", pour.priority);
        }
        let _ = writeln!(
            out,
            "\t\t(connect_pads {connect}(clearance {:.6}))",
            nm_to_mm(clr)
        );
        let filled_polys = filled_for_spec.get(&pour_idx);
        let has_filled = filled_polys.is_some_and(|ps| !ps.is_empty());
        // Pre-filled polygons already store exact copper edges; min_thickness > 0 inflates them in KiCad.
        let min_thick_mm = if has_filled {
            0.0
        } else {
            nm_to_mm(pour.min_width_nm.max(10_000))
        };
        let _ = writeln!(out, "\t\t(min_thickness {min_thick_mm:.6})");
        let _ = writeln!(
            out,
            "\t\t(fill yes (thermal_gap {:.6}) (thermal_bridge_width {:.6}) (island_removal_mode {irm}))",
            nm_to_mm(pour.thermal_gap_nm),
            nm_to_mm(pour.thermal_spoke_nm)
        );
        out.push_str("\t\t(polygon\n");
        write_pts(&mut out, &pour.outline, -1, "\t\t\t");
        out.push_str("\t\t)\n");
        if let Some(polys) = filled_polys {
            for poly in polys {
                let ring = fracture_pour_poly(poly);
                if ring.len() >= 3 {
                    let _ = writeln!(out, "\t\t(filled_polygon");
                    let _ = writeln!(out, "\t\t\t(layer \"{}\")", esc(&pour.layer));
                    write_pts(&mut out, &ring, -1, "\t\t\t");
                    out.push_str("\t\t)\n");
                }
            }
        }
        out.push_str("\t)\n");
    }

    out.push_str(")\n");
    out
}

/// Renders a matching KiCad `.kicad_pro` JSON file carrying exact `DesignRules` and `NetClass` rules.
pub fn render_kicad_pro(board: &BoardIr) -> String {
    let r = &board.rules;
    let mut defined_classes: HashSet<&str> = HashSet::new();
    let mut classes = Vec::new();
    if !board.net_classes.contains_key("Default") {
        defined_classes.insert("Default");
        classes.push(json!({
            "name": "Default",
            "clearance": nm_to_mm(r.clearance_nm),
            "track_width": nm_to_mm(r.track_width_nm),
            "via_diameter": nm_to_mm(r.via_dia_nm),
            "via_drill": nm_to_mm(r.via_drill_nm),
        }));
    }
    for nc in board.net_classes.values() {
        defined_classes.insert(nc.name.as_str());
        classes.push(json!({
            "name": nc.name,
            "clearance": nm_to_mm(nc.clearance_nm),
            "track_width": nm_to_mm(nc.track_width_nm),
            "via_diameter": nm_to_mm(nc.via_dia_nm),
            "via_drill": nm_to_mm(nc.via_drill_nm),
            "diff_pair_width": nc.diff_pair_width_nm.map(nm_to_mm).unwrap_or(0.2),
            "diff_pair_gap": nc.diff_pair_gap_nm.map(nm_to_mm).unwrap_or(0.2),
        }));
    }
    let mut assignments = serde_json::Map::new();
    for (net_name, nd) in &board.nets {
        let cls = nd.class.trim();
        let class_name = if cls.is_empty() { "Default" } else { cls };
        if defined_classes.insert(class_name) {
            classes.push(json!({
                "name": class_name,
                "clearance": nm_to_mm(r.clearance_nm),
                "track_width": nm_to_mm(r.track_width_nm),
                "via_diameter": nm_to_mm(r.via_dia_nm),
                "via_drill": nm_to_mm(r.via_drill_nm),
            }));
        }
        assignments.insert(net_name.clone(), json!([class_name]));
    }
    let doc = json!({
        "meta": { "filename": format!("{}.kicad_pro", board.name), "version": 1 },
        "board": {
            "design_settings": {
                "meta": { "version": 2 },
                "rule_severities": {
                    "annular_width": "error",
                    "clearance": "error",
                    "copper_edge_clearance": "error",
                    "courtyards_overlap": "error",
                    "hole_clearance": "error",
                    "hole_near_hole": "error",
                    "hole_to_hole": "error",
                    "keepout": "error",
                    "rule_area": "error",
                    "shorting_items": "error",
                    "track_dangling": "error",
                    "track_width": "error",
                    "tracks_crossing": "error",
                    "unconnected_items": "error",
                    "via_dangling": "error",
                    "zone_to_zone": "error"
                },
                "rules": {
                    "min_clearance": nm_to_mm(r.clearance_nm),
                    "min_track_width": nm_to_mm(r.track_width_nm),
                    "min_via_diameter": nm_to_mm(r.via_dia_nm),
                    "min_via_annular_width": nm_to_mm(r.min_annular_ring_nm),
                    "min_through_hole_diameter": nm_to_mm(r.via_drill_nm),
                    "min_hole_to_hole": nm_to_mm(r.hole_to_hole_nm),
                    "min_hole_clearance": nm_to_mm(r.effective_hole_clearance_nm()),
                    "min_copper_edge_clearance": nm_to_mm(r.edge_clearance_nm),
                    "min_silk_clearance": nm_to_mm(r.silk_clearance_nm),
                    "min_connection": 0.0,
                    "min_resolved_spokes": 1,
                    "solder_mask_to_copper_clearance": 0.0
                }
            }
        },
        "net_settings": {
            "classes": classes,
            "netclass_assignments": assignments
        }
    });
    serde_json::to_string_pretty(&doc).unwrap_or_else(|_| "{}".into())
}

/// Exports `BoardIr` to a KiCad `.kicad_pcb` (and sibling `.kicad_pro` if `write_pro`).
pub fn export_kicad_pcb(board: &BoardIr, pcb_path: &Path, write_pro: bool) -> Result<(), String> {
    if let Some(parent) = pcb_path.parent()
        && !parent.as_os_str().is_empty()
    {
        fs::create_dir_all(parent)
            .map_err(|e| format!("failed to create dir '{}': {e}", parent.display()))?;
    }
    fs::write(pcb_path, render_kicad_pcb(board))
        .map_err(|e| format!("failed to write '{}': {e}", pcb_path.display()))?;
    if write_pro {
        let pro_path = pcb_path.with_extension("kicad_pro");
        fs::write(&pro_path, render_kicad_pro(board))
            .map_err(|e| format!("failed to write '{}': {e}", pro_path.display()))?;
    }
    Ok(())
}
