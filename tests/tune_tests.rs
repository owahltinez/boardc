use std::collections::{BTreeMap, HashSet};
use std::fs;
use std::path::PathBuf;
use std::process::Command;

use boardc::drc::{ViolationKind, check_board_drc};
use boardc::ir::{
    BoardIr, BoardSpec, Constraints, DesignRules, FilledPour, FootprintSpec, Keepout, KeepoutKind,
    LayerDef, LayerKind, MatchGroupRule, MaxLengthRule, NetClass, NetDef, PadDef, PadKind,
    PadShape, Placement, PourPoly, PourSpec, Pt, Side, Track, Via, mm,
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

fn pad_fp_layer(dia_mm: f64, layer: &str) -> FootprintSpec {
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
            layers: vec![layer.into()],
            mask_margin_nm: None,
            paste_margin_nm: None,
        }],
        courtyard: Vec::new(),
        graphics: Vec::new(),
        keepouts: Vec::new(),
        models: Vec::new(),
        pin_types: Default::default(),
    }
}

fn pad_fp(dia_mm: f64) -> FootprintSpec {
    pad_fp_layer(dia_mm, "F.Cu")
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

#[allow(clippy::too_many_arguments)]
fn add_h_net(
    placements: &mut Vec<Placement>,
    tracks: &mut Vec<Track>,
    tag: &str,
    net: &str,
    x0: f64,
    x1: f64,
    y: f64,
    locked: bool,
) {
    placements.push(place_pad(&format!("{tag}1"), "PAD", x0, y, net));
    placements.push(place_pad(&format!("{tag}2"), "PAD", x1, y, net));
    tracks.push(Track {
        net: net.into(),
        layer: "F.Cu".into(),
        a: Pt::new(mm(x0), mm(y)),
        b: Pt::new(mm(x1), mm(y)),
        width: mm(0.15),
        locked,
    });
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
    let target_x = [14.0, 17.0, 21.0, 25.0, 29.0, 34.0];
    for (i, (&net, &tx)) in qspi_nets.iter().zip(&target_x).enumerate() {
        placements.push(place_pad(
            &format!("J{}", i + 1),
            "PAD",
            tx,
            14.0 + (i as f64) * 1.6,
            net,
        ));
    }

    BoardIr::from_spec(&BoardSpec {
        name: "qspi_bus".into(),
        size_nm: Some([mm(44.0), mm(36.0)]),
        footprints,
        placements,
        pours: vec![PourSpec {
            name: "GND_BOT".into(),
            net: "GND".into(),
            layer: "B.Cu".into(),
            outline: rect_pts(1.0, 1.0, 43.0, 35.0),
            ..Default::default()
        }],
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
    })
    .unwrap()
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
    assert!(
        check_board_drc(&untuned)
            .violations
            .iter()
            .any(|v| v.kind == ViolationKind::MatchGroup)
    );

    let mut board = qspi_board();
    let summary = route_board_ir(&mut board, &RouteOptions::default()).unwrap();
    assert_eq!(summary.unconnected_pairs, 0);
    assert_eq!(summary.length_violations, 0);

    let _zone_report = fill_zones(&mut board);
    let drc = check_board_drc(&board);
    assert!(drc.passed, "expected clean DRC, got {:?}", drc.violations);
}

#[test]
fn explicit_target_nm_lengthens_short_net_and_preserves_in_tolerance_net() {
    let footprints = BTreeMap::from([("PAD".into(), pad_fp(0.5))]);
    let (mut placements, mut tracks) = (Vec::new(), Vec::new());
    add_h_net(
        &mut placements,
        &mut tracks,
        "S",
        "SIG_SHORT",
        4.0,
        14.0,
        14.0,
        false,
    );
    add_h_net(
        &mut placements,
        &mut tracks,
        "O",
        "SIG_OK",
        4.0,
        28.5,
        6.0,
        false,
    );

    let mut board = BoardIr::from_spec(&BoardSpec {
        name: "explicit_target".into(),
        size_nm: Some([mm(40.0), mm(20.0)]),
        footprints,
        placements,
        tracks,
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
    })
    .unwrap();
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
    assert!(short_len >= mm(24.0) as f64 - 1.0 && short_len <= mm(26.0) as f64 + 1.0);
    let ok_after: Vec<Track> = board
        .tracks
        .iter()
        .filter(|t| t.net == "SIG_OK")
        .cloned()
        .collect();
    assert_eq!(ok_before, ok_after);

    let drc = check_board_drc(&board);
    assert!(drc.passed, "expected clean DRC, got {:?}", drc.violations);
}

#[test]
fn tight_obstacles_flip_side_and_shrink_amplitude_without_drc_violation() {
    let mut footprints = BTreeMap::from([("PAD".into(), pad_fp(0.5))]);
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
            models: Vec::new(),
            pin_types: Default::default(),
        },
    );

    let (mut placements, mut tracks) = (Vec::new(), Vec::new());
    add_h_net(
        &mut placements,
        &mut tracks,
        "A",
        "SIG_A",
        4.0,
        20.0,
        16.0,
        false,
    );
    add_h_net(
        &mut placements,
        &mut tracks,
        "B",
        "SIG_B",
        4.0,
        20.0,
        8.0,
        false,
    );
    add_h_net(
        &mut placements,
        &mut tracks,
        "R",
        "SIG_REF",
        4.0,
        27.0,
        2.0,
        true,
    );
    add_h_net(
        &mut placements,
        &mut tracks,
        "WT",
        "WALL_TOP",
        4.0,
        20.0,
        9.0,
        true,
    );
    add_h_net(
        &mut placements,
        &mut tracks,
        "WB",
        "WALL_BOT",
        4.0,
        20.0,
        7.0,
        true,
    );
    placements.push(Placement {
        refdes: "H1".into(),
        footprint: "NPTH".into(),
        pos: Pt::new(mm(12.0), mm(14.5)),
        locked: true,
        ..Default::default()
    });

    let mut board = BoardIr::from_spec(&BoardSpec {
        name: "obstacle_channel".into(),
        size_nm: Some([mm(32.0), mm(22.0)]),
        footprints,
        placements,
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
        tracks,
        rules: DesignRules {
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
    })
    .unwrap();
    let report = tune_match_groups(&mut board, &TuneOptions::default());
    assert!(report.ok, "expected tune ok in tight channel: {report:?}");
    assert!(
        board
            .tracks
            .iter()
            .filter(|t| t.net == "SIG_A")
            .all(|t| t.a.y <= mm(16.0) && t.b.y <= mm(16.0))
    );
    assert!(
        board
            .tracks
            .iter()
            .filter(|t| t.net == "SIG_B")
            .all(|t| t.a.y > mm(7.0) && t.a.y < mm(9.0) && t.b.y > mm(7.0) && t.b.y < mm(9.0))
    );

    let drc = check_board_drc(&board);
    assert!(drc.passed, "expected clean DRC, got {:?}", drc.violations);
}

#[test]
fn max_length_rule_caps_tuning_and_respects_multiple_match_groups() {
    let footprints = BTreeMap::from([("PAD".into(), pad_fp(0.5))]);
    let (mut placements, mut tracks) = (Vec::new(), Vec::new());
    add_h_net(
        &mut placements,
        &mut tracks,
        "A",
        "NET_CAPPED",
        4.0,
        16.0,
        14.0,
        false,
    );
    add_h_net(
        &mut placements,
        &mut tracks,
        "B",
        "NET_MULTI",
        4.0,
        16.0,
        10.0,
        false,
    );
    add_h_net(
        &mut placements,
        &mut tracks,
        "R",
        "NET_LONG",
        4.0,
        30.0,
        5.0,
        true,
    );

    let mut board = BoardIr::from_spec(&BoardSpec {
        name: "max_cap".into(),
        size_nm: Some([mm(40.0), mm(20.0)]),
        footprints,
        placements,
        tracks,
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
    })
    .unwrap();
    let report = tune_match_groups(&mut board, &TuneOptions::default());
    assert!(!report.ok);
    assert!(report.unmet_nets.contains(&"NET_CAPPED".to_string()));
    assert!(net_length_nm(&board, "NET_CAPPED") <= mm(20.0) as f64 + 1.0);
    assert!(net_length_nm(&board, "NET_MULTI") <= mm(19.0) as f64 + 1.0);

    let drc = check_board_drc(&board);
    assert!(
        !drc.violations.iter().any(|v| matches!(
            v.kind,
            ViolationKind::MaxLength | ViolationKind::Clearance | ViolationKind::OpenNet
        )),
        "unexpected violations: {:?}",
        drc.violations
    );
}

#[test]
fn locked_tracks_preserved_cli_tune_and_full_build_pipeline() {
    let footprints = BTreeMap::from([("PAD".into(), pad_fp(0.5))]);
    let (mut placements, mut tracks) = (Vec::new(), Vec::new());
    add_h_net(
        &mut placements,
        &mut tracks,
        "L",
        "SIG_LOCKED",
        4.0,
        12.0,
        15.0,
        true,
    );
    add_h_net(
        &mut placements,
        &mut tracks,
        "U",
        "SIG_FREE",
        4.0,
        14.0,
        10.0,
        false,
    );
    add_h_net(
        &mut placements,
        &mut tracks,
        "R",
        "SIG_REF",
        4.0,
        26.0,
        5.0,
        true,
    );
    let mut board = BoardIr::from_spec(&BoardSpec {
        name: "locked_and_cli".into(),
        size_nm: Some([mm(36.0), mm(20.0)]),
        footprints,
        placements,
        tracks,
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
    assert!(drc.passed, "expected clean DRC, got {:?}", drc.violations);

    let build_rep = compile_board(
        qspi,
        &BuildOptions {
            cache_dir: tmp.join("cache"),
            emit_outputs: false,
            ..Default::default()
        },
    )
    .unwrap();
    assert!(build_rep.ok && build_rep.drc.passed);
    let _ = fs::remove_dir_all(&tmp);
}

#[test]
fn interior_contacts_preserve_connectivity_and_zero_open_net() {
    let mut footprints = BTreeMap::from([
        ("PAD".into(), pad_fp(0.5)),
        ("PAD_B".into(), pad_fp_layer(0.5, "B.Cu")),
        ("PAD_IN".into(), pad_fp_layer(0.5, "In1.Cu")),
    ]);
    footprints.insert(
        "THT_FB".into(),
        FootprintSpec::Custom {
            pads: vec![PadDef {
                name: "1".into(),
                kind: PadKind::Tht,
                shape: PadShape::Circle { diameter: mm(0.8) },
                pos: Pt::new(0, 0),
                rot_deg: 0.0,
                drill_nm: Some(mm(0.4)),
                layers: vec!["F.Cu".into(), "B.Cu".into()],
                mask_margin_nm: None,
                paste_margin_nm: None,
            }],
            courtyard: Vec::new(),
            graphics: Vec::new(),
            keepouts: Vec::new(),
            models: Vec::new(),
            pin_types: Default::default(),
        },
    );

    let (mut placements, mut tracks) = (Vec::new(), Vec::new());
    add_h_net(
        &mut placements,
        &mut tracks,
        "R",
        "REF",
        4.0,
        34.0,
        2.0,
        true,
    );

    // 1. Off-center interior same-net pad.
    add_h_net(
        &mut placements,
        &mut tracks,
        "PO",
        "PAD_OFF",
        4.0,
        20.0,
        25.0,
        false,
    );
    placements.push(place_pad("PO_M", "PAD", 12.0, 25.28, "PAD_OFF"));

    // 2. Off-center interior same-net via bridging to B.Cu pad.
    add_h_net(
        &mut placements,
        &mut tracks,
        "VO",
        "VIA_OFF",
        4.0,
        20.0,
        20.0,
        false,
    );
    placements.push(place_pad("VO_B", "PAD_B", 12.0, 20.22, "VIA_OFF"));
    let vias = vec![Via {
        net: "VIA_OFF".into(),
        pos: Pt::new(mm(12.0), mm(20.22)),
        diameter: mm(0.5),
        drill: mm(0.25),
        layers: ("F.Cu".into(), "B.Cu".into()),
        locked: true,
        tented: true,
    }];

    // 3. Inner-layer THT barrel (pad copper on F.Cu/B.Cu only, track on In1.Cu).
    placements.push(place_pad("T1", "PAD_IN", 4.0, 15.0, "THT_INNER"));
    placements.push(place_pad("T2", "PAD_IN", 20.0, 15.0, "THT_INNER"));
    placements.push(place_pad("TM", "THT_FB", 12.0, 15.0, "THT_INNER"));
    tracks.push(Track {
        net: "THT_INNER".into(),
        layer: "In1.Cu".into(),
        a: Pt::new(mm(4.0), mm(15.0)),
        b: Pt::new(mm(20.0), mm(15.0)),
        width: mm(0.15),
        locked: false,
    });

    // 4. X-crossing track and wider T-branch whose endpoint stops short of centerline.
    add_h_net(
        &mut placements,
        &mut tracks,
        "XT",
        "X_AND_T",
        4.0,
        20.0,
        10.0,
        false,
    );
    placements.push(place_pad("XC1", "PAD", 10.0, 8.5, "X_AND_T"));
    placements.push(place_pad("XC2", "PAD", 10.0, 11.5, "X_AND_T"));
    tracks.push(Track {
        net: "X_AND_T".into(),
        layer: "F.Cu".into(),
        a: Pt::new(mm(10.0), mm(8.5)),
        b: Pt::new(mm(10.0), mm(11.5)),
        width: mm(0.15),
        locked: true,
    });
    placements.push(place_pad("TB", "PAD", 15.0, 12.5, "X_AND_T"));
    tracks.push(Track {
        net: "X_AND_T".into(),
        layer: "F.Cu".into(),
        a: Pt::new(mm(15.0), mm(10.35)),
        b: Pt::new(mm(15.0), mm(12.5)),
        width: mm(0.6),
        locked: true,
    });

    // 5. Same-net filled pour touching track on F.Cu.
    add_h_net(
        &mut placements,
        &mut tracks,
        "PR",
        "POUR_NET",
        4.0,
        20.0,
        5.5,
        false,
    );
    placements.push(place_pad("PR_M", "PAD", 12.0, 4.0, "POUR_NET"));

    let mut board = BoardIr::from_spec(&BoardSpec {
        name: "interior_contacts".into(),
        size_nm: Some([mm(44.0), mm(30.0)]),
        layers: vec![
            layer_sig("F.Cu", Some(Side::Top)),
            layer_sig("In1.Cu", None),
            layer_sig("In2.Cu", None),
            layer_sig("B.Cu", Some(Side::Bottom)),
        ],
        footprints,
        placements,
        vias,
        tracks,
        constraints: Constraints {
            match_groups: vec![MatchGroupRule {
                name: "CONTACTS".into(),
                nets: vec![
                    "REF".into(),
                    "PAD_OFF".into(),
                    "VIA_OFF".into(),
                    "THT_INNER".into(),
                    "X_AND_T".into(),
                    "POUR_NET".into(),
                ],
                tolerance_nm: mm(1.0),
                target_nm: None,
            }],
            ..Default::default()
        },
        ..Default::default()
    })
    .unwrap();
    board.filled_pours.push(FilledPour {
        net: "POUR_NET".into(),
        layer: "F.Cu".into(),
        priority: 0,
        polys: vec![PourPoly {
            outer: rect_pts(10.5, 3.5, 13.5, 5.55),
            holes: Vec::new(),
        }],
    });

    let pour_len_before = net_length_nm(&board, "POUR_NET");
    let _rep = tune_match_groups(&mut board, &TuneOptions::default());
    for net in ["PAD_OFF", "VIA_OFF", "THT_INNER", "X_AND_T"] {
        assert!(
            net_length_nm(&board, net) > mm(24.0) as f64,
            "{net} failed to meander around interior contact"
        );
    }
    assert_eq!(
        net_length_nm(&board, "POUR_NET"),
        pour_len_before,
        "track touching same-net pour must be skipped"
    );

    let drc = check_board_drc(&board);
    assert!(
        !drc.violations.iter().any(|v| matches!(
            v.kind,
            ViolationKind::OpenNet | ViolationKind::Clearance | ViolationKind::EdgeClearance
        )),
        "unexpected connectivity/clearance violations: {:?}",
        drc.violations
    );
}

#[test]
fn small_deficit_mid_obstacle_and_short_span_monotone() {
    let footprints = BTreeMap::from([("PAD".into(), pad_fp(0.5))]);

    // 1. Mid-span obstacle on 12mm trace (exercises probe_clear_runs at probe_heights[0]).
    let (mut p1, mut t1) = (Vec::new(), Vec::new());
    add_h_net(&mut p1, &mut t1, "S", "SIG", 4.0, 16.0, 10.0, false);
    add_h_net(&mut p1, &mut t1, "R", "REF", 4.0, 17.5, 3.0, true);
    let mut b1 = BoardIr::from_spec(&BoardSpec {
        name: "mid_obs".into(),
        size_nm: Some([mm(24.0), mm(20.0)]),
        footprints: footprints.clone(),
        placements: p1,
        tracks: t1,
        keepouts: vec![
            Keepout {
                kind: KeepoutKind::Track,
                layers: vec!["F.Cu".into()],
                poly: rect_pts(9.0, 10.2, 11.0, 15.0),
            },
            Keepout {
                kind: KeepoutKind::Track,
                layers: vec!["F.Cu".into()],
                poly: rect_pts(9.0, 5.0, 11.0, 9.8),
            },
        ],
        constraints: Constraints {
            match_groups: vec![MatchGroupRule {
                name: "M".into(),
                nets: vec!["SIG".into(), "REF".into()],
                tolerance_nm: mm(0.2),
                target_nm: None,
            }],
            ..Default::default()
        },
        ..Default::default()
    })
    .unwrap();
    let r1 = tune_match_groups(&mut b1, &TuneOptions::default());
    assert!(r1.ok, "mid-span obstacle tune failed: {r1:?}");
    assert!(check_board_drc(&b1).passed);

    // 2. Small 60um deficit (< 88um) and 3. 500um deficit on 1.4mm span (former dead zones).
    for (name, span_mm, target_mm, tol_nm) in [
        ("small_60um", 10.0, 10.06, 20_000),
        ("short_500um", 1.4, 1.9, 50_000),
    ] {
        let (mut pl, mut tr) = (Vec::new(), Vec::new());
        add_h_net(&mut pl, &mut tr, "S", "SIG", 4.0, 4.0 + span_mm, 8.0, false);
        let mut board = BoardIr::from_spec(&BoardSpec {
            name: name.into(),
            size_nm: Some([mm(20.0), mm(16.0)]),
            footprints: footprints.clone(),
            placements: pl,
            tracks: tr,
            constraints: Constraints {
                match_groups: vec![MatchGroupRule {
                    name: "M".into(),
                    nets: vec!["SIG".into()],
                    tolerance_nm: tol_nm,
                    target_nm: Some(mm(target_mm)),
                }],
                ..Default::default()
            },
            ..Default::default()
        })
        .unwrap();
        let rep = tune_match_groups(&mut board, &TuneOptions::default());
        assert!(rep.ok, "{name} failed: {rep:?}");
        let drc = check_board_drc(&board);
        assert!(drc.passed, "{name} DRC failed: {:?}", drc.violations);
    }
}

#[test]
fn odd_nm_diagonal_relative_group_stays_one_lobe() {
    let footprints = BTreeMap::from([("PAD".into(), pad_fp(0.5))]);
    let mut board = BoardIr::from_spec(&BoardSpec {
        name: "diag_1lobe".into(),
        size_nm: Some([mm(30.0), mm(30.0)]),
        footprints,
        placements: vec![
            place_pad("D1", "PAD", 4.0, 4.0, "DIAG"),
            place_pad("D2", "PAD", 14.0, 14.0, "DIAG"),
            place_pad("R1", "PAD", 24.0, 4.0, "REF"),
            place_pad("R2", "PAD", 24.0, 22.0, "REF"),
        ],
        tracks: vec![
            Track {
                net: "DIAG".into(),
                layer: "F.Cu".into(),
                a: Pt::new(mm(4.0), mm(4.0)),
                b: Pt::new(mm(14.0), mm(14.0)),
                width: mm(0.15),
                locked: false,
            },
            Track {
                net: "REF".into(),
                layer: "F.Cu".into(),
                a: Pt::new(mm(24.0), mm(4.0)),
                b: Pt::new(mm(24.0), mm(22.0)),
                width: mm(0.15),
                locked: true,
            },
        ],
        constraints: Constraints {
            match_groups: vec![MatchGroupRule {
                name: "DIAG_BUS".into(),
                nets: vec!["DIAG".into(), "REF".into()],
                tolerance_nm: mm(0.5),
                target_nm: None,
            }],
            ..Default::default()
        },
        ..Default::default()
    })
    .unwrap();

    let rep = tune_match_groups(&mut board, &TuneOptions::default());
    assert!(rep.ok, "diag tune failed: {rep:?}");
    assert_eq!(rep.added_meanders, 1);
    assert!(
        rep.added_segments <= 8,
        "expected clean 1-lobe meander, added {} segments",
        rep.added_segments
    );
    let drc = check_board_drc(&board);
    assert!(drc.passed, "expected clean DRC, got {:?}", drc.violations);
}

#[test]
fn net_class_filters_and_ignore_route_respected() {
    let footprints = BTreeMap::from([("PAD".into(), pad_fp(0.5))]);
    let (mut placements, mut tracks) = (Vec::new(), Vec::new());
    add_h_net(
        &mut placements,
        &mut tracks,
        "A",
        "SIG_A",
        4.0,
        14.0,
        16.0,
        false,
    );
    add_h_net(
        &mut placements,
        &mut tracks,
        "B",
        "SIG_B",
        4.0,
        14.0,
        12.0,
        false,
    );
    add_h_net(
        &mut placements,
        &mut tracks,
        "I",
        "SIG_IGN",
        4.0,
        14.0,
        8.0,
        false,
    );
    add_h_net(
        &mut placements,
        &mut tracks,
        "R",
        "SIG_REF",
        4.0,
        28.0,
        4.0,
        true,
    );

    let spec = BoardSpec {
        name: "class_filters".into(),
        size_nm: Some([mm(32.0), mm(22.0)]),
        footprints,
        placements,
        tracks,
        net_classes: BTreeMap::from([(
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
        )]),
        nets: BTreeMap::from([
            (
                "SIG_A".into(),
                NetDef {
                    name: "SIG_A".into(),
                    class: "HS".into(),
                    pins: Vec::new(),
                    ignore_route: false,
                    power_driven: false,
                },
            ),
            (
                "SIG_B".into(),
                NetDef {
                    name: "SIG_B".into(),
                    class: "HS".into(),
                    pins: Vec::new(),
                    ignore_route: false,
                    power_driven: false,
                },
            ),
            (
                "SIG_IGN".into(),
                NetDef {
                    name: "SIG_IGN".into(),
                    class: "Default".into(),
                    pins: Vec::new(),
                    ignore_route: true,
                    power_driven: false,
                },
            ),
        ]),
        constraints: Constraints {
            match_groups: vec![MatchGroupRule {
                name: "BUS".into(),
                nets: vec![
                    "SIG_A".into(),
                    "SIG_B".into(),
                    "SIG_IGN".into(),
                    "SIG_REF".into(),
                ],
                tolerance_nm: mm(0.5),
                target_nm: None,
            }],
            ..Default::default()
        },
        ..Default::default()
    };

    let mut b_ign_class = BoardIr::from_spec(&spec).unwrap();
    let rep_ign = tune_match_groups(
        &mut b_ign_class,
        &TuneOptions {
            ignored_nets: HashSet::from(["HS".into()]),
            ..Default::default()
        },
    );
    assert_eq!(rep_ign.tuned_nets, 0);

    let mut b_def = BoardIr::from_spec(&spec).unwrap();
    let rep_def = tune_match_groups(&mut b_def, &TuneOptions::default());
    assert_eq!(rep_def.tuned_nets, 2);
    assert_eq!(net_length_nm(&b_def, "SIG_IGN"), mm(10.0) as f64);

    let mut b_reroute = BoardIr::from_spec(&spec).unwrap();
    b_reroute.tracks.retain(|t| t.net == "SIG_REF");
    let sum = route_board_ir(
        &mut b_reroute,
        &RouteOptions {
            reroute_nets: Some(HashSet::from(["HS".into()])),
            ..Default::default()
        },
    )
    .unwrap();
    assert_eq!(sum.unconnected_pairs, 0);
    assert!(net_length_nm(&b_reroute, "SIG_A") >= mm(23.5) as f64);
    assert!(net_length_nm(&b_reroute, "SIG_B") >= mm(23.5) as f64);
    assert_eq!(net_length_nm(&b_reroute, "SIG_IGN"), 0.0);
}

#[test]
fn overlapping_match_groups_converge_cleanly() {
    let footprints = BTreeMap::from([("PAD".into(), pad_fp(0.5))]);
    for with_target in [false, true] {
        let (mut placements, mut tracks) = (Vec::new(), Vec::new());
        add_h_net(
            &mut placements,
            &mut tracks,
            "A",
            "A",
            4.0,
            14.0,
            14.0,
            false,
        );
        add_h_net(
            &mut placements,
            &mut tracks,
            "B",
            "B",
            4.0,
            19.0,
            10.0,
            false,
        );
        let mut groups = vec![MatchGroupRule {
            name: "G1".into(),
            nets: vec!["A".into(), "B".into()],
            tolerance_nm: mm(0.5),
            target_nm: None,
        }];
        if with_target {
            groups.push(MatchGroupRule {
                name: "G2".into(),
                nets: vec!["B".into()],
                tolerance_nm: mm(0.5),
                target_nm: Some(mm(25.0)),
            });
        } else {
            add_h_net(&mut placements, &mut tracks, "C", "C", 4.0, 29.0, 6.0, true);
            groups.push(MatchGroupRule {
                name: "G2".into(),
                nets: vec!["B".into(), "C".into()],
                tolerance_nm: mm(0.5),
                target_nm: None,
            });
        }

        let mut board = BoardIr::from_spec(&BoardSpec {
            name: "overlap".into(),
            size_nm: Some([mm(36.0), mm(20.0)]),
            footprints: footprints.clone(),
            placements,
            tracks,
            constraints: Constraints {
                match_groups: groups,
                ..Default::default()
            },
            ..Default::default()
        })
        .unwrap();
        let rep = tune_match_groups(&mut board, &TuneOptions::default());
        assert!(rep.ok, "with_target={with_target} failed: {rep:?}");
        assert!(net_length_nm(&board, "A") >= mm(24.5) as f64 - 1.0);
        assert!(net_length_nm(&board, "B") >= mm(24.5) as f64 - 1.0);
        assert!(check_board_drc(&board).passed);
    }
}

#[test]
fn validation_cutouts_and_prefilled_pours() {
    let footprints = BTreeMap::from([("PAD".into(), pad_fp(0.5))]);
    let (mut placements, mut tracks) = (Vec::new(), Vec::new());
    add_h_net(
        &mut placements,
        &mut tracks,
        "S",
        "SIG_A",
        4.0,
        20.0,
        10.0,
        false,
    );

    let mut bad = BoardIr::from_spec(&BoardSpec {
        name: "bad_mg".into(),
        size_nm: Some([mm(30.0), mm(20.0)]),
        footprints: footprints.clone(),
        placements: placements.clone(),
        constraints: Constraints {
            match_groups: vec![MatchGroupRule {
                name: "BAD".into(),
                nets: vec!["SIG_A".into(), "NO_SUCH_NET".into()],
                tolerance_nm: mm(0.5),
                target_nm: None,
            }],
            ..Default::default()
        },
        ..Default::default()
    })
    .unwrap();
    let err = route_board_ir(&mut bad, &RouteOptions::default()).unwrap_err();
    assert!(err.contains("NO_SUCH_NET"), "unexpected error: {err}");

    let mut board = BoardIr::from_spec(&BoardSpec {
        name: "cutout_pour".into(),
        size_nm: Some([mm(30.0), mm(20.0)]),
        cutouts: vec![rect_pts(10.0, 10.35, 14.0, 15.0)],
        footprints,
        placements,
        pours: vec![PourSpec {
            name: "GND_TOP".into(),
            net: "GND".into(),
            layer: "F.Cu".into(),
            outline: rect_pts(1.0, 1.0, 29.0, 8.5),
            ..Default::default()
        }],
        tracks,
        constraints: Constraints {
            match_groups: vec![MatchGroupRule {
                name: "G".into(),
                nets: vec!["SIG_A".into()],
                tolerance_nm: mm(0.5),
                target_nm: Some(mm(22.0)),
            }],
            ..Default::default()
        },
        ..Default::default()
    })
    .unwrap();
    let _zr = fill_zones(&mut board);

    let tmp: PathBuf = std::env::temp_dir().join(format!("boardc_tune_val_{}", std::process::id()));
    let _ = fs::remove_dir_all(&tmp);
    fs::create_dir_all(&tmp).unwrap();
    let in_json = tmp.join("in.json");
    let out_json = tmp.join("out.json");
    fs::write(&in_json, serde_json::to_string_pretty(&board).unwrap()).unwrap();

    let bad_cli = Command::new(env!("CARGO_BIN_EXE_boardc"))
        .args([
            "tune",
            in_json.to_str().unwrap(),
            "--only",
            "NO_SUCH_TARGET",
        ])
        .output()
        .unwrap();
    assert!(!bad_cli.status.success());

    let ok_cli = Command::new(env!("CARGO_BIN_EXE_boardc"))
        .args([
            "tune",
            in_json.to_str().unwrap(),
            "-o",
            out_json.to_str().unwrap(),
            "--only",
            "Default",
            "--json",
        ])
        .output()
        .unwrap();
    assert!(
        ok_cli.status.success(),
        "{}",
        String::from_utf8_lossy(&ok_cli.stderr)
    );
    let rep: TuneReport = serde_json::from_slice(&ok_cli.stdout).unwrap();
    assert!(rep.ok, "expected CLI tune ok, got {rep:?}");
    let tuned: BoardIr = serde_json::from_str(&fs::read_to_string(&out_json).unwrap()).unwrap();
    let drc = check_board_drc(&tuned);
    assert!(drc.passed, "expected clean DRC, got {:?}", drc.violations);
    let _ = fs::remove_dir_all(&tmp);
}

#[test]
fn tune_match_groups_respects_mask_to_copper() {
    let mut footprints = BTreeMap::new();
    footprints.insert("PAD".into(), pad_fp(0.5));
    footprints.insert(
        "MASK_PAD".into(),
        FootprintSpec::Custom {
            pads: vec![PadDef {
                name: "1".into(),
                kind: PadKind::Smd,
                shape: PadShape::Circle { diameter: mm(0.6) },
                pos: Pt::new(0, 0),
                rot_deg: 0.0,
                drill_nm: None,
                layers: vec!["F.Cu".into(), "F.Mask".into()],
                mask_margin_nm: Some(mm(0.25)),
                paste_margin_nm: None,
            }],
            courtyard: Vec::new(),
            graphics: Vec::new(),
            keepouts: Vec::new(),
            models: Vec::new(),
            pin_types: Default::default(),
        },
    );

    let (mut placements, mut tracks) = (Vec::new(), Vec::new());
    add_h_net(
        &mut placements,
        &mut tracks,
        "A",
        "CLK_A",
        4.0,
        24.0,
        10.0,
        false,
    );
    add_h_net(
        &mut placements,
        &mut tracks,
        "B",
        "CLK_B",
        4.0,
        20.0,
        14.0,
        false,
    );
    // Place masked obstacle pads near CLK_B where copper clearance allows meanders, but mask_to_copper forbids them.
    placements.push(place_pad("OBS1", "MASK_PAD", 12.0, 15.1, "OTHER1"));
    placements.push(place_pad("OBS2", "MASK_PAD", 16.0, 12.9, "OTHER2"));

    let mut board = BoardIr::from_spec(&BoardSpec {
        name: "tune_m2c".into(),
        size_nm: Some([mm(30.0), mm(20.0)]),
        layers: vec![
            layer_sig("F.Cu", Some(Side::Top)),
            layer_sig("B.Cu", Some(Side::Bottom)),
        ],
        footprints,
        placements,
        tracks,
        rules: DesignRules {
            clearance_nm: mm(0.15),
            mask_expansion_nm: mm(0.05),
            mask_to_copper_nm: Some(mm(0.35)),
            ..Default::default()
        },
        constraints: Constraints {
            match_groups: vec![MatchGroupRule {
                name: "CLK".into(),
                nets: vec!["CLK_A".into(), "CLK_B".into()],
                tolerance_nm: mm(0.2),
                target_nm: None,
            }],
            ..Default::default()
        },
        ..Default::default()
    })
    .unwrap();

    let report = tune_match_groups(&mut board, &TuneOptions::default());
    assert!(report.ok, "expected tune_match_groups ok: {report:?}");
    let drc = check_board_drc(&board);
    assert!(
        drc.passed,
        "tuned match_group must produce zero SolderMaskToCopper/DRC violations: {:?}",
        drc.violations
    );

    // Also verify end-to-end route_board_ir with match_groups + mask_to_copper_nm enabled.
    board.tracks.clear();
    let rsum = route_board_ir(&mut board, &RouteOptions::default()).unwrap();
    assert_eq!(rsum.unconnected_pairs, 0);
    let rdrc = check_board_drc(&board);
    assert!(
        rdrc.passed,
        "routed+tuned board with mask_to_copper_nm must pass DRC: {:?}",
        rdrc.violations
    );
}
