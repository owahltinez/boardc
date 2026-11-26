use std::collections::BTreeMap;
use std::fmt::Write as _;

use crate::geom::{local_to_board, pad_copper_layers, remap_layer_for_side};
use crate::ir::{
    BBoxNm, BoardIr, FootprintDef, Graphic, Nm, PadKind, Placement, Pt, Side, nm_to_mm, safe_slug,
};
use crate::shapes::pad_to_world_shape;
use crate::svg::{
    board_bbox_mm, poly_path_d, render_graphic_svg, render_shape_svg, svg_pt, xml_escape,
};

const DRILL_COLORS: [&str; 6] = [
    "#0f172a", "#1d4ed8", "#b91c1c", "#047857", "#7e22ce", "#b45309",
];
const SYMBOL_COUNT: usize = 12;

pub(crate) struct DrillSection {
    pub(crate) filename: String,
    pub(crate) plated: bool,
    pub(crate) span: Option<(String, String)>,
    pub(crate) by_dia: BTreeMap<Nm, Vec<Pt>>,
}

impl DrillSection {
    fn total_hits(&self) -> usize {
        self.by_dia.values().map(Vec::len).sum()
    }

    fn type_label(&self) -> String {
        match (&self.span, self.plated) {
            (Some((l0, l1)), true) => format!("PTH ({l0}..{l1})"),
            (Some((l0, l1)), false) => format!("NPTH ({l0}..{l1})"),
            (None, true) => "PTH".into(),
            (None, false) => "NPTH".into(),
        }
    }
}

pub(crate) fn collect_drill_sections(board: &BoardIr, cu_layers: &[String]) -> Vec<DrillSection> {
    let slug = safe_slug(&board.name);
    let mut pth_by_dia: BTreeMap<Nm, Vec<Pt>> = BTreeMap::new();
    let mut npth_by_dia: BTreeMap<Nm, Vec<Pt>> = BTreeMap::new();
    let mut span_by_dia: BTreeMap<(String, String), BTreeMap<Nm, Vec<Pt>>> = BTreeMap::new();

    for via in &board.vias {
        if via.drill <= 0 {
            continue;
        }
        if via.is_through_hole(cu_layers, &board.layers) {
            pth_by_dia.entry(via.drill).or_default().push(via.pos);
        } else {
            let span = via.span_layer_pair(cu_layers, &board.layers);
            span_by_dia
                .entry(span)
                .or_default()
                .entry(via.drill)
                .or_default()
                .push(via.pos);
        }
    }
    for pl in &board.placements {
        let Some(fp) = board.footprints.get(&pl.footprint) else {
            continue;
        };
        for pad in &fp.pads {
            if let Some(drill) = pad.drill_nm
                && drill > 0
            {
                let world = local_to_board(pad.pos, pl.pos.x, pl.pos.y, pl.rot_deg, pl.side);
                match pad.kind {
                    PadKind::Tht => pth_by_dia.entry(drill).or_default().push(world),
                    PadKind::Npth => npth_by_dia.entry(drill).or_default().push(world),
                    PadKind::Smd => {}
                }
            }
        }
    }

    let mut sections = vec![
        DrillSection {
            filename: format!("{slug}-PTH.drl"),
            plated: true,
            span: None,
            by_dia: pth_by_dia,
        },
        DrillSection {
            filename: format!("{slug}-NPTH.drl"),
            plated: false,
            span: None,
            by_dia: npth_by_dia,
        },
    ];
    for ((l0, l1), by_dia) in span_by_dia {
        sections.push(DrillSection {
            filename: format!("{slug}-PTH-{}-{}.drl", safe_slug(&l0), safe_slug(&l1)),
            plated: true,
            span: Some((l0, l1)),
            by_dia,
        });
    }
    sections
}

pub(crate) fn format_drill_report(
    board: &BoardIr,
    cu_layers: &[String],
    sections: &[DrillSection],
) -> String {
    let mut out = String::new();
    let _ = writeln!(out, "Drill report for {}", board.name);
    let _ = writeln!(out, "Created by boardc {}\n", env!("CARGO_PKG_VERSION"));
    out.push_str("Copper Layer Stackup:\n");
    out.push_str("    =============================================================\n");
    for (idx, layer_name) in cu_layers.iter().enumerate() {
        let pos = if cu_layers.len() == 1 {
            let bottom = board
                .layers
                .iter()
                .find(|l| &l.name == layer_name)
                .is_some_and(|l| l.side == Some(Side::Bottom));
            if bottom {
                "back".to_string()
            } else {
                "front".to_string()
            }
        } else if idx == 0 {
            "front".to_string()
        } else if idx + 1 == cu_layers.len() {
            "back".to_string()
        } else {
            format!("in{idx}")
        };
        let _ = writeln!(out, "    L{:<2} :  {:<24}  {pos}", idx + 1, layer_name);
    }

    for sec in sections {
        let kind_desc = match (&sec.span, sec.plated) {
            (Some((l0, l1)), true) => format!("plated holes from {l0} to {l1}"),
            (Some((l0, l1)), false) => format!("unplated holes from {l0} to {l1}"),
            (None, true) => "plated through holes".to_string(),
            (None, false) => "unplated through holes".to_string(),
        };
        let _ = writeln!(out, "\n\nDrill file '{}' contains", sec.filename);
        let _ = writeln!(out, "    {kind_desc}:");
        out.push_str("    =============================================================\n");
        for (idx, (&dia, pts)) in sec.by_dia.iter().enumerate() {
            let mm_val = nm_to_mm(dia);
            let in_val = mm_val / 25.4;
            let noun = if pts.len() == 1 { "hole" } else { "holes" };
            let _ = writeln!(
                out,
                "    T{:02}  {mm_val:.4}mm  {in_val:.4}\"  ({} {noun})",
                idx + 1,
                pts.len()
            );
        }
        let total = sec.total_hits();
        let plating = if sec.plated { "plated" } else { "unplated" };
        let _ = writeln!(out, "\n    Total {plating} holes count {total}");
    }
    out
}

/// Renders a deterministic text drill report matching the emitted Excellon drill tool tables.
pub fn render_drill_report(board: &BoardIr) -> String {
    let cu_layers = board.copper_layer_names();
    let sections = collect_drill_sections(board, &cu_layers);
    format_drill_report(board, &cu_layers, &sections)
}

pub(crate) fn has_bottom_assembly(board: &BoardIr) -> bool {
    board.placements.iter().any(|pl| {
        pl.side == Side::Bottom
            || board.footprints.get(&pl.footprint).is_some_and(|fp| {
                fp.graphics.iter().any(|g| {
                    matches!(
                        remap_layer_for_side(g.layer(), pl.side).as_str(),
                        "B.Fab" | "B.CrtYd"
                    )
                })
            })
    }) || board
        .graphics
        .iter()
        .any(|g| matches!(g.layer(), "B.Fab" | "B.CrtYd"))
}

fn push_graphic_local_pts(g: &Graphic, pts: &mut Vec<Pt>) {
    match g {
        Graphic::Line { a, b, .. } => {
            pts.push(*a);
            pts.push(*b);
        }
        Graphic::Rect { min, max, .. } => {
            pts.push(*min);
            pts.push(*max);
        }
        Graphic::Circle { center, radius, .. } if *radius > 0 => {
            pts.push(Pt::new(center.x - *radius, center.y - *radius));
            pts.push(Pt::new(center.x + *radius, center.y + *radius));
        }
        Graphic::Poly { verts, .. } => pts.extend_from_slice(verts),
        _ => {}
    }
}

fn fp_assembly_local_bbox(fp: &FootprintDef, pl_side: Side, fab_layer: &str) -> BBoxNm {
    let mut fab_pts = Vec::new();
    for g in &fp.graphics {
        if remap_layer_for_side(g.layer(), pl_side) == fab_layer {
            push_graphic_local_pts(g, &mut fab_pts);
        }
    }
    if !fab_pts.is_empty() {
        return BBoxNm::from_pts(&fab_pts);
    }
    if !fp.courtyard.is_empty() {
        return BBoxNm::from_pts(&fp.courtyard);
    }
    if !fp.pads.is_empty() {
        let pts: Vec<Pt> = fp.pads.iter().map(|p| p.pos).collect();
        return BBoxNm::from_pts(&pts).expand(400_000);
    }
    BBoxNm::new(-500_000, -500_000, 500_000, 500_000)
}

fn fp_dnp_local_bbox(fp: &FootprintDef, pl_side: Side, fab_layer: &str) -> BBoxNm {
    if !fp.courtyard.is_empty() {
        return BBoxNm::from_pts(&fp.courtyard);
    }
    fp_assembly_local_bbox(fp, pl_side, fab_layer)
}

fn write_board_outline(out: &mut String, board: &BoardIr, mirror_x: bool) {
    if board.outline.is_empty() {
        return;
    }
    let mut d = poly_path_d(&board.outline, mirror_x);
    for cut in &board.cutouts {
        d.push(' ');
        d.push_str(&poly_path_d(cut, mirror_x));
    }
    let _ = writeln!(
        out,
        r##"  <path id="board_outline" d="{d}" fill="#f8fafc" fill-rule="evenodd" stroke="#111827" stroke-width="0.15" />"##
    );
}

pub(crate) fn render_assembly_svg(board: &BoardIr, side: Side, margin_mm: f64) -> String {
    let mirror_x = side == Side::Bottom;
    let cu_layers = board.copper_layer_names();
    let cu_layer = side.cu_layer();
    let (fab_layer, crtyd_layer, side_title) = match side {
        Side::Top => ("F.Fab", "F.CrtYd", "ASSEMBLY TOP"),
        Side::Bottom => ("B.Fab", "B.CrtYd", "ASSEMBLY BOTTOM (MIRRORED)"),
    };
    let bb = board_bbox_mm(board);
    let board_w = nm_to_mm(bb.width()).max(1.0);
    let header_h = (board_w * 0.045).clamp(1.6, 4.0);
    let min_x = if mirror_x {
        -nm_to_mm(bb.max_x)
    } else {
        nm_to_mm(bb.min_x)
    } - margin_mm;
    let max_x = if mirror_x {
        -nm_to_mm(bb.min_x)
    } else {
        nm_to_mm(bb.max_x)
    } + margin_mm;
    let min_y = -nm_to_mm(bb.max_y) - margin_mm - header_h;
    let max_y = -nm_to_mm(bb.min_y) + margin_mm;
    let vw = (max_x - min_x).max(1.0);
    let vh = (max_y - min_y).max(1.0);

    let mut out = String::new();
    let _ = writeln!(
        out,
        r#"<svg xmlns="http://www.w3.org/2000/svg" viewBox="{min_x:.4} {min_y:.4} {vw:.4} {vh:.4}" width="{:.0}px" height="{:.0}px">"#,
        (vw * 20.0).clamp(320.0, 2400.0),
        (vh * 20.0).clamp(240.0, 2400.0)
    );
    let _ = writeln!(
        out,
        "  <title>{} - {side_title}</title>",
        xml_escape(&board.name)
    );
    let _ = writeln!(
        out,
        r##"  <rect x="{min_x:.4}" y="{min_y:.4}" width="{vw:.4}" height="{vh:.4}" fill="#ffffff" />"##
    );
    let title_fs = (header_h * 0.55).clamp(0.8, 2.4);
    let _ = writeln!(
        out,
        r##"  <text x="{:.4}" y="{:.4}" fill="#0f172a" font-family="monospace" font-size="{title_fs:.4}" font-weight="700">{} - {side_title}</text>"##,
        min_x + margin_mm,
        min_y + header_h * 0.72,
        xml_escape(&board.name)
    );

    write_board_outline(&mut out, board, mirror_x);

    // Subtle pad outlines on this side help verify pin-1 and orientation against component bodies.
    out.push_str("  <g id=\"pads\" opacity=\"0.85\">\n");
    for pl in &board.placements {
        let Some(fp) = board.footprints.get(&pl.footprint) else {
            continue;
        };
        for pad in &fp.pads {
            if pad.kind != PadKind::Npth
                && pad_copper_layers(pad, pl.side, &cu_layers)
                    .iter()
                    .any(|l| l == cu_layer)
            {
                let shape = pad_to_world_shape(pad, pl, 16);
                render_shape_svg(&mut out, &shape, "#e2e8f0", mirror_x);
            }
            if let Some(drill) = pad.drill_nm
                && drill > 0
            {
                let pos = local_to_board(pad.pos, pl.pos.x, pl.pos.y, pl.rot_deg, pl.side);
                let (cx, cy) = svg_pt(pos, mirror_x);
                let _ = writeln!(
                    out,
                    r##"  <circle cx="{cx:.4}" cy="{cy:.4}" r="{:.4}" fill="#ffffff" stroke="#94a3b8" stroke-width="0.04" />"##,
                    nm_to_mm(drill / 2)
                );
            }
        }
    }
    out.push_str("  </g>\n");

    // Courtyard outlines (`F.CrtYd` / `B.CrtYd`).
    out.push_str("  <g id=\"courtyard\">\n");
    for pl in &board.placements {
        let Some(fp) = board.footprints.get(&pl.footprint) else {
            continue;
        };
        if pl.side == side && !fp.courtyard.is_empty() {
            let pts: Vec<Pt> = fp
                .courtyard
                .iter()
                .map(|&p| local_to_board(p, pl.pos.x, pl.pos.y, pl.rot_deg, pl.side))
                .collect();
            let d = poly_path_d(&pts, mirror_x);
            let _ = writeln!(
                out,
                r##"  <path d="{d}" fill="none" stroke="#64748b" stroke-width="0.06" stroke-dasharray="0.28,0.18" stroke-linejoin="round" />"##
            );
        }
        for g in &fp.graphics {
            if remap_layer_for_side(g.layer(), pl.side) == crtyd_layer {
                render_graphic_svg(
                    &mut out,
                    g,
                    Some(pl),
                    "#64748b",
                    mirror_x,
                    Some("0.28,0.18"),
                );
            }
        }
    }
    for g in &board.graphics {
        if g.layer() == crtyd_layer {
            render_graphic_svg(&mut out, g, None, "#64748b", mirror_x, Some("0.28,0.18"));
        }
    }
    out.push_str("  </g>\n");

    // Fabrication outlines (`F.Fab` / `B.Fab`).
    out.push_str("  <g id=\"fab\">\n");
    for pl in &board.placements {
        let Some(fp) = board.footprints.get(&pl.footprint) else {
            continue;
        };
        let (color, dash) = if pl.dnp {
            ("#94a3b8", Some("0.20,0.15"))
        } else {
            ("#0f172a", None)
        };
        for g in &fp.graphics {
            if remap_layer_for_side(g.layer(), pl.side) == fab_layer {
                render_graphic_svg(&mut out, g, Some(pl), color, mirror_x, dash);
            }
        }
    }
    for g in &board.graphics {
        if g.layer() == fab_layer {
            render_graphic_svg(&mut out, g, None, "#0f172a", mirror_x, None);
        }
    }
    out.push_str("  </g>\n");

    // DNP crossout X marks across courtyard/body bbox.
    let dnp_parts: Vec<&Placement> = board
        .placements
        .iter()
        .filter(|pl| pl.side == side && pl.dnp)
        .collect();
    if !dnp_parts.is_empty() {
        out.push_str("  <g id=\"dnp\">\n");
        for pl in dnp_parts {
            let lb = board
                .footprints
                .get(&pl.footprint)
                .map(|fp| fp_dnp_local_bbox(fp, pl.side, fab_layer))
                .unwrap_or_else(|| BBoxNm::new(-500_000, -500_000, 500_000, 500_000));
            let corners = [
                Pt::new(lb.min_x, lb.min_y),
                Pt::new(lb.max_x, lb.min_y),
                Pt::new(lb.max_x, lb.max_y),
                Pt::new(lb.min_x, lb.max_y),
            ]
            .map(|p| {
                svg_pt(
                    local_to_board(p, pl.pos.x, pl.pos.y, pl.rot_deg, pl.side),
                    mirror_x,
                )
            });
            for (a, b) in [(corners[0], corners[2]), (corners[1], corners[3])] {
                let _ = writeln!(
                    out,
                    r##"  <line x1="{:.4}" y1="{:.4}" x2="{:.4}" y2="{:.4}" stroke="#dc2626" stroke-opacity="0.65" stroke-width="0.08" />"##,
                    a.0, a.1, b.0, b.1
                );
            }
        }
        out.push_str("  </g>\n");
    }

    // Reference designators centered in each component body, always upright and readable.
    out.push_str("  <g id=\"refdes\">\n");
    for pl in &board.placements {
        if pl.side != side || pl.refdes.is_empty() {
            continue;
        }
        let lb = board
            .footprints
            .get(&pl.footprint)
            .map(|fp| fp_assembly_local_bbox(fp, pl.side, fab_layer))
            .unwrap_or_else(|| BBoxNm::new(-500_000, -500_000, 500_000, 500_000));
        let corners = [
            Pt::new(lb.min_x, lb.min_y),
            Pt::new(lb.max_x, lb.min_y),
            Pt::new(lb.max_x, lb.max_y),
            Pt::new(lb.min_x, lb.max_y),
        ]
        .map(|p| {
            svg_pt(
                local_to_board(p, pl.pos.x, pl.pos.y, pl.rot_deg, pl.side),
                mirror_x,
            )
        });
        let min_sx = corners.iter().map(|c| c.0).fold(f64::INFINITY, f64::min);
        let max_sx = corners
            .iter()
            .map(|c| c.0)
            .fold(f64::NEG_INFINITY, f64::max);
        let min_sy = corners.iter().map(|c| c.1).fold(f64::INFINITY, f64::min);
        let max_sy = corners
            .iter()
            .map(|c| c.1)
            .fold(f64::NEG_INFINITY, f64::max);
        let sw = (max_sx - min_sx).max(0.5);
        let sh = (max_sy - min_sy).max(0.5);
        let (cx, cy) = svg_pt(
            local_to_board(lb.center(), pl.pos.x, pl.pos.y, pl.rot_deg, pl.side),
            mirror_x,
        );
        let vertical = sh > sw * 1.4 && pl.refdes.len() >= 2;
        let long_side = sw.max(sh);
        let short_side = sw.min(sh);
        let fs = ((long_side * 0.80) / (pl.refdes.len() as f64 * 0.62))
            .min(short_side * 0.68)
            .clamp(0.30, 1.60);
        let (fill, extra) = if pl.dnp {
            ("#dc2626", r#" text-decoration="line-through""#)
        } else {
            ("#1d4ed8", "")
        };
        let rot = if vertical {
            format!(r#" transform="rotate(-90 {cx:.4} {cy:.4})""#)
        } else {
            String::new()
        };
        let _ = writeln!(
            out,
            r#"  <text x="{cx:.4}" y="{cy:.4}" fill="{fill}" font-family="monospace" font-size="{fs:.4}" font-weight="600" text-anchor="middle" dominant-baseline="central"{extra}{rot}>{}</text>"#,
            xml_escape(&pl.refdes)
        );
    }
    out.push_str("  </g>\n");

    out.push_str("</svg>\n");
    out
}

fn draw_drill_symbol(
    out: &mut String,
    sym_idx: usize,
    cx: f64,
    cy: f64,
    r: f64,
    color: &str,
    sw: f64,
) {
    let s = r * 0.82;
    let diag = r * 0.72;
    let draw_x = |out: &mut String, d: f64| {
        let _ = writeln!(
            out,
            r#"    <path d="M{:.4},{:.4} L{:.4},{:.4} M{:.4},{:.4} L{:.4},{:.4}" fill="none" stroke="{color}" stroke-width="{sw:.4}" stroke-linecap="round" />"#,
            cx - d,
            cy - d,
            cx + d,
            cy + d,
            cx - d,
            cy + d,
            cx + d,
            cy - d
        );
    };
    let draw_plus = |out: &mut String, d: f64| {
        let _ = writeln!(
            out,
            r#"    <path d="M{:.4},{cy:.4} L{:.4},{cy:.4} M{cx:.4},{:.4} L{cx:.4},{:.4}" fill="none" stroke="{color}" stroke-width="{sw:.4}" stroke-linecap="round" />"#,
            cx - d,
            cx + d,
            cy - d,
            cy + d
        );
    };
    let draw_circle = |out: &mut String| {
        let _ = writeln!(
            out,
            r#"    <circle cx="{cx:.4}" cy="{cy:.4}" r="{r:.4}" fill="none" stroke="{color}" stroke-width="{sw:.4}" />"#
        );
    };
    let draw_square = |out: &mut String| {
        let _ = writeln!(
            out,
            r#"    <rect x="{:.4}" y="{:.4}" width="{:.4}" height="{:.4}" fill="none" stroke="{color}" stroke-width="{sw:.4}" />"#,
            cx - s,
            cy - s,
            s * 2.0,
            s * 2.0
        );
    };

    match sym_idx % SYMBOL_COUNT {
        0 => draw_x(out, diag),
        1 => draw_plus(out, r),
        2 => draw_circle(out),
        3 => draw_square(out),
        4 => {
            let _ = writeln!(
                out,
                r#"    <path d="M{cx:.4},{:.4} L{:.4},{cy:.4} L{cx:.4},{:.4} L{:.4},{cy:.4} Z" fill="none" stroke="{color}" stroke-width="{sw:.4}" stroke-linejoin="round" />"#,
                cy - r,
                cx + r,
                cy + r,
                cx - r
            );
        }
        5 => {
            let dx = r * 0.866;
            let dy = r * 0.5;
            let _ = writeln!(
                out,
                r#"    <path d="M{cx:.4},{:.4} L{:.4},{:.4} L{:.4},{:.4} Z" fill="none" stroke="{color}" stroke-width="{sw:.4}" stroke-linejoin="round" />"#,
                cy - r,
                cx + dx,
                cy + dy,
                cx - dx,
                cy + dy
            );
        }
        6 => {
            let dx = r * 0.866;
            let dy = r * 0.5;
            let _ = writeln!(
                out,
                r#"    <path d="M{cx:.4},{:.4} L{:.4},{:.4} L{:.4},{:.4} Z" fill="none" stroke="{color}" stroke-width="{sw:.4}" stroke-linejoin="round" />"#,
                cy + r,
                cx + dx,
                cy - dy,
                cx - dx,
                cy - dy
            );
        }
        7 => {
            draw_circle(out);
            draw_plus(out, r);
        }
        8 => {
            draw_circle(out);
            draw_x(out, diag);
        }
        9 => {
            draw_square(out);
            draw_x(out, s);
        }
        10 => {
            draw_square(out);
            draw_plus(out, s);
        }
        _ => {
            draw_plus(out, r);
            draw_x(out, diag);
        }
    }
}

pub(crate) fn render_drill_map_svg_with_sections(
    board: &BoardIr,
    sections: &[DrillSection],
    margin_mm: f64,
) -> String {
    struct ToolEntry<'a> {
        sym_idx: usize,
        tool_code: String,
        kind_label: String,
        plated: bool,
        dia_nm: Nm,
        pts: &'a [Pt],
    }

    let mut tools: Vec<ToolEntry<'_>> = Vec::new();
    for sec in sections {
        let kind_label = sec.type_label();
        for (idx, (&dia_nm, pts)) in sec.by_dia.iter().enumerate() {
            if pts.is_empty() {
                continue;
            }
            let sym_idx = tools.len();
            tools.push(ToolEntry {
                sym_idx,
                tool_code: format!("T{:02}", idx + 1),
                kind_label: kind_label.clone(),
                plated: sec.plated,
                dia_nm,
                pts,
            });
        }
    }

    let bb = board_bbox_mm(board);
    let board_min_x = nm_to_mm(bb.min_x);
    let board_max_x = nm_to_mm(bb.max_x);
    let board_min_y = -nm_to_mm(bb.max_y);
    let board_max_y = -nm_to_mm(bb.min_y);
    let board_w = (board_max_x - board_min_x).max(1.0);

    let font_size = (board_w * 0.032).clamp(0.85, 2.8);
    let row_h = font_size * 1.6;
    let legend_rows = tools.len().max(1);
    let legend_gap = margin_mm.max(row_h);
    let legend_h = legend_gap + row_h * (legend_rows as f64 + 3.2);
    let min_legend_w = font_size * 38.0;

    let min_x = board_min_x - margin_mm;
    let max_x = board_max_x.max(board_min_x + min_legend_w) + margin_mm;
    let min_y = board_min_y - margin_mm;
    let max_y = board_max_y + margin_mm + legend_h;
    let vw = (max_x - min_x).max(1.0);
    let vh = (max_y - min_y).max(1.0);

    let mut out = String::new();
    let _ = writeln!(
        out,
        r#"<svg xmlns="http://www.w3.org/2000/svg" viewBox="{min_x:.4} {min_y:.4} {vw:.4} {vh:.4}" width="{:.0}px" height="{:.0}px">"#,
        (vw * 20.0).clamp(320.0, 2400.0),
        (vh * 20.0).clamp(240.0, 2400.0)
    );
    let _ = writeln!(
        out,
        "  <title>{} - DRILL MAP</title>",
        xml_escape(&board.name)
    );
    let _ = writeln!(
        out,
        r##"  <rect x="{min_x:.4}" y="{min_y:.4}" width="{vw:.4}" height="{vh:.4}" fill="#ffffff" />"##
    );

    write_board_outline(&mut out, board, false);

    out.push_str("  <g id=\"drill_hits\">\n");
    for tool in &tools {
        let color = DRILL_COLORS[tool.sym_idx % DRILL_COLORS.len()];
        let dia_mm = nm_to_mm(tool.dia_nm);
        let r = (dia_mm * 0.5).max(0.15);
        let sw = (r * 0.22).clamp(0.04, 0.14);
        let _ = writeln!(
            out,
            r#"  <g data-tool="{}" data-type="{}" data-diameter-mm="{dia_mm:.4}">"#,
            tool.tool_code,
            xml_escape(&tool.kind_label)
        );
        for &pt in tool.pts {
            let (cx, cy) = svg_pt(pt, false);
            draw_drill_symbol(&mut out, tool.sym_idx, cx, cy, r, color, sw);
        }
        out.push_str("  </g>\n");
    }
    out.push_str("  </g>\n");

    out.push_str("  <g id=\"drill_legend\">\n");
    let lx = board_min_x;
    let mut ly = board_max_y + legend_gap;
    let _ = writeln!(
        out,
        r##"  <text x="{lx:.4}" y="{ly:.4}" fill="#0f172a" font-family="monospace" font-size="{:.4}" font-weight="700">DRILL MAP: {}</text>"##,
        font_size * 1.1,
        xml_escape(&board.name)
    );
    ly += row_h * 1.1;
    let col_tool = lx + font_size * 2.4;
    let col_dia = lx + font_size * 6.5;
    let col_type = lx + font_size * 22.0;
    let col_qty = lx + font_size * 34.0;
    let _ = writeln!(
        out,
        r##"  <text x="{lx:.4}" y="{ly:.4}" fill="#475569" font-family="monospace" font-size="{font_size:.4}" font-weight="700">Sym</text>"##
    );
    let _ = writeln!(
        out,
        r##"  <text x="{col_tool:.4}" y="{ly:.4}" fill="#475569" font-family="monospace" font-size="{font_size:.4}" font-weight="700">Tool</text>"##
    );
    let _ = writeln!(
        out,
        r##"  <text x="{col_dia:.4}" y="{ly:.4}" fill="#475569" font-family="monospace" font-size="{font_size:.4}" font-weight="700">Diameter</text>"##
    );
    let _ = writeln!(
        out,
        r##"  <text x="{col_type:.4}" y="{ly:.4}" fill="#475569" font-family="monospace" font-size="{font_size:.4}" font-weight="700">Type</text>"##
    );
    let _ = writeln!(
        out,
        r##"  <text x="{col_qty:.4}" y="{ly:.4}" fill="#475569" font-family="monospace" font-size="{font_size:.4}" font-weight="700">Qty</text>"##
    );
    let rule_y = ly + font_size * 0.35;
    let _ = writeln!(
        out,
        r##"  <line x1="{lx:.4}" y1="{rule_y:.4}" x2="{:.4}" y2="{rule_y:.4}" stroke="#cbd5e1" stroke-width="{:.4}" />"##,
        max_x - margin_mm,
        (font_size * 0.08).clamp(0.05, 0.2)
    );
    ly += row_h;

    if tools.is_empty() {
        let _ = writeln!(
            out,
            r##"  <text x="{lx:.4}" y="{ly:.4}" fill="#475569" font-family="monospace" font-size="{font_size:.4}">No drill hits</text>"##
        );
    } else {
        let sym_r = font_size * 0.38;
        let sym_sw = (font_size * 0.10).clamp(0.05, 0.20);
        let mut total_plated = 0usize;
        let mut total_unplated = 0usize;
        for tool in &tools {
            let color = DRILL_COLORS[tool.sym_idx % DRILL_COLORS.len()];
            let dia_mm = nm_to_mm(tool.dia_nm);
            let dia_in = dia_mm / 25.4;
            if tool.plated {
                total_plated += tool.pts.len();
            } else {
                total_unplated += tool.pts.len();
            }
            draw_drill_symbol(
                &mut out,
                tool.sym_idx,
                lx + font_size * 0.8,
                ly - font_size * 0.30,
                sym_r,
                color,
                sym_sw,
            );
            let _ = writeln!(
                out,
                r##"  <text x="{col_tool:.4}" y="{ly:.4}" fill="#0f172a" font-family="monospace" font-size="{font_size:.4}">{}</text>"##,
                tool.tool_code
            );
            let _ = writeln!(
                out,
                r##"  <text x="{col_dia:.4}" y="{ly:.4}" fill="#0f172a" font-family="monospace" font-size="{font_size:.4}">{dia_mm:.4} mm ({dia_in:.4}&quot;)</text>"##
            );
            let _ = writeln!(
                out,
                r##"  <text x="{col_type:.4}" y="{ly:.4}" fill="#0f172a" font-family="monospace" font-size="{font_size:.4}">{}</text>"##,
                xml_escape(&tool.kind_label)
            );
            let _ = writeln!(
                out,
                r##"  <text x="{col_qty:.4}" y="{ly:.4}" fill="#0f172a" font-family="monospace" font-size="{font_size:.4}">{}</text>"##,
                tool.pts.len()
            );
            ly += row_h;
        }
        let total = total_plated + total_unplated;
        let _ = writeln!(
            out,
            r##"  <text x="{lx:.4}" y="{:.4}" fill="#334155" font-family="monospace" font-size="{font_size:.4}" font-weight="600">Total: {total} holes ({total_plated} plated, {total_unplated} unplated)</text>"##,
            ly + row_h * 0.2
        );
    }
    out.push_str("  </g>\n");

    out.push_str("</svg>\n");
    out
}

pub(crate) fn render_drill_map_svg(board: &BoardIr, margin_mm: f64) -> String {
    let cu_layers = board.copper_layer_names();
    let sections = collect_drill_sections(board, &cu_layers);
    render_drill_map_svg_with_sections(board, &sections, margin_mm)
}
