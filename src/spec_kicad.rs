use std::collections::{BTreeMap, HashMap};
use std::fs;
use std::path::{Component, Path, PathBuf};

use crate::ir::{FootprintDef, NetDef, Placement};
use crate::kicad_io::{KicadNetlist, load_kicad_mod};

pub(crate) fn resolve_spec_path(
    raw: &str,
    base_dir: Option<&Path>,
    label: &str,
) -> Result<PathBuf, String> {
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        return Err(format!("{label} path cannot be empty"));
    }
    let p = Path::new(trimmed);
    if p.is_absolute() {
        return Ok(p.to_path_buf());
    }
    Ok(match base_dir {
        Some(base) => base.join(p),
        None => p.to_path_buf(),
    })
}

pub(crate) fn resolve_footprint_libs(
    libs: &[String],
    base_dir: Option<&Path>,
) -> Result<Vec<PathBuf>, String> {
    let mut resolved = Vec::with_capacity(libs.len());
    for raw in libs {
        let path = resolve_spec_path(raw, base_dir, "footprint_libs")?;
        if !path.exists() {
            return Err(format!("footprint library '{}' not found", path.display()));
        }
        if path.is_file() {
            let is_mod = path
                .extension()
                .and_then(|s| s.to_str())
                .is_some_and(|e| e.eq_ignore_ascii_case("kicad_mod"));
            if !is_mod {
                return Err(format!(
                    "footprint library '{}' must be a directory or .kicad_mod file",
                    path.display()
                ));
            }
        }
        resolved.push(path);
    }
    Ok(resolved)
}

fn is_safe_path_segment(seg: &str) -> bool {
    !seg.is_empty()
        && seg != "."
        && seg != ".."
        && !seg.contains('/')
        && !seg.contains('\\')
        && !seg.contains(':')
        && !seg.contains('\0')
}

pub(crate) fn bare_footprint_name(name: &str) -> &str {
    name.split_once(':').map_or(name, |(_, fp)| fp)
}

fn last_dir_name(dir: &Path) -> Option<String> {
    if let Some(Component::Normal(os)) = dir.components().next_back() {
        return os.to_str().map(str::to_string);
    }
    fs::canonicalize(dir)
        .ok()?
        .file_name()?
        .to_str()
        .map(str::to_string)
}

fn is_pretty_dir(dir: &Path) -> bool {
    last_dir_name(dir).is_some_and(|n| {
        let b = n.as_bytes();
        b.len() > 7 && b[b.len() - 7..].eq_ignore_ascii_case(b".pretty")
    })
}

fn dir_has_kicad_mod(dir: &Path) -> bool {
    let Ok(entries) = fs::read_dir(dir) else {
        return false;
    };
    entries.flatten().any(|e| {
        let p = e.path();
        p.is_file()
            && p.extension()
                .and_then(|s| s.to_str())
                .is_some_and(|ext| ext.eq_ignore_ascii_case("kicad_mod"))
    })
}

fn dir_library_name(dir: &Path) -> Option<String> {
    let name = last_dir_name(dir)?;
    let bytes = name.as_bytes();
    if bytes.len() > 7 && bytes[bytes.len() - 7..].eq_ignore_ascii_case(b".pretty") {
        Some(name[..bytes.len() - 7].to_string())
    } else {
        Some(name)
    }
}

fn find_mod_in_dir(dir: &Path, bare: &str) -> Option<PathBuf> {
    let fast = dir.join(format!("{bare}.kicad_mod"));
    if fast.is_file() {
        return Some(fast);
    }
    let entries = fs::read_dir(dir).ok()?;
    for entry in entries.flatten() {
        let p = entry.path();
        if p.is_file()
            && p.file_stem().and_then(|s| s.to_str()) == Some(bare)
            && p.extension()
                .and_then(|s| s.to_str())
                .is_some_and(|e| e.eq_ignore_ascii_case("kicad_mod"))
        {
            return Some(p);
        }
    }
    None
}

fn find_pretty_subdir(dir: &Path, lib: &str) -> Option<PathBuf> {
    let fast = dir.join(format!("{lib}.pretty"));
    if fast.is_dir() {
        return Some(fast);
    }
    let entries = fs::read_dir(dir).ok()?;
    for entry in entries.flatten() {
        let p = entry.path();
        if p.is_dir()
            && p.extension()
                .and_then(|s| s.to_str())
                .is_some_and(|e| e.eq_ignore_ascii_case("pretty"))
            && dir_library_name(&p).as_deref() == Some(lib)
        {
            return Some(p);
        }
    }
    None
}

fn find_footprint_file(name: &str, libs: &[PathBuf]) -> Result<Option<PathBuf>, String> {
    let (lib_opt, bare) = match name.split_once(':') {
        Some((lib, fp)) => (Some(lib), fp),
        None => (None, name),
    };
    if !is_safe_path_segment(bare) || lib_opt.is_some_and(|l| !is_safe_path_segment(l)) {
        return Err(format!("invalid footprint name '{name}'"));
    }

    // Pass 1: library-qualified match (<Lib>.pretty/<Fp>.kicad_mod or dir/<Lib>.pretty/<Fp>.kicad_mod).
    let mut matched_lib = false;
    if let Some(lib) = lib_opt {
        for dir in libs {
            if !dir.is_dir() {
                continue;
            }
            if dir_library_name(dir).as_deref() == Some(lib) {
                if let Some(p) = find_mod_in_dir(dir, bare) {
                    return Ok(Some(p));
                }
                if is_pretty_dir(dir) || dir_has_kicad_mod(dir) {
                    matched_lib = true;
                }
            }
            if let Some(sub) = find_pretty_subdir(dir, lib) {
                matched_lib = true;
                if let Some(p) = find_mod_in_dir(&sub, bare) {
                    return Ok(Some(p));
                }
            }
        }
        // If the requested library exists in footprint_libs, do not steal a same-named footprint from another library.
        if matched_lib {
            return Ok(None);
        }
    }

    // Pass 2: bare <Fp>.kicad_mod inside any listed directory, or a directly listed .kicad_mod file.
    for entry in libs {
        if entry.is_file() {
            if entry.file_stem().and_then(|s| s.to_str()) == Some(bare) {
                return Ok(Some(entry.clone()));
            }
        } else if entry.is_dir()
            && let Some(p) = find_mod_in_dir(entry, bare)
        {
            return Ok(Some(p));
        }
    }
    Ok(None)
}

pub(crate) fn resolve_footprint_from_libs(
    name: &str,
    qualified_fallback: Option<&str>,
    libs: &[PathBuf],
) -> Result<Option<FootprintDef>, String> {
    if libs.is_empty() {
        return Ok(None);
    }
    let mut found = find_footprint_file(name, libs)?;
    if found.is_none()
        && let Some(qual) = qualified_fallback
        && qual != name
    {
        found = find_footprint_file(qual, libs)?;
    }
    match found {
        Some(path) => {
            let mut fp = load_kicad_mod(&path)?;
            fp.name = name.to_string();
            Ok(Some(fp))
        }
        None => Ok(None),
    }
}

fn non_empty_opt(opt: Option<String>) -> Option<String> {
    opt.filter(|s| !s.trim().is_empty())
}

fn sync_override_field(fields: &mut BTreeMap<String, String>, key_ci: &str, value: &str) {
    if let Some((_, v)) = fields
        .iter_mut()
        .find(|(k, _)| k.eq_ignore_ascii_case(key_ci))
    {
        *v = value.to_string();
    }
}

pub(crate) fn apply_netlist_components(
    placements: &mut [Placement],
    nl: &KicadNetlist,
) -> Result<(), String> {
    for p in placements {
        p.mpn = non_empty_opt(p.mpn.take());
        p.lcsc = non_empty_opt(p.lcsc.take());
        let Some(comp) = nl.components.get(&p.refdes) else {
            continue;
        };
        if comp.exclude_from_board {
            return Err(format!(
                "placement '{}' is marked exclude_from_board in netlist",
                p.refdes
            ));
        }
        if p.footprint.trim().is_empty() && !comp.footprint.is_empty() {
            p.footprint = comp.footprint.clone();
        }
        if p.value.trim().is_empty() && !comp.value.is_empty() {
            p.value = comp.value.clone();
        }
        let spec_mpn_override = p.mpn.clone();
        let spec_lcsc_override = p.lcsc.clone();
        p.mpn = p.mpn.take().or_else(|| comp.mpn.clone());
        p.lcsc = p.lcsc.take().or_else(|| comp.lcsc.clone());
        p.dnp |= comp.dnp;
        p.exclude_from_bom |= comp.exclude_from_bom;

        let mut sch = p.schematic.take().unwrap_or_default();
        sch.path = non_empty_opt(sch.path).or_else(|| comp.path.clone());
        sch.sheetname = non_empty_opt(sch.sheetname).or_else(|| comp.sheetname.clone());
        sch.sheetfile = non_empty_opt(sch.sheetfile).or_else(|| comp.sheetfile.clone());
        sch.footprint = non_empty_opt(sch.footprint).or_else(|| {
            (!comp.footprint.is_empty()
                && (p.footprint == comp.footprint
                    || bare_footprint_name(&comp.footprint) == p.footprint))
                .then(|| comp.footprint.clone())
        });
        for (k, v) in &comp.fields {
            sch.fields.entry(k.clone()).or_insert_with(|| v.clone());
        }
        if let Some(ref mpn) = spec_mpn_override {
            sync_override_field(&mut sch.fields, "mpn", mpn);
        }
        if let Some(ref lcsc) = spec_lcsc_override {
            sync_override_field(&mut sch.fields, "lcsc", lcsc);
        }
        p.schematic = (!sch.is_empty()).then_some(sch);
    }
    Ok(())
}

pub(crate) fn sync_spec_and_netlist_nets(
    placements: &mut [Placement],
    nets: &mut BTreeMap<String, NetDef>,
    netlist: Option<&KicadNetlist>,
) {
    let by_ref: HashMap<String, usize> = placements
        .iter()
        .enumerate()
        .filter(|(_, p)| !p.refdes.trim().is_empty())
        .map(|(i, p)| (p.refdes.clone(), i))
        .collect();

    // Apply explicit spec.nets pins onto placements before netlist fallback so spec overrides win.
    for (net_name, net_def) in nets.iter() {
        if net_name.trim().is_empty() {
            continue;
        }
        for (refdes, pin) in &net_def.pins {
            if let Some(&idx) = by_ref.get(refdes.as_str()) {
                placements[idx]
                    .pin_nets
                    .entry(pin.clone())
                    .or_insert_with(|| net_name.clone());
            }
        }
    }

    let mut netlist_classes: HashMap<&str, &str> = HashMap::new();
    if let Some(nl) = netlist {
        for nl_net in &nl.nets {
            if nl_net.class != "Default" && !nl_net.class.is_empty() {
                netlist_classes
                    .entry(nl_net.name.as_str())
                    .or_insert(nl_net.class.as_str());
            }
            for (refdes, pin) in &nl_net.nodes {
                if let Some(&idx) = by_ref.get(refdes.as_str()) {
                    placements[idx]
                        .pin_nets
                        .entry(pin.clone())
                        .or_insert_with(|| nl_net.name.clone());
                }
            }
        }
    }

    // Strip empty-net entries (explicit pin disconnects) so no empty net leaks into IR.
    for p in placements.iter_mut() {
        p.pin_nets.retain(|_, net| !net.trim().is_empty());
    }

    // Prune empty-named nets and any stale pin overridden onto another net or disconnected.
    nets.retain(|k, _| !k.trim().is_empty());
    for (net_name, nd) in nets.iter_mut() {
        if nd.name.trim().is_empty() {
            nd.name = net_name.clone();
        }
        nd.pins
            .retain(|(refdes, pin)| match by_ref.get(refdes.as_str()) {
                Some(&idx) => {
                    placements[idx].pin_nets.get(pin).map(String::as_str) == Some(net_name.as_str())
                }
                None => true,
            });
    }

    // Keep Placement::pin_nets and NetDef::pins mutually consistent.
    for p in placements.iter() {
        for (pin, net_name) in &p.pin_nets {
            let entry = nets.entry(net_name.clone()).or_insert_with(|| NetDef {
                name: net_name.clone(),
                ..Default::default()
            });
            if entry.class == "Default"
                && let Some(&c) = netlist_classes.get(net_name.as_str())
            {
                entry.class = c.to_string();
            }
            let pair = (p.refdes.clone(), pin.clone());
            if !entry.pins.contains(&pair) {
                entry.pins.push(pair);
            }
        }
    }
}
