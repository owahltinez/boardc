use std::collections::BTreeMap;
use std::process::Command;

use boardc::drc::{ViolationKind, check_board_drc};
use boardc::ir::{
    BgaShadowRule, BoardIr, BoardSpec, CapProximityRule, Constraints, DesignRules, FootprintSpec,
    Keepout, KeepoutKind, LayerDef, LayerKind, MinViasRule, Placement, PourSpec, Pt, Side, mm, um,
};
use boardc::place::{PlaceOptions, PowerViaOptions, legalize_placement, solve_power_vias};
use boardc::preflight::run_preflight;
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

fn layer_plane(name: &str) -> LayerDef {
    LayerDef {
        name: name.into(),
        kind: LayerKind::Plane,
        side: None,
        thickness_nm: None,
    }
}

fn cap_placement(
    refdes: &str,
    pos_mm: (f64, f64),
    rot_deg: f64,
    side: Side,
    n1: &str,
    n2: &str,
) -> Placement {
    let mut pin_nets = BTreeMap::new();
    pin_nets.insert("1".into(), n1.into());
    pin_nets.insert("2".into(), n2.into());
    Placement {
        refdes: refdes.into(),
        footprint: "C_0402".into(),
        value: "100nF".into(),
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

#[test]
fn overlapping_decoupling_caps_legalized_with_cap_proximity_and_zero_overlaps() {
    let mut footprints = BTreeMap::new();
    footprints.insert(
        "C_0402".into(),
        FootprintSpec::Chip2Pin {
            package: "0402".into(),
        },
    );
    footprints.insert(
        "U_QFN16".into(),
        FootprintSpec::Qfn {
            pins_per_side: 4,
            pitch_nm: mm(0.5),
            body_nm: mm(3.0),
            ep_nm: mm(1.6),
        },
    );

    let mut u1_nets = BTreeMap::new();
    u1_nets.insert("1".into(), "VDD".into());
    u1_nets.insert("EP".into(), "GND".into());

    let spec = BoardSpec {
        name: "cap_legalize".into(),
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
                locked: true,
                pin_nets: u1_nets,
                mpn: None,
                lcsc: None,
                dnp: false,
                cpl_rotation_offset_deg: 0.0,
            },
            // All three caps initially overlap each other (and C1/C2 sit on top of U1 / far away).
            cap_placement("C1", (15.0, 10.0), 0.0, Side::Top, "VDD", "GND"),
            cap_placement("C2", (15.0, 10.0), 0.0, Side::Top, "VDD", "GND"),
            cap_placement("C3", (6.0, 4.0), 90.0, Side::Top, "VDD", "GND"),
        ],
        keepouts: Vec::new(),
        pours: Vec::new(),
        tracks: Vec::new(),
        vias: Vec::new(),
        net_classes: Default::default(),
        nets: Default::default(),
        rules: DesignRules {
            courtyard_clearance_nm: um(100.0),
            ..Default::default()
        },
        constraints: Constraints {
            cap_proximity: vec![
                CapProximityRule {
                    cap_refdes: "C1".into(),
                    ic_refdes: "U1".into(),
                    net: "VDD".into(),
                    max_dist_nm: mm(2.5),
                },
                CapProximityRule {
                    cap_refdes: "C2".into(),
                    ic_refdes: "U1".into(),
                    net: "VDD".into(),
                    max_dist_nm: mm(3.2),
                },
                CapProximityRule {
                    cap_refdes: "C3".into(),
                    ic_refdes: "U1".into(),
                    net: "VDD".into(),
                    max_dist_nm: mm(4.2),
                },
            ],
            ..Default::default()
        },
        graphics: Vec::new(),
    };

    let mut board = BoardIr::from_spec(&spec).unwrap();
    let report = legalize_placement(&mut board, &PlaceOptions::default()).unwrap();
    assert!(report.ok, "place report failed: {report:?}");
    assert!(report.unresolvable.is_empty(), "{:?}", report.unresolvable);
    assert_eq!(report.fixed_count, 1);
    assert_eq!(report.moved_parts.len(), 3);
    assert_eq!(report.cap_proximity.len(), 3);
    assert!(
        report.cap_proximity.iter().all(|c| c.satisfied),
        "{:?}",
        report.cap_proximity
    );
    assert_eq!(board.placements[0].pos, Pt::new(mm(15.0), mm(10.0)));

    let drc = check_board_drc(&board);
    let layout_errs: Vec<_> = drc
        .violations
        .iter()
        .filter(|v| {
            matches!(
                v.kind,
                ViolationKind::CourtyardOverlap
                    | ViolationKind::CapProximity
                    | ViolationKind::Keepout
                    | ViolationKind::ComponentToEdge
                    | ViolationKind::EdgeClearance
                    | ViolationKind::Clearance
            )
        })
        .collect();
    assert!(
        layout_errs.is_empty(),
        "unexpected layout DRC errors: {layout_errs:?}"
    );

    // Idempotency check: running again moves nothing.
    let second = legalize_placement(&mut board, &PlaceOptions::default()).unwrap();
    assert!(second.ok);
    assert!(second.moved_parts.is_empty(), "{:?}", second.moved_parts);
}

#[test]
fn movable_cap_pushed_out_of_bga_shadow_and_keepout() {
    let mut footprints = BTreeMap::new();
    footprints.insert(
        "C_0402".into(),
        FootprintSpec::Chip2Pin {
            package: "0402".into(),
        },
    );
    footprints.insert(
        "BGA16".into(),
        FootprintSpec::Bga {
            rows: 4,
            cols: 4,
            pitch_nm: mm(0.8),
            ball_dia_nm: mm(0.4),
        },
    );

    let mut u1_nets = BTreeMap::new();
    u1_nets.insert("A1".into(), "VDD".into());

    let spec = BoardSpec {
        name: "bga_shadow_keepout".into(),
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
                footprint: "BGA16".into(),
                value: "BGA".into(),
                pos: Pt::new(mm(15.0), mm(10.0)),
                rot_deg: 0.0,
                side: Side::Top,
                locked: true,
                pin_nets: u1_nets,
                mpn: None,
                lcsc: None,
                dnp: false,
                cpl_rotation_offset_deg: 0.0,
            },
            // Seeded directly inside U1's opposite-side BGA shadow on Bottom.
            cap_placement("C1", (15.0, 10.0), 0.0, Side::Bottom, "VDD", "GND"),
            // Seeded inside a Top-side placement keepout.
            cap_placement("C2", (6.0, 10.0), 0.0, Side::Top, "SIG1", "SIG2"),
        ],
        keepouts: vec![
            Keepout {
                kind: KeepoutKind::Placement,
                layers: vec!["F.Cu".into()],
                poly: rect_pts(4.0, 8.0, 8.0, 12.0),
            },
            Keepout {
                kind: KeepoutKind::Placement,
                layers: vec!["B.Cu".into()],
                poly: rect_pts(10.5, 8.0, 13.0, 12.0),
            },
        ],
        pours: Vec::new(),
        tracks: Vec::new(),
        vias: Vec::new(),
        net_classes: Default::default(),
        nets: Default::default(),
        rules: DesignRules {
            courtyard_clearance_nm: um(100.0),
            ..Default::default()
        },
        constraints: Constraints {
            bga_shadows: vec![BgaShadowRule {
                bga_refdes: "U1".into(),
                min_clearance_nm: mm(0.5),
                allow_passives: false,
            }],
            ..Default::default()
        },
        graphics: Vec::new(),
    };

    let mut board = BoardIr::from_spec(&spec).unwrap();
    let report = legalize_placement(&mut board, &PlaceOptions::default()).unwrap();
    assert!(report.ok, "{report:?}");
    assert_eq!(report.moved_parts.len(), 2);

    let drc = check_board_drc(&board);
    let violations: Vec<_> = drc
        .violations
        .iter()
        .filter(|v| {
            matches!(
                v.kind,
                ViolationKind::BgaShadow
                    | ViolationKind::Keepout
                    | ViolationKind::CourtyardOverlap
                    | ViolationKind::EdgeClearance
                    | ViolationKind::ComponentToEdge
            )
        })
        .collect();
    assert!(
        violations.is_empty(),
        "expected 0 layout/BGA shadow violations, got {violations:?}"
    );
}

#[test]
fn power_via_solver_thermal_array_and_plane_drops_clean_drc_and_no_chokes() {
    let mut footprints = BTreeMap::new();
    footprints.insert(
        "C_0402".into(),
        FootprintSpec::Chip2Pin {
            package: "0402".into(),
        },
    );
    footprints.insert(
        "U_QFN16".into(),
        FootprintSpec::Qfn {
            pins_per_side: 4,
            pitch_nm: mm(0.65),
            body_nm: mm(4.0),
            ep_nm: mm(2.0),
        },
    );

    let mut u1_nets = BTreeMap::new();
    u1_nets.insert("1".into(), "VDD".into());
    u1_nets.insert("EP".into(), "GND".into());

    let spec = BoardSpec {
        name: "power_vias".into(),
        outline: Vec::new(),
        size_nm: Some([mm(35.0), mm(25.0)]),
        cutouts: Vec::new(),
        layers: vec![
            layer_sig("F.Cu", Some(Side::Top)),
            layer_plane("In1.Cu"),
            layer_plane("In2.Cu"),
            layer_sig("B.Cu", Some(Side::Bottom)),
        ],
        footprints,
        placements: vec![
            Placement {
                refdes: "U1".into(),
                footprint: "U_QFN16".into(),
                value: "IC".into(),
                pos: Pt::new(mm(17.5), mm(12.5)),
                rot_deg: 0.0,
                side: Side::Top,
                locked: true,
                pin_nets: u1_nets,
                mpn: None,
                lcsc: None,
                dnp: false,
                cpl_rotation_offset_deg: 0.0,
            },
            cap_placement("C1", (12.0, 14.0), 0.0, Side::Top, "VDD", "GND"),
            cap_placement("C2", (12.0, 10.5), 0.0, Side::Top, "VDD", "GND"),
        ],
        keepouts: Vec::new(),
        pours: vec![
            PourSpec {
                name: "GND_PLANE".into(),
                net: "GND".into(),
                layer: "In1.Cu".into(),
                outline: rect_pts(0.0, 0.0, 35.0, 25.0),
                priority: 0,
                clearance_nm: None,
                min_width_nm: mm(0.2),
                thermal_relief: false,
                thermal_gap_nm: mm(0.2),
                thermal_spoke_nm: mm(0.3),
                remove_islands: true,
            },
            PourSpec {
                name: "VDD_PLANE".into(),
                net: "VDD".into(),
                layer: "In2.Cu".into(),
                outline: rect_pts(0.0, 0.0, 35.0, 25.0),
                priority: 0,
                clearance_nm: None,
                min_width_nm: mm(0.2),
                thermal_relief: false,
                thermal_gap_nm: mm(0.2),
                thermal_spoke_nm: mm(0.3),
                remove_islands: true,
            },
        ],
        tracks: Vec::new(),
        vias: Vec::new(),
        net_classes: Default::default(),
        nets: Default::default(),
        rules: DesignRules {
            courtyard_clearance_nm: um(100.0),
            via_at_smd: false,
            ..Default::default()
        },
        constraints: Constraints {
            cap_proximity: vec![
                CapProximityRule {
                    cap_refdes: "C1".into(),
                    ic_refdes: "U1".into(),
                    net: "VDD".into(),
                    max_dist_nm: mm(4.0),
                },
                CapProximityRule {
                    cap_refdes: "C2".into(),
                    ic_refdes: "U1".into(),
                    net: "VDD".into(),
                    max_dist_nm: mm(4.0),
                },
            ],
            min_vias: vec![MinViasRule {
                net: "GND".into(),
                refdes: Some("U1".into()),
                min_vias: 4,
            }],
            ..Default::default()
        },
        graphics: Vec::new(),
    };

    let mut board = BoardIr::from_spec(&spec).unwrap();
    let place_rep = legalize_placement(&mut board, &PlaceOptions::default()).unwrap();
    assert!(place_rep.ok, "{place_rep:?}");

    let pv_rep = solve_power_vias(&mut board, &PowerViaOptions::default()).unwrap();
    assert!(pv_rep.ok, "{pv_rep:?}");
    assert!(pv_rep.unresolved.is_empty(), "{:?}", pv_rep.unresolved);
    assert!(
        pv_rep.thermal_vias_added >= 4,
        "expected >=4 thermal vias in 2.0mm EP, got {}",
        pv_rep.thermal_vias_added
    );
    assert!(
        pv_rep.plane_vias_added >= 5,
        "expected plane vias for U1.1 + C1 + C2, got {}",
        pv_rep.plane_vias_added
    );
    assert_eq!(pv_rep.stubs_added, pv_rep.plane_vias_added);
    assert_eq!(pv_rep.chokes_after, 0);
    assert!(board.vias.iter().all(|v| v.locked));
    assert!(board.tracks.iter().all(|t| t.locked));

    let zone_rep = fill_zones(&mut board);
    assert!(zone_rep.chokes.is_empty(), "{:?}", zone_rep.chokes);

    let drc = check_board_drc(&board);
    assert!(
        drc.passed,
        "expected 0 DRC errors, got {:?}",
        drc.violations
    );

    // Idempotency: re-running solve_power_vias adds nothing new.
    let pv_rep2 = solve_power_vias(&mut board, &PowerViaOptions::default()).unwrap();
    assert!(pv_rep2.ok);
    assert_eq!(pv_rep2.thermal_vias_added, 0);
    assert_eq!(pv_rep2.plane_vias_added, 0);
    assert_eq!(pv_rep2.stubs_added, 0);
}

#[test]
fn power_via_solver_does_not_seal_adjacent_bga_pads_and_cli_place_works() {
    let mut footprints = BTreeMap::new();
    footprints.insert(
        "C_0402".into(),
        FootprintSpec::Chip2Pin {
            package: "0402".into(),
        },
    );
    footprints.insert(
        "BGA16".into(),
        FootprintSpec::Bga {
            rows: 4,
            cols: 4,
            pitch_nm: mm(0.8),
            ball_dia_nm: mm(0.4),
        },
    );

    let mut u1_nets = BTreeMap::new();
    u1_nets.insert("A1".into(), "SIG0".into());
    u1_nets.insert("A2".into(), "SIG1".into());
    u1_nets.insert("B2".into(), "VDD".into());
    u1_nets.insert("B3".into(), "GND".into());

    let spec = BoardSpec {
        name: "bga_pofv_cli".into(),
        outline: Vec::new(),
        size_nm: Some([mm(30.0), mm(20.0)]),
        cutouts: Vec::new(),
        layers: vec![
            layer_sig("F.Cu", Some(Side::Top)),
            layer_plane("In1.Cu"),
            layer_sig("B.Cu", Some(Side::Bottom)),
        ],
        footprints,
        placements: vec![
            Placement {
                refdes: "U1".into(),
                footprint: "BGA16".into(),
                value: "BGA".into(),
                pos: Pt::new(mm(15.0), mm(10.0)),
                rot_deg: 0.0,
                side: Side::Top,
                locked: true,
                pin_nets: u1_nets,
                mpn: None,
                lcsc: None,
                dnp: false,
                cpl_rotation_offset_deg: 0.0,
            },
            cap_placement("C1", (15.0, 10.0), 0.0, Side::Top, "VDD", "GND"),
        ],
        keepouts: Vec::new(),
        pours: vec![PourSpec {
            name: "GND_IN1".into(),
            net: "GND".into(),
            layer: "In1.Cu".into(),
            outline: rect_pts(0.0, 0.0, 30.0, 20.0),
            priority: 0,
            clearance_nm: None,
            min_width_nm: mm(0.2),
            thermal_relief: false,
            thermal_gap_nm: mm(0.2),
            thermal_spoke_nm: mm(0.3),
            remove_islands: true,
        }],
        tracks: Vec::new(),
        vias: Vec::new(),
        net_classes: Default::default(),
        nets: Default::default(),
        rules: DesignRules {
            via_at_smd: true,
            via_dia_nm: um(350.0),
            via_drill_nm: um(150.0),
            min_annular_ring_nm: um(75.0),
            clearance_nm: um(120.0),
            ..Default::default()
        },
        constraints: Constraints {
            cap_proximity: vec![CapProximityRule {
                cap_refdes: "C1".into(),
                ic_refdes: "U1".into(),
                net: "VDD".into(),
                max_dist_nm: mm(3.5),
            }],
            ..Default::default()
        },
        graphics: Vec::new(),
    };

    let mut board = BoardIr::from_spec(&spec).unwrap();
    let place_rep = legalize_placement(&mut board, &PlaceOptions::default()).unwrap();
    assert!(place_rep.ok, "{place_rep:?}");
    let pv_rep = solve_power_vias(&mut board, &PowerViaOptions::default()).unwrap();
    assert!(pv_rep.ok, "{pv_rep:?}");
    assert!(pv_rep.pofv_count >= 1, "expected POFV drop on BGA GND ball");

    let pre = run_preflight(&board, &Default::default()).unwrap();
    assert!(
        pre.sealed_pads.is_empty(),
        "power via sealed a BGA signal pad: {:?}",
        pre.sealed_pads
    );

    // CLI test: `boardc place <BOARD> -o <OUT.json> --power-vias --json`
    let tmp_dir = std::env::temp_dir().join("boardc_test_place_cli");
    std::fs::create_dir_all(&tmp_dir).unwrap();
    let in_path = tmp_dir.join("in.json");
    let out_path = tmp_dir.join("out.json");
    std::fs::write(&in_path, serde_json::to_string_pretty(&spec).unwrap()).unwrap();

    let out = Command::new(env!("CARGO_BIN_EXE_boardc"))
        .args([
            "place",
            in_path.to_str().unwrap(),
            "-o",
            out_path.to_str().unwrap(),
            "--power-vias",
            "--json",
        ])
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "cli failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let json: serde_json::Value = serde_json::from_slice(&out.stdout).unwrap();
    assert_eq!(json["placement"]["ok"], true);
    assert_eq!(json["power_vias"]["ok"], true);
    let saved: BoardIr =
        serde_json::from_str(&std::fs::read_to_string(&out_path).unwrap()).unwrap();
    assert!(!saved.vias.is_empty());

    let _ = std::fs::remove_dir_all(&tmp_dir);
}
