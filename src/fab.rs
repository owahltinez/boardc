use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};

use gerber_types::{ExtendedPosition, FileFunction, Polarity, Position, Profile};
use serde::{Deserialize, Serialize};

use crate::fab_assembly::{has_bottom_assembly, render_assembly_svg};
pub use crate::fab_drills::render_drill_report;
use crate::fab_drills::{
    collect_drill_sections, emit_excellon_section, format_drill_report,
    render_drill_map_svg_with_sections,
};
use crate::fab_gerber::{
    GerberBuilder, copper_gerber_filename, pad_anchor_world, pad_world_shape_with_margin,
};
use crate::fab_job::emit_gbrjob;
use crate::fab_silk::{emit_silk_layer, push_graphic_mask_shapes};
use crate::geom::{pad_copper_layers, pad_world_layers, polygon_area, remap_layer_for_side};
use crate::ir::{
    AutoRefdes, BoardIr, FabConfig, FootprintDef, LayerKind, Nm, PadKind, Placement, PourPoly,
    Side, nm_to_mm, safe_slug,
};
use crate::shapes::{ResolvedShape, pad_to_world_shape};
use crate::svg::DEFAULT_SVG_MARGIN_MM;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct FabOptions {
    pub clip_silk_to_mask: bool,
    pub assembly: bool,
    pub drill_map: bool,
    pub drill_report: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub auto_refdes: Option<AutoRefdes>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub refdes_stroke_nm: Option<Nm>,
}

impl Default for FabOptions {
    fn default() -> Self {
        Self {
            clip_silk_to_mask: true,
            assembly: false,
            drill_map: false,
            drill_report: false,
            auto_refdes: None,
            refdes_stroke_nm: None,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FabManifest {
    pub gerber_files: Vec<PathBuf>,
    pub drill_files: Vec<PathBuf>,
    pub bom_csv: PathBuf,
    pub cpl_csv: PathBuf,
    pub gbrjob: PathBuf,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub assembly_svgs: Vec<PathBuf>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub drill_map_svg: Option<PathBuf>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub drill_report: Option<PathBuf>,
}

impl FabManifest {
    pub fn all_files(&self) -> Vec<PathBuf> {
        let mut out = Vec::with_capacity(
            self.gerber_files.len() + self.drill_files.len() + self.assembly_svgs.len() + 5,
        );
        out.extend(self.gerber_files.iter().cloned());
        out.extend(self.drill_files.iter().cloned());
        out.push(self.bom_csv.clone());
        out.push(self.cpl_csv.clone());
        out.push(self.gbrjob.clone());
        out.extend(self.assembly_svgs.iter().cloned());
        out.extend(self.drill_map_svg.iter().chain(&self.drill_report).cloned());
        out
    }
}

fn copper_file_function(
    board: &BoardIr,
    layer_name: &str,
    idx: usize,
    total: usize,
) -> FileFunction {
    let pos = if idx == 0 {
        ExtendedPosition::Top
    } else if idx + 1 == total {
        ExtendedPosition::Bottom
    } else {
        ExtendedPosition::Inner
    };
    let copper_type = board
        .layers
        .iter()
        .find(|l| l.name == layer_name)
        .map(|l| match l.kind {
            LayerKind::Signal => gerber_types::CopperType::Signal,
            LayerKind::Plane => gerber_types::CopperType::Plane,
            LayerKind::Mixed => gerber_types::CopperType::Mixed,
        });
    FileFunction::Copper {
        layer: (idx + 1) as i32,
        pos,
        copper_type,
    }
}

fn emit_board_graphics(b: &mut GerberBuilder, board: &BoardIr, target_layer: &str) {
    for g in &board.graphics {
        if g.layer() == target_layer {
            b.emit_graphic(g, None);
        }
    }
}

fn emit_fp_graphics(b: &mut GerberBuilder, fp: &FootprintDef, pl: &Placement, target_layer: &str) {
    for g in &fp.graphics {
        if remap_layer_for_side(g.layer(), pl.side) == target_layer {
            b.emit_graphic(g, Some(pl));
        }
    }
}

fn emit_copper_layer(
    board: &BoardIr,
    cu_layers: &[String],
    idx: usize,
    out_path: &Path,
) -> Result<(), String> {
    let layer = &cu_layers[idx];
    let mut b = GerberBuilder::new(copper_file_function(board, layer, idx, cu_layers.len()));

    // Emit filled pours FIRST sorted by outer area descending so clear holes never erase nested copper.
    let mut layer_polys: Vec<&PourPoly> = board
        .filled_pours
        .iter()
        .filter(|fp| &fp.layer == layer)
        .flat_map(|fp| fp.polys.iter())
        .collect();
    layer_polys.sort_by(|a, b| polygon_area(&b.outer).total_cmp(&polygon_area(&a.outer)));
    for poly in layer_polys {
        b.fill_region(&poly.outer, Polarity::Dark);
        for hole in &poly.holes {
            b.fill_region(hole, Polarity::Clear);
        }
    }

    for tr in &board.tracks {
        if &tr.layer == layer {
            b.stroke_segment(tr.a, tr.b, tr.width);
        }
    }
    for pl in &board.placements {
        let Some(fp) = board.footprints.get(&pl.footprint) else {
            continue;
        };
        for pad in &fp.pads {
            if pad.kind == PadKind::Npth {
                continue;
            }
            if pad_copper_layers(pad, pl.side, cu_layers)
                .iter()
                .any(|l| l == layer)
            {
                let shape = pad_to_world_shape(pad, pl, 16);
                b.flash_shape(&shape, pad_anchor_world(pad, pl));
            }
        }
        emit_fp_graphics(&mut b, fp, pl, layer);
    }
    emit_board_graphics(&mut b, board, layer);
    for via in &board.vias {
        if via.touches_layer(layer, &board.layers) {
            b.flash_circle(via.pos, via.diameter);
        }
    }
    b.write_file(out_path)
}

fn emit_mask_layer(
    board: &BoardIr,
    cu_layers: &[String],
    side: Side,
    out_path: &Path,
    collect_openings: bool,
) -> Result<Vec<ResolvedShape>, String> {
    let (mask_name, pos) = match side {
        Side::Top => ("F.Mask", Position::Top),
        Side::Bottom => ("B.Mask", Position::Bottom),
    };
    let mut b = GerberBuilder::new(FileFunction::SolderMask { pos, index: None });
    let mut openings = Vec::new();
    for pl in &board.placements {
        let Some(fp) = board.footprints.get(&pl.footprint) else {
            continue;
        };
        for pad in &fp.pads {
            if !pad_world_layers(pad, pl.side, cu_layers)
                .iter()
                .any(|l| l == mask_name)
            {
                continue;
            }
            let margin = pad.mask_margin_nm.unwrap_or(board.rules.mask_expansion_nm);
            if let Some(shape) = pad_world_shape_with_margin(pad, pl, margin) {
                b.flash_shape(&shape, pad_anchor_world(pad, pl));
                if collect_openings {
                    openings.push(shape);
                }
            }
        }
        for g in &fp.graphics {
            if remap_layer_for_side(g.layer(), pl.side) == mask_name {
                b.emit_graphic(g, Some(pl));
                if collect_openings {
                    push_graphic_mask_shapes(g, Some(pl), &mut openings);
                }
            }
        }
    }
    for g in &board.graphics {
        if g.layer() == mask_name {
            b.emit_graphic(g, None);
            if collect_openings {
                push_graphic_mask_shapes(g, None, &mut openings);
            }
        }
    }
    for via in &board.vias {
        if !via.tented && via.touches_layer(side.cu_layer(), &board.layers) {
            let dia = via.diameter + 2 * board.rules.mask_expansion_nm;
            if dia > 0 {
                b.flash_circle(via.pos, dia);
                if collect_openings {
                    openings.push(ResolvedShape::Circle {
                        center: via.pos,
                        radius: dia / 2,
                    });
                }
            }
        }
    }
    b.write_file(out_path)?;
    Ok(openings)
}

fn emit_paste_layer(
    board: &BoardIr,
    cu_layers: &[String],
    side: Side,
    out_path: &Path,
) -> Result<(), String> {
    let (paste_name, pos) = match side {
        Side::Top => ("F.Paste", Position::Top),
        Side::Bottom => ("B.Paste", Position::Bottom),
    };
    let mut b = GerberBuilder::new(FileFunction::Paste(pos));
    for pl in &board.placements {
        if pl.dnp {
            continue;
        }
        let Some(fp) = board.footprints.get(&pl.footprint) else {
            continue;
        };
        for pad in &fp.pads {
            if pad.kind != PadKind::Smd {
                continue;
            }
            if !pad_world_layers(pad, pl.side, cu_layers)
                .iter()
                .any(|l| l == paste_name)
            {
                continue;
            }
            let margin = pad.paste_margin_nm.unwrap_or(board.rules.paste_margin_nm);
            if let Some(shape) = pad_world_shape_with_margin(pad, pl, margin) {
                b.flash_shape(&shape, pad_anchor_world(pad, pl));
            }
        }
        emit_fp_graphics(&mut b, fp, pl, paste_name);
    }
    emit_board_graphics(&mut b, board, paste_name);
    b.write_file(out_path)
}

fn emit_edge_cuts(board: &BoardIr, out_path: &Path) -> Result<(), String> {
    let mut b = GerberBuilder::new(FileFunction::Profile(Some(Profile::NonPlated)));
    let w = 50_000;
    b.stroke_closed_poly(&board.outline, w);
    for cut in &board.cutouts {
        b.stroke_closed_poly(cut, w);
    }
    b.write_file(out_path)
}

fn csv_escape(field: &str) -> String {
    if field.contains([',', '"', '\n', '\r']) {
        format!("\"{}\"", field.replace('"', "\"\""))
    } else {
        field.to_string()
    }
}

fn emit_bom_csv(board: &BoardIr, path: &Path) -> Result<(), String> {
    #[derive(PartialEq, Eq, PartialOrd, Ord)]
    struct BomKey {
        comment: String,
        footprint: String,
        lcsc: String,
        mpn: String,
    }
    let mut groups: BTreeMap<BomKey, Vec<String>> = BTreeMap::new();
    for pl in &board.placements {
        if pl.dnp || pl.exclude_from_bom {
            continue;
        }
        let comment = if pl.value.trim().is_empty() {
            pl.footprint.clone()
        } else {
            pl.value.clone()
        };
        let key = BomKey {
            comment,
            footprint: pl.footprint.clone(),
            lcsc: pl.lcsc.clone().unwrap_or_default(),
            mpn: pl.mpn.clone().unwrap_or_default(),
        };
        groups.entry(key).or_default().push(pl.refdes.clone());
    }
    let mut out = String::from("Comment,Designator,Footprint,LCSC Part #,MPN,Qty\n");
    for (k, mut refs) in groups {
        refs.sort();
        let joined = refs.join(",");
        out.push_str(&format!(
            "{},{},{},{},{},{}\n",
            csv_escape(&k.comment),
            csv_escape(&joined),
            csv_escape(&k.footprint),
            csv_escape(&k.lcsc),
            csv_escape(&k.mpn),
            refs.len()
        ));
    }
    fs::write(path, out).map_err(|e| format!("failed to write BOM '{}': {e}", path.display()))
}

fn emit_cpl_csv(board: &BoardIr, path: &Path) -> Result<(), String> {
    let mut active: Vec<&crate::ir::Placement> = board
        .placements
        .iter()
        .filter(|p| !p.dnp && !p.exclude_from_pos)
        .collect();
    active.sort_by(|a, b| a.refdes.cmp(&b.refdes));
    let mut out = String::from("Designator,Mid X,Mid Y,Layer,Rotation\n");
    for pl in active {
        let (layer_str, base_rot) = match pl.side {
            Side::Top => ("Top", pl.rot_deg),
            Side::Bottom => ("Bottom", (180.0 - pl.rot_deg).rem_euclid(360.0)),
        };
        let rot = (base_rot + pl.cpl_rotation_offset_deg).rem_euclid(360.0);
        out.push_str(&format!(
            "{},{:.4},{:.4},{},{:.2}\n",
            csv_escape(&pl.refdes),
            nm_to_mm(pl.pos.x),
            nm_to_mm(pl.pos.y),
            layer_str,
            rot
        ));
    }
    fs::write(path, out).map_err(|e| format!("failed to write CPL '{}': {e}", path.display()))
}

/// Exports RS-274X Gerbers, Excellon PTH/NPTH (and per-span blind/buried) drills, BOM CSV, and CPL CSV.
pub fn export_fab(
    board: &BoardIr,
    out_dir: &Path,
    cfg: &FabOptions,
) -> Result<FabManifest, String> {
    let fab = FabConfig {
        auto_refdes: cfg.auto_refdes.unwrap_or(board.fab.auto_refdes),
        refdes_stroke_nm: cfg.refdes_stroke_nm.unwrap_or(board.fab.refdes_stroke_nm),
    };
    fab.validate()?;
    fs::create_dir_all(out_dir)
        .map_err(|e| format!("failed to create fab dir '{}': {e}", out_dir.display()))?;
    let cu_layers = board.copper_layer_names();
    let slug = safe_slug(&board.name);

    let cu_paths: Vec<PathBuf> = cu_layers
        .iter()
        .enumerate()
        .map(|(idx, layer_name)| out_dir.join(copper_gerber_filename(layer_name, idx, &cu_layers)))
        .collect();
    let side_files = [
        (
            Side::Top,
            out_dir.join("F_Mask.gts"),
            out_dir.join("F_Paste.gtp"),
            out_dir.join("F_Silkscreen.gto"),
            Position::Top,
        ),
        (
            Side::Bottom,
            out_dir.join("B_Mask.gbs"),
            out_dir.join("B_Paste.gbp"),
            out_dir.join("B_Silkscreen.gbo"),
            Position::Bottom,
        ),
    ];
    let edge_path = out_dir.join("Edge_Cuts.gm1");

    let mut assembly_jobs: Vec<(Side, PathBuf)> = Vec::new();
    if cfg.assembly {
        assembly_jobs.push((Side::Top, out_dir.join(format!("{slug}-assembly-top.svg"))));
        if has_bottom_assembly(board) {
            assembly_jobs.push((
                Side::Bottom,
                out_dir.join(format!("{slug}-assembly-bottom.svg")),
            ));
        }
    }

    let drill_sections = collect_drill_sections(board, &cu_layers);
    let mut drill_files = Vec::with_capacity(drill_sections.len());
    let bom_csv = out_dir.join(format!("{slug}_bom.csv"));
    let cpl_csv = out_dir.join(format!("{slug}_cpl.csv"));
    let gbrjob = out_dir.join(format!("{slug}-job.gbrjob"));

    let mut drill_map_svg = None;
    let mut drill_report = None;

    std::thread::scope(|s| -> Result<(), String> {
        let mut handles = Vec::with_capacity(cu_paths.len() + 7);
        for (idx, path) in cu_paths.iter().enumerate() {
            let cu_ref = &cu_layers;
            handles.push(s.spawn(move || emit_copper_layer(board, cu_ref, idx, path)));
        }
        for (side, mask_p, paste_p, silk_p, fn_pos) in &side_files {
            let (side, fn_pos) = (*side, *fn_pos);
            let cu_ref = &cu_layers;
            let clip_silk = cfg.clip_silk_to_mask;
            handles.push(s.spawn(move || {
                let mask_openings = emit_mask_layer(board, cu_ref, side, mask_p, clip_silk)?;
                let mut silk_b = GerberBuilder::new(FileFunction::Legend {
                    pos: fn_pos,
                    index: None,
                });
                emit_silk_layer(&mut silk_b, board, side, &mask_openings, fab);
                silk_b.write_file(silk_p)
            }));
            handles.push(s.spawn(move || emit_paste_layer(board, cu_ref, side, paste_p)));
        }
        let edge_ref = &edge_path;
        handles.push(s.spawn(move || emit_edge_cuts(board, edge_ref)));

        for (side, path) in &assembly_jobs {
            let side = *side;
            handles.push(s.spawn(move || {
                fs::write(
                    path,
                    render_assembly_svg(board, side, DEFAULT_SVG_MARGIN_MM),
                )
                .map_err(|e| format!("failed to write '{}': {e}", path.display()))
            }));
        }

        for sec in &drill_sections {
            let path = out_dir.join(&sec.filename);
            emit_excellon_section(&path, sec)?;
            drill_files.push(path);
        }
        emit_bom_csv(board, &bom_csv)?;
        emit_cpl_csv(board, &cpl_csv)?;
        emit_gbrjob(board, &cu_layers, &gbrjob)?;

        if cfg.drill_map {
            let path = out_dir.join(format!("{slug}-drl_map.svg"));
            let svg =
                render_drill_map_svg_with_sections(board, &drill_sections, DEFAULT_SVG_MARGIN_MM);
            fs::write(&path, svg)
                .map_err(|e| format!("failed to write '{}': {e}", path.display()))?;
            drill_map_svg = Some(path);
        }
        if cfg.drill_report {
            let path = out_dir.join(format!("{slug}-drl.rpt"));
            let rpt = format_drill_report(board, &cu_layers, &drill_sections);
            fs::write(&path, rpt)
                .map_err(|e| format!("failed to write '{}': {e}", path.display()))?;
            drill_report = Some(path);
        }

        for h in handles {
            h.join().expect("fab export worker panicked")?;
        }
        Ok(())
    })?;

    let mut gerber_files = Vec::with_capacity(cu_paths.len() + 7);
    gerber_files.extend(cu_paths);
    for (_, mask_p, paste_p, silk_p, _) in side_files {
        gerber_files.extend([mask_p, paste_p, silk_p]);
    }
    gerber_files.push(edge_path);
    let assembly_svgs = assembly_jobs.into_iter().map(|(_, p)| p).collect();

    Ok(FabManifest {
        gerber_files,
        drill_files,
        bom_csv,
        cpl_csv,
        gbrjob,
        assembly_svgs,
        drill_map_svg,
        drill_report,
    })
}
