use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};

use gerber_types::{ExtendedPosition, FileFunction, Polarity, Position, Profile};
use serde::{Deserialize, Serialize};

use crate::fab_gerber::{
    GerberBuilder, copper_gerber_filename, pad_anchor_world, pad_world_shape_with_margin,
};
use crate::fab_job::emit_gbrjob;
use crate::fab_silk::emit_silk_layer;
use crate::geom::{
    local_to_board, pad_copper_layers, pad_world_layers, polygon_area, remap_layer_for_side,
};
use crate::ir::{
    BoardIr, FootprintDef, LayerKind, Nm, PadKind, Placement, PourPoly, Pt, Side, nm_to_mm,
    safe_slug,
};
use crate::shapes::pad_to_world_shape;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FabOptions {
    pub clip_silk_to_mask: bool,
}

impl Default for FabOptions {
    fn default() -> Self {
        Self {
            clip_silk_to_mask: true,
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
}

impl FabManifest {
    pub fn all_files(&self) -> Vec<PathBuf> {
        let mut out = Vec::with_capacity(self.gerber_files.len() + self.drill_files.len() + 3);
        out.extend(self.gerber_files.iter().cloned());
        out.extend(self.drill_files.iter().cloned());
        out.push(self.bom_csv.clone());
        out.push(self.cpl_csv.clone());
        out.push(self.gbrjob.clone());
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
) -> Result<(), String> {
    let (mask_name, pos) = match side {
        Side::Top => ("F.Mask", Position::Top),
        Side::Bottom => ("B.Mask", Position::Bottom),
    };
    let mut b = GerberBuilder::new(FileFunction::SolderMask { pos, index: None });
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
            }
        }
        emit_fp_graphics(&mut b, fp, pl, mask_name);
    }
    emit_board_graphics(&mut b, board, mask_name);
    for via in &board.vias {
        if !via.tented && via.touches_layer(side.cu_layer(), &board.layers) {
            let dia = via.diameter + 2 * board.rules.mask_expansion_nm;
            if dia > 0 {
                b.flash_circle(via.pos, dia);
            }
        }
    }
    b.write_file(out_path)
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

fn emit_excellon(
    path: &Path,
    hits: &[(Nm, Pt)],
    span_comment: Option<(&str, &str)>,
) -> Result<(), String> {
    let mut by_dia: BTreeMap<Nm, Vec<Pt>> = BTreeMap::new();
    for &(dia, pt) in hits {
        if dia > 0 {
            by_dia.entry(dia).or_default().push(pt);
        }
    }
    let mut out = String::new();
    out.push_str("M48\n");
    out.push_str("; DRILL file {boardc}\n");
    if let Some((l0, l1)) = span_comment {
        out.push_str(&format!("; LAYER_SPAN={l0}..{l1}\n"));
    }
    out.push_str("; FORMAT={-:-/ absolute / metric / decimal}\n");
    out.push_str("FMAT,2\n");
    out.push_str("METRIC,TZ\n");
    for (idx, dia) in by_dia.keys().enumerate() {
        out.push_str(&format!("T{:02}C{:.4}\n", idx + 1, nm_to_mm(*dia)));
    }
    out.push_str("%\n");
    out.push_str("G90\n");
    out.push_str("G05\n");
    for (idx, pts) in by_dia.values().enumerate() {
        out.push_str(&format!("T{:02}\n", idx + 1));
        for pt in pts {
            out.push_str(&format!("X{:.4}Y{:.4}\n", nm_to_mm(pt.x), nm_to_mm(pt.y)));
        }
    }
    if !by_dia.is_empty() {
        out.push_str("T00\n");
    }
    out.push_str("M30\n");
    fs::write(path, out).map_err(|e| format!("failed to write drill '{}': {e}", path.display()))
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
    fs::create_dir_all(out_dir)
        .map_err(|e| format!("failed to create fab dir '{}': {e}", out_dir.display()))?;
    let cu_layers = board.copper_layer_names();
    let slug = safe_slug(&board.name);
    let mut gerber_files = Vec::new();

    for (idx, layer_name) in cu_layers.iter().enumerate() {
        let path = out_dir.join(copper_gerber_filename(layer_name, idx, &cu_layers));
        emit_copper_layer(board, &cu_layers, idx, &path)?;
        gerber_files.push(path);
    }

    for (side, mask_file, paste_file, silk_file, fn_pos) in [
        (
            Side::Top,
            "F_Mask.gts",
            "F_Paste.gtp",
            "F_Silkscreen.gto",
            Position::Top,
        ),
        (
            Side::Bottom,
            "B_Mask.gbs",
            "B_Paste.gbp",
            "B_Silkscreen.gbo",
            Position::Bottom,
        ),
    ] {
        let mask_path = out_dir.join(mask_file);
        emit_mask_layer(board, &cu_layers, side, &mask_path)?;
        gerber_files.push(mask_path);

        let paste_path = out_dir.join(paste_file);
        emit_paste_layer(board, &cu_layers, side, &paste_path)?;
        gerber_files.push(paste_path);

        let silk_path = out_dir.join(silk_file);
        let mut silk_b = GerberBuilder::new(FileFunction::Legend {
            pos: fn_pos,
            index: None,
        });
        emit_silk_layer(&mut silk_b, board, side, &cu_layers, cfg.clip_silk_to_mask);
        silk_b.write_file(&silk_path)?;
        gerber_files.push(silk_path);
    }

    let edge_path = out_dir.join("Edge_Cuts.gm1");
    emit_edge_cuts(board, &edge_path)?;
    gerber_files.push(edge_path);

    let mut pth_hits = Vec::new();
    let mut npth_hits = Vec::new();
    let mut span_hits: BTreeMap<(String, String), Vec<(Nm, Pt)>> = BTreeMap::new();
    for via in &board.vias {
        if via.is_through_hole(&cu_layers, &board.layers) {
            pth_hits.push((via.drill, via.pos));
        } else {
            let span = via.span_layer_pair(&cu_layers, &board.layers);
            span_hits
                .entry(span)
                .or_default()
                .push((via.drill, via.pos));
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
                    PadKind::Tht => pth_hits.push((drill, world)),
                    PadKind::Npth => npth_hits.push((drill, world)),
                    PadKind::Smd => {}
                }
            }
        }
    }

    let pth_path = out_dir.join(format!("{slug}-PTH.drl"));
    let npth_path = out_dir.join(format!("{slug}-NPTH.drl"));
    emit_excellon(&pth_path, &pth_hits, None)?;
    emit_excellon(&npth_path, &npth_hits, None)?;
    let mut drill_files = vec![pth_path, npth_path];
    for ((l0, l1), hits) in span_hits {
        let path = out_dir.join(format!(
            "{slug}-PTH-{}-{}.drl",
            safe_slug(&l0),
            safe_slug(&l1)
        ));
        emit_excellon(&path, &hits, Some((&l0, &l1)))?;
        drill_files.push(path);
    }

    let bom_csv = out_dir.join(format!("{slug}_bom.csv"));
    let cpl_csv = out_dir.join(format!("{slug}_cpl.csv"));
    let gbrjob = out_dir.join(format!("{slug}-job.gbrjob"));
    emit_bom_csv(board, &bom_csv)?;
    emit_cpl_csv(board, &cpl_csv)?;
    emit_gbrjob(board, &cu_layers, &gbrjob)?;

    Ok(FabManifest {
        gerber_files,
        drill_files,
        bom_csv,
        cpl_csv,
        gbrjob,
    })
}
