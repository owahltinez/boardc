use std::collections::BTreeMap;
use std::fs;
use std::path::PathBuf;
use std::process::Command;

use boardc::drc::{ViolationKind, check_board_drc};
use boardc::ir::{
    BoardIr, BoardSpec, Constraints, DesignRules, FootprintSpec, Keepout, KeepoutKind, LayerDef,
    LayerKind, MatchGroupRule, MaxLengthRule, PadDef, PadKind, PadShape, Placement, PourSpec, Pt,
    Side, Track, Via, mm,
};
use boardc::pipeline::{BuildOptions, compile_board};
use boardc::route::{RouteOptions, route_board_ir};
use boardc::tune::{TuneOptions, TuneReport, tune_match_groups};
use boardc::zones::fill_zones;

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

fn pad_fp(dia_mm: f64) -> FootprintSpec {
    FootprintSpec::Custom {
        pads: vec![PadDef {
            name: "1".into(),
            kind: PadKind::Smd,
            shape: PadShape::Circle {
                diameter: mm(dia_mm),
            },
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
        pin_types: Default::default(),
    }
}

fn place_pad(refdes: &str, fp: &str, x_mm: f64, y_mm: f64, net: &str) -> Placement {
    Placement {
        refdes: refdes.into(),
        footprint: fp.into(),
        pos: Pt::new(mm(x_mm), mm(y_mm)),
        locked: true,
        pin_nets: BTreeMap::from([("1".into(), net.into())]),
        ..Default::default()
    }
}

fn net_length_nm(board: &BoardIr, net: &str) -> f64 {
    board
        .tracks
        .iter()
        .filter(|t| t.net == net)
        .map(|t| t.a.dist(t.b))
        .sum()
}

fn qspi_board() -> BoardIr {
    let mut footprints = BTreeMap::new();
    footprints.insert("PAD".into(), pad_fp(0.5));
    footprints.insert(
        "U_HOST".into(),
        FootprintSpec::PinHeader {
            pins: 6,
            pitch_nm: mm(1.5),
            pin_types: Default::default(),
        },
    );

    let qspi_nets = [
        "QSPI_CLK", "QSPI_NCS", "QSPI_IO0", "QSPI_IO1", "QSPI_IO2", "QSPI_IO3",
    ];
    let host_nets: BTreeMap<String, String> = qspi_nets
        .iter()
        .enumerate()
        .map(|(i, n)| ((i + 1).to_string(), (*n).into()))
        .collect();
    let mut placements = vec![Placement {
        refdes: "U1".into(),
        footprint: "U_HOST".into(),
        pos: Pt::new(mm(8.0), mm(18.0)),
        locked: true,
        pin_nets: host_nets,
        ..Default::default()
    }];

    // Stagger destination X coordinates from 14 mm to 34 mm so initial skew exceeds 15 mm.
    let target_x = [14.0, 17.0, 21.0, 25.0, 29.0, 34.0];
    for (i, (&net, &tx)) in qspi_nets.iter().zip(&target_x).enumerate() {
        let ty = 14.0 + (i as f64) * 1.6;
        placements.push(place_pad(&format!("J{}", i + 1), "PAD", tx, ty, net));
    }

    let spec = BoardSpec {
        name: "qspi_bus".into(),
        size_nm: Some([mm(44.0), mm(36.0)]),
        layers: vec![
            layer_sig("F.Cu", Some(Side::Top)),
            layer_sig("B.Cu", Some(Side::Bottom)),
        ],
        footprints,
        placements,
        pours: vec![PourSpec {
            name: "GND_BOT".into(),
            net: "GND".into(),
            layer: "B.Cu".into(),
            outline: rect_pts(1.0, 1.0, 43.0, 35.0),
            ..Default::default()
        }],
        rules: DesignRules {
            track_width_nm: mm(0.15),
            clearance_nm: mm(0.15),
            ..Default::default()
        },
        constraints: Constraints {
            match_groups: vec![MatchGroupRule {
                name: "QSPI".into(),
                nets: qspi_nets.iter().map(|s| (*s).into()).collect(),
                tolerance_nm: 3_000_000,
                target_nm: None,
            }],
            ..Default::default()
        },
        ..Default::default()
    };
    BoardIr::from_spec(&spec).unwrap()
}

#[test]
fn qspi_6wire_bus_route_tune_zones_drc_clean() {
    let mut untuned = qspi_board();
    let untuned_sum = route_board_ir(
        &mut untuned,
        &RouteOptions {
            tune_match_groups: false,
            ..Default::default()
        },
    )
    .unwrap();
    assert_eq!(untuned_sum.unconnected_pairs, 0);
    let untuned_drc = check_board_drc(&untuned);
    assert!(
        untuned_drc
            .violations
            .iter()
            .any(|v| v.kind == ViolationKind::MatchGroup),
        "expected > 3mm natural QSPI skew before tuning"
    );

    let mut board = qspi_board();
    let summary = route_board_ir(&mut board, &RouteOptions::default()).unwrap();
    assert_eq!(summary.unconnected_pairs, 0);
    assert_eq!(
        summary.length_violations, 0,
        "expected RouteSummary.length_violations == 0 after tuning"
    );

    let _zone_report = fill_zones(&mut board);
    let drc = check_board_drc(&board);
    assert!(
        drc.passed,
        "expected clean DRC after route+tune+zones, got {:?}",
        drc.violations
    );
    for kind in [
        ViolationKind::MatchGroup,
        ViolationKind::Clearance,
        ViolationKind::OpenNet,
        ViolationKind::EdgeClearance,
    ] {
        assert!(
            !drc.violations.iter().any(|v| v.kind == kind),
            "unexpected {kind:?} violation: {:?}",
            drc.violations
        );
    }
}

#[test]
fn explicit_target_nm_lengthens_short_net_and_preserves_in_tolerance_net() {
    let mut footprints = BTreeMap::new();
    footprints.insert("PAD".into(), pad_fp(0.5));

    let spec = BoardSpec {
        name: "explicit_target".into(),
        size_nm: Some([mm(40.0), mm(20.0)]),
        layers: vec![
            layer_sig("F.Cu", Some(Side::Top)),
            layer_sig("B.Cu", Some(Side::Bottom)),
        ],
        footprints,
        placements: vec![
            place_pad("P1", "PAD", 4.0, 14.0, "SIG_SHORT"),
            place_pad("P2", "PAD", 14.0, 14.0, "SIG_SHORT"),
            place_pad("P3", "PAD", 4.0, 6.0, "SIG_OK"),
            place_pad("P4", "PAD", 28.5, 6.0, "SIG_OK"),
        ],
        tracks: vec![
            Track {
                net: "SIG_SHORT".into(),
                layer: "F.Cu".into(),
                a: Pt::new(mm(4.0), mm(14.0)),
                b: Pt::new(mm(14.0), mm(14.0)),
                width: mm(0.15),
                locked: false,
            },
            Track {
                net: "SIG_OK".into(),
                layer: "F.Cu".into(),
                a: Pt::new(mm(4.0), mm(6.0)),
                b: Pt::new(mm(28.5), mm(6.0)),
                width: mm(0.15),
                locked: false,
            },
        ],
        constraints: Constraints {
            match_groups: vec![MatchGroupRule {
                name: "TARGET_BUS".into(),
                nets: vec!["SIG_SHORT".into(), "SIG_OK".into()],
                tolerance_nm: mm(1.0),
                target_nm: Some(mm(25.0)),
            }],
            ..Default::default()
        },
        ..Default::default()
    };
    let mut board = BoardIr::from_spec(&spec).unwrap();
    let ok_before: Vec<Track> = board
        .tracks
        .iter()
        .filter(|t| t.net == "SIG_OK")
        .cloned()
        .collect();

    let report = tune_match_groups(&mut board, &TuneOptions::default());
    assert!(report.ok, "expected tune ok, got {report:?}");
    assert_eq!(report.tuned_nets, 1);

    let short_len = net_length_nm(&board, "SIG_SHORT");
    assert!(
        short_len >= mm(24.0) as f64 - 1.0 && short_len <= mm(26.0) as f64 + 1.0,
        "SIG_SHORT len {short_len} outside [24mm, 26mm]"
    );
    let ok_after: Vec<Track> = board
        .tracks
        .iter()
        .filter(|t| t.net == "SIG_OK")
        .cloned()
        .collect();
    assert_eq!(
        ok_before, ok_after,
        "in-tolerance net must remain untouched"
    );

    let drc = check_board_drc(&board);
    assert!(drc.passed, "expected clean DRC, got {:?}", drc.violations);
}

#[test]
fn tight_obstacles_flip_side_and_shrink_amplitude_without_drc_violation() {
    let mut footprints = BTreeMap::new();
    footprints.insert("PAD".into(), pad_fp(0.5));
    footprints.insert(
        "NPTH".into(),
        FootprintSpec::Custom {
            pads: vec![PadDef {
                name: "1".into(),
                kind: PadKind::Npth,
                shape: PadShape::Circle { diameter: mm(0.8) },
                pos: Pt::new(0, 0),
                rot_deg: 0.0,
                drill_nm: Some(mm(0.8)),
                layers: Vec::new(),
                mask_margin_nm: None,
                paste_margin_nm: None,
            }],
            courtyard: Vec::new(),
            graphics: Vec::new(),
            keepouts: Vec::new(),
            pin_types: Default::default(),
        },
    );

    // Net A at y=16mm is blocked on +v (above) by a Track keepout and via, so it must flip to -v.
    // Net B at y=8mm sits inside a 1.4mm channel between two locked tracks and near an NPTH drill.
    let spec = BoardSpec {
        name: "obstacle_channel".into(),
        size_nm: Some([mm(32.0), mm(22.0)]),
        layers: vec![
            layer_sig("F.Cu", Some(Side::Top)),
            layer_sig("B.Cu", Some(Side::Bottom)),
        ],
        footprints,
        placements: vec![
            place_pad("A1", "PAD", 4.0, 16.0, "SIG_A"),
            place_pad("A2", "PAD", 20.0, 16.0, "SIG_A"),
            place_pad("B1", "PAD", 4.0, 8.0, "SIG_B"),
            place_pad("B2", "PAD", 20.0, 8.0, "SIG_B"),
            place_pad("R1", "PAD", 4.0, 2.0, "SIG_REF"),
            place_pad("R2", "PAD", 27.0, 2.0, "SIG_REF"),
            place_pad("W1", "PAD", 4.0, 9.0, "WALL_TOP"),
            place_pad("W2", "PAD", 20.0, 9.0, "WALL_TOP"),
            place_pad("W3", "PAD", 4.0, 7.0, "WALL_BOT"),
            place_pad("W4", "PAD", 20.0, 7.0, "WALL_BOT"),
            Placement {
                refdes: "H1".into(),
                footprint: "NPTH".into(),
                pos: Pt::new(mm(12.0), mm(14.5)),
                locked: true,
                ..Default::default()
            },
        ],
        keepouts: vec![Keepout {
            kind: KeepoutKind::Track,
            layers: vec!["F.Cu".into()],
            poly: rect_pts(3.0, 16.25, 21.0, 20.0),
        }],
        vias: vec![Via {
            net: "WALL_TOP".into(),
            pos: Pt::new(mm(12.0), mm(16.4)),
            diameter: mm(0.5),
            drill: mm(0.25),
            layers: ("F.Cu".into(), "B.Cu".into()),
            locked: true,
            tented: true,
        }],
        tracks: vec![
            Track {
                net: "SIG_A".into(),
                layer: "F.Cu".into(),
                a: Pt::new(mm(4.0), mm(16.0)),
                b: Pt::new(mm(20.0), mm(16.0)),
                width: mm(0.15),
                locked: false,
            },
            Track {
                net: "SIG_B".into(),
                layer: "F.Cu".into(),
                a: Pt::new(mm(4.0), mm(8.0)),
                b: Pt::new(mm(20.0), mm(8.0)),
                width: mm(0.15),
                locked: false,
            },
            Track {
                net: "SIG_REF".into(),
                layer: "F.Cu".into(),
                a: Pt::new(mm(4.0), mm(2.0)),
                b: Pt::new(mm(27.0), mm(2.0)),
                width: mm(0.15),
                locked: true,
            },
            Track {
                net: "WALL_TOP".into(),
                layer: "F.Cu".into(),
                a: Pt::new(mm(4.0), mm(9.0)),
                b: Pt::new(mm(20.0), mm(9.0)),
                width: mm(0.2),
                locked: true,
            },
            Track {
                net: "WALL_BOT".into(),
                layer: "F.Cu".into(),
                a: Pt::new(mm(4.0), mm(7.0)),
                b: Pt::new(mm(20.0), mm(7.0)),
                width: mm(0.2),
                locked: true,
            },
        ],
        rules: DesignRules {
            track_width_nm: mm(0.15),
            clearance_nm: mm(0.15),
            hole_clearance_nm: Some(mm(0.25)),
            ..Default::default()
        },
        constraints: Constraints {
            match_groups: vec![MatchGroupRule {
                name: "OBS_MATCH".into(),
                nets: vec!["SIG_A".into(), "SIG_B".into(), "SIG_REF".into()],
                tolerance_nm: mm(0.5),
                target_nm: None,
            }],
            ..Default::default()
        },
        ..Default::default()
    };
    let mut board = BoardIr::from_spec(&spec).unwrap();
    let report = tune_match_groups(&mut board, &TuneOptions::default());
    assert!(report.ok, "expected tune ok in tight channel: {report:?}");

    assert!(
        board
            .tracks
            .iter()
            .filter(|t| t.net == "SIG_A")
            .all(|t| t.a.y <= mm(16.0) && t.b.y <= mm(16.0)),
        "SIG_A must flip to -v side below y=16mm"
    );
    assert!(
        board
            .tracks
            .iter()
            .filter(|t| t.net == "SIG_B")
            .all(|t| t.a.y > mm(7.0) && t.a.y < mm(9.0) && t.b.y > mm(7.0) && t.b.y < mm(9.0)),
        "SIG_B must stay strictly inside [7mm, 9mm] channel"
    );

    let drc = check_board_drc(&board);
    assert!(
        drc.passed,
        "expected zero DRC violations in tight channel, got {:?}",
        drc.violations
    );
}

#[test]
fn max_length_rule_caps_tuning_and_respects_multiple_match_groups() {
    let mut footprints = BTreeMap::new();
    footprints.insert("PAD".into(), pad_fp(0.5));

    let spec = BoardSpec {
        name: "max_cap".into(),
        size_nm: Some([mm(40.0), mm(20.0)]),
        layers: vec![
            layer_sig("F.Cu", Some(Side::Top)),
            layer_sig("B.Cu", Some(Side::Bottom)),
        ],
        footprints,
        placements: vec![
            place_pad("A1", "PAD", 4.0, 14.0, "NET_CAPPED"),
            place_pad("A2", "PAD", 16.0, 14.0, "NET_CAPPED"),
            place_pad("B1", "PAD", 4.0, 10.0, "NET_MULTI"),
            place_pad("B2", "PAD", 16.0, 10.0, "NET_MULTI"),
            place_pad("R1", "PAD", 4.0, 5.0, "NET_LONG"),
            place_pad("R2", "PAD", 30.0, 5.0, "NET_LONG"),
        ],
        tracks: vec![
            Track {
                net: "NET_CAPPED".into(),
                layer: "F.Cu".into(),
                a: Pt::new(mm(4.0), mm(14.0)),
                b: Pt::new(mm(16.0), mm(14.0)),
                width: mm(0.15),
                locked: false,
            },
            Track {
                net: "NET_MULTI".into(),
                layer: "F.Cu".into(),
                a: Pt::new(mm(4.0), mm(10.0)),
                b: Pt::new(mm(16.0), mm(10.0)),
                width: mm(0.15),
                locked: false,
            },
            Track {
                net: "NET_LONG".into(),
                layer: "F.Cu".into(),
                a: Pt::new(mm(4.0), mm(5.0)),
                b: Pt::new(mm(30.0), mm(5.0)),
                width: mm(0.15),
                locked: true,
            },
        ],
        constraints: Constraints {
            max_lengths: vec![MaxLengthRule {
                nets: vec!["NET_CAPPED".into()],
                max_nm: mm(20.0),
            }],
            match_groups: vec![
                MatchGroupRule {
                    name: "G_WIDE".into(),
                    nets: vec!["NET_CAPPED".into(), "NET_MULTI".into(), "NET_LONG".into()],
                    tolerance_nm: mm(1.0),
                    target_nm: None,
                },
                MatchGroupRule {
                    name: "G_TIGHT".into(),
                    nets: vec!["NET_MULTI".into()],
                    tolerance_nm: mm(1.0),
                    target_nm: Some(mm(18.0)),
                },
            ],
            ..Default::default()
        },
        ..Default::default()
    };
    let mut board = BoardIr::from_spec(&spec).unwrap();
    let report = tune_match_groups(&mut board, &TuneOptions::default());
    assert!(!report.ok);
    assert!(report.unmet_nets.contains(&"NET_CAPPED".to_string()));

    let capped_len = net_length_nm(&board, "NET_CAPPED");
    assert!(
        capped_len <= mm(20.0) as f64 + 1.0,
        "NET_CAPPED exceeded max_nm 20mm: {capped_len}"
    );
    let multi_len = net_length_nm(&board, "NET_MULTI");
    assert!(
        multi_len <= mm(19.0) as f64 + 1.0,
        "NET_MULTI exceeded G_TIGHT upper bound 19mm: {multi_len}"
    );

    let drc = check_board_drc(&board);
    assert!(
        !drc.violations
            .iter()
            .any(|v| v.kind == ViolationKind::MaxLength),
        "tuning must never create MaxLength violation: {:?}",
        drc.violations
    );
    assert!(
        !drc.violations
            .iter()
            .any(|v| v.kind == ViolationKind::Clearance || v.kind == ViolationKind::OpenNet),
        "unexpected clearance/open violations: {:?}",
        drc.violations
    );
}

#[test]
fn locked_tracks_preserved_cli_tune_and_full_build_pipeline() {
    let mut footprints = BTreeMap::new();
    footprints.insert("PAD".into(), pad_fp(0.5));
    let mut board = BoardIr::from_spec(&BoardSpec {
        name: "locked_and_cli".into(),
        size_nm: Some([mm(36.0), mm(20.0)]),
        layers: vec![
            layer_sig("F.Cu", Some(Side::Top)),
            layer_sig("B.Cu", Some(Side::Bottom)),
        ],
        footprints,
        placements: vec![
            place_pad("L1", "PAD", 4.0, 15.0, "SIG_LOCKED"),
            place_pad("L2", "PAD", 12.0, 15.0, "SIG_LOCKED"),
            place_pad("U1", "PAD", 4.0, 10.0, "SIG_FREE"),
            place_pad("U2", "PAD", 14.0, 10.0, "SIG_FREE"),
            place_pad("R1", "PAD", 4.0, 5.0, "SIG_REF"),
            place_pad("R2", "PAD", 26.0, 5.0, "SIG_REF"),
        ],
        tracks: vec![
            Track {
                net: "SIG_LOCKED".into(),
                layer: "F.Cu".into(),
                a: Pt::new(mm(4.0), mm(15.0)),
                b: Pt::new(mm(12.0), mm(15.0)),
                width: mm(0.15),
                locked: true,
            },
            Track {
                net: "SIG_FREE".into(),
                layer: "F.Cu".into(),
                a: Pt::new(mm(4.0), mm(10.0)),
                b: Pt::new(mm(14.0), mm(10.0)),
                width: mm(0.15),
                locked: false,
            },
            Track {
                net: "SIG_REF".into(),
                layer: "F.Cu".into(),
                a: Pt::new(mm(4.0), mm(5.0)),
                b: Pt::new(mm(26.0), mm(5.0)),
                width: mm(0.15),
                locked: true,
            },
        ],
        constraints: Constraints {
            match_groups: vec![MatchGroupRule {
                name: "BUS".into(),
                nets: vec!["SIG_LOCKED".into(), "SIG_FREE".into(), "SIG_REF".into()],
                tolerance_nm: mm(1.0),
                target_nm: None,
            }],
            ..Default::default()
        },
        ..Default::default()
    })
    .unwrap();

    let locked_before: Vec<Track> = board.tracks.iter().filter(|t| t.locked).cloned().collect();
    let _rep = tune_match_groups(&mut board, &TuneOptions::default());
    let locked_after: Vec<Track> = board.tracks.iter().filter(|t| t.locked).cloned().collect();
    assert_eq!(locked_before, locked_after);
    assert!(net_length_nm(&board, "SIG_FREE") >= mm(21.0) as f64);

    // End-to-end CLI `boardc route --no-tune` -> `boardc tune` and `boardc build`.
    let tmp: PathBuf = std::env::temp_dir().join(format!("boardc_tune_cli_{}", std::process::id()));
    let _ = fs::remove_dir_all(&tmp);
    fs::create_dir_all(&tmp).unwrap();

    let qspi = qspi_board();
    let in_json = tmp.join("qspi.json");
    let routed_json = tmp.join("routed.json");
    let tuned_json = tmp.join("tuned.json");
    fs::write(&in_json, serde_json::to_string_pretty(&qspi).unwrap()).unwrap();

    let route_out = Command::new(env!("CARGO_BIN_EXE_boardc"))
        .args([
            "route",
            in_json.to_str().unwrap(),
            "-o",
            routed_json.to_str().unwrap(),
            "--no-tune",
            "--json",
        ])
        .output()
        .unwrap();
    assert!(
        route_out.status.success(),
        "{}",
        String::from_utf8_lossy(&route_out.stderr)
    );

    let tune_out = Command::new(env!("CARGO_BIN_EXE_boardc"))
        .args([
            "tune",
            routed_json.to_str().unwrap(),
            "-o",
            tuned_json.to_str().unwrap(),
            "--json",
        ])
        .output()
        .unwrap();
    assert!(
        tune_out.status.success(),
        "{}",
        String::from_utf8_lossy(&tune_out.stderr)
    );
    let tune_rep: TuneReport = serde_json::from_slice(&tune_out.stdout).unwrap();
    assert!(tune_rep.ok, "expected CLI tune ok, got {tune_rep:?}");
    assert!(tune_rep.added_meanders > 0);

    let tuned_board: BoardIr =
        serde_json::from_str(&fs::read_to_string(&tuned_json).unwrap()).unwrap();
    let drc = check_board_drc(&tuned_board);
    assert!(
        drc.passed,
        "expected clean DRC after CLI tune, got {:?}",
        drc.violations
    );

    // Full `compile_board` pipeline (equivalent to `boardc build`) passes DRC with match_groups.
    let cache_dir = tmp.join("cache");
    let build_cfg = BuildOptions {
        cache_dir,
        emit_outputs: false,
        ..Default::default()
    };
    let build_rep = compile_board(qspi, &build_cfg).unwrap();
    assert!(build_rep.ok, "expected pipeline build ok: {build_rep:?}");
    assert!(
        build_rep.drc.passed,
        "expected clean DRC from pipeline, got {:?}",
        build_rep.drc.violations
    );

    let _ = fs::remove_dir_all(&tmp);
}
