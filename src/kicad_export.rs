use std::collections::{BTreeMap, HashSet};
use std::fmt::Write as _;
use std::fs;
use std::path::Path;

use serde_json::json;

use crate::geom::{ensure_ccw, polygon_signed_area_2};
use crate::ir::{BoardIr, LayerKind, Nm, PourPoly, Pt, SOLDER_MASK_THICKNESS_NM, Side, nm_to_mm};
use crate::kicad_export_items::{
    UuidGen, esc, write_closed_poly_lines, write_graphic, write_keepout, write_pts,
};
use crate::kicad_raw_fp::write_footprints;
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

fn vertex_accepts_left_ray(p_prev: Pt, p_cur: Pt, p_next: Pt) -> bool {
    let e1x = (p_prev.x - p_cur.x) as i128;
    let e1y = (p_prev.y - p_cur.y) as i128;
    let e2x = (p_next.x - p_cur.x) as i128;
    let e2y = (p_next.y - p_cur.y) as i128;
    let cross = e2x * e1y - e2y * e1x;
    if cross < 0 {
        e2y >= 0 || e1y <= 0
    } else if cross == 0 && e2x * e1x + e2y * e1y > 0 {
        e2y == 0 && e2x < 0
    } else {
        e2y >= 0 && e1y <= 0
    }
}

// Fractures a `PourPoly` (outer + holes) into a single KiCad keyhole-slit polygon.
fn fracture_pour_poly(poly: &PourPoly) -> Vec<Pt> {
    let mut ring = poly.outer.clone();
    ring.dedup();
    if ring.len() > 1 && ring.first() == ring.last() {
        ring.pop();
    }
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
    if holes.is_empty() {
        return ring;
    }
    // Process rightmost holes first so no unprocessed hole can block the +X bridge ray.
    holes.sort_by_key(|h| std::cmp::Reverse((h.2.x, h.2.y)));

    let mut min_y = ring[0].y;
    let mut max_y = ring[0].y;
    for p in &ring {
        min_y = min_y.min(p.y);
        max_y = max_y.max(p.y);
    }
    let n0 = ring.len();
    let mut total_verts = n0;
    for (h, _, _) in &holes {
        total_verts += h.len() + 2;
        for p in h {
            min_y = min_y.min(p.y);
            max_y = max_y.max(p.y);
        }
    }

    let mut pts = ring;
    pts.reserve(total_verts - n0);
    let mut next: Vec<usize> = Vec::with_capacity(total_verts);
    let mut prev: Vec<usize> = Vec::with_capacity(total_verts);
    for i in 0..n0 {
        next.push((i + 1) % n0);
        prev.push((i + n0 - 1) % n0);
    }

    let h_span = (max_y - min_y).max(1);
    let num_slabs = (total_verts / 4).clamp(16, 512);
    let y_step = (h_span / num_slabs as Nm).max(1);
    let actual_slabs = ((h_span / y_step) as usize + 1).max(1);
    let slab_of =
        |y: Nm| -> usize { ((y - min_y) / y_step).clamp(0, actual_slabs as i64 - 1) as usize };
    let mut slabs: Vec<Vec<usize>> = vec![Vec::new(); actual_slabs];
    for i in 0..n0 {
        let a = pts[i];
        let b = pts[next[i]];
        let s0 = slab_of(a.y.min(b.y));
        let s1 = slab_of(a.y.max(b.y));
        for slab in &mut slabs[s0..=s1] {
            slab.push(i);
        }
    }

    let mut splice_pts: Vec<Pt> = Vec::new();
    for (h, m_idx, m) in holes {
        let s = slab_of(m.y);
        let mut best: Option<(f64, u8, usize)> = None;
        for &i in &slabs[s] {
            let a = pts[i];
            let b = pts[next[i]];
            if a.y == m.y && a.x >= m.x {
                let rank = u8::from(!vertex_accepts_left_ray(pts[prev[i]], a, b));
                let cand = (a.x as f64, rank, i);
                if best.is_none_or(|cur| cand.0 < cur.0 || (cand.0 == cur.0 && cand.1 < cur.1)) {
                    best = Some(cand);
                }
            }
            if (a.y > m.y && b.y < m.y) || (a.y < m.y && b.y > m.y) {
                let ix = a.x as f64 + (m.y - a.y) as f64 * (b.x - a.x) as f64 / (b.y - a.y) as f64;
                if ix >= m.x as f64 - 1e-6 {
                    let rx = ix.max(m.x as f64);
                    let cand = (rx, 2, i);
                    if best.is_none_or(|cur| cand.0 < cur.0) {
                        best = Some(cand);
                    }
                }
            }
        }
        let Some((bx, rank, u)) = best else {
            continue;
        };
        let split_pt = (rank == 2).then(|| Pt::new(bx.round() as Nm, m.y));
        let v = next[u];
        let old_s0 = slab_of(pts[u].y.min(pts[v].y));
        let old_s1 = slab_of(pts[u].y.max(pts[v].y));

        splice_pts.clear();
        if let Some(ip) = split_pt {
            splice_pts.push(ip);
        }
        let hl = h.len();
        for k in 0..=hl {
            splice_pts.push(h[(m_idx + k) % hl]);
        }
        splice_pts.push(split_pt.unwrap_or(pts[u]));
        splice_pts.dedup();
        if splice_pts.first() == Some(&pts[u]) {
            splice_pts.remove(0);
        }
        if splice_pts.last() == Some(&pts[v]) {
            splice_pts.pop();
        }
        if splice_pts.is_empty() {
            continue;
        }

        let first_new = pts.len();
        let count = splice_pts.len();
        for (k, &pt) in splice_pts.iter().enumerate() {
            let idx = first_new + k;
            pts.push(pt);
            prev.push(if k == 0 { u } else { idx - 1 });
            next.push(if k + 1 == count { v } else { idx + 1 });
        }
        let last_new = first_new + count - 1;
        next[u] = first_new;
        prev[v] = last_new;

        let new_s0 = slab_of(pts[u].y.min(pts[first_new].y));
        let new_s1 = slab_of(pts[u].y.max(pts[first_new].y));
        if (old_s0, old_s1) != (new_s0, new_s1) {
            for (s_idx, slab) in (old_s0..=old_s1).zip(&mut slabs[old_s0..=old_s1]) {
                if (s_idx < new_s0 || s_idx > new_s1)
                    && let Some(pos) = slab.iter().position(|&e| e == u)
                {
                    slab.swap_remove(pos);
                }
            }
            for (s_idx, slab) in (new_s0..=new_s1).zip(&mut slabs[new_s0..=new_s1]) {
                if s_idx < old_s0 || s_idx > old_s1 {
                    slab.push(u);
                }
            }
        }
        for idx in first_new..=last_new {
            let a = pts[idx];
            let b = pts[next[idx]];
            let s0 = slab_of(a.y.min(b.y));
            let s1 = slab_of(a.y.max(b.y));
            for slab in &mut slabs[s0..=s1] {
                slab.push(idx);
            }
        }
    }

    let mut out = Vec::with_capacity(pts.len());
    let mut cur = 0usize;
    for _ in 0..pts.len() {
        out.push(pts[cur]);
        cur = next[cur];
        if cur == 0 {
            break;
        }
    }
    out.dedup();
    if out.len() > 1 && out.first() == out.last() {
        out.pop();
    }
    out
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
    out.push_str("\t(title_block\n");
    let _ = writeln!(out, "\t\t(title \"{}\")", esc(board.effective_title()));
    if let Some(rev) = board.revision_trimmed() {
        let _ = writeln!(out, "\t\t(rev \"{}\")", esc(rev));
    }
    out.push_str("\t)\n");
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
        write_graphic(&mut out, g, None, &mut uuids);
    }

    write_footprints(&mut out, board, &net_ids, &mut uuids);

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
                    "solder_mask_bridge": if r.mask_to_copper_nm.is_some() { "error" } else { "ignore" },
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
                    "solder_mask_to_copper_clearance": r.mask_to_copper_nm.map(nm_to_mm).unwrap_or(0.0)
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::geom::{point_in_polygon, polygon_signed_area_2};
    use crate::ir::mm;
    use std::cmp::Ordering;
    use std::collections::HashMap;

    fn cmp_dir(a: Pt, b: Pt) -> Ordering {
        let half = |d: Pt| u8::from(d.y < 0 || (d.y == 0 && d.x < 0));
        half(a).cmp(&half(b)).then_with(|| {
            let cross = (a.x as i128) * (b.y as i128) - (a.y as i128) * (b.x as i128);
            0i128.cmp(&cross)
        })
    }

    fn in_open_ccw_arc(u: Pt, r: Pt, w: Pt) -> bool {
        match cmp_dir(u, w) {
            Ordering::Less => cmp_dir(u, r) == Ordering::Less && cmp_dir(r, w) == Ordering::Less,
            Ordering::Greater => cmp_dir(u, r) == Ordering::Less || cmp_dir(r, w) == Ordering::Less,
            Ordering::Equal => false,
        }
    }

    fn pairs_strictly_interleave(a1: Pt, b1: Pt, a2: Pt, b2: Pt) -> bool {
        if cmp_dir(a2, a1) == Ordering::Equal
            || cmp_dir(a2, b1) == Ordering::Equal
            || cmp_dir(b2, a1) == Ordering::Equal
            || cmp_dir(b2, b1) == Ordering::Equal
        {
            return false;
        }
        in_open_ccw_arc(a1, a2, b1) != in_open_ccw_arc(a1, b2, b1)
    }

    fn assert_no_repeated_vertex_crossings(poly: &[Pt]) {
        let n = poly.len();
        let mut by_vert: HashMap<Pt, Vec<(Pt, Pt)>> = HashMap::new();
        for i in 0..n {
            let p = poly[i];
            let prev = poly[(i + n - 1) % n];
            let next = poly[(i + 1) % n];
            by_vert.entry(p).or_default().push((
                Pt::new(prev.x - p.x, prev.y - p.y),
                Pt::new(next.x - p.x, next.y - p.y),
            ));
        }
        for (v, pairs) in by_vert {
            for (i, &(a1, b1)) in pairs.iter().enumerate() {
                for &(a2, b2) in &pairs[i + 1..] {
                    assert!(
                        !pairs_strictly_interleave(a1, b1, a2, b2),
                        "cyclic crossing at {v:?}: ({a1:?}, {b1:?}) vs ({a2:?}, {b2:?})"
                    );
                }
            }
        }
    }

    fn assert_fracture_valid(outer: &[Pt], holes: &[Vec<Pt>]) {
        let poly = PourPoly {
            outer: outer.to_vec(),
            holes: holes.to_vec(),
        };
        let fractured = fracture_pour_poly(&poly);
        assert!(fractured.len() >= outer.len() + holes.len() * 3);
        assert_ne!(fractured.first(), fractured.last());
        assert!(fractured.windows(2).all(|w| w[0] != w[1]));
        assert_no_repeated_vertex_crossings(&fractured);

        let hole_area_sum: i128 = holes.iter().map(|h| polygon_signed_area_2(h).abs()).sum();
        let expected_area_2 = polygon_signed_area_2(outer).abs() - hole_area_sum;
        assert_eq!(polygon_signed_area_2(&fractured), expected_area_2);

        for h in holes {
            let cx = h.iter().map(|p| p.x).sum::<Nm>() / h.len() as Nm;
            let cy = h.iter().map(|p| p.y).sum::<Nm>() / h.len() as Nm;
            assert!(!point_in_polygon(Pt::new(cx, cy), &fractured));
        }
    }

    #[test]
    fn fracture_pour_poly_handles_many_holes_same_y_and_bridge_on_bridge() {
        let outer = vec![
            Pt::new(0, 0),
            Pt::new(mm(100.0), 0),
            Pt::new(mm(100.0), mm(100.0)),
            Pt::new(0, mm(100.0)),
        ];
        let mut holes = Vec::new();
        // 8x7 = 56 grid holes with shared Y coordinates across rows.
        for row in 0..7 {
            for col in 0..8 {
                let x0 = mm(5.0 + col as f64 * 10.0);
                let y0 = mm(5.0 + row as f64 * 10.0);
                holes.push(vec![
                    Pt::new(x0, y0),
                    Pt::new(x0 + mm(5.0), y0),
                    Pt::new(x0 + mm(5.0), y0 + mm(5.0)),
                    Pt::new(x0, y0 + mm(5.0)),
                ]);
            }
        }
        // Top-tip and bottom-tip holes on the same Y = 85mm to force bridge-on-bridge vertex hits.
        holes.push(vec![
            Pt::new(mm(40.0), mm(80.0)),
            Pt::new(mm(45.0), mm(85.0)),
            Pt::new(mm(37.0), mm(82.0)),
        ]);
        holes.push(vec![
            Pt::new(mm(20.0), mm(85.0)),
            Pt::new(mm(15.0), mm(88.0)),
            Pt::new(mm(17.0), mm(82.0)),
        ]);
        holes.push(vec![
            Pt::new(mm(60.0), mm(88.0)),
            Pt::new(mm(65.0), mm(85.0)),
            Pt::new(mm(57.0), mm(90.0)),
        ]);
        holes.push(vec![
            Pt::new(mm(28.0), mm(82.0)),
            Pt::new(mm(33.0), mm(85.0)),
            Pt::new(mm(28.0), mm(88.0)),
        ]);

        assert!(holes.len() >= 60);
        assert_fracture_valid(&outer, &holes);

        // Large diagonal edge (|dy| * |dx| > 2^53) ending at y = 1mm must not bypass vertex_accepts_left_ray.
        let diag_outer = vec![
            Pt::new(0, 0),
            Pt::new(mm(150.0), 0),
            Pt::new(mm(150.0), mm(120.0)),
            Pt::new(0, mm(120.0)),
        ];
        let diag_holes = vec![
            vec![
                Pt::new(mm(1.0), mm(119.0) + 1),
                Pt::new(mm(95.0) + 1, mm(1.0)),
                Pt::new(mm(90.0), mm(2.0)),
            ],
            vec![
                Pt::new(mm(10.0), mm(0.5)),
                Pt::new(mm(15.0), mm(0.5)),
                Pt::new(mm(15.0), mm(1.0)),
                Pt::new(mm(10.0), mm(1.0)),
            ],
        ];
        assert_fracture_valid(&diag_outer, &diag_holes);
    }
}
