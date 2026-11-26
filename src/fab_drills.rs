use std::collections::BTreeMap;
use std::fmt::Write as _;
use std::fs;
use std::path::Path;

use crate::geom::local_to_board;
use crate::ir::{BoardIr, Nm, PadKind, Pt, Side, nm_to_mm, safe_slug};
use crate::svg::{
    begin_svg, board_svg_bounds, sanitize_margin_mm, svg_pt, write_board_outline, xml_escape,
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
    pub(crate) fn flat_hits(&self) -> Vec<(Nm, Pt)> {
        let mut out = Vec::with_capacity(self.total_hits());
        for (&dia, pts) in &self.by_dia {
            for &pt in pts {
                out.push((dia, pt));
            }
        }
        out
    }

    fn total_hits(&self) -> usize {
        self.by_dia.values().map(Vec::len).sum()
    }

    fn type_label(&self) -> String {
        let base = if self.plated { "PTH" } else { "NPTH" };
        match &self.span {
            Some((l0, l1)) => format!("{base} ({l0}..{l1})"),
            None => base.into(),
        }
    }
}

/// Quantizes positive drill diameters to Excellon's 0.0001 mm (100 nm) tool grid.
fn quantize_excellon_nm(dia: Nm) -> Nm {
    ((dia + 50) / 100).max(1) * 100
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
        let dia = quantize_excellon_nm(via.drill);
        if via.is_through_hole(cu_layers, &board.layers) {
            pth_by_dia.entry(dia).or_default().push(via.pos);
        } else {
            let span = via.span_layer_pair(cu_layers, &board.layers);
            span_by_dia
                .entry(span)
                .or_default()
                .entry(dia)
                .or_default()
                .push(via.pos);
        }
    }
    for pl in &board.placements {
        let Some(fp) = board.footprints.get(&pl.footprint) else {
            continue;
        };
        for pad in &fp.pads {
            if let Some(raw_drill) = pad.drill_nm
                && raw_drill > 0
            {
                let dia = quantize_excellon_nm(raw_drill);
                let world = local_to_board(pad.pos, pl.pos.x, pl.pos.y, pl.rot_deg, pl.side);
                match pad.kind {
                    PadKind::Tht => pth_by_dia.entry(dia).or_default().push(world),
                    PadKind::Npth => npth_by_dia.entry(dia).or_default().push(world),
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

pub(crate) fn emit_excellon_section(path: &Path, sec: &DrillSection) -> Result<(), String> {
    let mut out = String::new();
    out.push_str("M48\n");
    out.push_str("; DRILL file {boardc}\n");
    if let Some((l0, l1)) = &sec.span {
        let _ = writeln!(out, "; LAYER_SPAN={l0}..{l1}");
    }
    out.push_str("; FORMAT={-:-/ absolute / metric / decimal}\n");
    out.push_str("FMAT,2\n");
    out.push_str("METRIC,TZ\n");
    for (idx, dia) in sec.by_dia.keys().enumerate() {
        let _ = writeln!(out, "T{:02}C{:.4}", idx + 1, nm_to_mm(*dia));
    }
    out.push_str("%\nG90\nG05\n");
    for (idx, pts) in sec.by_dia.values().enumerate() {
        let _ = writeln!(out, "T{:02}", idx + 1);
        for pt in pts {
            let _ = writeln!(out, "X{:.4}Y{:.4}", nm_to_mm(pt.x), nm_to_mm(pt.y));
        }
    }
    if !sec.by_dia.is_empty() {
        out.push_str("T00\n");
    }
    out.push_str("M30\n");
    fs::write(path, out).map_err(|e| format!("failed to write drill '{}': {e}", path.display()))
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
            let bottom = layer_name == "B.Cu"
                || board
                    .layers
                    .iter()
                    .find(|l| &l.name == layer_name)
                    .is_some_and(|l| l.side == Some(Side::Bottom));
            if bottom {
                "back".into()
            } else {
                "front".into()
            }
        } else if idx == 0 {
            "front".into()
        } else if idx + 1 == cu_layers.len() {
            "back".into()
        } else {
            format!("in{idx}")
        };
        let _ = writeln!(out, "    L{:<2} :  {:<24}  {pos}", idx + 1, layer_name);
    }

    for sec in sections {
        let plating = if sec.plated { "plated" } else { "unplated" };
        let _ = writeln!(out, "\n\nDrill file '{}' contains", sec.filename);
        match &sec.span {
            Some((l0, l1)) => {
                let _ = writeln!(out, "    {plating} holes from {l0} to {l1}:");
            }
            None => {
                let _ = writeln!(out, "    {plating} through holes:");
            }
        }
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
        let _ = writeln!(
            out,
            "\n    Total {plating} holes count {}",
            sec.total_hits()
        );
    }
    out
}

/// Renders a deterministic text drill report matching the emitted Excellon drill tool tables.
pub fn render_drill_report(board: &BoardIr) -> String {
    let cu_layers = board.copper_layer_names();
    let sections = collect_drill_sections(board, &cu_layers);
    format_drill_report(board, &cu_layers, &sections)
}

fn drill_color(sym_idx: usize) -> &'static str {
    // Shift color by pass index so all SYMBOL_COUNT * DRILL_COLORS.len() = 72 pairs are unique.
    DRILL_COLORS[(sym_idx + sym_idx / SYMBOL_COUNT) % DRILL_COLORS.len()]
}

fn draw_drill_symbol(out: &mut String, sym_idx: usize, cx: f64, cy: f64, r: f64) {
    let s = r * 0.82;
    let diag = r * 0.72;
    let draw_x = |out: &mut String, d: f64| {
        let _ = writeln!(
            out,
            r#"    <path d="M{:.4},{:.4} L{:.4},{:.4} M{:.4},{:.4} L{:.4},{:.4}" />"#,
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
            r#"    <path d="M{:.4},{cy:.4} L{:.4},{cy:.4} M{cx:.4},{:.4} L{cx:.4},{:.4}" />"#,
            cx - d,
            cx + d,
            cy - d,
            cy + d
        );
    };
    let draw_circle = |out: &mut String| {
        let _ = writeln!(
            out,
            r#"    <circle cx="{cx:.4}" cy="{cy:.4}" r="{r:.4}" />"#
        );
    };
    let draw_square = |out: &mut String| {
        let _ = writeln!(
            out,
            r#"    <rect x="{:.4}" y="{:.4}" width="{:.4}" height="{:.4}" />"#,
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
                r#"    <path d="M{cx:.4},{:.4} L{:.4},{cy:.4} L{cx:.4},{:.4} L{:.4},{cy:.4} Z" />"#,
                cy - r,
                cx + r,
                cy + r,
                cx - r
            );
        }
        sym @ (5 | 6) => {
            let dir = if sym == 5 { 1.0 } else { -1.0 };
            let dx = r * 0.866;
            let dy = r * 0.5 * dir;
            let _ = writeln!(
                out,
                r#"    <path d="M{cx:.4},{:.4} L{:.4},{:.4} L{:.4},{:.4} Z" />"#,
                cy - r * dir,
                cx + dx,
                cy + dy,
                cx - dx,
                cy + dy
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
    let margin = sanitize_margin_mm(margin_mm);
    let mut tools: Vec<(usize, String, bool, Nm, &[Pt])> = Vec::new();
    for sec in sections {
        let kind_label = sec.type_label();
        for (idx, (&dia_nm, pts)) in sec.by_dia.iter().enumerate() {
            tools.push((idx + 1, kind_label.clone(), sec.plated, dia_nm, pts));
        }
    }

    let (board_min_x, board_max_x, board_min_y, board_max_y) = board_svg_bounds(board, false);
    let board_w = (board_max_x - board_min_x).max(1.0);
    let fs = (board_w * 0.032).clamp(0.85, 2.8);
    let row_h = fs * 1.6;
    let legend_gap = margin.max(row_h);
    let legend_h = legend_gap + row_h * (tools.len().max(1) as f64 + 3.2);

    let col_tool_off = fs * 2.4;
    let col_dia_off = fs * 6.5;
    let col_type_off = fs * 22.0;
    let max_type_chars = tools
        .iter()
        .map(|(_, kind, _, _, _)| kind.chars().count())
        .max()
        .unwrap_or(4)
        .max(4);
    let col_qty_off = col_type_off + fs * (max_type_chars as f64 * 0.65 + 1.5).max(8.0);
    let max_qty_chars = tools
        .iter()
        .map(|(_, _, _, _, pts)| pts.len().to_string().len())
        .max()
        .unwrap_or(3)
        .max(3);
    let table_w = col_qty_off + fs * (max_qty_chars as f64 * 0.65 + 1.0);
    let title_text = format!("DRILL MAP: {}", board.name);
    let title_w = title_text.chars().count() as f64 * (fs * 1.1) * 0.64;

    let total_plated: usize = tools.iter().filter(|t| t.2).map(|t| t.4.len()).sum();
    let total_unplated: usize = tools.iter().filter(|t| !t.2).map(|t| t.4.len()).sum();
    let total = total_plated + total_unplated;
    let hole_noun = if total == 1 { "hole" } else { "holes" };
    let total_line =
        format!("Total: {total} {hole_noun} ({total_plated} plated, {total_unplated} unplated)");
    let total_w = total_line.chars().count() as f64 * fs * 0.64;
    let min_legend_w = table_w.max(title_w).max(total_w);

    let min_x = board_min_x - margin;
    let max_x = board_max_x.max(board_min_x + min_legend_w) + margin;
    let min_y = board_min_y - margin;
    let max_y = board_max_y + margin + legend_h;
    let vw = (max_x - min_x).max(1.0);
    let vh = (max_y - min_y).max(1.0);

    let mut out = begin_svg(
        &format!("{} - DRILL MAP", board.name),
        min_x,
        min_y,
        vw,
        vh,
        "#ffffff",
    );
    write_board_outline(&mut out, board, false, "#f8fafc", "#111827", "0.15");

    out.push_str("  <g id=\"drill_hits\">\n");
    for (sym_idx, (tool_num, kind_label, _, dia_nm, pts)) in tools.iter().enumerate() {
        let color = drill_color(sym_idx);
        let dia_mm = nm_to_mm(*dia_nm);
        let r = (dia_mm * 0.5).max(0.15);
        let sw = (r * 0.22).clamp(0.04, 0.14);
        let _ = writeln!(
            out,
            r#"  <g data-tool="T{tool_num:02}" data-type="{}" data-diameter-mm="{dia_mm:.4}" fill="none" stroke="{color}" stroke-width="{sw:.4}" stroke-linecap="round" stroke-linejoin="round">"#,
            xml_escape(kind_label)
        );
        for &pt in *pts {
            let (cx, cy) = svg_pt(pt, false);
            draw_drill_symbol(&mut out, sym_idx, cx, cy, r);
        }
        out.push_str("  </g>\n");
    }
    out.push_str("  </g>\n");

    let lx = board_min_x;
    let mut ly = board_max_y + legend_gap;
    let col_tool = lx + col_tool_off;
    let col_dia = lx + col_dia_off;
    let col_type = lx + col_type_off;
    let col_qty = lx + col_qty_off;
    let _ = writeln!(
        out,
        r##"  <g id="drill_legend" font-family="monospace" font-size="{fs:.4}" fill="#0f172a">"##
    );
    let _ = writeln!(
        out,
        r#"  <text x="{lx:.4}" y="{ly:.4}" font-size="{:.4}" font-weight="700">{}</text>"#,
        fs * 1.1,
        xml_escape(&title_text)
    );
    ly += row_h * 1.1;
    for (x, label) in [
        (lx, "Sym"),
        (col_tool, "Tool"),
        (col_dia, "Diameter"),
        (col_type, "Type"),
        (col_qty, "Qty"),
    ] {
        let _ = writeln!(
            out,
            r##"  <text x="{x:.4}" y="{ly:.4}" fill="#475569" font-weight="700">{label}</text>"##
        );
    }
    let rule_y = ly + fs * 0.35;
    let _ = writeln!(
        out,
        r##"  <line x1="{lx:.4}" y1="{rule_y:.4}" x2="{:.4}" y2="{rule_y:.4}" stroke="#cbd5e1" stroke-width="{:.4}" />"##,
        max_x - margin,
        (fs * 0.08).clamp(0.05, 0.2)
    );
    ly += row_h;

    if tools.is_empty() {
        let _ = writeln!(
            out,
            r##"  <text x="{lx:.4}" y="{ly:.4}" fill="#475569">No drill hits</text>"##
        );
    } else {
        let sym_r = fs * 0.38;
        let sym_sw = (fs * 0.10).clamp(0.05, 0.20);
        for (sym_idx, (tool_num, kind_label, _, dia_nm, pts)) in tools.iter().enumerate() {
            let color = drill_color(sym_idx);
            let dia_mm = nm_to_mm(*dia_nm);
            let dia_in = dia_mm / 25.4;
            let _ = writeln!(
                out,
                r#"  <g fill="none" stroke="{color}" stroke-width="{sym_sw:.4}" stroke-linecap="round" stroke-linejoin="round">"#
            );
            draw_drill_symbol(&mut out, sym_idx, lx + fs * 0.8, ly - fs * 0.30, sym_r);
            out.push_str("  </g>\n");
            let _ = writeln!(
                out,
                r#"  <text x="{col_tool:.4}" y="{ly:.4}">T{tool_num:02}</text>"#
            );
            let _ = writeln!(
                out,
                r#"  <text x="{col_dia:.4}" y="{ly:.4}">{dia_mm:.4} mm ({dia_in:.4}&quot;)</text>"#
            );
            let _ = writeln!(
                out,
                r#"  <text x="{col_type:.4}" y="{ly:.4}">{}</text>"#,
                xml_escape(kind_label)
            );
            let _ = writeln!(
                out,
                r#"  <text x="{col_qty:.4}" y="{ly:.4}">{}</text>"#,
                pts.len()
            );
            ly += row_h;
        }
        let _ = writeln!(
            out,
            r##"  <text x="{lx:.4}" y="{:.4}" fill="#334155" font-weight="600">{total_line}</text>"##,
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
