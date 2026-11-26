use std::collections::{BTreeMap, HashSet};
use std::process::Command;
use std::time::Instant;

use boardc::drc::{ViolationKind, check_board_drc};
use boardc::ir::{
    BoardIr, BoardSpec, CapProximityRule, Constraints, DesignRules, FootprintSpec, Keepout,
    KeepoutKind, LayerDef, LayerKind, MatchGroupRule, MaxLengthRule, NetClass, NetDef, PadDef,
    PadKind, PadShape, Placement, PourSpec, Pt, Side, Track, mm, um,
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
        cpl_rotation_offset_deg: 0.0,
    }
}

fn build_2layer_board() -> BoardIr {
    let mut footprints = BTreeMap::new();
    footprints.insert(
        "R_0603".into(),
        FootprintSpec::Chip2Pin {
            package: "0603".into(),
        },
    );
    footprints.insert(
        "U_QFN16".into(),
        FootprintSpec::Qfn {
            pins_per_side: 4,
            pitch_nm: mm(0.8),
            body_nm: mm(4.0),
            ep_nm: 0,
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
                cpl_rotation_offset_deg: 0.0,
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
}

#[test]
fn route_4layer_board_ir_with_inner_planes_clean_drc() {
    let mut footprints = BTreeMap::new();
    footprints.insert(
        "R_0603".into(),
        FootprintSpec::Chip2Pin {
            package: "0603".into(),
        },
    );
    let mut nets = BTreeMap::new();
    for (name, ignore) in [("GND", true), ("VCC", true)] {
        nets.insert(
            name.into(),
            NetDef {
                name: name.into(),
                class: "Default".into(),
                pins: Vec::new(),
                ignore_route: ignore,
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
                pins: Vec::new(),
                ignore_route: false,
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
        },
    );
    footprints.insert(
        "U_BGA9".into(),
        FootprintSpec::Bga {
            rows: 3,
            cols: 3,
            pitch_nm: mm(0.5),
            ball_dia_nm: mm(0.3),
        },
    );
    footprints.insert(
        "PAD".into(),
        FootprintSpec::Custom {
            pads: vec![PadDef {
                name: "1".into(),
                kind: PadKind::Smd,
                shape: PadShape::Circle { diameter: mm(0.4) },
                pos: Pt::new(0, 0),
                rot_deg: 0.0,
                drill_nm: None,
                layers: vec!["F.Cu".into()],
                mask_margin_nm: None,
                paste_margin_nm: None,
            }],
            courtyard: Vec::new(),
            graphics: Vec::new(),
            keepouts: Vec::new(),
        },
    );

    // Connect all 9 balls of the 0.5mm-pitch BGA so center ball B2 is surrounded and sealed.
    let rows = ["A", "B", "C"];
    let mut bga_nets = BTreeMap::new();
    let mut placements = Vec::new();
    for (ri, r) in rows.iter().enumerate() {
        for ci in 1..=3 {
            let pin = format!("{r}{ci}");
            let net = format!("BGA_{pin}");
            bga_nets.insert(pin.clone(), net.clone());
            let mut pn = BTreeMap::new();
            pn.insert("1".into(), net);
            placements.push(Placement {
                refdes: format!("TP_{pin}"),
                footprint: "PAD".into(),
                value: String::new(),
                pos: Pt::new(mm(1.5 + (ci as f64) * 1.1), mm(1.5 + (ri as f64) * 1.1)),
                rot_deg: 0.0,
                side: Side::Top,
                locked: false,
                pin_nets: pn,
                mpn: None,
                lcsc: None,
                dnp: false,
                cpl_rotation_offset_deg: 0.0,
            });
        }
    }
    placements.push(Placement {
        refdes: "U1".into(),
        footprint: "U_BGA9".into(),
        value: "BGA9".into(),
        pos: Pt::new(mm(8.0), mm(4.0)),
        rot_deg: 0.0,
        side: Side::Top,
        locked: false,
        pin_nets: bga_nets,
        mpn: None,
        lcsc: None,
        dnp: false,
        cpl_rotation_offset_deg: 0.0,
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
    placements.push(Placement {
        refdes: "G1".into(),
        footprint: "PAD".into(),
        value: String::new(),
        pos: Pt::new(mm(3.0), mm(9.5)),
        rot_deg: 0.0,
        side: Side::Bottom,
        locked: false,
        pin_nets: BTreeMap::from([("1".into(), "GND".into())]),
        mpn: None,
        lcsc: None,
        dnp: false,
        cpl_rotation_offset_deg: 0.0,
    });
    placements.push(Placement {
        refdes: "G2".into(),
        footprint: "PAD".into(),
        value: String::new(),
        pos: Pt::new(mm(11.0), mm(9.5)),
        rot_deg: 0.0,
        side: Side::Bottom,
        locked: false,
        pin_nets: BTreeMap::from([("1".into(), "GND".into())]),
        mpn: None,
        lcsc: None,
        dnp: false,
        cpl_rotation_offset_deg: 0.0,
    });

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
    footprints.insert(
        "PAD".into(),
        FootprintSpec::Custom {
            pads: vec![PadDef {
                name: "1".into(),
                kind: PadKind::Smd,
                shape: PadShape::Circle { diameter: mm(0.5) },
                pos: Pt::new(0, 0),
                rot_deg: 0.0,
                drill_nm: None,
                layers: vec!["F.Cu".into()],
                mask_margin_nm: None,
                paste_margin_nm: None,
            }],
            courtyard: Vec::new(),
            graphics: Vec::new(),
            keepouts: Vec::new(),
        },
    );

    let place = |refdes: &str, x_mm: f64, y_mm: f64, net: &str| Placement {
        refdes: refdes.into(),
        footprint: "PAD".into(),
        value: String::new(),
        pos: Pt::new(mm(x_mm), mm(y_mm)),
        rot_deg: 0.0,
        side: Side::Top,
        locked: false,
        pin_nets: BTreeMap::from([("1".into(), net.into())]),
        mpn: None,
        lcsc: None,
        dnp: false,
        cpl_rotation_offset_deg: 0.0,
    };

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
            place("P1", 0.3, 4.0, "EDGE_NET"), // Violates 0.3mm edge clearance (center 0.3mm, r=0.25mm => 0.05mm).
            place("P2", 3.0, 4.0, "BLK"),
            place("P3", 9.0, 4.0, "BLK"),
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
    footprints.insert(
        "PAD".into(),
        FootprintSpec::Custom {
            pads: vec![PadDef {
                name: "1".into(),
                kind: PadKind::Smd,
                shape: PadShape::Circle { diameter: mm(0.6) },
                pos: Pt::new(0, 0),
                rot_deg: 0.0,
                drill_nm: None,
                layers: vec!["F.Cu".into()],
                mask_margin_nm: None,
                paste_margin_nm: None,
            }],
            courtyard: Vec::new(),
            graphics: Vec::new(),
            keepouts: Vec::new(),
        },
    );
    let pad = |refdes: &str, x: f64, y: f64, net: &str| Placement {
        refdes: refdes.into(),
        footprint: "PAD".into(),
        pos: Pt::new(mm(x), mm(y)),
        locked: true,
        pin_nets: BTreeMap::from([("1".into(), net.into())]),
        ..Default::default()
    };

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
            pad("G1", 5.0, 8.0, "GND"),
            pad("G2", 15.0, 8.0, "GND"),
            pad("A1", 8.0, 4.0, "SIG_A"),
            pad("A2", 8.0, 12.0, "SIG_A"),
            pad("B1", 12.0, 4.0, "SIG_B"),
            pad("B2", 12.0, 12.0, "SIG_B"),
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
