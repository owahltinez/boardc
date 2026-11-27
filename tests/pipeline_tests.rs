use std::collections::HashSet;
use std::fs;
use std::path::PathBuf;
use std::process::Command;

use boardc::drc::ViolationKind;
use boardc::ir::{
    BoardIr, BoardSpec, FilledPour, Keepout, KeepoutKind, MatchGroupRule, MaxLengthRule,
    MaxViasRule, PourPoly, Pt, Track, Via, load_spec_file, mm,
};
use boardc::pipeline::{BuildOptions, StageKind, compile_board, content_hash_hex};
use boardc::route::{RouteOptions, route_board_ir};

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

    // Run 3b: tighten max_vias -> `place` + `power_via` + `preflight` stay cached, `route` + downstream invalidate.
    let mut board3b = BoardIr::from_spec(&spec).unwrap();
    board3b.constraints.max_lengths = r3.board.constraints.max_lengths.clone();
    board3b.constraints.max_vias = vec![MaxViasRule {
        nets: vec!["SIG0".into()],
        max_vias: 2,
    }];
    let r3b = compile_board(board3b, &cfg).unwrap();
    assert!(
        r3b.stage_hit(StageKind::Place),
        "place should remain cached"
    );
    assert!(
        r3b.stage_hit(StageKind::PowerVia),
        "power_via should remain cached"
    );
    assert!(
        r3b.stage_hit(StageKind::Preflight),
        "preflight should remain cached"
    );
    assert!(
        !r3b.stage_hit(StageKind::Route),
        "route must invalidate when max_vias changes"
    );
    assert!(
        !r3b.stage_hit(StageKind::Zones),
        "zones must invalidate when max_vias changes"
    );
    assert!(
        !r3b.stage_hit(StageKind::Drc),
        "drc must invalidate when max_vias changes"
    );

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

fn boxed_crossing_board_toml(with_plane_and_sig_b: bool) -> String {
    let mut toml = String::from(
        r#"
name = "boxed_repair"
size_nm = [30000000, 26000000]

[[layers]]
name = "F.Cu"
kind = "signal"
side = "top"

[footprints.PAD1]
kind = "custom"
[[footprints.PAD1.pads]]
name = "1"
kind = "smd"
shape = { shape = "circle", diameter = 600000 }
pos = { x = 0, y = 0 }
layers = ["F.Cu"]

[[placements]]
refdes = "P_A1"
footprint = "PAD1"
pos = { x = 10000000, y = 13000000 }
locked = true
pin_nets = { "1" = "SIG_A" }

[[placements]]
refdes = "P_A2"
footprint = "PAD1"
pos = { x = 16000000, y = 13000000 }
locked = true
pin_nets = { "1" = "SIG_A" }

[[placements]]
refdes = "P_N1"
footprint = "PAD1"
pos = { x = 14000000, y = 14500000 }
locked = true
pin_nets = { "1" = "SIG_N1" }

[[placements]]
refdes = "P_N2"
footprint = "PAD1"
pos = { x = 14000000, y = 11500000 }
locked = true
pin_nets = { "1" = "SIG_N1" }

[[placements]]
refdes = "P_M1"
footprint = "PAD1"
pos = { x = 13300000, y = 17000000 }
locked = true
pin_nets = { "1" = "SIG_N2" }

[[placements]]
refdes = "P_M2"
footprint = "PAD1"
pos = { x = 13300000, y = 9000000 }
locked = true
pin_nets = { "1" = "SIG_N2" }

[rules]
track_width_nm = 200000
clearance_nm = 200000
edge_clearance_nm = 300000
"#,
    );
    if with_plane_and_sig_b {
        toml.push_str(
            r#"
via_dia_nm = 450000
via_drill_nm = 200000
min_annular_ring_nm = 100000
hole_to_hole_nm = 200000

[[layers]]
name = "B.Cu"
kind = "plane"
side = "bottom"

[footprints.THT1]
kind = "custom"
[[footprints.THT1.pads]]
name = "1"
kind = "tht"
shape = { shape = "circle", diameter = 1000000 }
drill_nm = 500000
pos = { x = 0, y = 0 }
layers = ["F.Cu", "B.Cu"]

[[placements]]
refdes = "P_B1"
footprint = "PAD1"
pos = { x = 5000000, y = 4000000 }
locked = true
pin_nets = { "1" = "SIG_B" }

[[placements]]
refdes = "P_B2"
footprint = "PAD1"
pos = { x = 13000000, y = 4000000 }
locked = true
pin_nets = { "1" = "SIG_B" }

[[placements]]
refdes = "G1"
footprint = "THT1"
pos = { x = 4000000, y = 22000000 }
locked = true
pin_nets = { "1" = "GND" }

[[placements]]
refdes = "G2"
footprint = "THT1"
pos = { x = 26000000, y = 22000000 }
locked = true
pin_nets = { "1" = "GND" }

[[pours]]
name = "GND_PLANE"
net = "GND"
layer = "B.Cu"
outline = [
  { x = 500000, y = 500000 },
  { x = 29500000, y = 500000 },
  { x = 29500000, y = 25500000 },
  { x = 500000, y = 25500000 },
]
priority = 0
min_width_nm = 200000
thermal_relief = false
remove_islands = true
"#,
        );
    }
    toml
}

fn boxed_cfg(repair_passes: usize, reroute_sig_a: bool) -> BuildOptions {
    let mut cfg = BuildOptions {
        use_cache: false,
        emit_outputs: false,
        repair_passes,
        ..Default::default()
    };
    cfg.route.max_passes = 20;
    cfg.route.multi_start = 1;
    if reroute_sig_a {
        cfg.route.reroute_nets = Some(HashSet::from(["SIG_A".to_string()]));
        cfg.route.reroute_pad_mm = Some(0.5);
    }
    cfg
}

fn add_cage_tracks(board: &mut BoardIr, locked: bool) {
    let w = mm(0.2);
    for (a, b, net) in [
        (
            Pt::new(mm(14.0), mm(14.5)),
            Pt::new(mm(18.0), mm(14.5)),
            "SIG_N1",
        ),
        (
            Pt::new(mm(18.0), mm(14.5)),
            Pt::new(mm(18.0), mm(11.5)),
            "SIG_N1",
        ),
        (
            Pt::new(mm(18.0), mm(11.5)),
            Pt::new(mm(14.0), mm(11.5)),
            "SIG_N1",
        ),
        (
            Pt::new(mm(13.3), mm(17.0)),
            Pt::new(mm(13.3), mm(9.0)),
            "SIG_N2",
        ),
    ] {
        board.tracks.push(Track {
            net: net.into(),
            layer: "F.Cu".into(),
            a,
            b,
            width: w,
            locked,
        });
    }
}

#[test]
fn repair_passes_widens_boxed_cluster_to_neighbors_and_caches_cleanly() {
    let tmp = fresh_tmp_dir("repair_widening");
    let cache_dir = tmp.join("cache");
    let board_path = tmp.join("boxed.board.json");

    let spec: BoardSpec = toml::from_str(&boxed_crossing_board_toml(false)).unwrap();
    let mut board = BoardIr::from_spec(&spec).unwrap();
    add_cage_tracks(&mut board, false);
    boardc::ir::save_board_json(&board, &board_path).unwrap();

    let mut base_cfg = boxed_cfg(0, true);
    base_cfg.cache_dir = cache_dir.clone();
    base_cfg.use_cache = true;

    // 0 repair passes preserves existing behavior and fails with OpenNet on boxed-in SIG_A.
    let r0 = compile_board(board.clone(), &base_cfg).unwrap();
    assert!(!r0.ok);
    assert_eq!(r0.route.repair_passes_run, 0);
    assert_eq!(r0.route.unconnected_pairs, 1);

    // 1 repair pass only retries affected_nets (SIG_A), still deadlocks, and stays bounded.
    let mut cfg1 = base_cfg.clone();
    cfg1.repair_passes = 1;
    let r1 = compile_board(board.clone(), &cfg1).unwrap();
    assert!(!r1.ok);
    assert_eq!(r1.route.repair_passes_run, 1);
    assert_eq!(r1.route.unconnected_pairs, 1);
    assert!(r1.stage_hit(StageKind::Place));
    assert!(!r1.stage_hit(StageKind::Route));

    // 3 repair passes widens on pass 2 to unlocked neighbor SIG_N1 + wider pad and resolves DRC.
    let mut cfg3 = base_cfg.clone();
    cfg3.repair_passes = 3;
    let r3 = compile_board(board.clone(), &cfg3).unwrap();
    assert!(r3.ok, "expected repair_passes=3 to succeed: {:?}", r3.drc);
    assert_eq!(r3.route.repair_passes_run, 2);
    assert_eq!(r3.route.unconnected_pairs, 0);
    assert!(r3.drc.passed);

    // Warm cache hit for repair_passes=3 and no collision with repair_passes=0.
    let r3_warm = compile_board(board.clone(), &cfg3).unwrap();
    assert!(r3_warm.stages.iter().all(|s| s.cache_hit));
    assert_eq!(r3_warm.route.repair_passes_run, 2);
    assert_eq!(r3_warm.board.tracks, r3.board.tracks);

    let r0_warm = compile_board(board, &base_cfg).unwrap();
    assert!(!r0_warm.ok);
    assert!(r0_warm.stage_hit(StageKind::Route));
    assert_eq!(r0_warm.route.repair_passes_run, 0);

    // Verify CLI `--repair-passes 3` parses and hits the populated route cache.
    let out_dir = tmp.join("cli_out");
    let cli_out = Command::new(env!("CARGO_BIN_EXE_boardc"))
        .args([
            "build",
            board_path.to_str().unwrap(),
            "--out-dir",
            out_dir.to_str().unwrap(),
            "--cache-dir",
            cache_dir.to_str().unwrap(),
            "--reroute-nets",
            "SIG_A",
            "--reroute-pad-mm",
            "0.5",
            "--max-passes",
            "20",
            "--multi-start",
            "1",
            "--repair-passes",
            "3",
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
    assert_eq!(json["route"]["repair_passes_run"], 2);
    assert_eq!(json["drc"]["error_count"], 0);
    let stages = json["stages"].as_array().unwrap();
    assert!(stages.iter().all(|s| s["cache_hit"] == true));

    let _ = fs::remove_dir_all(&tmp);
}

#[test]
fn repair_defers_match_group_tuning_ignores_plane_opens_and_stops_bounded() {
    let tmp = fresh_tmp_dir("repair_mg_bounded");

    // 1. Boxed-in board with B.Cu GND plane pour + match_group deferred until after repair.
    let spec: BoardSpec = toml::from_str(&boxed_crossing_board_toml(true)).unwrap();
    let mut board = BoardIr::from_spec(&spec).unwrap();
    board.constraints.match_groups = vec![MatchGroupRule {
        name: "MG_AB".into(),
        nets: vec!["SIG_A".into(), "SIG_B".into()],
        tolerance_nm: mm(0.5),
        target_nm: None,
    }];
    add_cage_tracks(&mut board, false);
    board.tracks.push(Track {
        net: "SIG_B".into(),
        layer: "F.Cu".into(),
        a: Pt::new(mm(5.0), mm(4.0)),
        b: Pt::new(mm(13.0), mm(4.0)),
        width: mm(0.2),
        locked: false,
    });

    let cfg = boxed_cfg(3, true);
    let rep = compile_board(board, &cfg).unwrap();
    assert!(
        rep.ok,
        "expected clean tuned+zoned build: pv={:?} pf_ok={} route={:?} zones={:?} drc={:?}",
        rep.power_via, rep.preflight.ok, rep.route, rep.zones, rep.drc
    );
    assert!(
        (2..=3).contains(&rep.route.repair_passes_run),
        "expected 2..=3 repair passes, got {}",
        rep.route.repair_passes_run
    );
    assert_eq!(rep.route.length_violations, 0);
    assert!(!rep.board.filled_pours.is_empty());
    assert!(rep.drc.passed);

    // 2. Clean initial route stops early with 0 repair passes executed.
    let clean_spec: BoardSpec = toml::from_str(&boxed_crossing_board_toml(false)).unwrap();
    let clean_board = BoardIr::from_spec(&clean_spec).unwrap();
    let clean_rep = compile_board(clean_board, &boxed_cfg(5, false)).unwrap();
    assert!(clean_rep.ok);
    assert_eq!(clean_rep.route.repair_passes_run, 0);

    // 3. Non-improving widening candidate (rips up pre-routed SIG_N1 through locked gap) rolls back tracks/vias exactly.
    let mut unfixable = BoardIr::from_spec(&clean_spec).unwrap();
    for (a, b, net, locked) in [
        (
            Pt::new(mm(15.2), mm(13.8)),
            Pt::new(mm(16.8), mm(13.8)),
            "SIG_N2",
            true,
        ),
        (
            Pt::new(mm(16.8), mm(13.8)),
            Pt::new(mm(16.8), mm(12.2)),
            "SIG_N2",
            true,
        ),
        (
            Pt::new(mm(16.8), mm(12.2)),
            Pt::new(mm(15.2), mm(12.2)),
            "SIG_N2",
            true,
        ),
        (
            Pt::new(mm(15.2), mm(12.2)),
            Pt::new(mm(15.2), mm(13.8)),
            "SIG_N2",
            true,
        ),
        (
            Pt::new(mm(13.3), mm(15.2)),
            Pt::new(mm(14.7), mm(15.2)),
            "SIG_N2",
            true,
        ),
        (
            Pt::new(mm(14.7), mm(15.2)),
            Pt::new(mm(14.7), mm(10.8)),
            "SIG_N2",
            true,
        ),
        (
            Pt::new(mm(14.7), mm(10.8)),
            Pt::new(mm(13.3), mm(10.8)),
            "SIG_N2",
            true,
        ),
        (
            Pt::new(mm(13.3), mm(10.8)),
            Pt::new(mm(13.3), mm(15.2)),
            "SIG_N2",
            true,
        ),
        (
            Pt::new(mm(13.3), mm(17.0)),
            Pt::new(mm(13.2), mm(16.9)),
            "SIG_N2",
            true,
        ),
        (
            Pt::new(mm(13.2), mm(16.9)),
            Pt::new(mm(13.2), mm(9.1)),
            "SIG_N2",
            true,
        ),
        (
            Pt::new(mm(13.2), mm(9.1)),
            Pt::new(mm(13.3), mm(9.0)),
            "SIG_N2",
            true,
        ),
        (
            Pt::new(mm(14.0), mm(14.5)),
            Pt::new(mm(14.0), mm(11.5)),
            "SIG_N1",
            false,
        ),
    ] {
        unfixable.tracks.push(Track {
            net: net.into(),
            layer: "F.Cu".into(),
            a,
            b,
            width: mm(0.2),
            locked,
        });
    }
    let expected_tracks = unfixable.tracks.clone();
    let expected_vias = unfixable.vias.clone();
    let mut bounded_cfg = boxed_cfg(2, true);
    bounded_cfg.route.resume = true;
    let unfix_rep = compile_board(unfixable, &bounded_cfg).unwrap();
    assert!(!unfix_rep.ok);
    assert_eq!(unfix_rep.route.repair_passes_run, 2);
    assert_eq!(unfix_rep.board.tracks, expected_tracks);
    assert_eq!(unfix_rep.board.vias, expected_vias);

    // 4. Route seeds stay isolated between repair_passes=0 and repair_passes>0, and --reroute-nets reuses base seed=0.
    let seed_cache = tmp.join("seed_cache");
    let light_board = BoardIr::from_spec(&clean_spec).unwrap();
    let mut s0 = boxed_cfg(0, false);
    s0.use_cache = true;
    s0.cache_dir = seed_cache.clone();
    let mut s3 = s0.clone();
    s3.repair_passes = 3;
    let r_s0 = compile_board(light_board.clone(), &s0).unwrap();
    assert!(r_s0.ok);
    assert!(compile_board(light_board.clone(), &s3).unwrap().ok);
    let seed_count = fs::read_dir(&seed_cache)
        .unwrap()
        .filter_map(|e| e.ok())
        .filter(|e| {
            e.file_name()
                .to_str()
                .is_some_and(|n| n.starts_with("route_seed_"))
        })
        .count();
    assert_eq!(seed_count, 2);

    // Cross-mode fallback: --reroute-nets SIG_A --repair-passes 2 reuses the repair_passes=0 seed in a fresh cache with only s0.
    let cross_cache = tmp.join("cross_cache");
    let mut c0 = boxed_cfg(0, false);
    c0.use_cache = true;
    c0.cache_dir = cross_cache.clone();
    let rc0 = compile_board(light_board.clone(), &c0).unwrap();
    assert!(rc0.ok);
    let sig_n1_s0: Vec<_> = rc0
        .board
        .tracks
        .iter()
        .filter(|t| t.net == "SIG_N1")
        .collect();
    let mut c2_reroute = boxed_cfg(2, true);
    c2_reroute.use_cache = true;
    c2_reroute.cache_dir = cross_cache;
    let rc2 = compile_board(light_board, &c2_reroute).unwrap();
    assert!(
        rc2.ok,
        "expected cross-mode seed fallback to preserve SIG_N1/SIG_N2: {:?}",
        rc2.drc
    );
    let sig_n1_c2: Vec<_> = rc2
        .board
        .tracks
        .iter()
        .filter(|t| t.net == "SIG_N1")
        .collect();
    assert_eq!(sig_n1_s0, sig_n1_c2);

    let _ = fs::remove_dir_all(&tmp);
}

#[test]
fn repair_full_board_without_reroute_nets_fixes_boxed_cluster() {
    let spec: BoardSpec = toml::from_str(&boxed_crossing_board_toml(false)).unwrap();
    let mut board = BoardIr::from_spec(&spec).unwrap();
    add_cage_tracks(&mut board, false);
    // With resume=true and max_passes=0, initial full-board routing keeps the cage and leaves SIG_A open;
    // repair_passes=3 widens without pre-specified --reroute-nets and resolves it.
    let mut cfg = boxed_cfg(3, false);
    cfg.route.resume = true;
    cfg.route.max_passes = 0;
    let mut c0 = cfg.clone();
    c0.repair_passes = 0;
    assert!(!compile_board(board.clone(), &c0).unwrap().ok);
    let rep = compile_board(board, &cfg).unwrap();
    assert!(
        rep.ok,
        "expected full-board repair to succeed: {:?}",
        rep.drc
    );
    assert!((1..=3).contains(&rep.route.repair_passes_run));
    assert_eq!(rep.route.unconnected_pairs, 0);
    assert!(rep.drc.passed);
}

#[test]
fn repair_ignores_stale_filled_pours_and_skips_signal_pour_dangling_opens() {
    // 1. Stale filled_pours overlapping SIG_A must be cleared before repair when board.pours is non-empty.
    let spec: BoardSpec = toml::from_str(&boxed_crossing_board_toml(true)).unwrap();
    let mut board = BoardIr::from_spec(&spec).unwrap();
    add_cage_tracks(&mut board, false);
    board.tracks.push(Track {
        net: "SIG_B".into(),
        layer: "F.Cu".into(),
        a: Pt::new(mm(5.0), mm(4.0)),
        b: Pt::new(mm(13.0), mm(4.0)),
        width: mm(0.2),
        locked: false,
    });
    board.filled_pours.push(FilledPour {
        net: "GND".into(),
        layer: "F.Cu".into(),
        priority: 0,
        polys: vec![PourPoly {
            outer: vec![
                Pt::new(mm(1.0), mm(1.0)),
                Pt::new(mm(29.0), mm(1.0)),
                Pt::new(mm(29.0), mm(25.0)),
                Pt::new(mm(1.0), mm(25.0)),
            ],
            holes: Vec::new(),
        }],
    });
    let rep = compile_board(board, &boxed_cfg(3, true)).unwrap();
    assert!(
        rep.ok,
        "stale filled_pours must not cause phantom errors: {:?}",
        rep.drc
    );
    assert!(rep.drc.passed);

    // 2. Signal-layer pour net (LayerKind::Signal + PourSpec + power/thermal vias) must not trigger bogus repair passes.
    let sig_pour_toml = r#"
name = "signal_pour_2layer"
size_nm = [24000000, 20000000]

[[layers]]
name = "F.Cu"
kind = "signal"
side = "top"

[[layers]]
name = "B.Cu"
kind = "signal"
side = "bottom"

[footprints.PAD1]
kind = "custom"
[[footprints.PAD1.pads]]
name = "1"
kind = "smd"
shape = { shape = "circle", diameter = 600000 }
pos = { x = 0, y = 0 }
layers = ["F.Cu"]

[[placements]]
refdes = "G1"
footprint = "PAD1"
pos = { x = 5000000, y = 10000000 }
locked = true
pin_nets = { "1" = "GND" }

[[placements]]
refdes = "G2"
footprint = "PAD1"
pos = { x = 19000000, y = 10000000 }
locked = true
pin_nets = { "1" = "GND" }

[[placements]]
refdes = "S1"
footprint = "PAD1"
pos = { x = 8000000, y = 5000000 }
locked = true
pin_nets = { "1" = "SIG_A" }

[[placements]]
refdes = "S2"
footprint = "PAD1"
pos = { x = 16000000, y = 5000000 }
locked = true
pin_nets = { "1" = "SIG_A" }

[[pours]]
name = "GND_BOT"
net = "GND"
layer = "B.Cu"
outline = [
  { x = 500000, y = 500000 },
  { x = 23500000, y = 500000 },
  { x = 23500000, y = 19500000 },
  { x = 500000, y = 19500000 },
]
priority = 0
min_width_nm = 200000
thermal_relief = false
remove_islands = true

[rules]
track_width_nm = 200000
clearance_nm = 200000
via_dia_nm = 450000
via_drill_nm = 200000
min_annular_ring_nm = 100000
hole_to_hole_nm = 200000
edge_clearance_nm = 300000
via_at_smd = true
"#;
    let sp_spec: BoardSpec = toml::from_str(sig_pour_toml).unwrap();
    let mut sp_board = BoardIr::from_spec(&sp_spec).unwrap();
    // Pre-drop thermal vias on GND SMD pads to B.Cu pour; pre-zone DRC sees ViaDangling/OpenNet on GND.
    for x in [mm(5.0), mm(19.0)] {
        sp_board.vias.push(Via {
            net: "GND".into(),
            pos: Pt::new(x, mm(10.0)),
            diameter: mm(0.45),
            drill: mm(0.2),
            layers: ("F.Cu".into(), "B.Cu".into()),
            locked: false,
            tented: true,
        });
    }
    let mut sp_cfg = boxed_cfg(3, false);
    sp_cfg.route.ignored_nets.insert("GND".into());
    sp_cfg.route.resume = true;
    // Also test without ignored_nets so GND is literally a non-ignored signal-layer pour net.
    sp_cfg.route.ignored_nets.clear();
    let sp_rep = compile_board(sp_board, &sp_cfg).unwrap();
    assert!(
        sp_rep.ok,
        "expected signal-layer pour board to pass: {:?}",
        sp_rep.drc
    );
    assert_eq!(sp_rep.route.repair_passes_run, 0);
}

#[test]
fn repair_scopes_reroute_nets_and_tunes_match_group_partners() {
    let toml = r#"
name = "scope_and_mg_partners"
size_nm = [30000000, 26000000]

[[layers]]
name = "F.Cu"
kind = "signal"
side = "top"

[footprints.PAD1]
kind = "custom"
[[footprints.PAD1.pads]]
name = "1"
kind = "smd"
shape = { shape = "circle", diameter = 600000 }
pos = { x = 0, y = 0 }
layers = ["F.Cu"]

[[placements]]
refdes = "P_A1"
footprint = "PAD1"
pos = { x = 10000000, y = 13000000 }
locked = true
pin_nets = { "1" = "SIG_A" }

[[placements]]
refdes = "P_A2"
footprint = "PAD1"
pos = { x = 16000000, y = 13000000 }
locked = true
pin_nets = { "1" = "SIG_A" }

[[placements]]
refdes = "P_N1"
footprint = "PAD1"
pos = { x = 14000000, y = 14500000 }
locked = true
pin_nets = { "1" = "SIG_N1" }

[[placements]]
refdes = "P_N2"
footprint = "PAD1"
pos = { x = 14000000, y = 11500000 }
locked = true
pin_nets = { "1" = "SIG_N1" }

[[placements]]
refdes = "P_M1"
footprint = "PAD1"
pos = { x = 13300000, y = 17000000 }
locked = true
pin_nets = { "1" = "SIG_N2" }

[[placements]]
refdes = "P_M2"
footprint = "PAD1"
pos = { x = 13300000, y = 9000000 }
locked = true
pin_nets = { "1" = "SIG_N2" }

[[placements]]
refdes = "P_P1"
footprint = "PAD1"
pos = { x = 5000000, y = 21000000 }
locked = true
pin_nets = { "1" = "SIG_P" }

[[placements]]
refdes = "P_P2"
footprint = "PAD1"
pos = { x = 10000000, y = 21000000 }
locked = true
pin_nets = { "1" = "SIG_P" }

[[placements]]
refdes = "P_B1"
footprint = "PAD1"
pos = { x = 4000000, y = 3000000 }
locked = true
pin_nets = { "1" = "NET_B" }

[[placements]]
refdes = "P_B2"
footprint = "PAD1"
pos = { x = 10000000, y = 3000000 }
locked = true
pin_nets = { "1" = "NET_B" }

[rules]
track_width_nm = 200000
clearance_nm = 200000
edge_clearance_nm = 300000
"#;
    let spec: BoardSpec = toml::from_str(toml).unwrap();
    let mut board = BoardIr::from_spec(&spec).unwrap();
    add_cage_tracks(&mut board, false);
    // Partner SIG_P is shorter (5 mm) than widened SIG_N1 (>= 8 mm); widening SIG_N1 requires tuning SIG_P.
    board.constraints.match_groups = vec![MatchGroupRule {
        name: "MG_N1_P".into(),
        nets: vec!["SIG_N1".into(), "SIG_P".into()],
        tolerance_nm: mm(0.5),
        target_nm: None,
    }];
    board.tracks.push(Track {
        net: "SIG_P".into(),
        layer: "F.Cu".into(),
        a: Pt::new(mm(5.0), mm(21.0)),
        b: Pt::new(mm(10.0), mm(21.0)),
        width: mm(0.2),
        locked: false,
    });
    // Unrelated violation on NET_B (connected with a dangling stub near edge) outside SIG_A scope must remain untouched.
    let net_b_tracks = vec![
        Track {
            net: "NET_B".into(),
            layer: "F.Cu".into(),
            a: Pt::new(mm(4.0), mm(3.0)),
            b: Pt::new(mm(10.0), mm(3.0)),
            width: mm(0.2),
            locked: false,
        },
        Track {
            net: "NET_B".into(),
            layer: "F.Cu".into(),
            a: Pt::new(mm(4.0), mm(3.0)),
            b: Pt::new(mm(4.0), mm(0.15)),
            width: mm(0.2),
            locked: false,
        },
    ];
    board.tracks.extend(net_b_tracks.clone());

    let rep = compile_board(board, &boxed_cfg(2, true)).unwrap();
    assert_eq!(rep.route.unconnected_pairs, 0);
    assert!((1..=3).contains(&rep.route.repair_passes_run));
    assert_eq!(
        rep.route.length_violations, 0,
        "expected SIG_P partner to be tuned: tracks={:?} drc={:?}",
        rep.board.tracks, rep.drc
    );
    let actual_net_b: Vec<_> = rep
        .board
        .tracks
        .iter()
        .filter(|t| t.net == "NET_B")
        .cloned()
        .collect();
    assert_eq!(actual_net_b, net_b_tracks);
    // Only NET_B's untouched violations remain.
    assert!(
        rep.drc
            .violations
            .iter()
            .all(|v| v.net_a == "NET_B" || v.net_b == "NET_B"),
        "expected only untouched NET_B violations, got {:?}",
        rep.drc.violations
    );
}

#[test]
fn repair_ignores_fixed_pad_and_locked_copper_violations() {
    let toml = r#"
name = "fixed_violations_ignored"
size_nm = [24000000, 20000000]

[[layers]]
name = "F.Cu"
kind = "signal"
side = "top"

[footprints.PAD1]
kind = "custom"
[[footprints.PAD1.pads]]
name = "1"
kind = "smd"
shape = { shape = "circle", diameter = 600000 }
pos = { x = 0, y = 0 }
layers = ["F.Cu"]

[[placements]]
refdes = "F1"
footprint = "PAD1"
pos = { x = 5000000, y = 10000000 }
locked = true
pin_nets = { "1" = "FIX1" }

[[placements]]
refdes = "F2"
footprint = "PAD1"
pos = { x = 5650000, y = 10000000 }
locked = true
pin_nets = { "1" = "FIX2" }

[[placements]]
refdes = "A1"
footprint = "PAD1"
pos = { x = 12000000, y = 10000000 }
locked = true
pin_nets = { "1" = "SIG_A" }

[[placements]]
refdes = "A2"
footprint = "PAD1"
pos = { x = 18000000, y = 10000000 }
locked = true
pin_nets = { "1" = "SIG_A" }

[rules]
track_width_nm = 200000
clearance_nm = 200000
edge_clearance_nm = 300000
"#;
    let spec: BoardSpec = toml::from_str(toml).unwrap();
    let mut board = BoardIr::from_spec(&spec).unwrap();
    // Add a locked dangling track too.
    board.tracks.push(Track {
        net: "FIX1".into(),
        layer: "F.Cu".into(),
        a: Pt::new(mm(5.0), mm(10.0)),
        b: Pt::new(mm(5.0), mm(12.0)),
        width: mm(0.2),
        locked: true,
    });
    let mut cfg = boxed_cfg(3, false);
    cfg.route.ignored_nets.insert("FIX1".into());
    cfg.route.ignored_nets.insert("FIX2".into());
    // Clear ignored_nets so F1/F2 pad-to-pad clearance is actually evaluated by DRC.
    cfg.route.ignored_nets.clear();
    let rep = compile_board(board, &cfg).unwrap();
    assert_eq!(rep.route.unconnected_pairs, 0);
    assert_eq!(rep.route.repair_passes_run, 0);
}

#[test]
fn repair_long_open_net_finds_pad_local_cage_neighbors() {
    let toml = r#"
name = "long_net_pad_cage"
size_nm = [32000000, 26000000]

[[layers]]
name = "F.Cu"
kind = "signal"
side = "top"

[footprints.PAD1]
kind = "custom"
[[footprints.PAD1.pads]]
name = "1"
kind = "smd"
shape = { shape = "circle", diameter = 600000 }
pos = { x = 0, y = 0 }
layers = ["F.Cu"]

[[placements]]
refdes = "P_A1"
footprint = "PAD1"
pos = { x = 4000000, y = 13000000 }
locked = true
pin_nets = { "1" = "SIG_A" }

[[placements]]
refdes = "P_A2"
footprint = "PAD1"
pos = { x = 24000000, y = 13000000 }
locked = true
pin_nets = { "1" = "SIG_A" }

[[placements]]
refdes = "P_N1"
footprint = "PAD1"
pos = { x = 22500000, y = 14500000 }
locked = true
pin_nets = { "1" = "SIG_N1" }

[[placements]]
refdes = "P_N2"
footprint = "PAD1"
pos = { x = 22500000, y = 11500000 }
locked = true
pin_nets = { "1" = "SIG_N1" }

[[placements]]
refdes = "P_M1"
footprint = "PAD1"
pos = { x = 21800000, y = 17000000 }
locked = true
pin_nets = { "1" = "SIG_N2" }

[[placements]]
refdes = "P_M2"
footprint = "PAD1"
pos = { x = 21800000, y = 9000000 }
locked = true
pin_nets = { "1" = "SIG_N2" }

[rules]
track_width_nm = 200000
clearance_nm = 200000
edge_clearance_nm = 300000
"#;
    let spec: BoardSpec = toml::from_str(toml).unwrap();
    let mut board = BoardIr::from_spec(&spec).unwrap();
    // Box P_A2 (x=24 mm) while OpenNet midpoint is at x=14 mm (10 mm away from the cage).
    for (a, b, net) in [
        (
            Pt::new(mm(22.5), mm(14.5)),
            Pt::new(mm(25.5), mm(14.5)),
            "SIG_N1",
        ),
        (
            Pt::new(mm(25.5), mm(14.5)),
            Pt::new(mm(25.5), mm(11.5)),
            "SIG_N1",
        ),
        (
            Pt::new(mm(25.5), mm(11.5)),
            Pt::new(mm(22.5), mm(11.5)),
            "SIG_N1",
        ),
        (
            Pt::new(mm(21.8), mm(17.0)),
            Pt::new(mm(21.8), mm(9.0)),
            "SIG_N2",
        ),
    ] {
        board.tracks.push(Track {
            net: net.into(),
            layer: "F.Cu".into(),
            a,
            b,
            width: mm(0.2),
            locked: false,
        });
    }
    let rep = compile_board(board, &boxed_cfg(3, true)).unwrap();
    assert!(
        rep.ok,
        "expected pad-centered widening to repair 20mm net: {:?}",
        rep.drc
    );
    assert!((1..=3).contains(&rep.route.repair_passes_run));
    assert!(rep.drc.passed);
}

#[test]
fn repair_prior_cluster_widening_does_not_contaminate_distant_hotspot() {
    let toml = r#"
name = "prior_cluster_isolation"
size_nm = [32000000, 26000000]

[[layers]]
name = "F.Cu"
kind = "signal"
side = "top"

[footprints.PAD1]
kind = "custom"
[[footprints.PAD1.pads]]
name = "1"
kind = "smd"
shape = { shape = "circle", diameter = 600000 }
pos = { x = 0, y = 0 }
layers = ["F.Cu"]

[[placements]]
refdes = "A1"
footprint = "PAD1"
pos = { x = 4000000, y = 13000000 }
locked = true
pin_nets = { "1" = "SIG_A" }

[[placements]]
refdes = "A2"
footprint = "PAD1"
pos = { x = 8000000, y = 13000000 }
locked = true
pin_nets = { "1" = "SIG_A" }

[[placements]]
refdes = "B1"
footprint = "PAD1"
pos = { x = 6000000, y = 15000000 }
locked = true
pin_nets = { "1" = "NET_B" }

[[placements]]
refdes = "B2"
footprint = "PAD1"
pos = { x = 6000000, y = 11000000 }
locked = true
pin_nets = { "1" = "NET_B" }

[[placements]]
refdes = "C1"
footprint = "PAD1"
pos = { x = 22000000, y = 13000000 }
locked = true
pin_nets = { "1" = "SIG_C" }

[[placements]]
refdes = "C2"
footprint = "PAD1"
pos = { x = 26000000, y = 13000000 }
locked = true
pin_nets = { "1" = "SIG_C" }

[rules]
track_width_nm = 200000
clearance_nm = 200000
edge_clearance_nm = 300000
"#;
    let spec: BoardSpec = toml::from_str(toml).unwrap();
    let mut board = BoardIr::from_spec(&spec).unwrap();
    // Hotspot 1 at (6, 13): locked cage around A1 so SIG_A cannot solve, plus unlocked NET_B track nearby.
    for (a, b, net, locked) in [
        (
            Pt::new(mm(3.0), mm(14.0)),
            Pt::new(mm(5.0), mm(14.0)),
            "SIG_LOCK",
            true,
        ),
        (
            Pt::new(mm(5.0), mm(14.0)),
            Pt::new(mm(5.0), mm(12.0)),
            "SIG_LOCK",
            true,
        ),
        (
            Pt::new(mm(5.0), mm(12.0)),
            Pt::new(mm(3.0), mm(12.0)),
            "SIG_LOCK",
            true,
        ),
        (
            Pt::new(mm(3.0), mm(12.0)),
            Pt::new(mm(3.0), mm(14.0)),
            "SIG_LOCK",
            true,
        ),
        (
            Pt::new(mm(6.0), mm(15.0)),
            Pt::new(mm(6.0), mm(11.0)),
            "NET_B",
            false,
        ),
        // Hotspot 2 at (24, 13): locked NET_B bar colliding with pre-routed unlocked SIG_C track.
        (
            Pt::new(mm(24.0), mm(15.0)),
            Pt::new(mm(24.0), mm(11.0)),
            "NET_B",
            true,
        ),
        (
            Pt::new(mm(22.0), mm(13.0)),
            Pt::new(mm(26.0), mm(13.0)),
            "SIG_C",
            false,
        ),
    ] {
        board.tracks.push(Track {
            net: net.into(),
            layer: "F.Cu".into(),
            a,
            b,
            width: mm(0.2),
            locked,
        });
    }
    let mut cfg = boxed_cfg(3, false);
    cfg.route.resume = true;
    cfg.route.reroute_pad_mm = Some(0.5);
    let rep = compile_board(board, &cfg).unwrap();
    assert_eq!(rep.route.repair_passes_run, 3);
    assert_eq!(rep.route.unconnected_pairs, 1);
    assert!(
        !rep.drc
            .violations
            .iter()
            .any(|v| v.net_a == "SIG_C" || v.net_b == "SIG_C"),
        "expected distant hotspot 2 (SIG_C) to be selected and repaired on pass 3: {:?}",
        rep.drc.violations
    );
}

fn max_length_corridor_board_toml(with_partner: bool) -> String {
    let mut toml = String::from(
        r#"
name = "max_length_corridor"
size_nm = [30000000, 22000000]

[[layers]]
name = "F.Cu"
kind = "signal"
side = "top"

[footprints.PAD1]
kind = "custom"
[[footprints.PAD1.pads]]
name = "1"
kind = "smd"
shape = { shape = "circle", diameter = 600000 }
pos = { x = 0, y = 0 }
layers = ["F.Cu"]

[[placements]]
refdes = "A1"
footprint = "PAD1"
pos = { x = 6000000, y = 11000000 }
locked = true
pin_nets = { "1" = "SIG_A" }

[[placements]]
refdes = "A2"
footprint = "PAD1"
pos = { x = 20000000, y = 11000000 }
locked = true
pin_nets = { "1" = "SIG_A" }

[[placements]]
refdes = "N1"
footprint = "PAD1"
pos = { x = 13000000, y = 16000000 }
locked = true
pin_nets = { "1" = "SIG_N1" }

[[placements]]
refdes = "N2"
footprint = "PAD1"
pos = { x = 13000000, y = 6000000 }
locked = true
pin_nets = { "1" = "SIG_N1" }
"#,
    );
    if with_partner {
        toml.push_str(
            r#"
[[placements]]
refdes = "P1"
footprint = "PAD1"
pos = { x = 4000000, y = 2500000 }
locked = true
pin_nets = { "1" = "SIG_P" }

[[placements]]
refdes = "P2"
footprint = "PAD1"
pos = { x = 14000000, y = 2500000 }
locked = true
pin_nets = { "1" = "SIG_P" }
"#,
        );
    }
    toml.push_str(
        r#"
[rules]
track_width_nm = 200000
clearance_nm = 200000
edge_clearance_nm = 300000

[[constraints.max_lengths]]
nets = ["SIG_A"]
max_nm = 16000000
"#,
    );
    toml
}

#[test]
fn repair_max_length_reroutes_corridor_neighbors_to_pass() {
    let spec: BoardSpec = toml::from_str(&max_length_corridor_board_toml(true)).unwrap();
    let mut board = BoardIr::from_spec(&spec).unwrap();
    board.tracks.push(Track {
        net: "SIG_N1".into(),
        layer: "F.Cu".into(),
        a: Pt::new(mm(13.0), mm(16.0)),
        b: Pt::new(mm(13.0), mm(6.0)),
        width: mm(0.2),
        locked: false,
    });
    board.tracks.push(Track {
        net: "SIG_P".into(),
        layer: "F.Cu".into(),
        a: Pt::new(mm(4.0), mm(2.5)),
        b: Pt::new(mm(14.0), mm(2.5)),
        width: mm(0.2),
        locked: false,
    });
    board.constraints.match_groups = vec![MatchGroupRule {
        name: "MG_N1_P".into(),
        nets: vec!["SIG_N1".into(), "SIG_P".into()],
        tolerance_nm: mm(0.5),
        target_nm: None,
    }];

    let mut cfg0 = boxed_cfg(0, true);
    cfg0.route.resume = true;
    cfg0.route.reroute_pad_mm = None;
    let r0 = compile_board(board.clone(), &cfg0).unwrap();
    assert!(!r0.ok);
    assert!(!r0.drc.passed);
    assert_eq!(r0.route.repair_passes_run, 0);
    assert!(
        r0.drc
            .violations
            .iter()
            .any(|v| v.kind == ViolationKind::MaxLength && v.net_a == "SIG_A"),
        "expected MaxLength violation on SIG_A when repair_passes=0: {:?}",
        r0.drc.violations
    );

    let mut cfg3 = cfg0;
    cfg3.repair_passes = 3;
    let r3 = compile_board(board, &cfg3).unwrap();
    assert!(
        r3.ok,
        "expected repair_passes=3 to clear MaxLength and tune partner: {:?}",
        r3.drc
    );
    assert!(r3.drc.passed);
    assert_eq!(r3.route.length_violations, 0);
    assert!((1..=3).contains(&r3.route.repair_passes_run));
    assert!(
        !r3.drc
            .violations
            .iter()
            .any(|v| v.kind == ViolationKind::MaxLength),
        "expected 0 MaxLength violations, got {:?}",
        r3.drc.violations
    );
}

#[test]
fn repair_max_length_strict_rollback_when_unfixable() {
    let spec: BoardSpec = toml::from_str(&max_length_corridor_board_toml(false)).unwrap();
    let mut board = BoardIr::from_spec(&spec).unwrap();
    // Locked blocker forces SIG_A to detour > 16 mm, making MaxLength unfixable.
    board.tracks.push(Track {
        net: "SIG_N1".into(),
        layer: "F.Cu".into(),
        a: Pt::new(mm(13.0), mm(16.0)),
        b: Pt::new(mm(13.0), mm(6.0)),
        width: mm(0.2),
        locked: true,
    });
    let mut cfg0 = boxed_cfg(0, true);
    cfg0.route.resume = true;
    cfg0.route.reroute_pad_mm = None;
    let r0 = compile_board(board.clone(), &cfg0).unwrap();
    assert!(!r0.ok);
    let expected_tracks = r0.board.tracks.clone();
    let expected_vias = r0.board.vias.clone();

    let mut cfg2 = cfg0;
    cfg2.repair_passes = 2;
    let rep = compile_board(board, &cfg2).unwrap();
    assert!(!rep.ok);
    assert!(!rep.drc.passed);
    assert!(rep.route.repair_passes_run >= 1);
    assert!(rep.route.repair_passes_run <= 2);
    assert_eq!(rep.board.tracks, expected_tracks);
    assert_eq!(rep.board.vias, expected_vias);
}

#[test]
fn repair_max_length_respects_exclusions_and_reroute_scope() {
    // 1. Power (power_driven + class="Power"), ignored, and locked blockers must not be ripped up.
    let excl_toml = r#"
name = "max_length_exclusions"
size_nm = [32000000, 24000000]

[[layers]]
name = "F.Cu"
kind = "signal"
side = "top"

[footprints.PAD1]
kind = "custom"
[[footprints.PAD1.pads]]
name = "1"
kind = "smd"
shape = { shape = "circle", diameter = 600000 }
pos = { x = 0, y = 0 }
layers = ["F.Cu"]

[net_classes.Power]
name = "Power"
track_width_nm = 200000
clearance_nm = 200000
via_dia_nm = 450000
via_drill_nm = 200000

[nets.PWR_DRIVEN]
power_driven = true

[nets.PWR_CLASS]
class = "Power"

[[placements]]
refdes = "A1"
footprint = "PAD1"
pos = { x = 5000000, y = 12000000 }
locked = true
pin_nets = { "1" = "SIG_A" }

[[placements]]
refdes = "A2"
footprint = "PAD1"
pos = { x = 25000000, y = 12000000 }
locked = true
pin_nets = { "1" = "SIG_A" }

[[placements]]
refdes = "PD1"
footprint = "PAD1"
pos = { x = 10000000, y = 15000000 }
locked = true
pin_nets = { "1" = "PWR_DRIVEN" }

[[placements]]
refdes = "PD2"
footprint = "PAD1"
pos = { x = 10000000, y = 9000000 }
locked = true
pin_nets = { "1" = "PWR_DRIVEN" }

[[placements]]
refdes = "PC1"
footprint = "PAD1"
pos = { x = 14000000, y = 15000000 }
locked = true
pin_nets = { "1" = "PWR_CLASS" }

[[placements]]
refdes = "PC2"
footprint = "PAD1"
pos = { x = 14000000, y = 9000000 }
locked = true
pin_nets = { "1" = "PWR_CLASS" }

[[placements]]
refdes = "IG1"
footprint = "PAD1"
pos = { x = 18000000, y = 15000000 }
locked = true
pin_nets = { "1" = "IGN_NET" }

[[placements]]
refdes = "IG2"
footprint = "PAD1"
pos = { x = 18000000, y = 9000000 }
locked = true
pin_nets = { "1" = "IGN_NET" }

[[placements]]
refdes = "LK1"
footprint = "PAD1"
pos = { x = 22000000, y = 15000000 }
locked = true
pin_nets = { "1" = "LOCK_NET" }

[[placements]]
refdes = "LK2"
footprint = "PAD1"
pos = { x = 22000000, y = 9000000 }
locked = true
pin_nets = { "1" = "LOCK_NET" }

[rules]
track_width_nm = 200000
clearance_nm = 200000
edge_clearance_nm = 300000

[[constraints.max_lengths]]
nets = ["SIG_A"]
max_nm = 21000000
"#;
    let spec: BoardSpec = toml::from_str(excl_toml).unwrap();
    let mut board = BoardIr::from_spec(&spec).unwrap();
    let protected = vec![
        ("PWR_DRIVEN", 10.0, false),
        ("PWR_CLASS", 14.0, false),
        ("IGN_NET", 18.0, false),
        ("LOCK_NET", 22.0, true),
    ];
    for &(net, x_mm, locked) in &protected {
        board.tracks.push(Track {
            net: net.into(),
            layer: "F.Cu".into(),
            a: Pt::new(mm(x_mm), mm(15.0)),
            b: Pt::new(mm(x_mm), mm(9.0)),
            width: mm(0.2),
            locked,
        });
    }
    let orig_protected: Vec<Track> = board.tracks.clone();
    let mut cfg = boxed_cfg(3, true);
    cfg.route.resume = true;
    cfg.route.reroute_pad_mm = None;
    cfg.route.ignored_nets.insert("IGN_NET".into());
    let rep = compile_board(board, &cfg).unwrap();
    assert!(!rep.ok);
    assert_eq!(rep.route.repair_passes_run, 1);
    for expected in &orig_protected {
        let actual: Vec<&Track> = rep
            .board
            .tracks
            .iter()
            .filter(|t| t.net == expected.net)
            .collect();
        assert_eq!(
            actual,
            vec![expected],
            "protected blocker {} must not be ripped up",
            expected.net
        );
    }

    // 2. --reroute-nets scope must ignore out-of-scope over-length nets and repair in-scope ones.
    let scope_toml = r#"
name = "max_length_scope"
size_nm = [30000000, 22000000]

[[layers]]
name = "F.Cu"
kind = "signal"
side = "top"

[footprints.PAD1]
kind = "custom"
[[footprints.PAD1.pads]]
name = "1"
kind = "smd"
shape = { shape = "circle", diameter = 600000 }
pos = { x = 0, y = 0 }
layers = ["F.Cu"]

[[placements]]
refdes = "A1"
footprint = "PAD1"
pos = { x = 4000000, y = 18000000 }
locked = true
pin_nets = { "1" = "SIG_A" }

[[placements]]
refdes = "A2"
footprint = "PAD1"
pos = { x = 10000000, y = 18000000 }
locked = true
pin_nets = { "1" = "SIG_A" }

[[placements]]
refdes = "B1"
footprint = "PAD1"
pos = { x = 6000000, y = 10000000 }
locked = true
pin_nets = { "1" = "SIG_B" }

[[placements]]
refdes = "B2"
footprint = "PAD1"
pos = { x = 20000000, y = 10000000 }
locked = true
pin_nets = { "1" = "SIG_B" }

[[placements]]
refdes = "N1"
footprint = "PAD1"
pos = { x = 13000000, y = 15000000 }
locked = true
pin_nets = { "1" = "SIG_N1" }

[[placements]]
refdes = "N2"
footprint = "PAD1"
pos = { x = 13000000, y = 5000000 }
locked = true
pin_nets = { "1" = "SIG_N1" }

[rules]
track_width_nm = 200000
clearance_nm = 200000
edge_clearance_nm = 300000

[[constraints.max_lengths]]
nets = ["SIG_B"]
max_nm = 16000000
"#;
    let s_spec: BoardSpec = toml::from_str(scope_toml).unwrap();
    let mut s_board = BoardIr::from_spec(&s_spec).unwrap();
    for (net, a, b) in [
        (
            "SIG_A",
            Pt::new(mm(4.0), mm(18.0)),
            Pt::new(mm(10.0), mm(18.0)),
        ),
        (
            "SIG_N1",
            Pt::new(mm(13.0), mm(15.0)),
            Pt::new(mm(13.0), mm(5.0)),
        ),
        (
            "SIG_B",
            Pt::new(mm(6.0), mm(10.0)),
            Pt::new(mm(13.0), mm(16.0)),
        ),
        (
            "SIG_B",
            Pt::new(mm(13.0), mm(16.0)),
            Pt::new(mm(20.0), mm(10.0)),
        ),
    ] {
        s_board.tracks.push(Track {
            net: net.into(),
            layer: "F.Cu".into(),
            a,
            b,
            width: mm(0.2),
            locked: false,
        });
    }
    let expected_sig_b: Vec<Track> = s_board
        .tracks
        .iter()
        .filter(|t| t.net == "SIG_B")
        .cloned()
        .collect();
    let mut out_scope_cfg = boxed_cfg(3, true);
    out_scope_cfg.route.resume = true;
    out_scope_cfg.route.reroute_pad_mm = None;
    let out_rep = compile_board(s_board.clone(), &out_scope_cfg).unwrap();
    assert!(!out_rep.ok);
    assert_eq!(out_rep.route.repair_passes_run, 0);
    assert_eq!(
        out_rep
            .board
            .tracks
            .iter()
            .filter(|t| t.net == "SIG_B")
            .cloned()
            .collect::<Vec<_>>(),
        expected_sig_b
    );
    assert!(
        out_rep
            .drc
            .violations
            .iter()
            .any(|v| v.kind == ViolationKind::MaxLength && v.net_a == "SIG_B")
    );

    let mut in_scope_cfg = out_scope_cfg;
    in_scope_cfg.route.reroute_nets = Some(HashSet::from(["SIG_B".into()]));
    let in_rep = compile_board(s_board, &in_scope_cfg).unwrap();
    assert!(
        in_rep.ok,
        "expected in-scope SIG_B to repair: {:?}",
        in_rep.drc
    );
    assert!(in_rep.drc.passed);
    assert!(in_rep.route.repair_passes_run >= 1);
}

#[test]
fn repair_full_board_fixes_geom_then_max_length_in_one_build() {
    let toml = r#"
name = "geom_then_length_full"
size_nm = [30000000, 26000000]

[[layers]]
name = "F.Cu"
kind = "signal"
side = "top"

[footprints.PAD1]
kind = "custom"
[[footprints.PAD1.pads]]
name = "1"
kind = "smd"
shape = { shape = "circle", diameter = 600000 }
pos = { x = 0, y = 0 }
layers = ["F.Cu"]

[[placements]]
refdes = "A1"
footprint = "PAD1"
pos = { x = 10000000, y = 13000000 }
locked = true
pin_nets = { "1" = "SIG_A" }

[[placements]]
refdes = "A2"
footprint = "PAD1"
pos = { x = 16000000, y = 13000000 }
locked = true
pin_nets = { "1" = "SIG_A" }

[[placements]]
refdes = "P_N1"
footprint = "PAD1"
pos = { x = 14000000, y = 14500000 }
locked = true
pin_nets = { "1" = "SIG_N1" }

[[placements]]
refdes = "P_N2"
footprint = "PAD1"
pos = { x = 14000000, y = 11500000 }
locked = true
pin_nets = { "1" = "SIG_N1" }

[[placements]]
refdes = "P_M1"
footprint = "PAD1"
pos = { x = 13300000, y = 17000000 }
locked = true
pin_nets = { "1" = "SIG_N2" }

[[placements]]
refdes = "P_M2"
footprint = "PAD1"
pos = { x = 13300000, y = 9000000 }
locked = true
pin_nets = { "1" = "SIG_N2" }

[[placements]]
refdes = "L1"
footprint = "PAD1"
pos = { x = 4000000, y = 21000000 }
locked = true
pin_nets = { "1" = "SIG_L" }

[[placements]]
refdes = "L2"
footprint = "PAD1"
pos = { x = 18000000, y = 21000000 }
locked = true
pin_nets = { "1" = "SIG_L" }

[[placements]]
refdes = "LN1"
footprint = "PAD1"
pos = { x = 11000000, y = 23500000 }
locked = true
pin_nets = { "1" = "SIG_LN1" }

[[placements]]
refdes = "LN2"
footprint = "PAD1"
pos = { x = 11000000, y = 18500000 }
locked = true
pin_nets = { "1" = "SIG_LN1" }

[rules]
track_width_nm = 200000
clearance_nm = 200000
edge_clearance_nm = 300000

[[constraints.max_lengths]]
nets = ["SIG_L"]
max_nm = 15500000
"#;
    let spec: BoardSpec = toml::from_str(toml).unwrap();
    let mut board = BoardIr::from_spec(&spec).unwrap();
    add_cage_tracks(&mut board, false);
    for (net, a, b) in [
        (
            "SIG_LN1",
            Pt::new(mm(11.0), mm(23.5)),
            Pt::new(mm(11.0), mm(18.5)),
        ),
        (
            "SIG_L",
            Pt::new(mm(4.0), mm(21.0)),
            Pt::new(mm(11.0), mm(24.8)),
        ),
        (
            "SIG_L",
            Pt::new(mm(11.0), mm(24.8)),
            Pt::new(mm(18.0), mm(21.0)),
        ),
    ] {
        board.tracks.push(Track {
            net: net.into(),
            layer: "F.Cu".into(),
            a,
            b,
            width: mm(0.2),
            locked: false,
        });
    }

    let mut cfg0 = boxed_cfg(0, false);
    cfg0.route.resume = true;
    cfg0.route.max_passes = 0;
    let r0 = compile_board(board.clone(), &cfg0).unwrap();
    assert!(!r0.ok);
    assert!(
        r0.drc
            .violations
            .iter()
            .any(|v| v.kind == ViolationKind::OpenNet && v.net_a == "SIG_A")
    );
    assert!(
        r0.drc
            .violations
            .iter()
            .any(|v| v.kind == ViolationKind::MaxLength && v.net_a == "SIG_L")
    );

    let mut cfg5 = cfg0;
    cfg5.repair_passes = 5;
    let r5 = compile_board(board, &cfg5).unwrap();
    assert!(
        r5.ok,
        "expected full-board geom+length repair to pass: {:?}",
        r5.drc
    );
    assert!(r5.drc.passed);
    assert_eq!(r5.route.unconnected_pairs, 0);
    assert_eq!(r5.route.length_violations, 0);
}

#[test]
fn repair_scoped_max_length_runs_with_out_of_scope_geom_and_constrained_neighbors() {
    let toml = r#"
name = "scoped_length_with_out_of_scope_geom"
size_nm = [30000000, 24000000]

[[layers]]
name = "F.Cu"
kind = "signal"
side = "top"

[footprints.PAD1]
kind = "custom"
[[footprints.PAD1.pads]]
name = "1"
kind = "smd"
shape = { shape = "circle", diameter = 600000 }
pos = { x = 0, y = 0 }
layers = ["F.Cu"]

[[placements]]
refdes = "B1"
footprint = "PAD1"
pos = { x = 6000000, y = 12000000 }
locked = true
pin_nets = { "1" = "SIG_B" }

[[placements]]
refdes = "B2"
footprint = "PAD1"
pos = { x = 20000000, y = 12000000 }
locked = true
pin_nets = { "1" = "SIG_B" }

[[placements]]
refdes = "N1"
footprint = "PAD1"
pos = { x = 13000000, y = 17000000 }
locked = true
pin_nets = { "1" = "SIG_N1" }

[[placements]]
refdes = "N2"
footprint = "PAD1"
pos = { x = 13000000, y = 7000000 }
locked = true
pin_nets = { "1" = "SIG_N1" }

[[placements]]
refdes = "C1"
footprint = "PAD1"
pos = { x = 4000000, y = 3000000 }
locked = true
pin_nets = { "1" = "NET_C" }

[[placements]]
refdes = "C2"
footprint = "PAD1"
pos = { x = 10000000, y = 3000000 }
locked = true
pin_nets = { "1" = "NET_C" }

[rules]
track_width_nm = 200000
clearance_nm = 200000
edge_clearance_nm = 300000

[[constraints.max_lengths]]
nets = ["SIG_B"]
max_nm = 16000000
"#;
    let spec: BoardSpec = toml::from_str(toml).unwrap();
    let mut board = BoardIr::from_spec(&spec).unwrap();
    let net_c_tracks = vec![
        Track {
            net: "NET_C".into(),
            layer: "F.Cu".into(),
            a: Pt::new(mm(4.0), mm(3.0)),
            b: Pt::new(mm(10.0), mm(3.0)),
            width: mm(0.2),
            locked: false,
        },
        Track {
            net: "NET_C".into(),
            layer: "F.Cu".into(),
            a: Pt::new(mm(4.0), mm(3.0)),
            b: Pt::new(mm(4.0), mm(0.15)),
            width: mm(0.2),
            locked: false,
        },
    ];
    board.tracks.extend(net_c_tracks.clone());
    for (net, a, b) in [
        (
            "SIG_N1",
            Pt::new(mm(13.0), mm(17.0)),
            Pt::new(mm(13.0), mm(7.0)),
        ),
        (
            "SIG_B",
            Pt::new(mm(6.0), mm(12.0)),
            Pt::new(mm(13.0), mm(18.0)),
        ),
        (
            "SIG_B",
            Pt::new(mm(13.0), mm(18.0)),
            Pt::new(mm(20.0), mm(12.0)),
        ),
    ] {
        board.tracks.push(Track {
            net: net.into(),
            layer: "F.Cu".into(),
            a,
            b,
            width: mm(0.2),
            locked: false,
        });
    }

    let mut cfg = boxed_cfg(3, false);
    cfg.route.resume = true;
    cfg.route.reroute_pad_mm = None;
    cfg.route.reroute_nets = Some(HashSet::from(["SIG_B".into()]));

    let rep = compile_board(board.clone(), &cfg).unwrap();
    assert!(!rep.ok);
    assert!(
        !rep.drc
            .violations
            .iter()
            .any(|v| v.kind == ViolationKind::MaxLength),
        "expected SIG_B MaxLength to be repaired despite out-of-scope NET_C geom violation: {:?}",
        rep.drc.violations
    );
    let actual_net_c: Vec<Track> = rep
        .board
        .tracks
        .iter()
        .filter(|t| t.net == "NET_C")
        .cloned()
        .collect();
    assert_eq!(actual_net_c, net_c_tracks);
    assert!(
        rep.drc
            .violations
            .iter()
            .all(|v| v.net_a == "NET_C" || v.net_b == "NET_C")
    );

    // Out-of-scope max_lengths-constrained blocker SIG_N1 must not be ripped up under scoped repair.
    let expected_sig_n1: Vec<Track> = board
        .tracks
        .iter()
        .filter(|t| t.net == "SIG_N1")
        .cloned()
        .collect();
    board.constraints.max_lengths.push(MaxLengthRule {
        nets: vec!["SIG_N1".into()],
        max_nm: mm(11.0),
    });
    let guarded = compile_board(board, &cfg).unwrap();
    assert_eq!(
        guarded
            .board
            .tracks
            .iter()
            .filter(|t| t.net == "SIG_N1")
            .cloned()
            .collect::<Vec<_>>(),
        expected_sig_n1
    );
    assert!(
        !guarded
            .drc
            .violations
            .iter()
            .any(|v| v.kind == ViolationKind::MaxLength && v.net_a == "SIG_N1")
    );
}

#[test]
fn repair_max_length_handles_constrained_net_with_unfilled_pour() {
    let toml = r#"
name = "max_length_with_pour"
size_nm = [30000000, 22000000]

[[layers]]
name = "F.Cu"
kind = "signal"
side = "top"

[[layers]]
name = "B.Cu"
kind = "plane"
side = "bottom"

[footprints.PAD1]
kind = "custom"
[[footprints.PAD1.pads]]
name = "1"
kind = "smd"
shape = { shape = "circle", diameter = 600000 }
pos = { x = 0, y = 0 }
layers = ["F.Cu"]

[[placements]]
refdes = "A1"
footprint = "PAD1"
pos = { x = 6000000, y = 11000000 }
locked = true
pin_nets = { "1" = "SIG_A" }

[[placements]]
refdes = "A2"
footprint = "PAD1"
pos = { x = 20000000, y = 11000000 }
locked = true
pin_nets = { "1" = "SIG_A" }

[[placements]]
refdes = "N1"
footprint = "PAD1"
pos = { x = 13000000, y = 16000000 }
locked = true
pin_nets = { "1" = "SIG_N1" }

[[placements]]
refdes = "N2"
footprint = "PAD1"
pos = { x = 13000000, y = 6000000 }
locked = true
pin_nets = { "1" = "SIG_N1" }

[[pours]]
name = "SIG_A_BOT"
net = "SIG_A"
layer = "B.Cu"
outline = [
  { x = 500000, y = 500000 },
  { x = 29500000, y = 500000 },
  { x = 29500000, y = 21500000 },
  { x = 500000, y = 21500000 },
]
priority = 0
min_width_nm = 200000
thermal_relief = false
remove_islands = true

[rules]
track_width_nm = 200000
clearance_nm = 200000
edge_clearance_nm = 300000
via_dia_nm = 450000
via_drill_nm = 200000
min_annular_ring_nm = 100000
hole_to_hole_nm = 200000

[[constraints.max_lengths]]
nets = ["SIG_A"]
max_nm = 16000000
"#;
    let spec: BoardSpec = toml::from_str(toml).unwrap();
    let mut board = BoardIr::from_spec(&spec).unwrap();
    for (net, a, b) in [
        (
            "SIG_N1",
            Pt::new(mm(13.0), mm(16.0)),
            Pt::new(mm(13.0), mm(6.0)),
        ),
        (
            "SIG_A",
            Pt::new(mm(6.0), mm(11.0)),
            Pt::new(mm(13.0), mm(17.0)),
        ),
        (
            "SIG_A",
            Pt::new(mm(13.0), mm(17.0)),
            Pt::new(mm(20.0), mm(11.0)),
        ),
    ] {
        board.tracks.push(Track {
            net: net.into(),
            layer: "F.Cu".into(),
            a,
            b,
            width: mm(0.2),
            locked: false,
        });
    }

    let mut cfg0 = boxed_cfg(0, true);
    cfg0.route.resume = true;
    cfg0.route.reroute_pad_mm = None;
    let r0 = compile_board(board.clone(), &cfg0).unwrap();
    assert!(!r0.ok);
    assert!(
        r0.drc
            .violations
            .iter()
            .any(|v| v.kind == ViolationKind::MaxLength && v.net_a == "SIG_A")
    );

    let mut cfg3 = cfg0;
    cfg3.repair_passes = 3;
    let r3 = compile_board(board, &cfg3).unwrap();
    assert!(
        r3.ok,
        "expected MaxLength on net with PourSpec to be repaired: {:?}",
        r3.drc
    );
    assert!(r3.drc.passed);
    assert_eq!(r3.route.length_violations, 0);
}

#[test]
fn repair_max_length_finds_corridor_neighbor_on_pad_layer_only() {
    let toml = r#"
name = "pad_layer_corridor_blocker"
size_nm = [30000000, 22000000]

[[layers]]
name = "F.Cu"
kind = "signal"
side = "top"

[[layers]]
name = "B.Cu"
kind = "signal"
side = "bottom"

[footprints.PAD1]
kind = "custom"
[[footprints.PAD1.pads]]
name = "1"
kind = "smd"
shape = { shape = "circle", diameter = 600000 }
pos = { x = 0, y = 0 }
layers = ["F.Cu"]

[footprints.PAD_BOT]
kind = "custom"
[[footprints.PAD_BOT.pads]]
name = "1"
kind = "smd"
shape = { shape = "circle", diameter = 600000 }
pos = { x = 0, y = 0 }
layers = ["B.Cu"]

[[placements]]
refdes = "A1"
footprint = "PAD1"
pos = { x = 6000000, y = 11000000 }
locked = true
pin_nets = { "1" = "SIG_A" }

[[placements]]
refdes = "A2"
footprint = "PAD1"
pos = { x = 20000000, y = 11000000 }
locked = true
pin_nets = { "1" = "SIG_A" }

[[placements]]
refdes = "N1"
footprint = "PAD1"
pos = { x = 13000000, y = 16000000 }
locked = true
pin_nets = { "1" = "SIG_N1" }

[[placements]]
refdes = "N2"
footprint = "PAD1"
pos = { x = 13000000, y = 6000000 }
locked = true
pin_nets = { "1" = "SIG_N1" }

[[placements]]
refdes = "L1"
footprint = "PAD_BOT"
pos = { x = 13000000, y = 17500000 }
locked = true
pin_nets = { "1" = "SIG_LOCK" }

[[placements]]
refdes = "L2"
footprint = "PAD_BOT"
pos = { x = 13000000, y = 4500000 }
locked = true
pin_nets = { "1" = "SIG_LOCK" }

[rules]
track_width_nm = 200000
clearance_nm = 200000
edge_clearance_nm = 300000
via_dia_nm = 450000
via_drill_nm = 200000
min_annular_ring_nm = 100000
hole_to_hole_nm = 200000
via_at_smd = true

[[constraints.max_lengths]]
nets = ["SIG_A"]
max_nm = 16000000
"#;
    let spec: BoardSpec = toml::from_str(toml).unwrap();
    let mut board = BoardIr::from_spec(&spec).unwrap();
    // SIG_A drops vias immediately at its F.Cu pads and detours on B.Cu around locked SIG_LOCK.
    for x in [mm(6.0), mm(20.0)] {
        board.vias.push(Via {
            net: "SIG_A".into(),
            pos: Pt::new(x, mm(11.0)),
            diameter: mm(0.45),
            drill: mm(0.2),
            layers: ("F.Cu".into(), "B.Cu".into()),
            locked: false,
            tented: true,
        });
    }
    for (net, layer, a, b, locked) in [
        (
            "SIG_A",
            "B.Cu",
            Pt::new(mm(6.0), mm(11.0)),
            Pt::new(mm(13.0), mm(18.5)),
            false,
        ),
        (
            "SIG_A",
            "B.Cu",
            Pt::new(mm(13.0), mm(18.5)),
            Pt::new(mm(20.0), mm(11.0)),
            false,
        ),
        (
            "SIG_LOCK",
            "B.Cu",
            Pt::new(mm(13.0), mm(17.5)),
            Pt::new(mm(13.0), mm(4.5)),
            true,
        ),
        (
            "SIG_N1",
            "F.Cu",
            Pt::new(mm(13.0), mm(16.0)),
            Pt::new(mm(13.0), mm(6.0)),
            false,
        ),
    ] {
        board.tracks.push(Track {
            net: net.into(),
            layer: layer.into(),
            a,
            b,
            width: mm(0.2),
            locked,
        });
    }

    let mut cfg0 = boxed_cfg(0, true);
    cfg0.route.resume = true;
    cfg0.route.reroute_pad_mm = None;
    let r0 = compile_board(board.clone(), &cfg0).unwrap();
    assert!(!r0.ok);
    assert!(
        r0.drc
            .violations
            .iter()
            .any(|v| v.kind == ViolationKind::MaxLength && v.net_a == "SIG_A")
    );

    let mut cfg3 = cfg0;
    cfg3.repair_passes = 3;
    let r3 = compile_board(board, &cfg3).unwrap();
    assert!(
        r3.ok,
        "expected pad-layer corridor neighbor SIG_N1 to be discovered and rerouted: {:?}",
        r3.drc
    );
    assert!(r3.drc.passed);
    assert_eq!(r3.route.length_violations, 0);
}

#[test]
fn repair_max_length_batch_pass_fixes_multiple_over_length_nets() {
    let toml = r#"
name = "multi_net_max_length_batch"
size_nm = [26000000, 22000000]

[[layers]]
name = "F.Cu"
kind = "signal"
side = "top"

[footprints.PAD1]
kind = "custom"
[[footprints.PAD1.pads]]
name = "1"
kind = "smd"
shape = { shape = "circle", diameter = 600000 }
pos = { x = 0, y = 0 }
layers = ["F.Cu"]

[[placements]]
refdes = "A1"
footprint = "PAD1"
pos = { x = 4000000, y = 15000000 }
locked = true
pin_nets = { "1" = "SIG_A" }

[[placements]]
refdes = "A2"
footprint = "PAD1"
pos = { x = 18000000, y = 15000000 }
locked = true
pin_nets = { "1" = "SIG_A" }

[[placements]]
refdes = "B1"
footprint = "PAD1"
pos = { x = 4000000, y = 7000000 }
locked = true
pin_nets = { "1" = "SIG_B" }

[[placements]]
refdes = "B2"
footprint = "PAD1"
pos = { x = 18000000, y = 7000000 }
locked = true
pin_nets = { "1" = "SIG_B" }

[rules]
track_width_nm = 200000
clearance_nm = 200000
edge_clearance_nm = 300000

[[constraints.max_lengths]]
nets = ["SIG_A", "SIG_B"]
max_nm = 15000000
"#;
    let spec: BoardSpec = toml::from_str(toml).unwrap();
    let mut board = BoardIr::from_spec(&spec).unwrap();
    for (net, a, b) in [
        (
            "SIG_A",
            Pt::new(mm(4.0), mm(15.0)),
            Pt::new(mm(11.0), mm(20.0)),
        ),
        (
            "SIG_A",
            Pt::new(mm(11.0), mm(20.0)),
            Pt::new(mm(18.0), mm(15.0)),
        ),
        (
            "SIG_B",
            Pt::new(mm(4.0), mm(7.0)),
            Pt::new(mm(11.0), mm(2.0)),
        ),
        (
            "SIG_B",
            Pt::new(mm(11.0), mm(2.0)),
            Pt::new(mm(18.0), mm(7.0)),
        ),
    ] {
        board.tracks.push(Track {
            net: net.into(),
            layer: "F.Cu".into(),
            a,
            b,
            width: mm(0.2),
            locked: false,
        });
    }

    let mut cfg0 = boxed_cfg(0, false);
    cfg0.route.resume = true;
    cfg0.route.optimize = false;
    cfg0.route.max_passes = 0;
    cfg0.route.reroute_pad_mm = None;
    let r0 = compile_board(board.clone(), &cfg0).unwrap();
    assert!(!r0.ok);
    assert_eq!(r0.route.length_violations, 2);

    let mut cfg1 = cfg0;
    cfg1.repair_passes = 1;
    let r1 = compile_board(board, &cfg1).unwrap();
    assert!(
        r1.ok,
        "expected batch pass to fix both SIG_A and SIG_B in 1 pass: {:?}",
        r1.drc
    );
    assert!(r1.drc.passed);
    assert_eq!(r1.route.length_violations, 0);
    assert_eq!(r1.route.repair_passes_run, 1);
}

fn two_layer_pad_board(
    name: &str,
    size_mm: (f64, f64),
    pads: &[(&str, &str, f64, f64, &str)],
    extra_toml: &str,
) -> BoardIr {
    let mut toml = format!(
        r#"name = "{name}"
size_nm = [{}, {}]
[[layers]]
name = "F.Cu"
kind = "signal"
side = "top"
[[layers]]
name = "B.Cu"
kind = "signal"
side = "bottom"
[footprints.PAD_TOP]
kind = "custom"
[[footprints.PAD_TOP.pads]]
name = "1"
kind = "smd"
shape = {{ shape = "circle", diameter = 600000 }}
pos = {{ x = 0, y = 0 }}
layers = ["F.Cu"]
[footprints.PAD_BOT]
kind = "custom"
[[footprints.PAD_BOT.pads]]
name = "1"
kind = "smd"
shape = {{ shape = "circle", diameter = 600000 }}
pos = {{ x = 0, y = 0 }}
layers = ["B.Cu"]
[rules]
track_width_nm = 200000
clearance_nm = 200000
edge_clearance_nm = 300000
via_dia_nm = 450000
via_drill_nm = 200000
min_annular_ring_nm = 100000
hole_to_hole_nm = 200000
"#,
        mm(size_mm.0),
        mm(size_mm.1),
    );
    for &(refdes, fp, x, y, net) in pads {
        toml.push_str(&format!(
            "[[placements]]\nrefdes = \"{refdes}\"\nfootprint = \"{fp}\"\npos = {{ x = {}, y = {} }}\nlocked = true\npin_nets = {{ \"1\" = \"{net}\" }}\n",
            mm(x),
            mm(y)
        ));
    }
    toml.push_str(extra_toml);
    let spec: BoardSpec = toml::from_str(&toml).unwrap();
    BoardIr::from_spec(&spec).unwrap()
}

type SegSpec<'a> = (&'a str, &'a str, (f64, f64), (f64, f64), bool);

fn push_tracks(board: &mut BoardIr, segs: &[SegSpec<'_>]) {
    for &(net, layer, a, b, locked) in segs {
        board.tracks.push(Track {
            net: net.into(),
            layer: layer.into(),
            a: Pt::new(mm(a.0), mm(a.1)),
            b: Pt::new(mm(b.0), mm(b.1)),
            width: mm(0.2),
            locked,
        });
    }
}

fn push_vias(board: &mut BoardIr, vias: &[(&str, (f64, f64), bool)]) {
    for &(net, pos, locked) in vias {
        board.vias.push(Via {
            net: net.into(),
            pos: Pt::new(mm(pos.0), mm(pos.1)),
            diameter: mm(0.45),
            drill: mm(0.2),
            layers: ("F.Cu".into(), "B.Cu".into()),
            locked,
            tented: true,
        });
    }
}

#[test]
fn repair_max_vias_reroutes_blocker_to_pass() {
    let mut board = two_layer_pad_board(
        "max_vias_repair",
        (30.0, 22.0),
        &[
            ("A1", "PAD_TOP", 6.0, 11.0, "SIG_A"),
            ("A2", "PAD_TOP", 20.0, 11.0, "SIG_A"),
            ("N1", "PAD_TOP", 10.0, 14.0, "SIG_N1"),
            ("N2", "PAD_TOP", 16.0, 14.0, "SIG_N1"),
        ],
        "[[constraints.max_vias]]\nnets = [\"SIG_A\"]\nmax_vias = 0\n",
    );
    // Unlocked U-shaped detour on F.Cu blocks SIG_A on F.Cu, forcing initial SIG_A routing onto B.Cu with 2 vias.
    push_tracks(
        &mut board,
        &[
            ("SIG_N1", "F.Cu", (10.0, 14.0), (10.0, 10.0), false),
            ("SIG_N1", "F.Cu", (10.0, 10.0), (16.0, 10.0), false),
            ("SIG_N1", "F.Cu", (16.0, 10.0), (16.0, 14.0), false),
            ("SIG_A", "F.Cu", (6.0, 11.0), (8.0, 11.0), false),
            ("SIG_A", "B.Cu", (8.0, 11.0), (18.0, 11.0), false),
            ("SIG_A", "F.Cu", (18.0, 11.0), (20.0, 11.0), false),
        ],
    );
    push_vias(
        &mut board,
        &[
            ("SIG_A", (8.0, 11.0), false),
            ("SIG_A", (18.0, 11.0), false),
        ],
    );

    let mut cfg0 = boxed_cfg(0, true);
    cfg0.route.resume = true;
    cfg0.route.reroute_pad_mm = None;
    let r0 = compile_board(board.clone(), &cfg0).unwrap();
    assert!(!r0.ok);
    assert!(!r0.drc.passed);
    assert_eq!(r0.route.repair_passes_run, 0);
    assert!(
        r0.drc
            .violations
            .iter()
            .any(|v| v.kind == ViolationKind::MaxVias && v.net_a == "SIG_A"),
        "expected MaxVias violation with repair_passes=0: {:?}",
        r0.drc.violations
    );

    let mut cfg3 = cfg0;
    cfg3.repair_passes = 3;
    let r3 = compile_board(board, &cfg3).unwrap();
    assert!(
        r3.ok,
        "expected repair_passes=3 to eliminate MaxVias: {:?}",
        r3.drc
    );
    assert!(r3.drc.passed);
    assert!(r3.route.repair_passes_run >= 1);
    assert!(
        !r3.drc
            .violations
            .iter()
            .any(|v| v.kind == ViolationKind::MaxVias),
        "expected 0 MaxVias violations, got {:?}",
        r3.drc.violations
    );
}

#[test]
fn repair_max_vias_fresh_build_fixes_crossing_net() {
    // Longer-span max_vias=0 net SIG_A (14 mm) crosses shorter-span unconstrained SIG_N1 (8 mm) on F.Cu.
    let board = two_layer_pad_board(
        "max_vias_fresh_crossing",
        (30.0, 22.0),
        &[
            ("A1", "PAD_TOP", 6.0, 11.0, "SIG_A"),
            ("A2", "PAD_TOP", 20.0, 11.0, "SIG_A"),
            ("N1", "PAD_TOP", 13.0, 15.0, "SIG_N1"),
            ("N2", "PAD_TOP", 13.0, 7.0, "SIG_N1"),
        ],
        "[[constraints.max_vias]]\nnets = [\"SIG_A\"]\nmax_vias = 0\n",
    );
    let cfg0 = boxed_cfg(0, false);
    let r0 = compile_board(board.clone(), &cfg0).unwrap();
    assert!(!r0.ok);
    assert!(
        r0.drc
            .violations
            .iter()
            .any(|v| v.kind == ViolationKind::MaxVias && v.net_a == "SIG_A"),
        "expected MaxVias on SIG_A in fresh build with repair_passes=0: {:?}",
        r0.drc.violations
    );

    let cfg1 = boxed_cfg(1, false);
    let r1 = compile_board(board, &cfg1).unwrap();
    assert!(
        r1.ok,
        "expected prioritized MaxVias repair on fresh build to pass: {:?}",
        r1.drc
    );
    assert!(r1.drc.passed);
    assert_eq!(r1.route.repair_passes_run, 1);
    assert!(
        !r1.drc
            .violations
            .iter()
            .any(|v| v.kind == ViolationKind::MaxVias)
    );
    assert_eq!(r1.board.vias.iter().filter(|v| v.net == "SIG_A").count(), 0);
}

#[test]
fn repair_max_vias_rollback_no_trade_and_reroute_scope() {
    // 1. Strict rollback when max_vias = 0 between F.Cu and B.Cu pads is physically unfixable.
    let u_board = two_layer_pad_board(
        "max_vias_unfixable",
        (26.0, 16.0),
        &[
            ("A1", "PAD_TOP", 6.0, 8.0, "SIG_A"),
            ("A2", "PAD_BOT", 20.0, 8.0, "SIG_A"),
        ],
        "[[constraints.max_vias]]\nnets = [\"SIG_A\"]\nmax_vias = 0\n",
    );
    let mut cfg0 = boxed_cfg(0, true);
    cfg0.route.reroute_pad_mm = None;
    let r0 = compile_board(u_board.clone(), &cfg0).unwrap();
    assert!(!r0.ok);
    assert!(
        r0.drc
            .violations
            .iter()
            .any(|v| v.kind == ViolationKind::MaxVias && v.net_a == "SIG_A")
    );

    let mut cfg2 = cfg0;
    cfg2.repair_passes = 2;
    let r2 = compile_board(u_board, &cfg2).unwrap();
    assert!(!r2.ok);
    assert_eq!(r2.route.repair_passes_run, 1);
    assert_eq!(r2.board.tracks, r0.board.tracks);
    assert_eq!(r2.board.vias, r0.board.vias);

    // 2. Repair must not trade a MaxLength violation for a new MaxVias violation.
    let mut t_board = two_layer_pad_board(
        "max_vias_no_trade",
        (30.0, 22.0),
        &[
            ("A1", "PAD_TOP", 6.0, 11.0, "SIG_A"),
            ("A2", "PAD_TOP", 20.0, 11.0, "SIG_A"),
            ("L1", "PAD_TOP", 13.0, 16.0, "SIG_LOCK"),
            ("L2", "PAD_TOP", 13.0, 6.0, "SIG_LOCK"),
        ],
        "[[constraints.max_lengths]]\nnets = [\"SIG_A\"]\nmax_nm = 16000000\n[[constraints.max_vias]]\nnets = [\"SIG_A\"]\nmax_vias = 0\n",
    );
    push_tracks(
        &mut t_board,
        &[
            ("SIG_LOCK", "F.Cu", (13.0, 16.0), (13.0, 6.0), true),
            ("SIG_A", "F.Cu", (6.0, 11.0), (13.0, 18.0), false),
            ("SIG_A", "F.Cu", (13.0, 18.0), (20.0, 11.0), false),
        ],
    );
    let mut t_cfg = boxed_cfg(2, false);
    t_cfg.route.resume = true;
    t_cfg.route.optimize = false;
    t_cfg.route.max_passes = 0;
    t_cfg.route.reroute_pad_mm = None;
    let t_rep = compile_board(t_board.clone(), &t_cfg).unwrap();
    assert!(!t_rep.ok);
    assert_eq!(t_rep.route.repair_passes_run, 1);
    assert!(
        !t_rep
            .drc
            .violations
            .iter()
            .any(|v| v.kind == ViolationKind::MaxVias),
        "must not regress passing max_vias net to fix max_length: {:?}",
        t_rep.drc.violations
    );
    assert_eq!(t_rep.board.tracks, t_board.tracks);
    assert_eq!(t_rep.board.vias, t_board.vias);

    // 2b. Symmetrically, repair must not regress a passing MaxLength net (SIG_N1) to fix MaxVias (SIG_A).
    let mut t2_board = two_layer_pad_board(
        "max_vias_no_trade_vv",
        (30.0, 22.0),
        &[
            ("A1", "PAD_TOP", 6.0, 11.0, "SIG_A"),
            ("A2", "PAD_TOP", 20.0, 11.0, "SIG_A"),
            ("N1", "PAD_TOP", 13.0, 15.0, "SIG_N1"),
            ("N2", "PAD_TOP", 13.0, 7.0, "SIG_N1"),
        ],
        "[[constraints.max_vias]]\nnets = [\"SIG_A\"]\nmax_vias = 0\n[[constraints.max_lengths]]\nnets = [\"SIG_N1\"]\nmax_nm = 9000000\n",
    );
    // Keepout on B.Cu over N1/N2 forces SIG_N1 > 9 mm if SIG_A claims F.Cu at y=11, creating a real trade-off.
    t2_board.keepouts.push(Keepout {
        kind: KeepoutKind::All,
        layers: vec!["B.Cu".into()],
        poly: vec![
            Pt::new(mm(10.0), mm(6.0)),
            Pt::new(mm(16.0), mm(6.0)),
            Pt::new(mm(16.0), mm(16.5)),
            Pt::new(mm(10.0), mm(16.5)),
        ],
    });
    push_tracks(
        &mut t2_board,
        &[
            ("SIG_N1", "F.Cu", (13.0, 15.0), (13.0, 7.0), false),
            ("SIG_A", "F.Cu", (6.0, 11.0), (8.0, 11.0), false),
            ("SIG_A", "B.Cu", (8.0, 11.0), (8.0, 4.5), false),
            ("SIG_A", "B.Cu", (8.0, 4.5), (18.0, 4.5), false),
            ("SIG_A", "B.Cu", (18.0, 4.5), (18.0, 11.0), false),
            ("SIG_A", "F.Cu", (18.0, 11.0), (20.0, 11.0), false),
        ],
    );
    push_vias(
        &mut t2_board,
        &[
            ("SIG_A", (8.0, 11.0), false),
            ("SIG_A", (18.0, 11.0), false),
        ],
    );
    let mut t2_cfg0 = t_cfg.clone();
    t2_cfg0.repair_passes = 0;
    let t2_r0 = compile_board(t2_board.clone(), &t2_cfg0).unwrap();
    let t2_rep = compile_board(t2_board, &t_cfg).unwrap();
    assert!(!t2_rep.ok);
    assert_eq!(t2_rep.route.repair_passes_run, 1);
    assert!(
        !t2_rep
            .drc
            .violations
            .iter()
            .any(|v| v.kind == ViolationKind::MaxLength),
        "must not regress passing max_length net to fix max_vias: {:?}",
        t2_rep.drc.violations
    );
    assert_eq!(t2_rep.board.tracks, t2_r0.board.tracks);
    assert_eq!(t2_rep.board.vias, t2_r0.board.vias);

    // 3. Respects --reroute-nets scope: out-of-scope MaxVias net and detoured max_vias blocker stay untouched.
    let mut s_board = two_layer_pad_board(
        "max_vias_scope",
        (30.0, 22.0),
        &[
            ("A1", "PAD_TOP", 4.0, 18.0, "SIG_A"),
            ("A2", "PAD_TOP", 10.0, 18.0, "SIG_A"),
            ("B1", "PAD_TOP", 6.0, 10.0, "SIG_B"),
            ("B2", "PAD_TOP", 20.0, 10.0, "SIG_B"),
            ("N1", "PAD_TOP", 13.0, 15.0, "SIG_N1"),
            ("N2", "PAD_TOP", 13.0, 5.0, "SIG_N1"),
        ],
        "[[constraints.max_vias]]\nnets = [\"SIG_B\", \"SIG_N1\"]\nmax_vias = 0\n",
    );
    // Give SIG_N1 an unlocked detoured route so any unauthorized reroute changes its geometry.
    push_tracks(
        &mut s_board,
        &[
            ("SIG_A", "F.Cu", (4.0, 18.0), (10.0, 18.0), false),
            ("SIG_N1", "F.Cu", (13.0, 15.0), (15.0, 10.0), false),
            ("SIG_N1", "F.Cu", (15.0, 10.0), (13.0, 5.0), false),
            ("SIG_B", "F.Cu", (6.0, 10.0), (8.0, 10.0), false),
            ("SIG_B", "B.Cu", (8.0, 10.0), (18.0, 10.0), false),
            ("SIG_B", "F.Cu", (18.0, 10.0), (20.0, 10.0), false),
        ],
    );
    push_vias(
        &mut s_board,
        &[
            ("SIG_B", (8.0, 10.0), false),
            ("SIG_B", (18.0, 10.0), false),
        ],
    );

    let mut scope_a_cfg = boxed_cfg(3, true);
    scope_a_cfg.route.resume = true;
    scope_a_cfg.route.reroute_pad_mm = None;
    let rep_a = compile_board(s_board.clone(), &scope_a_cfg).unwrap();
    assert!(!rep_a.ok);
    assert_eq!(rep_a.route.repair_passes_run, 0);
    assert!(
        rep_a
            .drc
            .violations
            .iter()
            .any(|v| v.kind == ViolationKind::MaxVias && v.net_a == "SIG_B")
    );

    let expected_sig_n1: Vec<Track> = s_board
        .tracks
        .iter()
        .filter(|t| t.net == "SIG_N1")
        .cloned()
        .collect();
    let mut scope_b_cfg = scope_a_cfg;
    scope_b_cfg.route.reroute_nets = Some(HashSet::from(["SIG_B".into()]));
    let rep_b = compile_board(s_board, &scope_b_cfg).unwrap();
    assert_eq!(rep_b.route.repair_passes_run, 1);
    assert_eq!(
        rep_b
            .board
            .tracks
            .iter()
            .filter(|t| t.net == "SIG_N1")
            .cloned()
            .collect::<Vec<_>>(),
        expected_sig_n1
    );
    assert!(
        !rep_b
            .drc
            .violations
            .iter()
            .any(|v| v.kind == ViolationKind::MaxVias && v.net_a == "SIG_N1")
    );
    assert!(
        rep_b
            .drc
            .violations
            .iter()
            .any(|v| v.kind == ViolationKind::MaxVias && v.net_a == "SIG_B")
    );
}

#[test]
fn plane_net_with_max_vias_stays_ignored_unknown_errors_and_locked_vias_do_not_starve() {
    // 1. Resolved Plane net with constraints.max_vias stays auto-ignored (no signal tracks), while stage-6 DRC checks the cap.
    let plane_toml = r#"
name = "plane_max_vias"
size_nm = [24000000, 18000000]
[[layers]]
name = "F.Cu"
kind = "signal"
side = "top"
[[layers]]
name = "In1.Cu"
kind = "plane"
[[layers]]
name = "B.Cu"
kind = "signal"
side = "bottom"
[footprints.PAD_TOP]
kind = "custom"
[[footprints.PAD_TOP.pads]]
name = "1"
kind = "smd"
shape = { shape = "circle", diameter = 600000 }
pos = { x = 0, y = 0 }
layers = ["F.Cu"]
[[placements]]
refdes = "G1"
footprint = "PAD_TOP"
pos = { x = 6000000, y = 9000000 }
locked = true
pin_nets = { "1" = "GND" }
[[placements]]
refdes = "G2"
footprint = "PAD_TOP"
pos = { x = 18000000, y = 9000000 }
locked = true
pin_nets = { "1" = "GND" }
[[pours]]
name = "GND_PLANE"
net = "GND"
layer = "In1.Cu"
outline = [
  { x = 500000, y = 500000 },
  { x = 23500000, y = 500000 },
  { x = 23500000, y = 17500000 },
  { x = 500000, y = 17500000 },
]
priority = 0
min_width_nm = 200000
thermal_relief = false
remove_islands = true
[rules]
track_width_nm = 200000
clearance_nm = 200000
via_dia_nm = 450000
via_drill_nm = 200000
min_annular_ring_nm = 100000
hole_to_hole_nm = 200000
edge_clearance_nm = 300000
via_at_smd = true
[[constraints.max_vias]]
nets = ["GND"]
max_vias = 2
"#;
    let plane_spec: BoardSpec = toml::from_str(plane_toml).unwrap();
    let mut plane_board = BoardIr::from_spec(&plane_spec).unwrap();
    let p_rep = compile_board(plane_board.clone(), &boxed_cfg(0, false)).unwrap();
    assert!(
        p_rep.ok,
        "expected plane net with max_vias=2 to pass cleanly: {:?}",
        p_rep.drc
    );
    assert!(p_rep.board.tracks.iter().all(|t| t.net != "GND"));
    assert_eq!(
        p_rep.board.vias.iter().filter(|v| v.net == "GND").count(),
        2
    );

    plane_board.constraints.max_vias[0].max_vias = 1;
    let p_fail = compile_board(plane_board, &boxed_cfg(0, false)).unwrap();
    assert!(!p_fail.ok);
    assert!(
        p_fail
            .drc
            .violations
            .iter()
            .any(|v| v.kind == ViolationKind::MaxVias && v.net_a == "GND")
    );

    // 2. Unknown net in constraints.max_vias errors in route_board_ir and compile_board.
    let mut unk_board = two_layer_pad_board(
        "unknown_max_vias",
        (24.0, 18.0),
        &[
            ("A1", "PAD_TOP", 6.0, 9.0, "SIG_A"),
            ("A2", "PAD_TOP", 18.0, 9.0, "SIG_A"),
        ],
        "",
    );
    unk_board.constraints.max_vias.push(MaxViasRule {
        nets: vec!["NO_SUCH_NET".into()],
        max_vias: 0,
    });
    let err_route = route_board_ir(&mut unk_board.clone(), &RouteOptions::default()).unwrap_err();
    assert!(
        err_route.contains("max_vias constraint references unknown net 'NO_SUCH_NET'"),
        "unexpected route_board_ir error: {err_route}"
    );
    let err_compile = compile_board(unk_board, &boxed_cfg(0, false)).unwrap_err();
    assert!(err_compile.contains("max_vias constraint references unknown net 'NO_SUCH_NET'"));

    // 3. Locked-via MaxVias violation (zero unlocked vias) must not starve a fixable MaxVias net.
    let mut starve_board = two_layer_pad_board(
        "locked_max_vias_no_starve",
        (30.0, 22.0),
        &[
            ("L1", "PAD_TOP", 4.0, 18.0, "SIG_LOCK"),
            ("L2", "PAD_TOP", 10.0, 18.0, "SIG_LOCK"),
            ("A1", "PAD_TOP", 6.0, 11.0, "SIG_A"),
            ("A2", "PAD_TOP", 20.0, 11.0, "SIG_A"),
            ("N1", "PAD_TOP", 13.0, 15.0, "SIG_N1"),
            ("N2", "PAD_TOP", 13.0, 7.0, "SIG_N1"),
        ],
        "[[constraints.max_vias]]\nnets = [\"SIG_LOCK\", \"SIG_A\"]\nmax_vias = 0\n",
    );
    push_tracks(
        &mut starve_board,
        &[
            ("SIG_LOCK", "F.Cu", (4.0, 18.0), (5.5, 18.0), true),
            ("SIG_LOCK", "B.Cu", (5.5, 18.0), (8.5, 18.0), true),
            ("SIG_LOCK", "F.Cu", (8.5, 18.0), (10.0, 18.0), true),
            ("SIG_N1", "F.Cu", (13.0, 15.0), (13.0, 7.0), false),
            ("SIG_A", "F.Cu", (6.0, 11.0), (8.0, 11.0), false),
            ("SIG_A", "B.Cu", (8.0, 11.0), (18.0, 11.0), false),
            ("SIG_A", "F.Cu", (18.0, 11.0), (20.0, 11.0), false),
        ],
    );
    push_vias(
        &mut starve_board,
        &[
            ("SIG_LOCK", (5.5, 18.0), true),
            ("SIG_LOCK", (8.5, 18.0), true),
            ("SIG_A", (8.0, 11.0), false),
            ("SIG_A", (18.0, 11.0), false),
        ],
    );
    let mut starve_cfg = boxed_cfg(1, false);
    starve_cfg.route.resume = true;
    starve_cfg.route.reroute_pad_mm = None;
    let s_rep = compile_board(starve_board, &starve_cfg).unwrap();
    assert_eq!(s_rep.route.repair_passes_run, 1);
    assert!(
        !s_rep
            .drc
            .violations
            .iter()
            .any(|v| v.kind == ViolationKind::MaxVias && v.net_a == "SIG_A"),
        "expected SIG_A to be fixed without starvation by SIG_LOCK: {:?}",
        s_rep.drc.violations
    );
    assert!(
        s_rep
            .drc
            .violations
            .iter()
            .any(|v| v.kind == ViolationKind::MaxVias && v.net_a == "SIG_LOCK")
    );
}
