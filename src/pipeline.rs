use std::collections::{BTreeMap, BTreeSet, HashSet};
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Instant;

use serde::de::DeserializeOwned;
use serde::{Deserialize, Serialize};

use crate::drc::{DrcReport, Severity, check_board_drc};
use crate::erc::ErcViolation;
use crate::fab::{FabOptions, export_fab};
use crate::geom::point_in_polygon;
use crate::ir::{
    BgaShadowRule, BoardIr, CapProximityRule, Constraints, DesignRules, FabConfig, FilledPour,
    FootprintDef, Graphic, Keepout, LayerDef, LayerKind, MatchGroupRule, MaxLengthRule,
    MinViasRule, NetClass, NetDef, Nm, PadKind, Placement, PourSpec, Pt, Track, Via, fnv1a_128,
    safe_slug, save_board_json,
};
use crate::pipeline_repair::route_with_repair;
use crate::place::{
    PlaceOptions, PlaceReport, PowerViaOptions, PowerViaReport, legalize_placement,
    solve_power_vias,
};
use crate::preflight::{
    PreflightBlockedNet, PreflightFixedViolation, PreflightSealedPad, PreflightSummary,
    run_preflight,
};
use crate::route::{RouteOptions, RouteSummary};
use crate::shapes::transform_pad_local_pt;
use crate::svg::write_board_svgs;
use crate::zones::{ZoneChoke, ZoneReport, fill_zones};

const CACHE_SCHEMA: &str = "v8";
static CACHE_TMP_SEQ: AtomicU64 = AtomicU64::new(0);

/// Pipeline stage identifier in execution order.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum StageKind {
    Place,
    PowerVia,
    Preflight,
    Route,
    Zones,
    Drc,
}

/// Per-stage execution and cache hit record.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct StageRecord {
    pub stage: StageKind,
    pub cache_key: String,
    pub cache_hit: bool,
    pub elapsed_ms: u128,
}

/// Options configuring the staged compiler pipeline.
#[derive(Debug, Clone)]
pub struct BuildOptions {
    pub out_dir: Option<PathBuf>,
    pub cache_dir: PathBuf,
    pub use_cache: bool,
    pub fail_on_preflight: bool,
    pub repair_passes: usize,
    pub place: PlaceOptions,
    pub power_via: PowerViaOptions,
    pub route: RouteOptions,
    pub emit_outputs: bool,
}

impl Default for BuildOptions {
    fn default() -> Self {
        Self {
            out_dir: None,
            cache_dir: PathBuf::from(".boardc-cache"),
            use_cache: true,
            fail_on_preflight: false,
            repair_passes: 0,
            place: PlaceOptions::default(),
            power_via: PowerViaOptions::default(),
            route: RouteOptions::default(),
            emit_outputs: true,
        }
    }
}

/// Complete report and resulting compiled board emitted by `compile_board`.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct BuildReport {
    pub board_name: String,
    pub ok: bool,
    pub place: Option<PlaceReport>,
    pub power_via: PowerViaReport,
    pub preflight: PreflightSummary,
    pub route: RouteSummary,
    pub zones: ZoneReport,
    pub drc: DrcReport,
    #[serde(skip_serializing)]
    pub board: BoardIr,
    pub stages: Vec<StageRecord>,
    pub emitted_files: Vec<PathBuf>,
    pub elapsed_ms: u128,
}

impl BuildReport {
    /// Returns the `StageRecord` for a given stage if present.
    pub fn stage_record(&self, stage: StageKind) -> Option<&StageRecord> {
        self.stages.iter().find(|s| s.stage == stage)
    }

    /// Returns whether a given stage was served from cache.
    pub fn stage_hit(&self, stage: StageKind) -> bool {
        self.stage_record(stage).is_some_and(|s| s.cache_hit)
    }
}

#[derive(Serialize)]
struct PreflightBlockers<'a> {
    ok: bool,
    stopped_early: bool,
    layout_violations: &'a [crate::drc::Violation],
    zone_chokes: &'a [ZoneChoke],
    sealed_pads: &'a [PreflightSealedPad],
    blocked_nets: &'a [PreflightBlockedNet],
    fixed_violations: &'a [PreflightFixedViolation],
    erc_violations: &'a [ErcViolation],
}

fn normalize_neg_zero(val: &mut serde_json::Value) {
    match val {
        serde_json::Value::Number(n) => {
            if n.as_i64().is_none()
                && n.as_u64().is_none()
                && n.as_f64() == Some(0.0)
                && let Some(zero) = serde_json::Number::from_f64(0.0)
            {
                *n = zero;
            }
        }
        serde_json::Value::Array(items) => items.iter_mut().for_each(normalize_neg_zero),
        serde_json::Value::Object(map) => map.values_mut().for_each(normalize_neg_zero),
        _ => {}
    }
}

/// Computes a deterministic 128-bit FNV-1a hex digest over canonical JSON of `value`.
pub fn content_hash_hex<T: Serialize>(value: &T) -> Result<String, String> {
    let mut val = serde_json::to_value(value)
        .map_err(|e| format!("failed to convert value for hashing: {e}"))?;
    normalize_neg_zero(&mut val);
    // `serde_json::Map` uses `BTreeMap` without `preserve_order`, so keys serialize sorted.
    let bytes =
        serde_json::to_vec(&val).map_err(|e| format!("canonical JSON serialization error: {e}"))?;
    Ok(format!("{:032x}", fnv1a_128(&bytes)))
}

fn stage_cache_path(cache_dir: &Path, stage: StageKind, key: &str) -> PathBuf {
    let prefix = match stage {
        StageKind::Place => "place",
        StageKind::PowerVia => "power_via",
        StageKind::Preflight => "preflight",
        StageKind::Route => "route",
        StageKind::Zones => "zones",
        StageKind::Drc => "drc",
    };
    cache_dir.join(format!("{prefix}_{key}.json"))
}

fn load_cached<T: DeserializeOwned>(path: &Path) -> Option<T> {
    let text = fs::read_to_string(path).ok()?;
    serde_json::from_str(&text).ok()
}

fn save_cached<T: Serialize>(cache_dir: &Path, path: &Path, value: &T) -> Result<(), String> {
    fs::create_dir_all(cache_dir)
        .map_err(|e| format!("failed to create cache dir '{}': {e}", cache_dir.display()))?;
    let text = serde_json::to_string(value)
        .map_err(|e| format!("failed to serialize cache entry: {e}"))?;
    let seq = CACHE_TMP_SEQ.fetch_add(1, Ordering::Relaxed);
    let tmp = path.with_extension(format!("json.tmp.{}.{seq}", std::process::id()));
    fs::write(&tmp, text)
        .map_err(|e| format!("failed to write cache temp '{}': {e}", tmp.display()))?;
    fs::rename(&tmp, path)
        .map_err(|e| format!("failed to commit cache '{}': {e}", path.display()))?;
    Ok(())
}

fn run_cached_stage<T, F>(
    cfg: &BuildOptions,
    stage: StageKind,
    key: String,
    stages: &mut Vec<StageRecord>,
    compute: F,
) -> Result<T, String>
where
    T: Serialize + DeserializeOwned,
    F: FnOnce() -> Result<T, String>,
{
    let t0 = Instant::now();
    let path = stage_cache_path(&cfg.cache_dir, stage, &key);
    let mut hit = false;
    let val = if cfg.use_cache
        && let Some(cached) = load_cached::<T>(&path)
    {
        hit = true;
        cached
    } else {
        let computed = compute()?;
        if cfg.use_cache {
            save_cached(&cfg.cache_dir, &path, &computed)?;
        }
        computed
    };
    stages.push(StageRecord {
        stage,
        cache_key: key,
        cache_hit: hit,
        elapsed_ms: t0.elapsed().as_millis(),
    });
    Ok(val)
}

#[derive(Serialize)]
struct PlaceKeyInput<'a> {
    schema: &'static str,
    outline: &'a [Pt],
    cutouts: &'a [Vec<Pt>],
    layers: &'a [LayerDef],
    footprints: &'a BTreeMap<String, FootprintDef>,
    placements: &'a [Placement],
    keepouts: &'a [Keepout],
    courtyard_clearance_nm: Nm,
    edge_clearance_nm: Nm,
    clearance_nm: Nm,
    smd_to_smd_nm: Option<Nm>,
    hole_to_hole_nm: Nm,
    hole_clearance_nm: Option<Nm>,
    via_to_smd_nm: Option<Nm>,
    net_classes: &'a BTreeMap<String, NetClass>,
    nets: &'a BTreeMap<String, NetDef>,
    tracks: &'a [Track],
    vias: &'a [Via],
    cap_proximity: &'a [CapProximityRule],
    bga_shadows: &'a [BgaShadowRule],
    cfg: &'a PlaceOptions,
}

#[derive(Serialize, Deserialize)]
struct PlaceCache {
    placements: Vec<Placement>,
    report: Option<PlaceReport>,
}

#[derive(Serialize)]
struct PowerViaKeyInput<'a> {
    schema: &'static str,
    place_key: &'a str,
    tracks: &'a [Track],
    vias: &'a [Via],
    pours: &'a [PourSpec],
    net_classes: &'a BTreeMap<String, NetClass>,
    nets: &'a BTreeMap<String, NetDef>,
    rules: &'a DesignRules,
    min_vias: &'a [MinViasRule],
    cfg: &'a PowerViaOptions,
}

#[derive(Serialize, Deserialize)]
struct PowerViaCache {
    tracks: Vec<Track>,
    vias: Vec<Via>,
    report: PowerViaReport,
}

#[derive(Serialize)]
struct PreflightKeyInput<'a> {
    schema: &'static str,
    power_via_key: &'a str,
    ignored_nets: &'a BTreeSet<String>,
    filled_pours: &'a [FilledPour],
}

#[derive(Serialize)]
struct RouteBaseKeyInput<'a> {
    schema: &'static str,
    power_via_key: &'a str,
    max_lengths: &'a [MaxLengthRule],
    match_groups: &'a [MatchGroupRule],
    filled_pours: &'a [FilledPour],
    cfg: &'a RouteOptions,
    repair_passes: usize,
}

#[derive(Serialize)]
struct RouteKeyInput<'a> {
    schema: &'static str,
    power_via_key: &'a str,
    max_lengths: &'a [MaxLengthRule],
    match_groups: &'a [MatchGroupRule],
    filled_pours: &'a [FilledPour],
    cfg: &'a RouteOptions,
    repair_passes: usize,
    seed_hash: Option<&'a str>,
}

#[derive(Serialize, Deserialize)]
struct RouteCache {
    tracks: Vec<Track>,
    vias: Vec<Via>,
    summary: RouteSummary,
}

#[derive(Serialize, Deserialize)]
struct RouteSeed {
    tracks: Vec<Track>,
    vias: Vec<Via>,
}

#[derive(Serialize)]
struct ZonesKeyInput<'a> {
    schema: &'static str,
    route_key: &'a str,
    pours: &'a [PourSpec],
    tracks: &'a [Track],
    vias: &'a [Via],
}

#[derive(Serialize, Deserialize)]
struct ZonesCache {
    filled_pours: Vec<FilledPour>,
    report: ZoneReport,
}

#[derive(Serialize)]
struct DrcKeyInput<'a> {
    schema: &'static str,
    zones_key: &'a str,
    constraints: &'a Constraints,
    rules: &'a DesignRules,
    #[serde(skip_serializing_if = "Option::is_none")]
    fab: Option<&'a FabConfig>,
    #[serde(skip_serializing_if = "Option::is_none")]
    graphics: Option<&'a [Graphic]>,
}

fn all_tht_pads_in_plane_pours(board: &BoardIr, net: &str, plane_pours: &[&PourSpec]) -> bool {
    for pl in &board.placements {
        let Some(fp) = board.footprints.get(&pl.footprint) else {
            continue;
        };
        for pad in &fp.pads {
            if pad.kind != PadKind::Tht {
                continue;
            }
            if pl.pin_nets.get(&pad.name).map(String::as_str) != Some(net) {
                continue;
            }
            let center = transform_pad_local_pt(Pt::new(0, 0), pad, pl);
            if !plane_pours
                .iter()
                .any(|p| point_in_polygon(center, &p.outline))
            {
                return false;
            }
        }
    }
    true
}

fn effective_ignored_nets(
    board: &BoardIr,
    base_ignored: &HashSet<String>,
    pv_report: &PowerViaReport,
) -> BTreeSet<String> {
    let mut ignored: BTreeSet<String> = base_ignored.iter().cloned().collect();
    let constrained_nets: HashSet<&str> = board
        .constraints
        .max_lengths
        .iter()
        .flat_map(|r| r.nets.iter().map(String::as_str))
        .chain(
            board
                .constraints
                .match_groups
                .iter()
                .flat_map(|r| r.nets.iter().map(String::as_str)),
        )
        .collect();
    let unresolved_nets: HashSet<&str> = pv_report
        .unresolved
        .iter()
        .map(|u| u.net.as_str())
        .collect();
    let plane_layers: HashSet<&str> = board
        .layers
        .iter()
        .filter(|l| l.kind == LayerKind::Plane)
        .map(|l| l.name.as_str())
        .collect();
    for net in &pv_report.plane_nets {
        let plane_pours: Vec<&PourSpec> = board
            .pours
            .iter()
            .filter(|p| &p.net == net && plane_layers.contains(p.layer.as_str()))
            .collect();
        if !plane_pours.is_empty()
            && !unresolved_nets.contains(net.as_str())
            && !constrained_nets.contains(net.as_str())
            && all_tht_pads_in_plane_pours(board, net, &plane_pours)
        {
            ignored.insert(net.clone());
        }
    }
    ignored
}

fn emit_fab_and_svg(
    board: &BoardIr,
    drc: &DrcReport,
    out_dir: &Path,
) -> Result<Vec<PathBuf>, String> {
    let manifest = export_fab(board, &out_dir.join("fab"), &FabOptions::default())?;
    let mut files = manifest.all_files();
    files.extend(write_board_svgs(board, Some(drc), &out_dir.join("svg"))?);
    Ok(files)
}

/// Runs the cached staged compiler (`place -> power_via -> preflight -> route -> zones -> drc`).
pub fn compile_board(mut board: BoardIr, cfg: &BuildOptions) -> Result<BuildReport, String> {
    let total_start = Instant::now();
    let mut stages = Vec::with_capacity(6);

    // 1. Place (legalize movable placements if any exist).
    let place_key = content_hash_hex(&PlaceKeyInput {
        schema: CACHE_SCHEMA,
        outline: &board.outline,
        cutouts: &board.cutouts,
        layers: &board.layers,
        footprints: &board.footprints,
        placements: &board.placements,
        keepouts: &board.keepouts,
        courtyard_clearance_nm: board.rules.courtyard_clearance_nm,
        edge_clearance_nm: board.rules.edge_clearance_nm,
        clearance_nm: board.rules.clearance_nm,
        smd_to_smd_nm: board.rules.smd_to_smd_nm,
        hole_to_hole_nm: board.rules.hole_to_hole_nm,
        hole_clearance_nm: board.rules.hole_clearance_nm,
        via_to_smd_nm: board.rules.via_to_smd_nm,
        net_classes: &board.net_classes,
        nets: &board.nets,
        tracks: &board.tracks,
        vias: &board.vias,
        cap_proximity: &board.constraints.cap_proximity,
        bga_shadows: &board.constraints.bga_shadows,
        cfg: &cfg.place,
    })?;
    let place_cache = run_cached_stage(
        cfg,
        StageKind::Place,
        place_key.clone(),
        &mut stages,
        || {
            let report = if board.placements.iter().any(|p| !p.locked) {
                Some(legalize_placement(&mut board, &cfg.place)?)
            } else {
                None
            };
            Ok(PlaceCache {
                placements: board.placements.clone(),
                report,
            })
        },
    )?;
    board.placements = place_cache.placements;
    let place_report = place_cache.report;

    // 2. Power vias (thermal arrays + plane drops for plane nets).
    let power_via_key = content_hash_hex(&PowerViaKeyInput {
        schema: CACHE_SCHEMA,
        place_key: &place_key,
        tracks: &board.tracks,
        vias: &board.vias,
        pours: &board.pours,
        net_classes: &board.net_classes,
        nets: &board.nets,
        rules: &board.rules,
        min_vias: &board.constraints.min_vias,
        cfg: &cfg.power_via,
    })?;
    let pv_cache = run_cached_stage(
        cfg,
        StageKind::PowerVia,
        power_via_key.clone(),
        &mut stages,
        || {
            let report = solve_power_vias(&mut board, &cfg.power_via)?;
            Ok(PowerViaCache {
                tracks: board.tracks.clone(),
                vias: board.vias.clone(),
                report,
            })
        },
    )?;
    board.tracks = pv_cache.tracks;
    board.vias = pv_cache.vias;
    let pv_report = pv_cache.report;

    let eff_ignored_sorted = effective_ignored_nets(&board, &cfg.route.ignored_nets, &pv_report);
    let eff_ignored_set: HashSet<String> = eff_ignored_sorted.iter().cloned().collect();

    // 3. Preflight feasibility gate.
    let preflight_key = content_hash_hex(&PreflightKeyInput {
        schema: CACHE_SCHEMA,
        power_via_key: &power_via_key,
        ignored_nets: &eff_ignored_sorted,
        filled_pours: &board.filled_pours,
    })?;
    let preflight = run_cached_stage(
        cfg,
        StageKind::Preflight,
        preflight_key,
        &mut stages,
        || run_preflight(&board, &eff_ignored_set),
    )?;
    if cfg.fail_on_preflight && !preflight.ok {
        let blockers = PreflightBlockers {
            ok: preflight.ok,
            stopped_early: preflight.stopped_early,
            layout_violations: &preflight.layout_violations,
            zone_chokes: &preflight.zone_chokes,
            sealed_pads: &preflight.sealed_pads,
            blocked_nets: &preflight.blocked_nets,
            fixed_violations: &preflight.fixed_violations,
            erc_violations: &preflight.erc_violations,
        };
        let json = serde_json::to_string_pretty(&blockers)
            .map_err(|e| format!("failed to serialize preflight blockers: {e}"))?;
        return Err(format!("preflight failed with hard blockers:\n{json}"));
    }

    // 4. Route (in-memory viahop routing, honoring --reroute-nets over cached/initial routed board).
    let mut eff_route_cfg = cfg.route.clone();
    eff_route_cfg.ignored_nets = eff_ignored_set;
    let mut base_route_cfg = eff_route_cfg.clone();
    base_route_cfg.reroute_nets = None;
    base_route_cfg.reroute_pad_mm = None;
    // Clear stale filled_pours before repair when dynamic pours exist; keep pre-baked pours when board.pours is empty.
    let route_filled_pours: &[FilledPour] = if board.pours.is_empty() {
        &board.filled_pours
    } else {
        &[]
    };
    let base_route_key_for = |passes: usize| {
        content_hash_hex(&RouteBaseKeyInput {
            schema: CACHE_SCHEMA,
            power_via_key: &power_via_key,
            max_lengths: &board.constraints.max_lengths,
            match_groups: &board.constraints.match_groups,
            filled_pours: route_filled_pours,
            cfg: &base_route_cfg,
            repair_passes: passes,
        })
    };
    let base_route_key = base_route_key_for(cfg.repair_passes)?;
    let seed_path = cfg
        .cache_dir
        .join(format!("route_seed_{base_route_key}.json"));
    let has_unlocked =
        board.tracks.iter().any(|t| !t.locked) || board.vias.iter().any(|v| !v.locked);
    let loaded_seed = if eff_route_cfg.reroute_nets.is_some() && cfg.use_cache && !has_unlocked {
        let fallback_0 = || {
            (cfg.repair_passes > 0)
                .then(|| base_route_key_for(0).ok())
                .flatten()
                .and_then(|k0| {
                    load_cached::<RouteSeed>(&cfg.cache_dir.join(format!("route_seed_{k0}.json")))
                })
        };
        load_cached::<RouteSeed>(&seed_path).or_else(fallback_0)
    } else {
        None
    };
    let seed_hash = loaded_seed.as_ref().map(content_hash_hex).transpose()?;
    let route_key = content_hash_hex(&RouteKeyInput {
        schema: CACHE_SCHEMA,
        power_via_key: &power_via_key,
        max_lengths: &board.constraints.max_lengths,
        match_groups: &board.constraints.match_groups,
        filled_pours: route_filled_pours,
        cfg: &eff_route_cfg,
        repair_passes: cfg.repair_passes,
        seed_hash: seed_hash.as_deref(),
    })?;
    let route_cache = run_cached_stage(
        cfg,
        StageKind::Route,
        route_key.clone(),
        &mut stages,
        || {
            if let Some(seed) = loaded_seed {
                board.tracks = seed.tracks;
                board.vias = seed.vias;
            }
            if !board.pours.is_empty() && cfg.repair_passes > 0 {
                board.filled_pours.clear();
            }
            let summary = route_with_repair(&mut board, &eff_route_cfg, cfg.repair_passes)?;
            if cfg.use_cache && eff_route_cfg.reroute_nets.is_none() {
                save_cached(
                    &cfg.cache_dir,
                    &seed_path,
                    &RouteSeed {
                        tracks: board.tracks.clone(),
                        vias: board.vias.clone(),
                    },
                )?;
            }
            Ok(RouteCache {
                tracks: board.tracks.clone(),
                vias: board.vias.clone(),
                summary,
            })
        },
    )?;
    board.tracks = route_cache.tracks;
    board.vias = route_cache.vias;
    let route_summary = route_cache.summary;

    // 5. Zones (Clipper2 pour fill + choke check).
    let zones_key = content_hash_hex(&ZonesKeyInput {
        schema: CACHE_SCHEMA,
        route_key: &route_key,
        pours: &board.pours,
        tracks: &board.tracks,
        vias: &board.vias,
    })?;
    let zones_cache = run_cached_stage(
        cfg,
        StageKind::Zones,
        zones_key.clone(),
        &mut stages,
        || {
            let report = fill_zones(&mut board);
            Ok(ZonesCache {
                filled_pours: board.filled_pours.clone(),
                report,
            })
        },
    )?;
    board.filled_pours = zones_cache.filled_pours;
    let zone_report = zones_cache.report;

    // 6. DRC (exact continuous DRC + spatial hotspot clustering).
    let silk_drc_active =
        board.rules.silk_to_mask_nm.is_some() || board.rules.silk_to_edge_nm.is_some();
    let drc_key = content_hash_hex(&DrcKeyInput {
        schema: CACHE_SCHEMA,
        zones_key: &zones_key,
        constraints: &board.constraints,
        rules: &board.rules,
        fab: silk_drc_active.then_some(&board.fab),
        graphics: silk_drc_active.then_some(board.graphics.as_slice()),
    })?;
    let drc_report = run_cached_stage(cfg, StageKind::Drc, drc_key, &mut stages, || {
        Ok(check_board_drc(&board))
    })?;

    // Optional output emission (writes compiled BoardIr + fab/svg outputs).
    let mut emitted_files = Vec::new();
    if let Some(ref out_dir) = cfg.out_dir {
        fs::create_dir_all(out_dir)
            .map_err(|e| format!("failed to create out dir '{}': {e}", out_dir.display()))?;
        let board_out = out_dir.join(format!("{}.board.json", safe_slug(&board.name)));
        save_board_json(&board, &board_out)?;
        emitted_files.push(board_out);
        if cfg.emit_outputs {
            emitted_files.extend(emit_fab_and_svg(&board, &drc_report, out_dir)?);
        }
    }

    let erc_ok = !preflight
        .erc_violations
        .iter()
        .any(|v| v.severity == Severity::Error);
    let ok = place_report.as_ref().is_none_or(|p| p.ok)
        && pv_report.ok
        && erc_ok
        && route_summary.unconnected_pairs == 0
        && zone_report.chokes.is_empty()
        && drc_report.passed;

    Ok(BuildReport {
        board_name: board.name.clone(),
        ok,
        place: place_report,
        power_via: pv_report,
        preflight,
        route: route_summary,
        zones: zone_report,
        drc: drc_report,
        board,
        stages,
        emitted_files,
        elapsed_ms: total_start.elapsed().as_millis(),
    })
}
