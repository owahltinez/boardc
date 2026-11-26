use std::collections::BTreeMap;
use std::fmt::Write as _;

use crate::geom::remap_layer_for_side;
use crate::ir::{
    BoardIr, Graphic, Keepout, Nm, PadDef, PadKind, PadShape, Placement, Pt, Side, nm_to_mm,
};

pub(crate) struct UuidGen(pub(crate) u64);

impl UuidGen {
    pub(crate) fn next(&mut self) -> String {
        self.0 += 1;
        format!("00000000-0000-4000-8000-{:012x}", self.0)
    }
}

pub(crate) fn esc(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for c in s.chars() {
        match c {
            '\\' => out.push_str("\\\\"),
            '"' => out.push_str("\\\""),
            '\n' | '\r' => out.push(' '),
            _ => out.push(c),
        }
    }
    out
}

fn kicad_layer_name(layer: &str) -> &str {
    match layer {
        "F.Silk" | "F.Silkscreen" => "F.SilkS",
        "B.Silk" | "B.Silkscreen" => "B.SilkS",
        other => other,
    }
}

fn remap_kicad_layer(layer: &str, side: Side) -> String {
    let mapped = remap_layer_for_side(layer, side);
    kicad_layer_name(&mapped).to_string()
}

pub(crate) fn write_pts(out: &mut String, pts: &[Pt], y_sign: i64, indent: &str) {
    let _ = writeln!(out, "{indent}(pts");
    let _ = write!(out, "{indent}\t");
    for (i, p) in pts.iter().enumerate() {
        if i > 0 {
            out.push(' ');
        }
        let _ = write!(
            out,
            "(xy {:.6} {:.6})",
            nm_to_mm(p.x),
            nm_to_mm(y_sign * p.y)
        );
    }
    out.push('\n');
    let _ = writeln!(out, "{indent})");
}

pub(crate) fn write_closed_poly_lines(
    out: &mut String,
    tag: &str,
    layer: &str,
    pts: &[Pt],
    width_nm: Nm,
    y_sign: i64,
    uuids: &mut UuidGen,
) {
    if pts.len() < 2 {
        return;
    }
    let indent = if tag.starts_with("fp_") { "\t\t" } else { "\t" };
    let w = nm_to_mm(width_nm.max(10_000));
    for i in 0..pts.len() {
        let a = pts[i];
        let b = pts[(i + 1) % pts.len()];
        if a == b {
            continue;
        }
        let _ = writeln!(
            out,
            "{indent}({tag} (start {:.6} {:.6}) (end {:.6} {:.6}) (stroke (width {w:.4}) (type solid)) (layer \"{}\") (uuid \"{}\"))",
            nm_to_mm(a.x),
            nm_to_mm(y_sign * a.y),
            nm_to_mm(b.x),
            nm_to_mm(y_sign * b.y),
            esc(layer),
            uuids.next(),
        );
    }
}

pub(crate) fn write_graphic(
    out: &mut String,
    tag_prefix: &str,
    g: &Graphic,
    y_sign: i64,
    side: Side,
    indent: &str,
    uuids: &mut UuidGen,
) {
    match g {
        Graphic::Line { a, b, width, layer } => {
            let l = remap_kicad_layer(layer, side);
            let _ = writeln!(
                out,
                "{indent}({tag_prefix}_line (start {:.6} {:.6}) (end {:.6} {:.6}) (stroke (width {:.4}) (type solid)) (layer \"{}\") (uuid \"{}\"))",
                nm_to_mm(a.x),
                nm_to_mm(y_sign * a.y),
                nm_to_mm(b.x),
                nm_to_mm(y_sign * b.y),
                nm_to_mm((*width).max(10_000)),
                esc(&l),
                uuids.next(),
            );
        }
        Graphic::Rect {
            layer,
            min,
            max,
            width,
            fill,
        } => {
            let l = remap_kicad_layer(layer, side);
            let fill_str = if *fill { "yes" } else { "no" };
            let _ = writeln!(
                out,
                "{indent}({tag_prefix}_rect (start {:.6} {:.6}) (end {:.6} {:.6}) (stroke (width {:.4}) (type solid)) (fill {fill_str}) (layer \"{}\") (uuid \"{}\"))",
                nm_to_mm(min.x),
                nm_to_mm(y_sign * min.y),
                nm_to_mm(max.x),
                nm_to_mm(y_sign * max.y),
                nm_to_mm((*width).max(10_000)),
                esc(&l),
                uuids.next(),
            );
        }
        Graphic::Circle {
            layer,
            center,
            radius,
            width,
            fill,
        } => {
            let l = remap_kicad_layer(layer, side);
            let fill_str = if *fill { "yes" } else { "no" };
            let _ = writeln!(
                out,
                "{indent}({tag_prefix}_circle (center {:.6} {:.6}) (end {:.6} {:.6}) (stroke (width {:.4}) (type solid)) (fill {fill_str}) (layer \"{}\") (uuid \"{}\"))",
                nm_to_mm(center.x),
                nm_to_mm(y_sign * center.y),
                nm_to_mm(center.x + *radius),
                nm_to_mm(y_sign * center.y),
                nm_to_mm((*width).max(10_000)),
                esc(&l),
                uuids.next(),
            );
        }
        Graphic::Poly {
            layer,
            verts,
            width,
            fill,
        } => {
            if verts.len() < 3 {
                return;
            }
            let l = remap_kicad_layer(layer, side);
            let fill_str = if *fill { "yes" } else { "no" };
            let _ = writeln!(out, "{indent}({tag_prefix}_poly");
            write_pts(out, verts, y_sign, &format!("{indent}\t"));
            let _ = writeln!(
                out,
                "{indent}\t(stroke (width {:.4}) (type solid)) (fill {fill_str}) (layer \"{}\") (uuid \"{}\")",
                nm_to_mm((*width).max(10_000)),
                esc(&l),
                uuids.next(),
            );
            let _ = writeln!(out, "{indent})");
        }
    }
}

pub(crate) fn write_keepout(
    out: &mut String,
    ko: &Keepout,
    y_sign: i64,
    side: Side,
    indent: &str,
    uuids: &mut UuidGen,
) {
    if ko.poly.len() < 3 {
        return;
    }
    let tracks = if ko.kind.blocks_tracks() {
        "not_allowed"
    } else {
        "allowed"
    };
    let vias = if ko.kind.blocks_vias() {
        "not_allowed"
    } else {
        "allowed"
    };
    let copperpour = if ko.kind.blocks_copper() {
        "not_allowed"
    } else {
        "allowed"
    };
    let footprints = if ko.kind.blocks_placement() {
        "not_allowed"
    } else {
        "allowed"
    };
    let layers: Vec<String> = if ko.layers.is_empty() {
        vec!["*.Cu".into()]
    } else {
        ko.layers
            .iter()
            .map(|l| remap_kicad_layer(l, side))
            .collect()
    };
    let _ = writeln!(out, "{indent}(zone");
    let _ = writeln!(out, "{indent}\t(net 0)");
    let _ = writeln!(out, "{indent}\t(net_name \"\")");
    let _ = write!(out, "{indent}\t(layers");
    for l in &layers {
        let _ = write!(out, " \"{}\"", esc(l));
    }
    out.push_str(")\n");
    let _ = writeln!(out, "{indent}\t(uuid \"{}\")", uuids.next());
    let _ = writeln!(out, "{indent}\t(hatch edge 0.5)");
    let _ = writeln!(
        out,
        "{indent}\t(keepout (tracks {tracks}) (vias {vias}) (pads allowed) (copperpour {copperpour}) (footprints {footprints}))"
    );
    let _ = writeln!(out, "{indent}\t(polygon");
    write_pts(out, &ko.poly, y_sign, &format!("{indent}\t\t"));
    let _ = writeln!(out, "{indent}\t)");
    let _ = writeln!(out, "{indent})");
}

fn write_pad(
    out: &mut String,
    pad: &PadDef,
    pl: &Placement,
    y_sign: i64,
    net_ids: &BTreeMap<String, usize>,
    uuids: &mut UuidGen,
) {
    let kind_str = match pad.kind {
        PadKind::Smd => "smd",
        PadKind::Tht => "thru_hole",
        PadKind::Npth => "np_thru_hole",
    };
    let (shape_str, w_nm, h_nm, rratio) = match &pad.shape {
        PadShape::Circle { diameter } => ("circle", *diameter, *diameter, None),
        PadShape::Rect { width, height } => ("rect", *width, *height, None),
        PadShape::RoundRect {
            width,
            height,
            corner_radius,
        } => {
            if *corner_radius <= 0 {
                ("rect", *width, *height, None)
            } else {
                let min_dim = (*width).min(*height).max(1) as f64;
                let ratio = (*corner_radius as f64 / min_dim).clamp(0.0, 0.5);
                ("roundrect", *width, *height, Some(ratio))
            }
        }
        PadShape::Oval { width, height } => ("oval", *width, *height, None),
        PadShape::Poly { .. } => ("custom", 1_000, 1_000, None),
    };
    // KiCad .kicad_pcb stores pad rotation as absolute board angle (matching kicad_io.rs).
    let pad_rot = if pl.side == Side::Bottom {
        (pl.rot_deg - pad.rot_deg).rem_euclid(360.0)
    } else {
        (pl.rot_deg + pad.rot_deg).rem_euclid(360.0)
    };
    let _ = writeln!(
        out,
        "\t\t(pad \"{}\" {kind_str} {shape_str}",
        esc(&pad.name)
    );
    let _ = writeln!(
        out,
        "\t\t\t(at {:.6} {:.6} {pad_rot:.4})",
        nm_to_mm(pad.pos.x),
        nm_to_mm(y_sign * pad.pos.y)
    );
    let _ = writeln!(
        out,
        "\t\t\t(size {:.6} {:.6})",
        nm_to_mm(w_nm.max(1_000)),
        nm_to_mm(h_nm.max(1_000))
    );
    if let Some(drill) = pad.drill_nm
        && drill > 0
    {
        let _ = writeln!(out, "\t\t\t(drill {:.6})", nm_to_mm(drill));
    }
    let mut layers: Vec<String> = if pad.layers.is_empty() {
        match pad.kind {
            PadKind::Smd => vec![
                remap_kicad_layer("F.Cu", pl.side),
                remap_kicad_layer("F.Mask", pl.side),
                remap_kicad_layer("F.Paste", pl.side),
            ],
            PadKind::Tht | PadKind::Npth => vec!["*.Cu".into(), "*.Mask".into()],
        }
    } else {
        pad.layers
            .iter()
            .map(|l| remap_kicad_layer(l, pl.side))
            .collect()
    };
    if pad.kind == PadKind::Npth && !layers.iter().any(|l| l == "*.Cu") {
        layers.insert(0, "*.Cu".into());
    }
    out.push_str("\t\t\t(layers");
    for l in &layers {
        let _ = write!(out, " \"{}\"", esc(l));
    }
    out.push_str(")\n");
    if let Some(rr) = rratio {
        let _ = writeln!(out, "\t\t\t(roundrect_rratio {rr:.6})");
    }
    if let Some(mm_val) = pad.mask_margin_nm {
        let _ = writeln!(out, "\t\t\t(solder_mask_margin {:.6})", nm_to_mm(mm_val));
    }
    if let Some(pm_val) = pad.paste_margin_nm {
        let _ = writeln!(out, "\t\t\t(solder_paste_margin {:.6})", nm_to_mm(pm_val));
    }
    if pad.kind != PadKind::Npth
        && let Some(net_name) = pl.pin_nets.get(&pad.name)
        && let Some(&net_id) = net_ids.get(net_name)
    {
        let _ = writeln!(out, "\t\t\t(net {net_id} \"{}\")", esc(net_name));
    }
    if let PadShape::Poly { verts } = &pad.shape
        && verts.len() >= 3
    {
        out.push_str("\t\t\t(options (clearance outline) (anchor rect))\n");
        out.push_str("\t\t\t(primitives\n\t\t\t\t(gr_poly\n");
        write_pts(out, verts, y_sign, "\t\t\t\t\t");
        out.push_str("\t\t\t\t\t(width 0) (fill yes)\n\t\t\t\t)\n\t\t\t)\n");
    }
    let _ = writeln!(out, "\t\t\t(uuid \"{}\")", uuids.next());
    out.push_str("\t\t)\n");
}

pub(crate) fn write_footprint(
    out: &mut String,
    pl: &Placement,
    board: &BoardIr,
    net_ids: &BTreeMap<String, usize>,
    uuids: &mut UuidGen,
) {
    let Some(fp) = board.footprints.get(&pl.footprint) else {
        return;
    };
    // B.Cu footprints in .kicad_pcb store Y-flipped local coords (y_sign = +1).
    let y_sign: i64 = if pl.side == Side::Bottom { 1 } else { -1 };
    let cu_layer = pl.side.cu_layer();
    let fab_layer = if pl.side == Side::Bottom {
        "B.Fab"
    } else {
        "F.Fab"
    };
    let crtyd_layer = if pl.side == Side::Bottom {
        "B.CrtYd"
    } else {
        "F.CrtYd"
    };
    let has_tht = fp.pads.iter().any(|p| p.kind == PadKind::Tht);
    let has_smd = fp.pads.iter().any(|p| p.kind == PadKind::Smd);
    let attr = if has_tht {
        "through_hole"
    } else if has_smd {
        "smd"
    } else {
        "board_only"
    };

    let fp_id = pl
        .schematic
        .as_ref()
        .and_then(|s| s.footprint.as_deref())
        .filter(|s| !s.is_empty())
        .unwrap_or(&fp.name);
    let _ = writeln!(out, "\t(footprint \"{}\"", esc(fp_id));
    let _ = writeln!(out, "\t\t(layer \"{cu_layer}\")");
    let _ = writeln!(out, "\t\t(uuid \"{}\")", uuids.next());
    let _ = writeln!(
        out,
        "\t\t(at {:.6} {:.6} {:.4})",
        nm_to_mm(pl.pos.x),
        nm_to_mm(-pl.pos.y),
        pl.rot_deg.rem_euclid(360.0)
    );
    let _ = writeln!(
        out,
        "\t\t(property \"Reference\" \"{}\" (at 0 0 0) (layer \"{fab_layer}\") (hide yes) (uuid \"{}\") (effects (font (size 1 1) (thickness 0.15))))",
        esc(&pl.refdes),
        uuids.next()
    );
    let _ = writeln!(
        out,
        "\t\t(property \"Value\" \"{}\" (at 0 0 0) (layer \"{fab_layer}\") (hide yes) (uuid \"{}\") (effects (font (size 1 1) (thickness 0.15))))",
        esc(&pl.value),
        uuids.next()
    );
    if let Some(sch) = &pl.schematic {
        for (k, v) in &sch.fields {
            if matches!(
                k.as_str(),
                "Reference" | "Value" | "Footprint" | "Sheetname" | "Sheetfile"
            ) {
                continue;
            }
            let _ = writeln!(
                out,
                "\t\t(property \"{}\" \"{}\" (at 0 0 0) (layer \"{fab_layer}\") (hide yes) (uuid \"{}\") (effects (font (size 1 1) (thickness 0.15))))",
                esc(k),
                esc(v),
                uuids.next()
            );
        }
        if let Some(path) = sch.path.as_deref().filter(|s| !s.is_empty()) {
            let _ = writeln!(out, "\t\t(path \"{}\")", esc(path));
        }
        if let Some(sheetname) = sch.sheetname.as_deref().filter(|s| !s.is_empty()) {
            let _ = writeln!(out, "\t\t(sheetname \"{}\")", esc(sheetname));
        }
        if let Some(sheetfile) = sch.sheetfile.as_deref().filter(|s| !s.is_empty()) {
            let _ = writeln!(out, "\t\t(sheetfile \"{}\")", esc(sheetfile));
        }
    }
    let _ = write!(out, "\t\t(attr {attr}");
    if pl.exclude_from_pos {
        out.push_str(" exclude_from_pos_files");
    }
    if pl.exclude_from_bom {
        out.push_str(" exclude_from_bom");
    }
    if pl.dnp {
        out.push_str(" dnp");
    }
    out.push_str(")\n");

    for g in &fp.graphics {
        let layer = g.layer();
        if layer == "F.CrtYd" || layer == "B.CrtYd" || layer == "Edge.Cuts" {
            continue;
        }
        write_graphic(out, "fp", g, y_sign, pl.side, "\t\t", uuids);
    }

    if fp.courtyard.len() >= 3 {
        write_closed_poly_lines(
            out,
            "fp_line",
            crtyd_layer,
            &fp.courtyard,
            50_000,
            y_sign,
            uuids,
        );
    }

    for ko in &fp.keepouts {
        write_keepout(out, ko, y_sign, pl.side, "\t\t", uuids);
    }
    for pad in &fp.pads {
        write_pad(out, pad, pl, y_sign, net_ids, uuids);
    }
    for m in &fp.models {
        let _ = writeln!(out, "\t\t(model \"{}\"", esc(&m.path));
        let _ = writeln!(
            out,
            "\t\t\t(offset (xyz {:.6} {:.6} {:.6}))",
            m.offset_mm[0], m.offset_mm[1], m.offset_mm[2]
        );
        let _ = writeln!(
            out,
            "\t\t\t(scale (xyz {:.6} {:.6} {:.6}))",
            m.scale[0], m.scale[1], m.scale[2]
        );
        let _ = writeln!(
            out,
            "\t\t\t(rotate (xyz {:.6} {:.6} {:.6}))",
            m.rotate_deg[0], m.rotate_deg[1], m.rotate_deg[2]
        );
        out.push_str("\t\t)\n");
    }
    out.push_str("\t)\n");
}
