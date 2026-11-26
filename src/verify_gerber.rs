use std::collections::HashMap;
use std::fs::{self, File};
use std::io::BufReader;
use std::path::{Path, PathBuf};

use clipper2_rust::{FillRule, Path64, Paths64, difference_64, union_64};
use gerber_types::{
    Aperture, ApertureMacro, Command as GCmd, Coordinates, DCode, ExtendedCode, FunctionCode,
    GCode, MacroBoolean, MacroContent, MacroDecimal, Operation, Polarity, Unit,
};

use crate::fab_gerber::copper_gerber_filename;
use crate::fab_job::verify_gbrjob;
use crate::geom::{ensure_ccw, pts_to_path64};
use crate::ir::{BBoxNm, BoardIr, Nm, Pt, mm, safe_slug};
use crate::poly_index::IndexedPour;
use crate::shapes::{capsule_poly, circle_poly, oval_poly, rect_poly};
use crate::verify::{ConnectivityCheck, DrillCheck, GerberFileCheck};
use crate::verify_excellon::{
    GerberIsland, check_gerber_connectivity, expected_drills, match_drill_lists,
    parse_excellon_file,
};
use crate::zone_scene::paths_to_pour_polys;

fn unit_scale_to_nm(unit: Option<Unit>) -> f64 {
    match unit {
        Some(Unit::Inches) => 25_400_000.0,
        _ => 1_000_000.0,
    }
}

fn coord_to_pt(c: &Coordinates, prev: Pt, scale_nm: f64) -> Pt {
    let x =
        c.x.map(|v| (f64::from(v) * scale_nm).round() as Nm)
            .unwrap_or(prev.x);
    let y =
        c.y.map(|v| (f64::from(v) * scale_nm).round() as Nm)
            .unwrap_or(prev.y);
    Pt::new(x, y)
}

fn mm_to_nm_scaled(v: f64, scale_nm: f64) -> Nm {
    (v * scale_nm).round() as Nm
}

fn aperture_half_extent(
    ap: &Aperture,
    macros: &HashMap<String, ApertureMacro>,
    scale_nm: f64,
) -> Nm {
    match ap {
        Aperture::Circle(c) => mm_to_nm_scaled(c.diameter * 0.5, scale_nm),
        Aperture::Rectangle(r) | Aperture::Obround(r) => {
            mm_to_nm_scaled(r.x.max(r.y) * 0.5, scale_nm)
        }
        Aperture::Polygon(p) => mm_to_nm_scaled(p.diameter * 0.5, scale_nm),
        Aperture::Macro(name, _) => {
            let Some(am) = macros.get(name) else {
                return 0;
            };
            let mut max_r: Nm = 0;
            for item in &am.content {
                if let MacroContent::Outline(out) = item {
                    for (mx, my) in &out.points {
                        if let (MacroDecimal::Value(x), MacroDecimal::Value(y)) = (mx, my) {
                            max_r = max_r
                                .max(mm_to_nm_scaled(*x, scale_nm).abs())
                                .max(mm_to_nm_scaled(*y, scale_nm).abs());
                        }
                    }
                }
            }
            max_r
        }
    }
}

fn flash_to_path64(
    ap: &Aperture,
    pt: Pt,
    macros: &HashMap<String, ApertureMacro>,
    scale_nm: f64,
) -> Option<Path64> {
    let mut verts = match ap {
        Aperture::Circle(c) => {
            circle_poly(pt, mm_to_nm_scaled(c.diameter * 0.5, scale_nm).max(1), 16)
        }
        Aperture::Rectangle(r) => rect_poly(
            mm_to_nm_scaled(r.x, scale_nm).max(1),
            mm_to_nm_scaled(r.y, scale_nm).max(1),
        )
        .into_iter()
        .map(|p| Pt::new(pt.x + p.x, pt.y + p.y))
        .collect(),
        Aperture::Obround(r) => oval_poly(
            mm_to_nm_scaled(r.x, scale_nm).max(1),
            mm_to_nm_scaled(r.y, scale_nm).max(1),
            8,
        )
        .into_iter()
        .map(|p| Pt::new(pt.x + p.x, pt.y + p.y))
        .collect(),
        Aperture::Polygon(p) => circle_poly(
            pt,
            mm_to_nm_scaled(p.diameter * 0.5, scale_nm).max(1),
            (p.vertices as usize).max(3),
        ),
        Aperture::Macro(name, _) => {
            let am = macros.get(name)?;
            let mut verts = Vec::new();
            for item in &am.content {
                if let MacroContent::Outline(out) = item {
                    if matches!(out.exposure, MacroBoolean::Value(false)) {
                        continue;
                    }
                    for (mx, my) in &out.points {
                        if let (MacroDecimal::Value(x), MacroDecimal::Value(y)) = (mx, my) {
                            verts.push(Pt::new(
                                pt.x + mm_to_nm_scaled(*x, scale_nm),
                                pt.y + mm_to_nm_scaled(*y, scale_nm),
                            ));
                        }
                    }
                    break;
                }
            }
            if verts.len() > 1 && verts.first() == verts.last() {
                verts.pop();
            }
            if verts.len() < 3 {
                return None;
            }
            verts
        }
    };
    ensure_ccw(&mut verts);
    Some(pts_to_path64(&verts))
}

fn stroke_to_path64(
    ap: &Aperture,
    a: Pt,
    b: Pt,
    macros: &HashMap<String, ApertureMacro>,
    scale_nm: f64,
) -> Option<Path64> {
    if a == b {
        return flash_to_path64(ap, a, macros, scale_nm);
    }
    let radius = match ap {
        Aperture::Circle(c) => mm_to_nm_scaled(c.diameter * 0.5, scale_nm).max(1),
        Aperture::Rectangle(r) | Aperture::Obround(r) => {
            mm_to_nm_scaled(r.x.min(r.y) * 0.5, scale_nm).max(1)
        }
        Aperture::Polygon(p) => mm_to_nm_scaled(p.diameter * 0.5, scale_nm).max(1),
        Aperture::Macro(_, _) => 10_000,
    };
    let mut verts = capsule_poly(a, b, radius, 8);
    ensure_ccw(&mut verts);
    Some(pts_to_path64(&verts))
}

fn update_bbox(bbox: &mut Option<BBoxNm>, pt: Pt, margin: Nm) {
    let b = BBoxNm::from_pt(pt, margin);
    *bbox = Some(match *bbox {
        Some(prev) => prev.union(b),
        None => b,
    });
}

fn failed_gerber_check(
    path: &Path,
    parse_errors: usize,
    command_count: usize,
    msg: String,
) -> (GerberFileCheck, Vec<GerberIsland>) {
    (
        GerberFileCheck {
            file: path.to_path_buf(),
            parse_errors,
            command_count,
            within_bounds: false,
            bbox_nm: None,
            error: Some(msg),
        },
        Vec::new(),
    )
}

fn push_polarity(
    p: Path64,
    polarity: Polarity,
    dark_batch: &mut Paths64,
    clear_batch: &mut Paths64,
) {
    if polarity == Polarity::Dark {
        dark_batch.push(p);
    } else {
        clear_batch.push(p);
    }
}

fn flush_region_poly(
    region_pts: &mut Vec<Pt>,
    collect_copper: bool,
    polarity: Polarity,
    dark_batch: &mut Paths64,
    clear_batch: &mut Paths64,
) {
    if collect_copper && region_pts.len() >= 3 {
        let mut poly = std::mem::take(region_pts);
        if poly.first() == poly.last() {
            poly.pop();
        }
        if poly.len() >= 3 {
            ensure_ccw(&mut poly);
            push_polarity(pts_to_path64(&poly), polarity, dark_batch, clear_batch);
        }
    } else {
        region_pts.clear();
    }
}

fn parse_gerber_file(
    path: &Path,
    layer_name: Option<&str>,
    collect_copper: bool,
    outline_bbox: BBoxNm,
    tol_nm: Nm,
) -> (GerberFileCheck, Vec<GerberIsland>) {
    let file = match File::open(path) {
        Ok(f) => f,
        Err(e) => {
            return failed_gerber_check(
                path,
                1,
                0,
                format!("failed to open '{}': {e}", path.display()),
            );
        }
    };
    let doc = match gerber_parser::parse(BufReader::new(file)) {
        Ok(d) => d,
        Err((_partial, err)) => {
            return failed_gerber_check(
                path,
                1,
                0,
                format!("parse error in '{}': {err}", path.display()),
            );
        }
    };

    let parse_errors = doc.errors().len();
    if parse_errors > 0 {
        let first = doc.errors()[0].to_string();
        return failed_gerber_check(
            path,
            parse_errors,
            doc.commands.len(),
            format!("gerber errors in '{}': {first}", path.display()),
        );
    }
    if doc.units.is_none() || doc.format_specification.is_none() {
        return failed_gerber_check(
            path,
            0,
            doc.commands.len(),
            format!("missing units or format spec in '{}'", path.display()),
        );
    }

    let scale_nm = unit_scale_to_nm(doc.units);
    let mut macros: HashMap<String, ApertureMacro> = HashMap::new();
    let mut apertures = doc.apertures.clone();
    let mut cur_pt = Pt::new(0, 0);
    let mut cur_ap: Option<i32> = None;
    let mut polarity = Polarity::Dark;
    let mut in_region = false;
    let mut region_pts: Vec<Pt> = Vec::new();
    let mut bbox: Option<BBoxNm> = None;

    let mut copper_paths = Paths64::new();
    let mut dark_batch = Paths64::new();
    let mut clear_batch = Paths64::new();

    let flush_polarity =
        |pol: Polarity, dark: &mut Paths64, clear: &mut Paths64, copper: &mut Paths64| {
            if pol == Polarity::Dark && !dark.is_empty() {
                *copper = union_64(copper, dark, FillRule::NonZero);
                dark.clear();
            } else if pol == Polarity::Clear && !clear.is_empty() {
                *copper = difference_64(copper, clear, FillRule::NonZero);
                clear.clear();
            }
        };

    for cmd in doc.commands() {
        match cmd {
            GCmd::ExtendedCode(ExtendedCode::ApertureMacro(am)) => {
                macros.insert(am.name.clone(), am.clone());
            }
            GCmd::ExtendedCode(ExtendedCode::ApertureDefinition(def)) => {
                apertures.insert(def.code, def.aperture.clone());
            }
            GCmd::ExtendedCode(ExtendedCode::LoadPolarity(pol)) => {
                if *pol != polarity {
                    if collect_copper {
                        flush_polarity(
                            polarity,
                            &mut dark_batch,
                            &mut clear_batch,
                            &mut copper_paths,
                        );
                    }
                    polarity = *pol;
                }
            }
            GCmd::FunctionCode(FunctionCode::GCode(GCode::RegionMode(on))) => {
                if *on {
                    in_region = true;
                    region_pts.clear();
                } else {
                    flush_region_poly(
                        &mut region_pts,
                        collect_copper,
                        polarity,
                        &mut dark_batch,
                        &mut clear_batch,
                    );
                    in_region = false;
                }
            }
            GCmd::FunctionCode(FunctionCode::DCode(DCode::SelectAperture(code))) => {
                cur_ap = Some(*code);
            }
            GCmd::FunctionCode(FunctionCode::DCode(DCode::Operation(op))) => match op {
                Operation::Move(coords_opt) => {
                    if let Some(c) = coords_opt {
                        cur_pt = coord_to_pt(c, cur_pt, scale_nm);
                    }
                    if in_region {
                        flush_region_poly(
                            &mut region_pts,
                            collect_copper,
                            polarity,
                            &mut dark_batch,
                            &mut clear_batch,
                        );
                        region_pts.push(cur_pt);
                    }
                    update_bbox(&mut bbox, cur_pt, 0);
                }
                Operation::Interpolate(coords_opt, _) => {
                    let prev = cur_pt;
                    if let Some(c) = coords_opt {
                        cur_pt = coord_to_pt(c, cur_pt, scale_nm);
                    }
                    if in_region {
                        if region_pts.last() != Some(&cur_pt) {
                            region_pts.push(cur_pt);
                        }
                        update_bbox(&mut bbox, cur_pt, 0);
                    } else {
                        let margin = cur_ap
                            .and_then(|c| apertures.get(&c))
                            .map(|ap| aperture_half_extent(ap, &macros, scale_nm))
                            .unwrap_or(0);
                        update_bbox(&mut bbox, prev, margin);
                        update_bbox(&mut bbox, cur_pt, margin);
                        if collect_copper
                            && let Some(ap) = cur_ap.and_then(|c| apertures.get(&c))
                            && let Some(p) = stroke_to_path64(ap, prev, cur_pt, &macros, scale_nm)
                        {
                            push_polarity(p, polarity, &mut dark_batch, &mut clear_batch);
                        }
                    }
                }
                Operation::Flash(coords_opt) => {
                    if let Some(c) = coords_opt {
                        cur_pt = coord_to_pt(c, cur_pt, scale_nm);
                    }
                    let margin = cur_ap
                        .and_then(|c| apertures.get(&c))
                        .map(|ap| aperture_half_extent(ap, &macros, scale_nm))
                        .unwrap_or(0);
                    update_bbox(&mut bbox, cur_pt, margin);
                    if collect_copper
                        && let Some(ap) = cur_ap.and_then(|c| apertures.get(&c))
                        && let Some(p) = flash_to_path64(ap, cur_pt, &macros, scale_nm)
                    {
                        push_polarity(p, polarity, &mut dark_batch, &mut clear_batch);
                    }
                }
            },
            _ => {}
        }
    }

    let mut islands = Vec::new();
    if collect_copper {
        flush_polarity(
            polarity,
            &mut dark_batch,
            &mut clear_batch,
            &mut copper_paths,
        );
        if !copper_paths.is_empty() {
            let layer = layer_name.unwrap_or("F.Cu");
            for poly in paths_to_pour_polys(&copper_paths) {
                islands.push(GerberIsland {
                    layer: layer.to_string(),
                    indexed: IndexedPour::new(&poly),
                });
            }
        }
    }

    let expanded = outline_bbox.expand(tol_nm);
    let within_bounds = bbox.is_none_or(|b| {
        b.min_x >= expanded.min_x
            && b.max_x <= expanded.max_x
            && b.min_y >= expanded.min_y
            && b.max_y <= expanded.max_y
    });
    let err = (!within_bounds).then(|| {
        format!(
            "geometry in '{}' exceeds board outline bounds {:?} (tol {:.3}mm)",
            path.display(),
            bbox,
            tol_nm as f64 / 1e6
        )
    });

    (
        GerberFileCheck {
            file: path.to_path_buf(),
            parse_errors: 0,
            command_count: doc.commands.len(),
            within_bounds,
            bbox_nm: bbox,
            error: err,
        },
        islands,
    )
}

fn copper_file_map(board: &BoardIr) -> Vec<(String, String)> {
    let layers = board.copper_layer_names();
    layers
        .iter()
        .enumerate()
        .map(|(idx, name)| (name.clone(), copper_gerber_filename(name, idx, &layers)))
        .collect()
}

pub(crate) fn verify_fab_outputs_impl(
    board: &BoardIr,
    fab_dir: &Path,
) -> Result<(Vec<GerberFileCheck>, DrillCheck, ConnectivityCheck), String> {
    if board.outline.len() < 3 {
        return Err("board outline must have at least 3 vertices".into());
    }
    if !fab_dir.is_dir() {
        return Err(format!(
            "fab directory '{}' does not exist",
            fab_dir.display()
        ));
    }
    let outline_bbox = BBoxNm::from_pts(&board.outline);
    let cu_layers = board.copper_layer_names();
    let cu_map = copper_file_map(board);
    let cu_by_fname: HashMap<String, String> = cu_map
        .iter()
        .map(|(layer, fname)| (fname.clone(), layer.clone()))
        .collect();
    let slug = safe_slug(&board.name);

    for (_, fname) in &cu_map {
        let p = fab_dir.join(fname);
        if !p.exists() {
            return Err(format!("missing copper Gerber '{}'", p.display()));
        }
    }
    for req_file in [
        "Edge_Cuts.gm1",
        "F_Mask.gts",
        "B_Mask.gbs",
        "F_Paste.gtp",
        "B_Paste.gbp",
        "F_Silkscreen.gto",
        "B_Silkscreen.gbo",
    ] {
        let p = fab_dir.join(req_file);
        if !p.exists() {
            return Err(format!("missing required fab Gerber '{}'", p.display()));
        }
    }
    for req_csv in [format!("{slug}_bom.csv"), format!("{slug}_cpl.csv")] {
        let p = fab_dir.join(&req_csv);
        if !p.exists() {
            return Err(format!("missing required assembly CSV '{}'", p.display()));
        }
    }
    verify_gbrjob(
        board,
        &cu_layers,
        fab_dir,
        &fab_dir.join(format!("{slug}-job.gbrjob")),
    )?;

    let mut gerber_paths: Vec<PathBuf> = Vec::new();
    for entry in fs::read_dir(fab_dir)
        .map_err(|e| format!("failed to read fab dir '{}': {e}", fab_dir.display()))?
    {
        let path = entry.map_err(|e| e.to_string())?.path();
        if !path.is_file() {
            continue;
        }
        let Some(ext) = path.extension().and_then(|s| s.to_str()) else {
            continue;
        };
        let is_gerber = matches!(
            ext,
            "gtl" | "gbl" | "gts" | "gbs" | "gtp" | "gbp" | "gto" | "gbo" | "gm1" | "gbr"
        ) || (ext.starts_with('g')
            && ext.len() >= 2
            && ext[1..].chars().all(|c| c.is_ascii_digit()));
        if is_gerber {
            gerber_paths.push(path);
        }
    }
    gerber_paths.sort();

    let mut checks = Vec::with_capacity(gerber_paths.len());
    let mut islands = Vec::new();
    for path in &gerber_paths {
        let fname = path
            .file_name()
            .and_then(|s| s.to_str())
            .unwrap_or("")
            .to_string();
        let cu_layer = cu_by_fname.get(&fname).map(String::as_str);
        // Edge_Cuts must match outline tightly; copper allows edge-mounted connectors (1mm); silk/mask allow overhang.
        let tol = if fname == "Edge_Cuts.gm1" {
            mm(0.1)
        } else if cu_layer.is_some() {
            mm(1.0)
        } else {
            mm(3.0)
        };
        let (chk, mut layer_islands) =
            parse_gerber_file(path, cu_layer, cu_layer.is_some(), outline_bbox, tol);
        checks.push(chk);
        islands.append(&mut layer_islands);
    }

    let exp = expected_drills(board);
    let pth_path = fab_dir.join(format!("{slug}-PTH.drl"));
    let npth_path = fab_dir.join(format!("{slug}-NPTH.drl"));
    let parsed_pth = parse_excellon_file(&pth_path)?;
    let parsed_npth = parse_excellon_file(&npth_path)?;
    let (mut pth_matched, mut pth_mismatches) =
        match_drill_lists(&exp.pth_through, &parsed_pth, 2_000);
    let (npth_matched, npth_mismatches) = match_drill_lists(&exp.npth, &parsed_npth, 2_000);
    let mut pth_expected_total = exp.pth_through.len();
    let mut pth_parsed_total = parsed_pth.len();

    let mut pth_groups = vec![(cu_layers.clone(), parsed_pth)];
    for ((l0, l1), exp_hits) in &exp.pth_spans {
        let span_path = fab_dir.join(format!(
            "{slug}-PTH-{}-{}.drl",
            safe_slug(l0),
            safe_slug(l1)
        ));
        let parsed_span = parse_excellon_file(&span_path)?;
        let (m, mm_cnt) = match_drill_lists(exp_hits, &parsed_span, 2_000);
        pth_expected_total += exp_hits.len();
        pth_parsed_total += parsed_span.len();
        pth_matched += m;
        pth_mismatches += mm_cnt;
        let span_layers: Vec<String> = {
            let i0 = cu_layers.iter().position(|l| l == l0);
            let i1 = cu_layers.iter().position(|l| l == l1);
            match (i0, i1) {
                (Some(a), Some(b)) => cu_layers[a.min(b)..=a.max(b)].to_vec(),
                _ => vec![l0.clone(), l1.clone()],
            }
        };
        pth_groups.push((span_layers, parsed_span));
    }

    let drill_check = DrillCheck {
        passed: pth_mismatches == 0 && npth_mismatches == 0,
        pth_expected: pth_expected_total,
        pth_parsed: pth_parsed_total,
        pth_matched,
        npth_expected: exp.npth.len(),
        npth_parsed: parsed_npth.len(),
        npth_matched,
        mismatches: pth_mismatches + npth_mismatches,
    };

    let connectivity = check_gerber_connectivity(board, &islands, &pth_groups);

    Ok((checks, drill_check, connectivity))
}
