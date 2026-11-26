use std::collections::BTreeMap;
use std::fs;
use std::path::PathBuf;
use std::process::Command;

use boardc::drc::{ViolationKind, check_board_drc};
use boardc::fab::{FabOptions, export_fab};
use boardc::ir::{
    BoardIr, BoardSpec, DesignRules, FootprintSpec, LayerDef, LayerKind, Placement, PourSpec, Pt,
    Side, Track, mm, save_board_json,
};
use boardc::kicad_io::import_kicad_pcb;
use boardc::pipeline::{BuildOptions, compile_board};
use boardc::route::{RouteOptions, route_board_ir};
use boardc::verify::verify_fab_outputs;
use boardc::zones::fill_zones;

fn fresh_tmp_dir(name: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("boardc_verify_e2e_{name}_{}", std::process::id()));
    let _ = fs::remove_dir_all(&dir);
    fs::create_dir_all(&dir).unwrap();
    dir
}

fn layer(name: &str, kind: LayerKind, side: Option<Side>) -> LayerDef {
    LayerDef {
        name: name.into(),
        kind,
        side,
        thickness_nm: None,
    }
}

fn build_2layer_top_bottom_board() -> BoardIr {
    let mut footprints = BTreeMap::new();
    footprints.insert(
        "C_0603".into(),
        FootprintSpec::Chip2Pin {
            package: "0603".into(),
        },
    );
    footprints.insert(
        "HDR_2".into(),
        FootprintSpec::PinHeader {
            pins: 2,
            pitch_nm: mm(2.54),
        },
    );

    let spec = BoardSpec {
        name: "synth_2layer_tb".into(),
        outline: Vec::new(),
        size_nm: Some([mm(40.0), mm(30.0)]),
        cutouts: Vec::new(),
        layers: vec![
            layer("F.Cu", LayerKind::Signal, Some(Side::Top)),
            layer("B.Cu", LayerKind::Signal, Some(Side::Bottom)),
        ],
        footprints,
        placements: vec![
            Placement {
                refdes: "R1".into(),
                footprint: "C_0603".into(),
                value: "1k".into(),
                pos: Pt::new(mm(8.0), mm(20.0)),
                rot_deg: 0.0,
                side: Side::Top,
                locked: true,
                pin_nets: BTreeMap::from([
                    ("1".into(), "SIG_A".into()),
                    ("2".into(), "GND".into()),
                ]),
                mpn: None,
                lcsc: None,
                dnp: false,
                cpl_rotation_offset_deg: 0.0,
            },
            Placement {
                refdes: "R2".into(),
                footprint: "C_0603".into(),
                value: "1k".into(),
                pos: Pt::new(mm(22.0), mm(22.0)),
                rot_deg: 90.0,
                side: Side::Bottom,
                locked: true,
                pin_nets: BTreeMap::from([
                    ("1".into(), "SIG_A".into()),
                    ("2".into(), "SIG_B".into()),
                ]),
                mpn: None,
                lcsc: None,
                dnp: false,
                cpl_rotation_offset_deg: 0.0,
            },
            Placement {
                refdes: "R3".into(),
                footprint: "C_0603".into(),
                value: "1k".into(),
                pos: Pt::new(mm(30.0), mm(15.0)),
                rot_deg: 180.0,
                side: Side::Bottom,
                locked: true,
                pin_nets: BTreeMap::from([
                    ("1".into(), "SIG_B".into()),
                    ("2".into(), "SIG_C".into()),
                ]),
                mpn: None,
                lcsc: None,
                dnp: false,
                cpl_rotation_offset_deg: 0.0,
            },
            Placement {
                refdes: "R4".into(),
                footprint: "C_0603".into(),
                value: "1k".into(),
                pos: Pt::new(mm(16.0), mm(8.0)),
                rot_deg: 270.0,
                side: Side::Top,
                locked: true,
                pin_nets: BTreeMap::from([
                    ("1".into(), "SIG_C".into()),
                    ("2".into(), "GND".into()),
                ]),
                mpn: None,
                lcsc: None,
                dnp: false,
                cpl_rotation_offset_deg: 0.0,
            },
            Placement {
                refdes: "J1".into(),
                footprint: "HDR_2".into(),
                value: "HDR2".into(),
                pos: Pt::new(mm(8.0), mm(8.0)),
                rot_deg: 0.0,
                side: Side::Top,
                locked: true,
                pin_nets: BTreeMap::from([
                    ("1".into(), "GND".into()),
                    ("2".into(), "SIG_A".into()),
                ]),
                mpn: None,
                lcsc: None,
                dnp: false,
                cpl_rotation_offset_deg: 0.0,
            },
        ],
        keepouts: Vec::new(),
        pours: vec![PourSpec {
            name: "GND_BOT".into(),
            net: "GND".into(),
            layer: "B.Cu".into(),
            outline: vec![
                Pt::new(mm(0.5), mm(0.5)),
                Pt::new(mm(39.5), mm(0.5)),
                Pt::new(mm(39.5), mm(29.5)),
                Pt::new(mm(0.5), mm(29.5)),
            ],
            priority: 0,
            clearance_nm: None,
            min_width_nm: mm(0.2),
            thermal_relief: true,
            thermal_gap_nm: mm(0.25),
            thermal_spoke_nm: mm(0.3),
            remove_islands: true,
        }],
        tracks: Vec::new(),
        vias: Vec::new(),
        net_classes: Default::default(),
        nets: Default::default(),
        rules: DesignRules {
            track_width_nm: mm(0.2),
            clearance_nm: mm(0.2),
            via_dia_nm: mm(0.6),
            via_drill_nm: mm(0.3),
            min_annular_ring_nm: mm(0.1),
            hole_to_hole_nm: mm(0.25),
            edge_clearance_nm: mm(0.3),
            courtyard_clearance_nm: 0,
            ..Default::default()
        },
        constraints: Default::default(),
        graphics: Vec::new(),
    };

    let mut board = BoardIr::from_spec(&spec).unwrap();
    let route_cfg = RouteOptions {
        max_passes: 20,
        multi_start: 2,
        threads: 1,
        ..Default::default()
    };
    let route_sum = route_board_ir(&mut board, &route_cfg).unwrap();
    assert_eq!(route_sum.unconnected_pairs, 0);
    fill_zones(&mut board);
    board
}

#[test]
fn e2e_2layer_top_bottom_rotations_passes_fab_verify_and_kicad_export_roundtrip() {
    let tmp = fresh_tmp_dir("2layer_tb");
    let fab_dir = tmp.join("fab");
    let board_path = tmp.join("board.json");
    let kicad_out = tmp.join("out.kicad_pcb");

    let board = build_2layer_top_bottom_board();
    let drc = check_board_drc(&board);
    assert!(
        drc.passed,
        "native DRC failed on 2-layer board: {:?}",
        drc.violations
    );

    export_fab(&board, &fab_dir, &FabOptions::default()).unwrap();
    let fab_rep = verify_fab_outputs(&board, &fab_dir).unwrap();
    assert!(
        fab_rep.passed,
        "fab verification failed: {:?}",
        fab_rep.errors
    );
    assert!(fab_rep.drills.passed);
    assert!(fab_rep.connectivity.passed);

    // Verify CLI `boardc verify --fab-dir ... --json` and `boardc export-kicad`.
    save_board_json(&board, &board_path).unwrap();
    let cli = Command::new(env!("CARGO_BIN_EXE_boardc"))
        .args([
            "verify",
            board_path.to_str().unwrap(),
            "--fab-dir",
            fab_dir.to_str().unwrap(),
            "--json",
        ])
        .output()
        .unwrap();
    assert!(
        cli.status.success(),
        "boardc verify CLI failed: {}",
        String::from_utf8_lossy(&cli.stderr)
    );
    let json: serde_json::Value = serde_json::from_slice(&cli.stdout).unwrap();
    assert_eq!(json["ok"], true);
    assert_eq!(json["fab"]["passed"], true);

    let exp = Command::new(env!("CARGO_BIN_EXE_boardc"))
        .args([
            "export-kicad",
            board_path.to_str().unwrap(),
            "-o",
            kicad_out.to_str().unwrap(),
        ])
        .output()
        .unwrap();
    assert!(
        exp.status.success(),
        "boardc export-kicad CLI failed: {}",
        String::from_utf8_lossy(&exp.stderr)
    );
    let pro_out = kicad_out.with_extension("kicad_pro");
    assert!(kicad_out.exists() && pro_out.exists());
    let reimported = import_kicad_pcb(&kicad_out, Some(&pro_out)).unwrap();
    assert_eq!(reimported.placements.len(), board.placements.len());
    assert_eq!(reimported.tracks.len(), board.tracks.len());
    assert_eq!(reimported.vias.len(), board.vias.len());

    let _ = fs::remove_dir_all(&tmp);
}

#[test]
fn e2e_4layer_bga_build_passes_fab_verify() {
    let tmp = fresh_tmp_dir("4layer_bga");
    let out_dir = tmp.join("out");
    let cache_dir = tmp.join("cache");

    let toml_spec = r#"
name = "bga_4layer_verify"
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
courtyard_clearance_nm = 0
via_at_smd = true

[[constraints.cap_proximity]]
cap_refdes = "C1"
ic_refdes = "U1"
net = "VDD"
max_dist_nm = 4500000

[[constraints.bga_shadows]]
bga_refdes = "U1"
min_clearance_nm = 500000
allow_passives = false
"#;
    let spec: BoardSpec = toml::from_str(toml_spec).unwrap();
    let board = BoardIr::from_spec(&spec).unwrap();
    let mut cfg = BuildOptions {
        out_dir: Some(out_dir.clone()),
        cache_dir,
        use_cache: false,
        fail_on_preflight: true,
        ..Default::default()
    };
    cfg.route.max_passes = 20;
    cfg.route.multi_start = 2;

    let report = compile_board(board, &cfg).unwrap();
    assert!(
        report.ok,
        "compile_board failed: {:?}",
        report.drc.violations
    );
    assert_eq!(report.drc.error_count, 0);

    let fab_dir = out_dir.join("fab");
    let fab_rep = verify_fab_outputs(&report.board, &fab_dir).unwrap();
    assert!(
        fab_rep.passed,
        "4-layer fab verification failed: {:?}",
        fab_rep.errors
    );
    assert!(fab_rep.drills.passed);
    assert!(fab_rep.connectivity.passed);

    let _ = fs::remove_dir_all(&tmp);
}

#[test]
fn negative_copper_short_caught_by_both_native_drc_and_gerber_verifier() {
    let tmp = fresh_tmp_dir("neg_short");
    let fab_dir = tmp.join("fab");
    let mut board = build_2layer_top_bottom_board();

    // Inject an intentional copper short between R1.1 (SIG_A) and R4.2 (GND) on F.Cu.
    let r1_pos = board.placements[0].pos;
    let r4_pos = board.placements[3].pos;
    board.tracks.push(Track {
        net: "SIG_A".into(),
        layer: "F.Cu".into(),
        a: Pt::new(r1_pos.x - mm(0.8), r1_pos.y),
        b: Pt::new(r4_pos.x, r4_pos.y + mm(0.8)),
        width: mm(0.25),
        locked: true,
    });

    let drc = check_board_drc(&board);
    assert!(!drc.passed, "native DRC must fail on injected copper short");
    assert!(
        drc.violations
            .iter()
            .any(|v| v.kind == ViolationKind::Clearance),
        "expected Clearance violation in native DRC: {:?}",
        drc.violations
    );

    export_fab(&board, &fab_dir, &FabOptions::default()).unwrap();
    let fab_rep = verify_fab_outputs(&board, &fab_dir).unwrap();
    assert!(
        !fab_rep.passed,
        "independent Gerber verifier must fail on injected short"
    );
    assert!(
        !fab_rep.connectivity.shorts.is_empty(),
        "expected Gerber-level short detection, got {:?}",
        fab_rep.connectivity
    );

    let _ = fs::remove_dir_all(&tmp);
}

#[test]
fn regression_verify_excellon_catches_plated_drill_short_beyond_2nm() {
    let tmp = fresh_tmp_dir("excellon_drill_short");
    let fab_dir = tmp.join("fab");

    let mut board = build_2layer_top_bottom_board();
    let r_cfg = RouteOptions {
        max_passes: 20,
        multi_start: 2,
        ..Default::default()
    };
    route_board_ir(&mut board, &r_cfg).unwrap();
    fill_zones(&mut board);
    let manifest = export_fab(&board, &fab_dir, &FabOptions::default()).unwrap();

    // Inject an unannounced plated drill hit right inside the B.Cu GND pour, well beyond 2 nm of any bbox edge.
    let pth_path = manifest
        .drill_files
        .iter()
        .find(|p| p.to_string_lossy().ends_with("-PTH.drl"))
        .expect("missing PTH drill file");
    let orig = fs::read_to_string(pth_path).unwrap();
    let corrupted = orig.replacen("T01\n", "T01\nX15.0000Y12.0000\n", 1);
    fs::write(pth_path, corrupted).unwrap();

    let fab_rep = verify_fab_outputs(&board, &fab_dir).unwrap();
    assert!(
        !fab_rep.drills.passed,
        "Excellon verifier must reject unexpected drill hit: {:?}",
        fab_rep.drills
    );

    // Also test a board where a SIG_A via sits right on top of a B.Cu GND pour island (shorting via drill into pour).
    let mut shorted = board.clone();
    shorted.vias.push(boardc::ir::Via {
        net: "SIG_A".into(),
        pos: Pt::new(mm(20.0), mm(2.0)),
        diameter: mm(0.45),
        drill: mm(0.20),
        layers: ("F.Cu".into(), "B.Cu".into()),
        locked: true,
        tented: true,
    });
    let fab_dir2 = tmp.join("fab2");
    export_fab(&shorted, &fab_dir2, &FabOptions::default()).unwrap();
    let fab_rep2 = verify_fab_outputs(&shorted, &fab_dir2).unwrap();
    assert!(
        !fab_rep2.connectivity.shorts.is_empty(),
        "Excellon + Gerber connectivity verifier must catch via shorting into pour: {:?}",
        fab_rep2.connectivity
    );

    let _ = fs::remove_dir_all(&tmp);
}
