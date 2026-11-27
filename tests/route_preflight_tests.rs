use std::collections::{BTreeMap, HashSet};
use std::process::Command;
use std::time::Instant;

use boardc::drc::{ViolationKind, check_board_drc};
use boardc::ir::{
    BoardIr, BoardSpec, CapProximityRule, Constraints, DesignRules, FootprintSpec, Keepout,
    KeepoutKind, LayerDef, LayerKind, MatchGroupRule, MaxLengthRule, NetClass, NetDef, PadDef,
    PadKind, PadShape, Placement, PourSpec, Pt, Side, Track, Via, mm, um,
};
use boardc::preflight::{PreflightSummary, run_preflight};
use boardc::route::{RouteOptions, RouteSummary, route_board_ir, to_viahop_board};

fn rect_pts(x0: f64, y0: f64, x1: f64, y1: f64) -> Vec<Pt> {
    vec![
        Pt::new(mm(x0), mm(y0)),
        Pt::new(mm(x1), mm(y0)),
        Pt::new(mm(x1), mm(y1)),
        Pt::new(mm(x0), mm(y1)),
    ]
}

fn layer_sig(name: &str, side: Option<Side>) -> LayerDef {
    LayerDef {
        name: name.into(),
        kind: LayerKind::Signal,
        side,
        thickness_nm: None,
    }
}

fn layer_plane(name: &str) -> LayerDef {
    LayerDef {
        name: name.into(),
        kind: LayerKind::Plane,
        side: None,
        thickness_nm: None,
    }
}

fn custom_fp(pads: Vec<PadDef>) -> FootprintSpec {
    FootprintSpec::Custom {
        pads,
        courtyard: Vec::new(),
        graphics: Vec::new(),
        keepouts: Vec::new(),
        models: Vec::new(),
        pin_types: Default::default(),
        pin_names: BTreeMap::new(),
    }
}

fn term_fp(dia_nm: i64, layers: &[&str]) -> FootprintSpec {
    custom_fp(vec![PadDef {
        name: "1".into(),
        kind: PadKind::Smd,
        shape: PadShape::Circle { diameter: dia_nm },
        pos: Pt::new(0, 0),
        rot_deg: 0.0,
        drill_nm: None,
        layers: layers.iter().map(|&s| s.into()).collect(),
        mask_margin_nm: None,
        paste_margin_nm: None,
    }])
}

fn place_1pin(refdes: &str, fp: &str, pos_mm: (f64, f64), side: Side, net: &str) -> Placement {
    let mut pin_nets = BTreeMap::new();
    if !net.is_empty() {
        pin_nets.insert("1".into(), net.into());
    }
    Placement {
        refdes: refdes.into(),
        footprint: fp.into(),
        pos: Pt::new(mm(pos_mm.0), mm(pos_mm.1)),
        side,
        locked: true,
        pin_nets,
        ..Default::default()
    }
}

fn place_2pin(
    refdes: &str,
    fp: &str,
    pos_mm: (f64, f64),
    rot_deg: f64,
    side: Side,
    n1: &str,
    n2: &str,
) -> Placement {
    let mut pin_nets = BTreeMap::new();
    if !n1.is_empty() {
        pin_nets.insert("1".into(), n1.into());
    }
    if !n2.is_empty() {
        pin_nets.insert("2".into(), n2.into());
    }
    Placement {
        refdes: refdes.into(),
        footprint: fp.into(),
        value: String::new(),
        pos: Pt::new(mm(pos_mm.0), mm(pos_mm.1)),
        rot_deg,
        side,
        locked: false,
        pin_nets,
        mpn: None,
        lcsc: None,
        dnp: false,
        exclude_from_bom: false,
        exclude_from_pos: false,
        cpl_rotation_offset_deg: 0.0,
        ..Default::default()
    }
}

fn build_2layer_board() -> BoardIr {
    let mut footprints = BTreeMap::new();
    footprints.insert(
        "R_0603".into(),
        FootprintSpec::Chip2Pin {
            package: "0603".into(),
            pin_types: Default::default(),
            pin_names: BTreeMap::new(),
        },
    );
    footprints.insert(
        "U_QFN16".into(),
        FootprintSpec::Qfn {
            pins_per_side: 4,
            pitch_nm: mm(0.8),
            body_nm: mm(4.0),
            ep_nm: 0,
            pin_types: Default::default(),
            pin_names: BTreeMap::new(),
        },
    );

    let mut u1_nets = BTreeMap::new();
    u1_nets.insert("1".into(), "SIG_A".into());
    u1_nets.insert("2".into(), "SIG_B".into());
    u1_nets.insert("5".into(), "SIG_C".into());
    u1_nets.insert("9".into(), "VCC".into());

    let spec = BoardSpec {
        name: "two_layer_route".into(),
        outline: Vec::new(),
        size_nm: Some([mm(30.0), mm(20.0)]),
        cutouts: Vec::new(),
        layers: vec![
            layer_sig("F.Cu", Some(Side::Top)),
            layer_sig("B.Cu", Some(Side::Bottom)),
        ],
        footprints,
        placements: vec![
            Placement {
                refdes: "U1".into(),
                footprint: "U_QFN16".into(),
                value: "IC".into(),
                pos: Pt::new(mm(15.0), mm(10.0)),
                rot_deg: 0.0,
                side: Side::Top,
                locked: false,
                pin_nets: u1_nets,
                mpn: None,
                lcsc: None,
                dnp: false,
                exclude_from_bom: false,
                exclude_from_pos: false,
                cpl_rotation_offset_deg: 0.0,
                ..Default::default()
            },
            place_2pin("R1", "R_0603", (23.0, 14.0), 0.0, Side::Top, "SIG_A", "VCC"),
            place_2pin(
                "R2",
                "R_0603",
                (23.0, 6.0),
                0.0,
                Side::Top,
                "SIG_B",
                "SIG_C",
            ),
            place_2pin("R3", "R_0603", (7.0, 10.0), 90.0, Side::Top, "SIG_C", "VCC"),
        ],
        keepouts: Vec::new(),
        pours: Vec::new(),
        tracks: Vec::new(),
        vias: Vec::new(),
        net_classes: BTreeMap::new(),
        nets: BTreeMap::new(),
        rules: DesignRules::default(),
        constraints: Constraints::default(),
        graphics: Vec::new(),
        stackup: Default::default(),
        ..Default::default()
    };
    BoardIr::from_spec(&spec).unwrap()
}

#[test]
fn route_2layer_board_ir_clean_drc() {
    let mut board = build_2layer_board();
    let summary = route_board_ir(&mut board, &RouteOptions::default()).unwrap();
    assert_eq!(summary.unconnected_pairs, 0);
    assert_eq!(summary.drc_violations, 0);
    assert_eq!(summary.routed_nets, summary.routable_nets);
    assert!(!board.tracks.is_empty());

    let drc = check_board_drc(&board);
    assert!(drc.passed, "expected clean DRC, got {:?}", drc.violations);
    assert_eq!(
        drc.warning_count, 0,
        "routed 2-layer board must have no dangling tracks/vias: {:?}",
        drc.violations
    );
}

#[test]
fn route_4layer_board_ir_with_inner_planes_clean_drc() {
    let mut footprints = BTreeMap::new();
    footprints.insert(
        "R_0603".into(),
        FootprintSpec::Chip2Pin {
            package: "0603".into(),
            pin_types: Default::default(),
            pin_names: BTreeMap::new(),
        },
    );
    let mut nets = BTreeMap::new();
    for (name, ignore) in [("GND", true), ("VCC", true)] {
        nets.insert(
            name.into(),
            NetDef {
                name: name.into(),
                ignore_route: ignore,
                ..Default::default()
            },
        );
    }

    let spec = BoardSpec {
        name: "four_layer_route".into(),
        outline: Vec::new(),
        size_nm: Some([mm(30.0), mm(20.0)]),
        cutouts: Vec::new(),
        layers: vec![
            layer_sig("F.Cu", Some(Side::Top)),
            layer_plane("In1.Cu"),
            layer_plane("In2.Cu"),
            layer_sig("B.Cu", Some(Side::Bottom)),
        ],
        footprints,
        placements: vec![
            place_2pin("R1", "R_0603", (6.0, 12.0), 0.0, Side::Top, "SIG1", "SIG2"),
            place_2pin(
                "R2",
                "R_0603",
                (22.0, 8.0),
                0.0,
                Side::Bottom,
                "SIG1",
                "SIG3",
            ),
            place_2pin("R3", "R_0603", (22.0, 14.0), 0.0, Side::Top, "SIG2", "SIG3"),
        ],
        keepouts: Vec::new(),
        pours: vec![
            PourSpec {
                name: "GND_IN1".into(),
                net: "GND".into(),
                layer: "In1.Cu".into(),
                outline: rect_pts(0.5, 0.5, 29.5, 19.5),
                priority: 0,
                clearance_nm: None,
                min_width_nm: mm(0.2),
                thermal_relief: true,
                thermal_gap_nm: mm(0.25),
                thermal_spoke_nm: mm(0.3),
                remove_islands: true,
            },
            PourSpec {
                name: "VCC_IN2".into(),
                net: "VCC".into(),
                layer: "In2.Cu".into(),
                outline: rect_pts(0.5, 0.5, 29.5, 19.5),
                priority: 0,
                clearance_nm: None,
                min_width_nm: mm(0.2),
                thermal_relief: true,
                thermal_gap_nm: mm(0.25),
                thermal_spoke_nm: mm(0.3),
                remove_islands: true,
            },
        ],
        tracks: Vec::new(),
        vias: Vec::new(),
        net_classes: BTreeMap::new(),
        nets,
        rules: DesignRules::default(),
        constraints: Constraints::default(),
        graphics: Vec::new(),
        stackup: Default::default(),
        ..Default::default()
    };
    let mut board = BoardIr::from_spec(&spec).unwrap();
    let vboard = to_viahop_board(&board, &HashSet::new(), None).unwrap();
    assert_eq!(vboard.signal_layers, vec![0, 3]);
    assert_eq!(vboard.planes.len(), 2);

    let summary = route_board_ir(&mut board, &RouteOptions::default()).unwrap();
    assert_eq!(summary.unconnected_pairs, 0);
    assert_eq!(summary.drc_violations, 0);
    assert!(
        !board.vias.is_empty(),
        "top-to-bottom 4-layer signals must use vias"
    );

    let drc = check_board_drc(&board);
    assert!(
        drc.passed,
        "expected clean 4-layer DRC, got {:?}",
        drc.violations
    );
}

#[test]
fn reroute_nets_preserves_frozen_nets_and_locked_items() {
    let mut board = build_2layer_board();
    board.footprints.insert(
        "PAD_AUX".into(),
        term_fp(mm(0.6), &["F.Cu"]).resolve("PAD_AUX").unwrap(),
    );
    board.placements.push(place_1pin(
        "AUX1",
        "PAD_AUX",
        (2.0, 2.0),
        Side::Top,
        "LOCKED_AUX",
    ));
    board.placements.push(place_1pin(
        "AUX2",
        "PAD_AUX",
        (4.0, 2.0),
        Side::Top,
        "LOCKED_AUX",
    ));
    board.tracks.push(Track {
        net: "LOCKED_AUX".into(),
        layer: "F.Cu".into(),
        a: Pt::new(mm(2.0), mm(2.0)),
        b: Pt::new(mm(4.0), mm(2.0)),
        width: um(200.0),
        locked: true,
    });
    let summary = route_board_ir(&mut board, &RouteOptions::default()).unwrap();
    assert_eq!(summary.unconnected_pairs, 0);

    let frozen_a_tracks: Vec<_> = board
        .tracks
        .iter()
        .filter(|t| t.net == "SIG_A")
        .cloned()
        .collect();
    let frozen_a_vias: Vec<_> = board
        .vias
        .iter()
        .filter(|v| v.net == "SIG_A")
        .cloned()
        .collect();
    assert!(!frozen_a_tracks.is_empty());

    // Remove SIG_B tracks/vias and reroute only SIG_B via CLI `boardc route --reroute-nets`.
    board.tracks.retain(|t| t.net != "SIG_B");
    board.vias.retain(|v| v.net != "SIG_B");

    let tmp_dir = std::env::temp_dir().join("boardc_test_reroute_cli");
    std::fs::create_dir_all(&tmp_dir).unwrap();
    let in_path = tmp_dir.join("in.json");
    let out_path = tmp_dir.join("out.json");
    std::fs::write(&in_path, serde_json::to_string_pretty(&board).unwrap()).unwrap();

    let out = Command::new(env!("CARGO_BIN_EXE_boardc"))
        .args([
            "route",
            in_path.to_str().unwrap(),
            "-o",
            out_path.to_str().unwrap(),
            "--reroute-nets",
            "SIG_B",
            "--max-passes",
            "15",
            "--multi-start",
            "2",
            "--threads",
            "2",
            "--json",
        ])
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "cli failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let cli_summary: RouteSummary = serde_json::from_slice(&out.stdout).unwrap();
    assert_eq!(cli_summary.unconnected_pairs, 0);

    let rerouted: BoardIr =
        serde_json::from_str(&std::fs::read_to_string(&out_path).unwrap()).unwrap();
    let after_a_tracks: Vec<_> = rerouted
        .tracks
        .iter()
        .filter(|t| t.net == "SIG_A")
        .cloned()
        .collect();
    let after_a_vias: Vec<_> = rerouted
        .vias
        .iter()
        .filter(|v| v.net == "SIG_A")
        .cloned()
        .collect();
    assert_eq!(frozen_a_tracks, after_a_tracks);
    assert_eq!(frozen_a_vias, after_a_vias);
    assert!(
        rerouted
            .tracks
            .iter()
            .any(|t| t.net == "LOCKED_AUX" && t.locked),
        "locked track must be preserved"
    );
    assert!(rerouted.tracks.iter().any(|t| t.net == "SIG_B"));
    let drc = check_board_drc(&rerouted);
    assert!(drc.passed, "rerouted DRC failed: {:?}", drc.violations);

    let _ = std::fs::remove_dir_all(&tmp_dir);
}

#[test]
fn length_constraints_propagate_to_viahop() {
    let mut footprints = BTreeMap::new();
    footprints.insert(
        "R_0603".into(),
        FootprintSpec::Chip2Pin {
            package: "0603".into(),
            pin_types: Default::default(),
            pin_names: BTreeMap::new(),
        },
    );
    let mut net_classes = BTreeMap::new();
    net_classes.insert(
        "HS".into(),
        NetClass {
            name: "HS".into(),
            track_width_nm: mm(0.15),
            clearance_nm: mm(0.15),
            via_dia_nm: mm(0.45),
            via_drill_nm: mm(0.2),
            diff_pair_width_nm: None,
            diff_pair_gap_nm: None,
        },
    );
    let mut nets = BTreeMap::new();
    for n in ["CLK_A", "CLK_B"] {
        nets.insert(
            n.into(),
            NetDef {
                name: n.into(),
                class: "HS".into(),
                ..Default::default()
            },
        );
    }

    let make_board = |max_mm: f64, match_tol_mm: f64| -> BoardIr {
        let spec = BoardSpec {
            name: "len_test".into(),
            outline: Vec::new(),
            size_nm: Some([mm(35.0), mm(20.0)]),
            cutouts: Vec::new(),
            layers: vec![
                layer_sig("F.Cu", Some(Side::Top)),
                layer_sig("B.Cu", Some(Side::Bottom)),
            ],
            footprints: footprints.clone(),
            placements: vec![
                place_2pin(
                    "R1",
                    "R_0603",
                    (5.0, 12.0),
                    90.0,
                    Side::Top,
                    "CLK_A",
                    "CLK_B",
                ),
                place_2pin("R2", "R_0603", (18.0, 12.0), 90.0, Side::Top, "CLK_A", ""),
                place_2pin("R3", "R_0603", (14.0, 6.0), 90.0, Side::Top, "", "CLK_B"),
            ],
            keepouts: Vec::new(),
            pours: Vec::new(),
            tracks: Vec::new(),
            vias: Vec::new(),
            net_classes: net_classes.clone(),
            nets: nets.clone(),
            rules: DesignRules::default(),
            constraints: Constraints {
                max_lengths: vec![MaxLengthRule {
                    nets: vec!["CLK_A".into()],
                    max_nm: mm(max_mm),
                }],
                match_groups: vec![MatchGroupRule {
                    name: "CLK_BUS".into(),
                    nets: vec!["CLK_A".into(), "CLK_B".into()],
                    tolerance_nm: mm(match_tol_mm),
                    target_nm: None,
                }],
                ..Default::default()
            },
            graphics: Vec::new(),
            stackup: Default::default(),
            ..Default::default()
        };
        BoardIr::from_spec(&spec).unwrap()
    };

    // Achievable max_length (25mm) + match_group (0.5mm tolerance): must pass both viahop & boardc DRC.
    let mut ok_board = make_board(25.0, 0.5);
    let ok_summary = route_board_ir(&mut ok_board, &RouteOptions::default()).unwrap();
    assert_eq!(ok_summary.unconnected_pairs, 0);
    assert_eq!(ok_summary.length_violations, 0);
    let drc = check_board_drc(&ok_board);
    assert!(
        drc.passed,
        "expected clean DRC with length tuning, got {:?}",
        drc.violations
    );

    // Impossible max_length (5mm for 13mm span): viahop must report length_violations > 0.
    let mut bad_board = make_board(5.0, 0.5);
    let bad_summary = route_board_ir(
        &mut bad_board,
        &RouteOptions {
            max_passes: 4,
            multi_start: 1,
            ..Default::default()
        },
    )
    .unwrap();
    assert!(
        bad_summary.length_violations > 0,
        "expected length violation when max_length=5mm for 13mm span"
    );
}

#[test]
fn preflight_catches_courtyard_cap_proximity_pinched_zone_and_sealed_bga_fast() {
    let mut footprints = BTreeMap::new();
    footprints.insert(
        "C_0603".into(),
        FootprintSpec::Chip2Pin {
            package: "0603".into(),
            pin_types: Default::default(),
            pin_names: BTreeMap::new(),
        },
    );
    footprints.insert(
        "U_BGA9".into(),
        FootprintSpec::Bga {
            rows: 3,
            cols: 3,
            pitch_nm: mm(0.5),
            ball_dia_nm: mm(0.3),
            pin_types: Default::default(),
            pin_names: BTreeMap::new(),
        },
    );
    footprints.insert("PAD".into(), term_fp(mm(0.4), &["F.Cu"]));

    // Connect all 9 balls of the 0.5mm-pitch BGA so center ball B2 is surrounded and sealed.
    let rows = ["A", "B", "C"];
    let mut bga_nets = BTreeMap::new();
    let mut placements = Vec::new();
    for (ri, r) in rows.iter().enumerate() {
        for ci in 1..=3 {
            let pin = format!("{r}{ci}");
            let net = format!("BGA_{pin}");
            bga_nets.insert(pin.clone(), net.clone());
            placements.push(place_1pin(
                &format!("TP_{pin}"),
                "PAD",
                (1.5 + (ci as f64) * 1.1, 1.5 + (ri as f64) * 1.1),
                Side::Top,
                &net,
            ));
        }
    }
    placements.push(Placement {
        refdes: "U1".into(),
        footprint: "U_BGA9".into(),
        value: "BGA9".into(),
        pos: Pt::new(mm(8.0), mm(4.0)),
        side: Side::Top,
        pin_nets: bga_nets,
        ..Default::default()
    });

    // Overlapping 0603 courtyards (C1 and C2, pads don't short) + C1 violates cap_proximity to U1 (> 3mm).
    placements.push(place_2pin(
        "C1",
        "C_0603",
        (12.5, 4.0),
        0.0,
        Side::Top,
        "VCC",
        "",
    ));
    placements.push(place_2pin(
        "C2",
        "C_0603",
        (14.8, 4.0),
        0.0,
        Side::Top,
        "",
        "",
    ));

    // Pinched zone: GND pour on B.Cu narrowed to ~0.24mm (< 0.4mm required neck) by two keepouts.
    placements.push(place_1pin("G1", "PAD", (3.0, 9.5), Side::Bottom, "GND"));
    placements.push(place_1pin("G2", "PAD", (11.0, 9.5), Side::Bottom, "GND"));

    let spec = BoardSpec {
        name: "preflight_fail".into(),
        outline: Vec::new(),
        size_nm: Some([mm(16.5), mm(12.0)]),
        cutouts: Vec::new(),
        layers: vec![
            layer_sig("F.Cu", Some(Side::Top)),
            layer_sig("B.Cu", Some(Side::Bottom)),
        ],
        footprints,
        placements,
        keepouts: vec![
            Keepout {
                kind: KeepoutKind::Copper,
                layers: vec!["B.Cu".into()],
                poly: rect_pts(6.5, 7.2, 7.5, 9.38),
            },
            Keepout {
                kind: KeepoutKind::Copper,
                layers: vec!["B.Cu".into()],
                poly: rect_pts(6.5, 9.62, 7.5, 11.8),
            },
        ],
        pours: vec![PourSpec {
            name: "GND_BOT".into(),
            net: "GND".into(),
            layer: "B.Cu".into(),
            outline: rect_pts(1.5, 7.2, 13.0, 11.8),
            priority: 0,
            clearance_nm: Some(mm(0.15)),
            min_width_nm: mm(0.2),
            thermal_relief: false,
            thermal_gap_nm: mm(0.25),
            thermal_spoke_nm: mm(0.3),
            remove_islands: true,
        }],
        tracks: Vec::new(),
        vias: Vec::new(),
        net_classes: BTreeMap::new(),
        nets: BTreeMap::new(),
        rules: DesignRules {
            via_at_smd: false,
            via_dia_nm: mm(0.45),
            via_drill_nm: mm(0.2),
            ..Default::default()
        },
        constraints: Constraints {
            cap_proximity: vec![CapProximityRule {
                cap_refdes: "C1".into(),
                ic_refdes: "U1".into(),
                net: String::new(),
                max_dist_nm: mm(3.0),
            }],
            ..Default::default()
        },
        graphics: Vec::new(),
        stackup: Default::default(),
        ..Default::default()
    };
    let board = BoardIr::from_spec(&spec).unwrap();

    let t0 = Instant::now();
    let summary = run_preflight(&board, &HashSet::new()).unwrap();
    let wall_ms = t0.elapsed().as_millis();
    assert!(wall_ms < 500, "preflight took {wall_ms}ms >= 500ms");
    assert!(!summary.ok);
    assert!(
        summary
            .layout_violations
            .iter()
            .any(|v| v.kind == ViolationKind::CourtyardOverlap),
        "expected CourtyardOverlap, got {:?}",
        summary.layout_violations
    );
    assert!(
        summary
            .layout_violations
            .iter()
            .any(|v| v.kind == ViolationKind::CapProximity),
        "expected CapProximity, got {:?}",
        summary.layout_violations
    );
    assert!(
        !summary.zone_chokes.is_empty(),
        "expected pinched zone choke"
    );
    assert!(
        !summary.sealed_pads.is_empty(),
        "expected sealed BGA pads, clusters={:?}",
        summary.bga_clusters
    );

    // Also smoke-check `boardc preflight --json` CLI.
    let tmp_dir = std::env::temp_dir().join("boardc_test_preflight_cli");
    std::fs::create_dir_all(&tmp_dir).unwrap();
    let board_path = tmp_dir.join("board.json");
    std::fs::write(&board_path, serde_json::to_string_pretty(&board).unwrap()).unwrap();
    let cli_out = Command::new(env!("CARGO_BIN_EXE_boardc"))
        .args(["preflight", board_path.to_str().unwrap(), "--json"])
        .output()
        .unwrap();
    assert!(cli_out.status.success());
    let cli_summary: PreflightSummary = serde_json::from_slice(&cli_out.stdout).unwrap();
    assert!(!cli_summary.ok);
    assert!(!cli_summary.sealed_pads.is_empty());
    assert!(!cli_summary.zone_chokes.is_empty());

    let _ = std::fs::remove_dir_all(&tmp_dir);
}

#[test]
fn preflight_catches_edge_keepout_blocked_net_and_fixed_violation() {
    let mut footprints = BTreeMap::new();
    footprints.insert("PAD".into(), term_fp(mm(0.5), &["F.Cu"]));

    let spec = BoardSpec {
        name: "preflight_extra".into(),
        outline: Vec::new(),
        size_nm: Some([mm(12.0), mm(8.0)]),
        cutouts: Vec::new(),
        layers: vec![
            layer_sig("F.Cu", Some(Side::Top)),
            layer_sig("B.Cu", Some(Side::Bottom)),
        ],
        footprints,
        placements: vec![
            place_1pin("P1", "PAD", (0.3, 4.0), Side::Top, "EDGE_NET"), // Violates 0.3mm edge clearance (center 0.3mm, r=0.25mm => 0.05mm).
            place_1pin("P2", "PAD", (3.0, 4.0), Side::Top, "BLK"),
            place_1pin("P3", "PAD", (9.0, 4.0), Side::Top, "BLK"),
        ],
        keepouts: vec![
            // Enclose P3 on all layers so BLK is statically blocked.
            Keepout {
                kind: KeepoutKind::All,
                layers: Vec::new(),
                poly: rect_pts(8.3, 3.3, 9.7, 4.7),
            },
        ],
        pours: Vec::new(),
        tracks: vec![
            // Two locked tracks on different nets overlapping on F.Cu => fixed_violations.
            Track {
                net: "FX_A".into(),
                layer: "F.Cu".into(),
                a: Pt::new(mm(4.0), mm(2.0)),
                b: Pt::new(mm(6.0), mm(2.0)),
                width: um(200.0),
                locked: true,
            },
            Track {
                net: "FX_B".into(),
                layer: "F.Cu".into(),
                a: Pt::new(mm(4.0), mm(2.1)),
                b: Pt::new(mm(6.0), mm(2.1)),
                width: um(200.0),
                locked: true,
            },
        ],
        vias: Vec::new(),
        net_classes: BTreeMap::new(),
        nets: BTreeMap::new(),
        rules: DesignRules::default(),
        constraints: Constraints::default(),
        graphics: Vec::new(),
        stackup: Default::default(),
        ..Default::default()
    };
    let board = BoardIr::from_spec(&spec).unwrap();
    let summary = run_preflight(&board, &HashSet::new()).unwrap();
    assert!(!summary.ok);
    assert!(
        summary
            .layout_violations
            .iter()
            .any(|v| v.kind == ViolationKind::EdgeClearance)
    );
    assert!(
        summary
            .layout_violations
            .iter()
            .any(|v| v.kind == ViolationKind::Keepout)
    );
    assert!(!summary.blocked_nets.is_empty());
    assert!(!summary.fixed_violations.is_empty());
}

#[test]
fn regression_ignore_and_reroute_preserve_unlocked_copper_and_copper_keepout_allows_tracks() {
    let mut footprints = BTreeMap::new();
    footprints.insert("PAD".into(), term_fp(mm(0.6), &["F.Cu"]));

    // Unlocked GND track runs horizontally across the middle; SIG_A and SIG_B must route around it
    // without wiping it when GND is ignored, and a pour-only KeepoutKind::Copper must NOT block tracks.
    let gnd_track = Track {
        net: "GND".into(),
        layer: "F.Cu".into(),
        a: Pt::new(mm(5.0), mm(8.0)),
        b: Pt::new(mm(15.0), mm(8.0)),
        width: mm(0.25),
        locked: false,
    };
    let spec = BoardSpec {
        name: "ignore_reroute_regression".into(),
        size_nm: Some([mm(22.0), mm(16.0)]),
        layers: vec![
            layer_sig("F.Cu", Some(Side::Top)),
            layer_sig("B.Cu", Some(Side::Bottom)),
        ],
        footprints,
        placements: vec![
            place_1pin("G1", "PAD", (5.0, 8.0), Side::Top, "GND"),
            place_1pin("G2", "PAD", (15.0, 8.0), Side::Top, "GND"),
            place_1pin("A1", "PAD", (8.0, 4.0), Side::Top, "SIG_A"),
            place_1pin("A2", "PAD", (8.0, 12.0), Side::Top, "SIG_A"),
            place_1pin("B1", "PAD", (12.0, 4.0), Side::Top, "SIG_B"),
            place_1pin("B2", "PAD", (12.0, 12.0), Side::Top, "SIG_B"),
        ],
        keepouts: vec![Keepout {
            kind: KeepoutKind::Copper,
            layers: vec!["F.Cu".into()],
            poly: rect_pts(6.0, 2.0, 14.0, 14.0),
        }],
        tracks: vec![gnd_track.clone()],
        rules: DesignRules::default(),
        ..Default::default()
    };
    let mut board = BoardIr::from_spec(&spec).unwrap();
    let cfg = RouteOptions {
        max_passes: 20,
        multi_start: 2,
        ignored_nets: HashSet::from(["GND".to_string()]),
        ..Default::default()
    };

    let sum1 = route_board_ir(&mut board, &cfg).unwrap();
    assert_eq!(sum1.unconnected_pairs, 0);
    assert!(
        board.tracks.contains(&gnd_track),
        "unlocked ignored-net track must be preserved after routing"
    );
    let drc1 = check_board_drc(&board);
    assert!(
        drc1.passed,
        "routed nets must clear unlocked ignored-net track: {:?}",
        drc1.violations
    );

    // Partial reroute of SIG_B must preserve both unlocked GND and unlocked SIG_A without shorts.
    let sig_a_before: Vec<Track> = board
        .tracks
        .iter()
        .filter(|t| t.net == "SIG_A")
        .cloned()
        .collect();
    let mut cfg2 = cfg.clone();
    cfg2.reroute_nets = Some(HashSet::from(["SIG_B".to_string()]));
    let sum2 = route_board_ir(&mut board, &cfg2).unwrap();
    assert_eq!(sum2.unconnected_pairs, 0);
    assert!(
        board.tracks.contains(&gnd_track),
        "unlocked ignored-net track must survive --reroute-nets"
    );
    let sig_a_after: Vec<Track> = board
        .tracks
        .iter()
        .filter(|t| t.net == "SIG_A")
        .cloned()
        .collect();
    assert_eq!(sig_a_before, sig_a_after);
    let drc2 = check_board_drc(&board);
    assert!(
        drc2.passed,
        "rerouted net must clear preserved unlocked tracks: {:?}",
        drc2.violations
    );
}

#[test]
fn route_outer_layer_mask_to_copper_bga_and_same_net() {
    let mut footprints = BTreeMap::new();
    let bga_pad = |name: &str, x_um: f64, y_um: f64| PadDef {
        name: name.into(),
        kind: PadKind::Smd,
        shape: PadShape::Circle {
            diameter: um(230.0),
        },
        pos: Pt::new(um(x_um), um(y_um)),
        rot_deg: 0.0,
        drill_nm: None,
        layers: vec!["F.Cu".into(), "F.Mask".into(), "F.Paste".into()],
        mask_margin_nm: None,
        paste_margin_nm: None,
    };
    footprints.insert(
        "BGA_04".into(),
        custom_fp(vec![
            bga_pad("A1", -200.0, 200.0),
            bga_pad("A2", 200.0, 200.0),
            bga_pad("B1", -200.0, -200.0),
            bga_pad("B2", 200.0, -200.0),
        ]),
    );
    footprints.insert("TERM".into(), term_fp(um(300.0), &["F.Cu", "F.Mask"]));

    // 2-layer board with track_width = 0.20mm, clearance = 0.09mm, mask_expansion = 0.03mm,
    // mask_to_copper = 0.08mm. BYPASS passes tightly near BGA A1/B1; A1 connects on F.Cu.
    let make_board = |m2c: Option<i64>| -> BoardIr {
        let spec = BoardSpec {
            name: "bga_m2c_route".into(),
            size_nm: Some([mm(12.0), mm(8.0)]),
            layers: vec![
                layer_sig("F.Cu", Some(Side::Top)),
                layer_sig("B.Cu", Some(Side::Bottom)),
            ],
            footprints: footprints.clone(),
            placements: vec![
                Placement {
                    refdes: "U1".into(),
                    footprint: "BGA_04".into(),
                    pos: Pt::new(mm(6.0), mm(4.0)),
                    locked: true,
                    pin_nets: BTreeMap::from([
                        ("A1".into(), "SIG_PAD".into()),
                        ("A2".into(), "ADJ_1".into()),
                        ("B1".into(), "ADJ_2".into()),
                        ("B2".into(), "ADJ_3".into()),
                    ]),
                    ..Default::default()
                },
                place_1pin("P_L", "TERM", (3.0, 4.2), Side::Top, "SIG_PAD"),
                place_1pin("P_B1", "TERM", (5.7, 2.5), Side::Top, "BYPASS"),
                place_1pin("P_B2", "TERM", (5.7, 5.5), Side::Top, "BYPASS"),
            ],
            keepouts: vec![Keepout {
                kind: KeepoutKind::All,
                layers: vec!["B.Cu".into()],
                poly: rect_pts(0.0, 0.0, 12.0, 8.0),
            }],
            rules: DesignRules {
                track_width_nm: um(200.0),
                clearance_nm: um(90.0),
                mask_expansion_nm: um(30.0),
                mask_to_copper_nm: m2c,
                ..Default::default()
            },
            ..Default::default()
        };
        BoardIr::from_spec(&spec).unwrap()
    };

    // Without mask_to_copper_nm, router hugs 90um copper clearance and fails post-hoc 80um m2c DRC.
    let mut unguarded = make_board(None);
    let vb_none = to_viahop_board(&unguarded, &HashSet::new(), None).unwrap();
    assert!(vb_none.planes.is_empty());
    let sum_none = route_board_ir(&mut unguarded, &RouteOptions::default()).unwrap();
    assert_eq!(sum_none.unconnected_pairs, 0);
    let mut check_m2c = unguarded.clone();
    check_m2c.rules.mask_to_copper_nm = Some(um(80.0));
    let post_hoc_drc = check_board_drc(&check_m2c);
    assert!(
        post_hoc_drc
            .violations
            .iter()
            .any(|v| v.kind == ViolationKind::SolderMaskToCopper),
        "expected unguarded route near 0.4mm BGA to fail post-hoc SolderMaskToCopper check"
    );

    // With mask_to_copper_nm = 80um, router keeps 110um copper clearance to adjacent mask openings
    // while still connecting same-net SIG_PAD into U1.A1.
    let mut guarded = make_board(Some(um(80.0)));
    let sum_guarded = route_board_ir(&mut guarded, &RouteOptions::default()).unwrap();
    assert_eq!(sum_guarded.unconnected_pairs, 0);
    let drc = check_board_drc(&guarded);
    assert!(
        drc.passed,
        "expected zero DRC violations with mask_to_copper_nm enabled, got {:?}",
        drc.violations
    );
}

#[test]
fn route_inner_layer_and_unmasked_pad_do_not_inflate_clearance() {
    // Two THT pads inside one footprint at x = 4.75mm and x = 5.25mm (500um pitch, 200um dia => 300um gap).
    // With track_width = 60um, clearance = 90um: 60 + 2*90 = 240um <= 300um (fits on In1.Cu).
    // With mask_expansion = 50um, mask_to_copper = 120um: 60 + 2*170 = 400um > 300um (blocked on F.Cu/B.Cu).
    let mut footprints = BTreeMap::new();
    let tht_pad = |name: &str, x_um: f64| PadDef {
        name: name.into(),
        kind: PadKind::Tht,
        shape: PadShape::Circle {
            diameter: um(200.0),
        },
        pos: Pt::new(um(x_um), 0),
        rot_deg: 0.0,
        drill_nm: Some(um(100.0)),
        layers: vec!["*.Cu".into(), "*.Mask".into()],
        mask_margin_nm: None,
        paste_margin_nm: None,
    };
    footprints.insert(
        "THT_WALL_PAIR".into(),
        custom_fp(vec![tht_pad("1", -250.0), tht_pad("2", 250.0)]),
    );
    footprints.insert("THT_SINGLE".into(), custom_fp(vec![tht_pad("1", 0.0)]));
    footprints.insert("SMD_NOMASK".into(), term_fp(um(200.0), &["F.Cu"]));

    let spec = BoardSpec {
        name: "inner_no_inflate".into(),
        size_nm: Some([mm(10.0), mm(8.0)]),
        layers: vec![
            layer_sig("F.Cu", Some(Side::Top)),
            layer_sig("In1.Cu", None),
            layer_sig("B.Cu", Some(Side::Bottom)),
        ],
        footprints,
        placements: vec![
            Placement {
                refdes: "W12".into(),
                footprint: "THT_WALL_PAIR".into(),
                pos: Pt::new(mm(5.0), mm(4.0)),
                locked: true,
                pin_nets: BTreeMap::from([
                    ("1".into(), "WALL_A".into()),
                    ("2".into(), "WALL_B".into()),
                ]),
                ..Default::default()
            },
            place_1pin("NM1", "SMD_NOMASK", (2.0, 4.0), Side::Top, "NM_A"),
            place_1pin("J1", "THT_SINGLE", (5.0, 2.0), Side::Top, "THRU"),
            place_1pin("J2", "THT_SINGLE", (5.0, 6.0), Side::Top, "THRU"),
        ],
        // Force THRU tracks onto In1.Cu through the x=4.75..5.25 corridor without blocking pads.
        keepouts: vec![
            Keepout {
                kind: KeepoutKind::Track,
                layers: vec!["F.Cu".into(), "B.Cu".into()],
                poly: rect_pts(0.0, 3.0, 10.0, 5.0),
            },
            Keepout {
                kind: KeepoutKind::Track,
                layers: vec!["In1.Cu".into()],
                poly: rect_pts(0.0, 3.0, 4.6, 5.0),
            },
            Keepout {
                kind: KeepoutKind::Track,
                layers: vec!["In1.Cu".into()],
                poly: rect_pts(5.4, 3.0, 10.0, 5.0),
            },
        ],
        rules: DesignRules {
            track_width_nm: um(60.0),
            clearance_nm: um(90.0),
            via_dia_nm: um(300.0),
            via_drill_nm: um(150.0),
            min_annular_ring_nm: um(50.0),
            mask_expansion_nm: um(50.0),
            mask_to_copper_nm: Some(um(120.0)),
            ..Default::default()
        },
        ..Default::default()
    };
    let mut board = BoardIr::from_spec(&spec).unwrap();
    let vboard = to_viahop_board(&board, &HashSet::new(), None).unwrap();
    // Only F.Cu (0) and B.Cu (2) should have mask guards; In1.Cu (1) must have none;
    // SMD_NOMASK (at x=2.0) must generate no mask guard at all.
    assert!(vboard.planes.iter().all(|p| p.layer == 0 || p.layer == 2));
    assert!(
        !vboard
            .planes
            .iter()
            .any(|p| { p.net_id == vboard.net_name_to_id.get("NM_A").copied() })
    );

    let summary = route_board_ir(&mut board, &RouteOptions::default()).unwrap();
    assert_eq!(summary.unconnected_pairs, 0);
    assert!(board.tracks.iter().any(|t| t.layer == "In1.Cu"));
    let drc = check_board_drc(&board);
    assert!(
        drc.passed,
        "expected clean DRC on inner-layer corridor: {:?}",
        drc.violations
    );
}

#[test]
fn route_bottom_side_bga_and_dense_4layer_bga_with_mask_to_copper() {
    let mut footprints = BTreeMap::new();
    footprints.insert(
        "U_BGA9".into(),
        FootprintSpec::Bga {
            rows: 3,
            cols: 3,
            pitch_nm: mm(0.4),
            ball_dia_nm: mm(0.23),
            pin_types: Default::default(),
            pin_names: BTreeMap::new(),
        },
    );
    footprints.insert("TERM".into(), term_fp(mm(0.3), &["F.Cu", "F.Mask"]));

    let rows = ["A", "B", "C"];
    let mut bga_nets = BTreeMap::new();
    let mut placements = Vec::new();
    for (ri, r) in rows.iter().enumerate() {
        for ci in 1..=3 {
            let pin = format!("{r}{ci}");
            let net = format!("N_{pin}");
            bga_nets.insert(pin.clone(), net.clone());
            placements.push(place_1pin(
                &format!("TP_{pin}"),
                "TERM",
                (2.0 + ci as f64 * 1.5, 2.0 + ri as f64 * 1.5),
                Side::Bottom,
                &net,
            ));
        }
    }
    placements.push(Placement {
        refdes: "U1".into(),
        footprint: "U_BGA9".into(),
        pos: Pt::new(mm(10.0), mm(5.0)),
        side: Side::Bottom,
        locked: true,
        pin_nets: bga_nets,
        ..Default::default()
    });

    let spec = BoardSpec {
        name: "dense_bottom_bga_m2c".into(),
        size_nm: Some([mm(15.0), mm(10.0)]),
        layers: vec![
            layer_sig("F.Cu", Some(Side::Top)),
            layer_sig("In1.Cu", None),
            layer_sig("In2.Cu", None),
            layer_sig("B.Cu", Some(Side::Bottom)),
        ],
        footprints,
        placements,
        rules: DesignRules {
            track_width_nm: um(80.0),
            clearance_nm: um(90.0),
            via_dia_nm: um(200.0),
            via_drill_nm: um(100.0),
            min_annular_ring_nm: um(50.0),
            hole_to_hole_nm: um(150.0),
            via_at_smd: true,
            mask_expansion_nm: um(30.0),
            mask_to_copper_nm: Some(um(80.0)),
            ..Default::default()
        },
        ..Default::default()
    };
    let mut board = BoardIr::from_spec(&spec).unwrap();
    let pre = run_preflight(&board, &HashSet::new()).unwrap();
    assert!(pre.ok, "expected preflight ok, got {pre:?}");

    let summary = route_board_ir(&mut board, &RouteOptions::default()).unwrap();
    assert_eq!(summary.unconnected_pairs, 0);
    let drc = check_board_drc(&board);
    assert!(
        drc.passed,
        "expected clean DRC for dense bottom-side 0.4mm BGA, got {:?}",
        drc.violations
    );
}

#[test]
fn route_mask_guards_cover_untented_via_npth_rect_and_per_pad_margin() {
    let mut footprints = BTreeMap::new();
    footprints.insert("TERM".into(), term_fp(mm(0.3), &["F.Cu", "F.Mask"]));
    footprints.insert(
        "RECT_OV".into(),
        custom_fp(vec![PadDef {
            name: "1".into(),
            kind: PadKind::Smd,
            shape: PadShape::Rect {
                width: um(240.0),
                height: um(240.0),
            },
            pos: Pt::new(0, 0),
            rot_deg: 0.0,
            drill_nm: None,
            layers: vec!["F.Cu".into(), "F.Mask".into()],
            mask_margin_nm: Some(um(60.0)),
            paste_margin_nm: None,
        }]),
    );
    footprints.insert(
        "NPTH_MASK".into(),
        custom_fp(vec![PadDef {
            name: "".into(),
            kind: PadKind::Npth,
            shape: PadShape::Circle { diameter: mm(1.2) },
            pos: Pt::new(0, 0),
            rot_deg: 0.0,
            drill_nm: Some(mm(0.8)),
            layers: vec!["*.Mask".into()],
            mask_margin_nm: None,
            paste_margin_nm: None,
        }]),
    );

    let spec = BoardSpec {
        name: "mask_variants".into(),
        size_nm: Some([mm(14.0), mm(10.0)]),
        layers: vec![
            layer_sig("F.Cu", Some(Side::Top)),
            layer_sig("B.Cu", Some(Side::Bottom)),
        ],
        footprints,
        placements: vec![
            place_1pin("P1", "TERM", (2.0, 5.0), Side::Top, "SIG"),
            place_1pin("P2", "TERM", (12.0, 5.0), Side::Top, "SIG"),
            place_1pin("R_OV", "RECT_OV", (5.0, 5.0), Side::Top, "AUX"),
            place_1pin("R_OV2", "TERM", (2.0, 2.0), Side::Top, "AUX"),
            place_1pin("H1", "NPTH_MASK", (9.0, 5.0), Side::Top, ""),
        ],
        nets: BTreeMap::from([(
            "AUX".into(),
            NetDef {
                name: "AUX".into(),
                ignore_route: true,
                ..Default::default()
            },
        )]),
        vias: vec![
            Via {
                net: String::new(),
                pos: Pt::new(mm(7.0), mm(5.0)),
                diameter: um(400.0),
                drill: um(200.0),
                layers: ("F.Cu".into(), "B.Cu".into()),
                locked: true,
                tented: false,
            },
            Via {
                net: "AUX".into(),
                pos: Pt::new(mm(7.0), mm(3.0)),
                diameter: um(400.0),
                drill: um(200.0),
                layers: ("F.Cu".into(), "B.Cu".into()),
                locked: true,
                tented: true,
            },
        ],
        // Force SIG through a tight F.Cu corridor directly past R_OV, the no-net untented via, and H1.
        keepouts: vec![
            Keepout {
                kind: KeepoutKind::Track,
                layers: vec!["B.Cu".into()],
                poly: rect_pts(0.0, 0.0, 14.0, 10.0),
            },
            Keepout {
                kind: KeepoutKind::Track,
                layers: vec!["F.Cu".into()],
                poly: rect_pts(0.0, 0.0, 14.0, 4.8),
            },
            Keepout {
                kind: KeepoutKind::Track,
                layers: vec!["F.Cu".into()],
                poly: rect_pts(0.0, 6.2, 14.0, 10.0),
            },
            Keepout {
                kind: KeepoutKind::Track,
                layers: vec!["F.Cu".into()],
                poly: rect_pts(4.3, 5.75, 7.7, 6.2),
            },
        ],
        rules: DesignRules {
            track_width_nm: um(150.0),
            clearance_nm: um(90.0),
            mask_expansion_nm: um(40.0),
            mask_to_copper_nm: Some(um(80.0)),
            ..Default::default()
        },
        ..Default::default()
    };

    let mut board = BoardIr::from_spec(&spec).unwrap();
    let pre = run_preflight(&board, &HashSet::new()).unwrap();
    assert!(
        pre.ok,
        "expected preflight ok with no-net untented via: {pre:?}"
    );
    assert!(pre.fixed_violations.is_empty());

    let vb = to_viahop_board(&board, &HashSet::new(), None).unwrap();
    // Untented via produces F.Cu + B.Cu guards; tented via at (7.0, 3.0) produces none.
    assert!(
        vb.planes.iter().any(|p| p.layer == 0
            && (p.bbox.min_x - 6770.0).abs() < 5.0
            && (p.bbox.min_y - 4770.0).abs() < 5.0),
        "expected untented via guard around (7000, 5000)"
    );
    assert!(
        !vb.planes
            .iter()
            .any(|p| p.bbox.min_y < 3500.0 && p.bbox.max_y > 2500.0),
        "tented via must not emit mask guard"
    );

    let summary = route_board_ir(&mut board, &RouteOptions::default()).unwrap();
    assert_eq!(summary.unconnected_pairs, 0);
    let drc = check_board_drc(&board);
    assert!(
        drc.violations
            .iter()
            .all(|v| v.kind == ViolationKind::ViaDangling
                && (v.item_a == "via#0" || v.item_a == "via#1")),
        "expected no non-dangling DRC errors across untented via, NPTH mask, and rect override, got {:?}",
        drc.violations
    );
}

#[test]
fn preflight_filters_mask_guard_false_positives_but_keeps_real_fixed_violations() {
    let mut footprints = BTreeMap::new();
    footprints.insert("TERM".into(), term_fp(mm(0.3), &["F.Cu", "F.Mask"]));

    let base_spec = || BoardSpec {
        name: "preflight_mask_filter".into(),
        size_nm: Some([mm(12.0), mm(8.0)]),
        layers: vec![
            layer_sig("F.Cu", Some(Side::Top)),
            layer_plane("In1.Cu"),
            layer_sig("B.Cu", Some(Side::Bottom)),
        ],
        footprints: footprints.clone(),
        placements: vec![
            place_1pin("P1", "TERM", (2.0, 4.0), Side::Top, "SIG"),
            place_1pin("P2", "TERM", (10.0, 4.0), Side::Top, "SIG"),
        ],
        vias: vec![Via {
            net: String::new(),
            pos: Pt::new(mm(6.0), mm(4.0)),
            diameter: um(400.0),
            drill: um(200.0),
            layers: ("F.Cu".into(), "B.Cu".into()),
            locked: true,
            tented: false,
        }],
        rules: DesignRules {
            track_width_nm: um(150.0),
            clearance_nm: um(90.0),
            mask_expansion_nm: um(40.0),
            mask_to_copper_nm: Some(um(80.0)),
            ..Default::default()
        },
        ..Default::default()
    };

    // No-net untented via must not self-collide with its synthetic mask guard FixedPlane.
    let clean_board = BoardIr::from_spec(&base_spec()).unwrap();
    let clean_pre = run_preflight(&clean_board, &HashSet::new()).unwrap();
    assert!(clean_pre.ok, "expected clean preflight, got {clean_pre:?}");
    assert!(clean_pre.fixed_violations.is_empty());

    // Real fixed-wire violation on F.Cu and real Plane violation on In1.Cu must still be reported.
    let mut bad_spec = base_spec();
    bad_spec.pours.push(PourSpec {
        name: "GND_IN1".into(),
        net: "GND".into(),
        layer: "In1.Cu".into(),
        outline: rect_pts(1.0, 1.0, 11.0, 7.0),
        ..Default::default()
    });
    bad_spec.tracks = vec![
        Track {
            net: "FX_A".into(),
            layer: "F.Cu".into(),
            a: Pt::new(mm(3.0), mm(2.0)),
            b: Pt::new(mm(5.0), mm(2.0)),
            width: um(200.0),
            locked: true,
        },
        Track {
            net: "FX_B".into(),
            layer: "F.Cu".into(),
            a: Pt::new(mm(3.0), mm(2.1)),
            b: Pt::new(mm(5.0), mm(2.1)),
            width: um(200.0),
            locked: true,
        },
        Track {
            net: "FX_A".into(),
            layer: "In1.Cu".into(),
            a: Pt::new(mm(3.0), mm(6.0)),
            b: Pt::new(mm(5.0), mm(6.0)),
            width: um(200.0),
            locked: true,
        },
    ];
    let bad_board = BoardIr::from_spec(&bad_spec).unwrap();
    let bad_pre = run_preflight(&bad_board, &HashSet::new()).unwrap();
    assert!(!bad_pre.ok);
    assert!(
        bad_pre
            .fixed_violations
            .iter()
            .any(|v| v.layer.as_deref() == Some("F.Cu") && v.blocker.starts_with("fixed_wire")),
        "expected F.Cu fixed_wire violation, got {:?}",
        bad_pre.fixed_violations
    );
}

#[test]
fn route_via_clearance_overrides_respect_mask_to_copper() {
    let mut footprints = BTreeMap::new();
    footprints.insert("TERM".into(), term_fp(mm(0.3), &["F.Cu", "F.Mask"]));
    footprints.insert(
        "BLK".into(),
        custom_fp(vec![PadDef {
            name: "1".into(),
            kind: PadKind::Smd,
            shape: PadShape::Rect {
                width: um(600.0),
                height: um(600.0),
            },
            pos: Pt::new(0, 0),
            rot_deg: 0.0,
            drill_nm: None,
            layers: vec!["F.Cu".into(), "F.Mask".into()],
            mask_margin_nm: None,
            paste_margin_nm: None,
        }]),
    );

    let spec = BoardSpec {
        name: "via_clr_override_m2c".into(),
        size_nm: Some([mm(14.0), mm(10.0)]),
        layers: vec![
            layer_sig("F.Cu", Some(Side::Top)),
            layer_sig("B.Cu", Some(Side::Bottom)),
        ],
        footprints,
        placements: vec![
            place_1pin("P1", "TERM", (2.0, 5.0), Side::Top, "SIG"),
            place_1pin("P2", "TERM", (12.0, 5.0), Side::Bottom, "SIG"),
            place_1pin("B1", "BLK", (7.0, 5.0), Side::Top, "OTHER"),
        ],
        // Force SIG through a tight F.Cu/B.Cu corridor past B1 so the transition via approaches B1.
        keepouts: vec![
            Keepout {
                kind: KeepoutKind::All,
                layers: vec!["F.Cu".into(), "B.Cu".into()],
                poly: rect_pts(0.0, 0.0, 14.0, 4.4),
            },
            Keepout {
                kind: KeepoutKind::All,
                layers: vec!["F.Cu".into(), "B.Cu".into()],
                poly: rect_pts(0.0, 6.2, 14.0, 10.0),
            },
        ],
        rules: DesignRules {
            track_width_nm: um(120.0),
            clearance_nm: um(150.0),
            via_dia_nm: um(300.0),
            via_drill_nm: um(150.0),
            min_annular_ring_nm: um(50.0),
            via_to_smd_nm: Some(um(60.0)),
            via_to_track_nm: Some(um(60.0)),
            via_to_via_nm: Some(um(60.0)),
            mask_expansion_nm: um(40.0),
            mask_to_copper_nm: Some(um(80.0)),
            ..Default::default()
        },
        ..Default::default()
    };

    let mut board = BoardIr::from_spec(&spec).unwrap();
    let summary = route_board_ir(&mut board, &RouteOptions::default()).unwrap();
    assert_eq!(summary.unconnected_pairs, 0);
    assert!(
        !board.vias.is_empty(),
        "top-to-bottom route must place a via"
    );
    let drc = check_board_drc(&board);
    assert!(
        drc.passed,
        "routed via with via_to_smd < clearance must pass SolderMaskToCopper DRC: {:?}",
        drc.violations
    );
}

#[test]
fn route_mask_solder_mask_defined_zero_m2c_roundrect_and_poly() {
    let mut footprints = BTreeMap::new();
    footprints.insert("TERM".into(), term_fp(mm(0.3), &["F.Cu", "F.Mask"]));
    footprints.insert(
        "SMD_NEG".into(),
        custom_fp(vec![PadDef {
            name: "1".into(),
            kind: PadKind::Smd,
            shape: PadShape::RoundRect {
                width: um(400.0),
                height: um(400.0),
                corner_radius: um(60.0),
            },
            pos: Pt::new(0, 0),
            rot_deg: 0.0,
            drill_nm: None,
            layers: vec!["F.Cu".into(), "F.Mask".into()],
            mask_margin_nm: Some(um(-50.0)),
            paste_margin_nm: None,
        }]),
    );
    footprints.insert(
        "POLY_L".into(),
        custom_fp(vec![PadDef {
            name: "1".into(),
            kind: PadKind::Smd,
            shape: PadShape::Poly {
                verts: vec![
                    Pt::new(um(-200.0), um(-200.0)),
                    Pt::new(um(200.0), um(-200.0)),
                    Pt::new(um(200.0), um(0.0)),
                    Pt::new(um(0.0), um(0.0)),
                    Pt::new(um(0.0), um(200.0)),
                    Pt::new(um(-200.0), um(200.0)),
                ],
            },
            pos: Pt::new(0, 0),
            rot_deg: 0.0,
            drill_nm: None,
            layers: vec!["F.Cu".into(), "F.Mask".into()],
            mask_margin_nm: Some(um(30.0)),
            paste_margin_nm: None,
        }]),
    );
    footprints.insert(
        "MASK_TINY".into(),
        custom_fp(vec![PadDef {
            name: "".into(),
            kind: PadKind::Smd,
            shape: PadShape::Circle { diameter: um(80.0) },
            pos: Pt::new(0, 0),
            rot_deg: 0.0,
            drill_nm: None,
            layers: vec!["F.Mask".into()],
            mask_margin_nm: Some(um(0.0)),
            paste_margin_nm: None,
        }]),
    );

    let make_board = |m2c: i64, clr: f64| -> BoardIr {
        let spec = BoardSpec {
            name: "mask_shapes_edge".into(),
            size_nm: Some([mm(14.0), mm(8.0)]),
            layers: vec![
                layer_sig("F.Cu", Some(Side::Top)),
                layer_sig("B.Cu", Some(Side::Bottom)),
            ],
            footprints: footprints.clone(),
            placements: vec![
                place_1pin("P1", "TERM", (2.0, 4.0), Side::Top, "SIG"),
                place_1pin("P2", "TERM", (12.0, 4.0), Side::Top, "SIG"),
                place_1pin("S_NEG", "SMD_NEG", (5.0, 4.0), Side::Top, "AUX_NEG"),
                place_1pin("S_POLY", "POLY_L", (7.5, 4.0), Side::Top, "AUX_POLY"),
                place_1pin("S_TINY", "MASK_TINY", (9.5, 4.0), Side::Top, ""),
            ],
            keepouts: vec![
                Keepout {
                    kind: KeepoutKind::Track,
                    layers: vec!["B.Cu".into()],
                    poly: rect_pts(0.0, 0.0, 14.0, 8.0),
                },
                Keepout {
                    kind: KeepoutKind::Track,
                    layers: vec!["F.Cu".into()],
                    poly: rect_pts(0.0, 0.0, 14.0, 3.8),
                },
            ],
            rules: DesignRules {
                track_width_nm: um(120.0),
                clearance_nm: um(clr),
                mask_expansion_nm: um(40.0),
                mask_to_copper_nm: Some(m2c),
                ..Default::default()
            },
            ..Default::default()
        };
        BoardIr::from_spec(&spec).unwrap()
    };

    // Negative mask margin (-50um) with m2c=80um, clr=90um (-50+80=30<=90) skips guard; tiny no-net mask-only uses Keepout.
    let mut b_neg = make_board(um(80.0), 90.0);
    let vb_neg = to_viahop_board(&b_neg, &HashSet::new(), None).unwrap();
    let neg_id = vb_neg.net_name_to_id.get("AUX_NEG").copied();
    assert!(
        !vb_neg.planes.iter().any(|p| p.net_id == neg_id),
        "solder-mask-defined pad already protected by copper must skip FixedPlane guard"
    );
    assert!(
        vb_neg
            .keepouts
            .iter()
            .any(|k| !k.is_structure && k.layers == vec![0] && (k.bbox.min_x - 9385.0).abs() < 5.0),
        "small no-net mask-only pad with delta_nm < 0 should emit Keepout instead of bloated FixedPlane"
    );
    let sum_neg = route_board_ir(&mut b_neg, &RouteOptions::default()).unwrap();
    assert_eq!(sum_neg.unconnected_pairs, 0);
    let drc_neg = check_board_drc(&b_neg);
    assert!(
        drc_neg.passed,
        "expected clean DRC: {:?}",
        drc_neg.violations
    );

    // Negative mask margin (-50um) with m2c=160um, clr=90um (-50+160=110>90, delta>0) emits RoundRect guard.
    let mut b_pos = make_board(um(160.0), 90.0);
    let vb_pos = to_viahop_board(&b_pos, &HashSet::new(), None).unwrap();
    let neg_id2 = vb_pos.net_name_to_id.get("AUX_NEG").copied();
    assert!(
        vb_pos.planes.iter().any(|p| p.net_id == neg_id2),
        "solder-mask-defined pad with margin + m2c > clr must emit guard"
    );
    let sum_pos = route_board_ir(&mut b_pos, &RouteOptions::default()).unwrap();
    assert_eq!(sum_pos.unconnected_pairs, 0);
    let drc_pos = check_board_drc(&b_pos);
    assert!(
        drc_pos.passed,
        "expected clean DRC: {:?}",
        drc_pos.violations
    );

    // Zero mask_to_copper_nm = Some(0) across RoundRect, Poly, and tiny mask-only pad.
    let mut b_zero = make_board(0, 90.0);
    let sum_zero = route_board_ir(&mut b_zero, &RouteOptions::default()).unwrap();
    assert_eq!(sum_zero.unconnected_pairs, 0);
    let drc_zero = check_board_drc(&b_zero);
    assert!(
        drc_zero.passed,
        "expected clean DRC with m2c=Some(0): {:?}",
        drc_zero.violations
    );
}

#[test]
fn route_prunes_dead_end_grid_snap_stubs_and_respects_guards() {
    let mk_tr = |net: &str, layer: &str, ax: f64, ay: f64, bx: f64, by: f64, locked: bool| Track {
        net: net.into(),
        layer: layer.into(),
        a: Pt::new(mm(ax), mm(ay)),
        b: Pt::new(mm(bx), mm(by)),
        width: um(200.0),
        locked,
    };
    let mk_via = |net: &str, x: f64, y: f64| Via {
        net: net.into(),
        pos: Pt::new(mm(x), mm(y)),
        diameter: um(400.0),
        drill: um(200.0),
        layers: ("F.Cu".into(), "B.Cu".into()),
        locked: false,
        tented: true,
    };

    let mut footprints = BTreeMap::new();
    footprints.insert("PAD".into(), term_fp(mm(0.6), &["F.Cu", "F.Mask"]));
    footprints.insert("PAD_B".into(), term_fp(mm(0.6), &["B.Cu", "B.Mask"]));

    let mut net_classes = BTreeMap::new();
    net_classes.insert(
        "FAST".into(),
        NetClass {
            name: "FAST".into(),
            track_width_nm: um(200.0),
            clearance_nm: um(150.0),
            via_dia_nm: um(400.0),
            via_drill_nm: um(200.0),
            diff_pair_width_nm: None,
            diff_pair_gap_nm: None,
        },
    );
    let mut nets = BTreeMap::new();
    nets.insert(
        "SIG_IGN".into(),
        NetDef {
            name: "SIG_IGN".into(),
            ignore_route: true,
            ..Default::default()
        },
    );
    nets.insert(
        "SIG_FAST".into(),
        NetDef {
            name: "SIG_FAST".into(),
            class: "FAST".into(),
            ..Default::default()
        },
    );

    let spec = BoardSpec {
        name: "stub_prune".into(),
        size_nm: Some([mm(26.0), mm(20.0)]),
        layers: vec![
            layer_sig("F.Cu", Some(Side::Top)),
            layer_sig("B.Cu", Some(Side::Bottom)),
        ],
        footprints,
        placements: vec![
            place_1pin("P_A1", "PAD", (4.0, 12.0), Side::Top, "SIG_A"),
            place_1pin("P_A2", "PAD", (14.0, 12.0), Side::Top, "SIG_A"),
            // SIG_VIA via at (4.05, 6.0) is > 0.3mm pad radius outside P_A3 at (3.0, 6.0).
            place_1pin("P_A3", "PAD", (3.0, 6.0), Side::Top, "SIG_VIA"),
            place_1pin("P_A4", "PAD_B", (10.0, 6.0), Side::Top, "SIG_VIA"),
            // SIG_TJ T-junction at (16.03, 6.0) is > 0.3mm pad radius outside P_A5 at (15.0, 6.0).
            place_1pin("P_A5", "PAD", (15.0, 6.0), Side::Top, "SIG_TJ"),
            place_1pin("P_A6", "PAD", (16.03, 3.0), Side::Top, "SIG_TJ"),
            place_1pin("P_B1", "PAD", (4.0, 15.0), Side::Top, "SIG_B"),
            place_1pin("P_B2", "PAD", (14.0, 15.0), Side::Top, "SIG_B"),
            // Copper-width bridges (anchor == None and anchor == Some).
            place_1pin("P_BR1", "PAD", (2.0, 9.0), Side::Top, "SIG_BR"),
            place_1pin("P_BR2", "PAD_B", (6.0, 9.0), Side::Top, "SIG_BR"),
            place_1pin("P_BA1", "PAD", (2.0, 7.5), Side::Top, "SIG_BA"),
            place_1pin("P_BA2", "PAD_B", (6.0, 7.5), Side::Top, "SIG_BA"),
            // Via whose B.Cu branch is only a 40um stub -> via pruned too.
            place_1pin("P_VP1", "PAD", (12.0, 2.0), Side::Top, "SIG_VP"),
            place_1pin("P_VP2", "PAD", (15.0, 2.0), Side::Top, "SIG_VP"),
            // Net-class reroute target.
            place_1pin("P_F1", "PAD", (4.0, 17.5), Side::Top, "SIG_FAST"),
            place_1pin("P_F2", "PAD", (14.0, 17.5), Side::Top, "SIG_FAST"),
        ],
        tracks: vec![
            mk_tr("SIG_A", "F.Cu", 4.0, 12.0, 14.0, 12.0, false),
            // Cascading 40um + 40um (= 80um) -> both pruned.
            mk_tr("SIG_A", "F.Cu", 10.0, 12.0, 10.0, 12.04, false),
            mk_tr("SIG_A", "F.Cu", 10.0, 12.04, 10.04, 12.04, false),
            // Cascading 45um + 45um (= 90um) -> tip pruned, base preserved.
            mk_tr("SIG_A", "F.Cu", 5.0, 12.0, 5.0, 12.045, false),
            mk_tr("SIG_A", "F.Cu", 5.0, 12.045, 5.045, 12.045, false),
            // Exact 80um stub -> pruned.
            mk_tr("SIG_A", "F.Cu", 8.0, 12.0, 8.0, 12.08, false),
            // 90um (> 80um) dead-end -> preserved.
            mk_tr("SIG_A", "F.Cu", 6.0, 12.0, 6.0, 12.09, false),
            // Locked 40um stub -> preserved.
            mk_tr("SIG_A", "F.Cu", 12.0, 12.0, 12.0, 11.96, true),
            // Ignored-net 40um stub -> preserved.
            mk_tr("SIG_IGN", "F.Cu", 2.0, 2.0, 2.04, 2.0, false),
            // SIG_VIA: trunk + 50um segment to via outside pad radius + B.Cu trunk -> preserved.
            mk_tr("SIG_VIA", "F.Cu", 3.0, 6.0, 4.0, 6.0, false),
            mk_tr("SIG_VIA", "F.Cu", 4.0, 6.0, 4.05, 6.0, false),
            mk_tr("SIG_VIA", "B.Cu", 4.05, 6.0, 10.0, 6.0, false),
            // SIG_TJ: trunk + 60um segment with locked T-junction outside pad radius -> preserved via track_has_external_touch(Some(anc)).
            mk_tr("SIG_TJ", "F.Cu", 15.0, 6.0, 16.0, 6.0, false),
            mk_tr("SIG_TJ", "F.Cu", 16.0, 6.0, 16.06, 6.0, false),
            mk_tr("SIG_TJ", "F.Cu", 16.03, 6.0, 16.03, 3.0, true),
            // SIG_BR: isolated 40um copper bridge (anchor == None) between P_BR1 (edge 2.30) and via (edge 2.36).
            mk_tr("SIG_BR", "F.Cu", 2.31, 9.0, 2.35, 9.0, false),
            mk_tr("SIG_BR", "B.Cu", 2.56, 9.0, 6.0, 9.0, false),
            // SIG_BA: anchored 40um copper bridge (anchor = (2.31, 7.5)) to via (edge 2.36).
            mk_tr("SIG_BA", "F.Cu", 2.0, 7.5, 2.31, 7.5, false),
            mk_tr("SIG_BA", "F.Cu", 2.31, 7.5, 2.35, 7.5, false),
            mk_tr("SIG_BA", "B.Cu", 2.56, 7.5, 6.0, 7.5, false),
            // SIG_VP: F.Cu trunk + via with 60um B.Cu dead-end stub exiting via rim -> stub & via pruned.
            mk_tr("SIG_VP", "F.Cu", 12.0, 2.0, 15.0, 2.0, false),
            mk_tr("SIG_VP", "B.Cu", 13.5, 2.18, 13.5, 2.24, false),
            // SIG_B & SIG_FAST trunks + 40um stubs.
            mk_tr("SIG_B", "F.Cu", 4.0, 15.0, 14.0, 15.0, false),
            mk_tr("SIG_B", "F.Cu", 9.0, 15.0, 9.0, 15.04, false),
            mk_tr("SIG_FAST", "F.Cu", 4.0, 17.5, 14.0, 17.5, false),
            mk_tr("SIG_FAST", "F.Cu", 9.0, 17.5, 9.0, 17.54, false),
        ],
        vias: vec![
            mk_via("SIG_VIA", 4.05, 6.0),
            mk_via("SIG_BR", 2.56, 9.0),
            mk_via("SIG_BA", 2.56, 7.5),
            mk_via("SIG_VP", 13.5, 2.0),
        ],
        net_classes,
        nets,
        rules: DesignRules {
            track_width_nm: um(150.0),
            clearance_nm: um(150.0),
            ..Default::default()
        },
        ..Default::default()
    };

    // 1. Rerouting by net class ("FAST") prunes SIG_FAST's stub while leaving SIG_B's stub untouched.
    let mut reroute_board = BoardIr::from_spec(&spec).unwrap();
    let sig_b_stub = mk_tr("SIG_B", "F.Cu", 9.0, 15.0, 9.0, 15.04, false);
    let sig_fast_stub = mk_tr("SIG_FAST", "F.Cu", 9.0, 17.5, 9.0, 17.54, false);
    let rr_sum = route_board_ir(
        &mut reroute_board,
        &RouteOptions {
            reroute_nets: Some(HashSet::from(["FAST".into()])),
            max_passes: 0,
            multi_start: 1,
            optimize: false,
            tune_match_groups: false,
            ..Default::default()
        },
    )
    .unwrap();
    assert_eq!(rr_sum.unconnected_pairs, 0);
    assert!(
        reroute_board.tracks.contains(&sig_b_stub),
        "non-target SIG_B stub must stay untouched during --reroute-nets FAST"
    );
    assert!(
        !reroute_board.tracks.contains(&sig_fast_stub),
        "target net class FAST stub must be pruned"
    );

    // 2. Resume mode (0 passes) prunes dead-end stubs + newly-dangling via and updates RouteSummary.
    let mut board = BoardIr::from_spec(&spec).unwrap();
    let sum = route_board_ir(
        &mut board,
        &RouteOptions {
            resume: true,
            max_passes: 0,
            multi_start: 1,
            optimize: false,
            tune_match_groups: false,
            ..Default::default()
        },
    )
    .unwrap();

    let unlocked_routed_after: Vec<&Track> = board
        .tracks
        .iter()
        .filter(|t| !t.locked && t.net != "SIG_IGN")
        .collect();
    let actual_len_um: f64 = unlocked_routed_after
        .iter()
        .map(|t| t.a.dist(t.b) / 1000.0)
        .sum();
    assert_eq!(sum.total_segments, unlocked_routed_after.len());
    assert_eq!(sum.total_vias, board.vias.len());
    assert_eq!(board.vias.len(), 3, "dangling SIG_VP via must be pruned");
    assert!(
        (sum.total_wirelength_um - actual_len_um).abs() < 1e-3,
        "expected summary wirelength {actual_len_um}, got {}",
        sum.total_wirelength_um
    );

    let has_seg = |ax: f64, ay: f64, bx: f64, by: f64| {
        board
            .tracks
            .iter()
            .any(|t| t.a == Pt::new(mm(ax), mm(ay)) && t.b == Pt::new(mm(bx), mm(by)))
    };
    // Pruned: 40+40um, tip of 45+45um, 80um, SIG_B stub, SIG_FAST stub, SIG_VP B.Cu stub.
    assert!(!has_seg(10.0, 12.0, 10.0, 12.04));
    assert!(!has_seg(10.0, 12.04, 10.04, 12.04));
    assert!(!has_seg(5.0, 12.045, 5.045, 12.045));
    assert!(!has_seg(8.0, 12.0, 8.0, 12.08));
    assert!(!has_seg(13.5, 2.18, 13.5, 2.24));
    assert!(!board.tracks.contains(&sig_b_stub));
    assert!(!board.tracks.contains(&sig_fast_stub));
    // Preserved: base of 45+45um, 90um, locked, ignored, SIG_VIA, SIG_TJ, and both copper bridges.
    assert!(
        has_seg(5.0, 12.0, 5.0, 12.045),
        "45+45um cap must preserve base"
    );
    assert!(has_seg(6.0, 12.0, 6.0, 12.09));
    assert!(has_seg(12.0, 12.0, 12.0, 11.96));
    assert!(board.tracks.iter().any(|t| t.net == "SIG_IGN"));
    assert!(
        has_seg(4.0, 6.0, 4.05, 6.0),
        "pad-to-via 50um segment must be preserved"
    );
    assert!(
        has_seg(16.0, 6.0, 16.06, 6.0),
        "SIG_TJ T-junction segment must be preserved"
    );
    assert!(
        has_seg(2.31, 9.0, 2.35, 9.0),
        "isolated copper-width bridge must be preserved"
    );
    assert!(
        has_seg(2.31, 7.5, 2.35, 7.5),
        "anchored copper-width bridge must be preserved"
    );
    assert!(
        check_board_drc(&board)
            .violations
            .iter()
            .all(|v| v.kind != ViolationKind::OpenNet && v.kind != ViolationKind::ViaDangling),
        "stub pruning must never leave OpenNet or ViaDangling"
    );

    // 3. Unfilled board.pours stub protection during route_board_ir (before fill_zones).
    let mut pour_fps = BTreeMap::new();
    pour_fps.insert("PAD".into(), term_fp(mm(0.6), &["F.Cu", "F.Mask"]));
    let pour_spec = BoardSpec {
        name: "unfilled_pour_stub".into(),
        size_nm: Some([mm(16.0), mm(10.0)]),
        layers: vec![
            layer_sig("F.Cu", Some(Side::Top)),
            layer_sig("B.Cu", Some(Side::Bottom)),
        ],
        footprints: pour_fps,
        placements: vec![
            place_1pin("P1", "PAD", (3.0, 5.0), Side::Top, "SIG_P"),
            place_1pin("P2", "PAD", (10.0, 5.0), Side::Top, "SIG_P"),
        ],
        pours: vec![PourSpec {
            name: "P_POUR".into(),
            net: "SIG_P".into(),
            layer: "F.Cu".into(),
            outline: rect_pts(6.0, 3.0, 12.0, 7.0),
            ..Default::default()
        }],
        tracks: vec![
            mk_tr("SIG_P", "F.Cu", 3.0, 5.0, 5.95, 5.0, false),
            // 40um segment ending 10um shy of pour edge at x=6.0; copper half-width bridges into unfilled pour.
            mk_tr("SIG_P", "F.Cu", 5.95, 5.0, 5.99, 5.0, false),
        ],
        ..Default::default()
    };
    let mut pour_board = BoardIr::from_spec(&pour_spec).unwrap();
    assert!(pour_board.filled_pours.is_empty());
    route_board_ir(
        &mut pour_board,
        &RouteOptions {
            resume: true,
            max_passes: 0,
            multi_start: 1,
            optimize: false,
            tune_match_groups: false,
            ..Default::default()
        },
    )
    .unwrap();
    assert!(
        pour_board
            .tracks
            .iter()
            .any(|t| t.a == Pt::new(mm(5.95), mm(5.0)) && t.b == Pt::new(mm(5.99), mm(5.0))),
        "40um bridge into unfilled board.pours must be preserved"
    );
}
