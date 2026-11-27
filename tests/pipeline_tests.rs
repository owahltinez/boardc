use std::collections::HashSet;
use std::fs;
use std::path::PathBuf;
use std::process::Command;

use boardc::drc::ViolationKind;
use boardc::ir::{BoardIr, MaxLengthRule, load_spec_file};
use boardc::pipeline::{BuildOptions, StageKind, compile_board, content_hash_hex};

fn four_layer_bga_toml() -> &'static str {
    r#"
name = "bga_4layer_synth"
size_nm = [40000000, 30000000]

[[layers]]
name = "F.Cu"
kind = "signal"
side = "top"

[[layers]]
name = "In1.Cu"
kind = "plane"

[[layers]]
name = "In2.Cu"
kind = "plane"

[[layers]]
name = "B.Cu"
kind = "signal"
side = "bottom"

[footprints.BGA16]
kind = "bga"
rows = 4
cols = 4
pitch_nm = 800000
ball_dia_nm = 400000

[footprints.C_0402]
kind = "chip2_pin"
package = "0402"

[footprints.R_0603]
kind = "chip2_pin"
package = "0603"

[[placements]]
refdes = "U1"
footprint = "BGA16"
value = "MCU_BGA16"
pos = { x = 15000000, y = 15000000 }
rot_deg = 0.0
side = "top"
locked = true
pin_nets = { A1 = "SIG0", A4 = "SIG1", B2 = "VDD", B3 = "GND", D1 = "SIG2", D4 = "SIG3" }

[[placements]]
refdes = "C1"
footprint = "C_0402"
value = "100nF"
pos = { x = 15000000, y = 15000000 }
rot_deg = 0.0
side = "top"
locked = false
pin_nets = { "1" = "VDD", "2" = "GND" }

[[placements]]
refdes = "C2"
footprint = "C_0402"
value = "100nF"
pos = { x = 15000000, y = 15000000 }
rot_deg = 0.0
side = "top"
locked = false
pin_nets = { "1" = "VDD", "2" = "GND" }

[[placements]]
refdes = "R1"
footprint = "R_0603"
value = "10k"
pos = { x = 28000000, y = 20000000 }
rot_deg = 0.0
side = "top"
locked = true
pin_nets = { "1" = "SIG0", "2" = "SIG1" }

[[placements]]
refdes = "R2"
footprint = "R_0603"
value = "10k"
pos = { x = 28000000, y = 10000000 }
rot_deg = 0.0
side = "bottom"
locked = true
pin_nets = { "1" = "SIG2", "2" = "SIG3" }

[[pours]]
name = "GND_IN1"
net = "GND"
layer = "In1.Cu"
outline = [
  { x = 500000, y = 500000 },
  { x = 39500000, y = 500000 },
  { x = 39500000, y = 29500000 },
  { x = 500000, y = 29500000 },
]
priority = 0
min_width_nm = 200000
thermal_relief = false
remove_islands = true

[[pours]]
name = "VDD_IN2"
net = "VDD"
layer = "In2.Cu"
outline = [
  { x = 500000, y = 500000 },
  { x = 39500000, y = 500000 },
  { x = 39500000, y = 29500000 },
  { x = 500000, y = 29500000 },
]
priority = 0
min_width_nm = 200000
thermal_relief = false
remove_islands = true

[rules]
track_width_nm = 120000
clearance_nm = 120000
via_dia_nm = 320000
via_drill_nm = 150000
min_annular_ring_nm = 75000
hole_to_hole_nm = 200000
edge_clearance_nm = 300000
courtyard_clearance_nm = 100000
via_at_smd = true

[[constraints.cap_proximity]]
cap_refdes = "C1"
ic_refdes = "U1"
net = "VDD"
max_dist_nm = 4500000

[[constraints.cap_proximity]]
cap_refdes = "C2"
ic_refdes = "U1"
net = "VDD"
max_dist_nm = 5500000

[[constraints.bga_shadows]]
bga_refdes = "U1"
min_clearance_nm = 500000
allow_passives = false

[[constraints.max_lengths]]
nets = ["SIG0"]
max_nm = 35000000
"#
}

fn fresh_tmp_dir(name: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("boardc_pipeline_{name}_{}", std::process::id()));
    let _ = fs::remove_dir_all(&dir);
    fs::create_dir_all(&dir).unwrap();
    dir
}

#[test]
fn full_4layer_bga_toml_pipeline_clean_drc_and_zero_opens() {
    let tmp = fresh_tmp_dir("full_4layer");
    let spec_path = tmp.join("bga_board.toml");
    let cache_dir = tmp.join("cache");
    let out_dir = tmp.join("out");
    fs::write(&spec_path, four_layer_bga_toml()).unwrap();

    let spec = load_spec_file(&spec_path).unwrap();
    let board = BoardIr::from_spec(&spec).unwrap();

    let mut cfg = BuildOptions {
        out_dir: Some(out_dir.clone()),
        cache_dir: cache_dir.clone(),
        fail_on_preflight: true,
        ..Default::default()
    };
    cfg.route.max_passes = 20;
    cfg.route.multi_start = 2;

    let report = compile_board(board, &cfg).unwrap();
    assert!(report.ok, "pipeline failed: {report:?}");
    let place = report
        .place
        .as_ref()
        .expect("movable caps should run place");
    assert!(place.ok, "place failed: {place:?}");
    assert_eq!(place.moved_parts.len(), 2);
    assert!(
        report.power_via.ok,
        "power_via failed: {:?}",
        report.power_via
    );
    assert!(
        report.power_via.pofv_count >= 2,
        "expected POFV drops on BGA GND/VDD balls, got {}",
        report.power_via.pofv_count
    );
    assert!(
        report.preflight.ok,
        "preflight failed: {:?}",
        report.preflight
    );
    assert_eq!(report.route.unconnected_pairs, 0);
    assert!(report.zones.chokes.is_empty(), "{:?}", report.zones.chokes);
    assert!(
        report.drc.passed,
        "expected 0 DRC errors, got {:?}",
        report.drc.violations
    );
    assert_eq!(report.drc.error_count, 0);
    assert!(
        !report
            .drc
            .violations
            .iter()
            .any(|v| v.kind == ViolationKind::OpenNet),
        "expected 0 OpenNet violations, got {:?}",
        report.drc.violations
    );
    assert_eq!(report.stages.len(), 6);
    assert!(report.stages.iter().all(|s| !s.cache_hit));
    assert!(!report.emitted_files.is_empty());
    assert!(report.emitted_files[0].exists());

    // Verify `boardc build <SPEC> --out-dir <DIR> ... --json` CLI hits the populated cache.
    let cli_out = Command::new(env!("CARGO_BIN_EXE_boardc"))
        .args([
            "build",
            spec_path.to_str().unwrap(),
            "--out-dir",
            out_dir.to_str().unwrap(),
            "--cache-dir",
            cache_dir.to_str().unwrap(),
            "--fail-on-preflight",
            "--max-passes",
            "20",
            "--multi-start",
            "2",
            "--threads",
            "1",
            "--json",
        ])
        .output()
        .unwrap();
    assert!(
        cli_out.status.success(),
        "cli build failed: {}",
        String::from_utf8_lossy(&cli_out.stderr)
    );
    let json: serde_json::Value = serde_json::from_slice(&cli_out.stdout).unwrap();
    assert_eq!(json["ok"], true);
    assert_eq!(json["drc"]["error_count"], 0);
    assert_eq!(json["route"]["unconnected_pairs"], 0);
    let stages = json["stages"].as_array().unwrap();
    assert_eq!(stages.len(), 6);
    assert!(
        stages.iter().all(|s| s["cache_hit"] == true),
        "expected CLI build to hit warm stage cache: {stages:?}"
    );

    let _ = fs::remove_dir_all(&tmp);
}

#[test]
fn cache_hits_and_route_constraint_invalidation_preserve_place_and_power_via() {
    let tmp = fresh_tmp_dir("cache_invalidation");
    let spec_path = tmp.join("bga_board.toml");
    let cache_dir = tmp.join("cache");
    fs::write(&spec_path, four_layer_bga_toml()).unwrap();

    let spec = load_spec_file(&spec_path).unwrap();
    let board1 = BoardIr::from_spec(&spec).unwrap();

    let mut cfg = BuildOptions {
        cache_dir: cache_dir.clone(),
        ..Default::default()
    };
    cfg.route.max_passes = 20;
    cfg.route.multi_start = 2;

    // Run 1: cold cache.
    let r1 = compile_board(board1.clone(), &cfg).unwrap();
    assert!(r1.ok);
    assert!(r1.stages.iter().all(|s| !s.cache_hit));

    // Run 2: warm cache hits every stage and drops elapsed time sharply.
    let r2 = compile_board(board1, &cfg).unwrap();
    assert!(r2.ok);
    assert!(r2.stages.iter().all(|s| s.cache_hit));
    assert!(
        r2.elapsed_ms < r1.elapsed_ms,
        "expected cached build ({}ms) to be faster than cold build ({}ms)",
        r2.elapsed_ms,
        r1.elapsed_ms
    );
    assert_eq!(r2.board.tracks, r1.board.tracks);
    assert_eq!(r2.board.vias, r1.board.vias);
    assert_eq!(r2.board.placements, r1.board.placements);

    // Run 3: tighten a route constraint -> `place` + `power_via` stay cached, `route` + downstream invalidate.
    let mut board3 = BoardIr::from_spec(&spec).unwrap();
    board3.constraints.max_lengths = vec![MaxLengthRule {
        nets: vec!["SIG0".into()],
        max_nm: 30_000_000,
    }];
    let r3 = compile_board(board3, &cfg).unwrap();
    assert!(r3.ok);
    assert!(r3.stage_hit(StageKind::Place), "place should remain cached");
    assert!(
        r3.stage_hit(StageKind::PowerVia),
        "power_via should remain cached"
    );
    assert!(
        r3.stage_hit(StageKind::Preflight),
        "preflight should remain cached"
    );
    assert!(!r3.stage_hit(StageKind::Route), "route must invalidate");
    assert!(!r3.stage_hit(StageKind::Zones), "zones must invalidate");
    assert!(!r3.stage_hit(StageKind::Drc), "drc must invalidate");
    assert_eq!(r3.board.constraints.max_lengths[0].max_nm, 30_000_000);

    // Run 4: `--reroute-nets SIG0` reuses cached routes for SIG1/SIG2/SIG3 while re-routing SIG0.
    let mut reroute_cfg = cfg.clone();
    reroute_cfg.route.reroute_nets = Some(HashSet::from(["SIG0".to_string()]));
    let mut board4 = BoardIr::from_spec(&spec).unwrap();
    board4.constraints.max_lengths = vec![MaxLengthRule {
        nets: vec!["SIG0".into()],
        max_nm: 30_000_000,
    }];
    let r4 = compile_board(board4, &reroute_cfg).unwrap();
    assert!(r4.ok);
    assert!(r4.stage_hit(StageKind::Place));
    assert!(r4.stage_hit(StageKind::PowerVia));
    assert!(!r4.stage_hit(StageKind::Route));
    let sig1_before: Vec<_> = r3.board.tracks.iter().filter(|t| t.net == "SIG1").collect();
    let sig1_after: Vec<_> = r4.board.tracks.iter().filter(|t| t.net == "SIG1").collect();
    assert_eq!(sig1_before, sig1_after);

    // Run 5: `--no-cache` bypasses reading cache.
    let mut no_cache_cfg = cfg;
    no_cache_cfg.use_cache = false;
    let r5 = compile_board(BoardIr::from_spec(&spec).unwrap(), &no_cache_cfg).unwrap();
    assert!(r5.ok);
    assert!(r5.stages.iter().all(|s| !s.cache_hit));

    let _ = fs::remove_dir_all(&tmp);
}

#[test]
fn fail_on_preflight_returns_actionable_json_blockers_and_hash_is_canonical() {
    let h_pos = content_hash_hex(&serde_json::json!({ "b": 0.0_f64, "a": 1 })).unwrap();
    let h_neg = content_hash_hex(&serde_json::json!({ "a": 1, "b": -0.0_f64 })).unwrap();
    assert_eq!(h_pos, h_neg);
    assert_eq!(h_pos.len(), 32);

    let bad_toml = r#"
name = "blocked_board"
size_nm = [20000000, 20000000]

[footprints.R_0603]
kind = "chip2_pin"
package = "0603"

[[placements]]
refdes = "R1"
footprint = "R_0603"
pos = { x = 10000000, y = 10000000 }
locked = true
pin_nets = { "1" = "N1", "2" = "N2" }

[[placements]]
refdes = "R2"
footprint = "R_0603"
pos = { x = 10000000, y = 10000000 }
locked = true
pin_nets = { "1" = "N1", "2" = "N2" }
"#;
    let spec: boardc::ir::BoardSpec = toml::from_str(bad_toml).unwrap();
    let board = BoardIr::from_spec(&spec).unwrap();
    let cfg = BuildOptions {
        use_cache: false,
        fail_on_preflight: true,
        ..Default::default()
    };
    let err = compile_board(board, &cfg).unwrap_err();
    let json_start = err.find('{').expect("expected JSON blockers in error");
    let blockers: serde_json::Value = serde_json::from_str(&err[json_start..]).unwrap();
    assert_eq!(blockers["ok"], false);
    assert!(
        !blockers["layout_violations"].as_array().unwrap().is_empty(),
        "expected layout_violations in blockers: {blockers}"
    );
}

#[test]
fn regression_cache_keys_invalidate_place_preflight_and_isolate_route_seeds() {
    let tmp = fresh_tmp_dir("cache_regressions");
    let spec_path = tmp.join("bga_board.toml");
    let cache_dir = tmp.join("cache");
    fs::write(&spec_path, four_layer_bga_toml()).unwrap();

    let spec = load_spec_file(&spec_path).unwrap();
    let base_board = BoardIr::from_spec(&spec).unwrap();
    let mut cfg = BuildOptions {
        cache_dir: cache_dir.clone(),
        emit_outputs: false,
        ..Default::default()
    };
    cfg.route.max_passes = 10;
    cfg.route.multi_start = 1;

    let r1 = compile_board(base_board.clone(), &cfg).unwrap();
    assert!(r1.ok);

    // Changing rules.clearance_nm must invalidate StageKind::Place (place checks pad clearances).
    let mut clr_board = base_board.clone();
    clr_board.rules.clearance_nm += 10_000;
    let r_clr = compile_board(clr_board, &cfg).unwrap();
    assert!(
        !r_clr.stage_hit(StageKind::Place),
        "Place cache must invalidate when clearance_nm changes"
    );

    // Changing allow_edge_overhang on a placement must invalidate StageKind::Place and downstream stages.
    let mut ov_board = base_board.clone();
    ov_board.placements[0].allow_edge_overhang = true;
    let r_ov = compile_board(ov_board, &cfg).unwrap();
    assert!(
        !r_ov.stage_hit(StageKind::Place),
        "Place cache must invalidate when allow_edge_overhang changes"
    );

    // Changing pre-existing board.filled_pours must invalidate StageKind::Preflight while Place & PowerVia stay warm.
    let mut pour_board = base_board.clone();
    pour_board.filled_pours = r1.board.filled_pours.clone();
    let r_pf = compile_board(pour_board, &cfg).unwrap();
    assert!(r_pf.stage_hit(StageKind::Place));
    assert!(r_pf.stage_hit(StageKind::PowerVia));
    assert!(
        !r_pf.stage_hit(StageKind::Preflight),
        "Preflight cache must invalidate when board.filled_pours changes"
    );

    // Different route configs (e.g. via_at_smd) must write distinct route_seed files without cross-contamination.
    let mut cfg_alt = cfg.clone();
    cfg_alt.route.via_at_smd = Some(false);
    let _ = compile_board(base_board.clone(), &cfg_alt).unwrap();
    let seed_count = fs::read_dir(&cache_dir)
        .unwrap()
        .filter_map(|e| e.ok())
        .filter(|e| {
            e.file_name()
                .to_str()
                .is_some_and(|n| n.starts_with("route_seed_"))
        })
        .count();
    assert!(
        seed_count >= 2,
        "expected distinct route_seed files for different route options, got {seed_count}"
    );

    let _ = fs::remove_dir_all(&tmp);
}
