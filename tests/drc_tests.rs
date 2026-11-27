use std::collections::{BTreeMap, HashSet};
use std::process::Command;

use boardc::drc::{
    DrcReport, Hotspot, Severity, Violation, ViolationKind, check_board_drc, cluster_hotspots,
    cluster_kicad_drc_json,
};
use boardc::ir::{
    BgaShadowRule, BoardIr, BoardSpec, CapProximityRule, Constraints, DesignRules, FilledPour,
    FootprintSpec, Keepout, KeepoutKind, LayerDef, LayerKind, MatchGroupRule, MaxLengthRule,
    MinViasRule, NetClass, NetDef, Nm, PadDef, PadKind, PadShape, Placement, PourPoly, PourSpec,
    Pt, Side, Track, Via, mm, um,
};
use boardc::route::{RouteOptions, route_board_ir};
use boardc::zones::fill_zones;

fn rect_pts(x0: f64, y0: f64, x1: f64, y1: f64) -> Vec<Pt> {
    vec![
        Pt::new(mm(x0), mm(y0)),
        Pt::new(mm(x1), mm(y0)),
        Pt::new(mm(x1), mm(y1)),
        Pt::new(mm(x0), mm(y1)),
    ]
}

fn single_pad_fp(
    size_mm: f64,
    kind: PadKind,
    layers: Vec<&str>,
    drill_mm: Option<f64>,
) -> FootprintSpec {
    FootprintSpec::Custom {
        pads: vec![PadDef {
            name: "1".into(),
            kind,
            shape: PadShape::Circle {
                diameter: mm(size_mm),
            },
            pos: Pt::new(0, 0),
            rot_deg: 0.0,
            drill_nm: drill_mm.map(mm),
            layers: layers.into_iter().map(String::from).collect(),
            mask_margin_nm: None,
            paste_margin_nm: None,
        }],
        courtyard: Vec::new(),
        graphics: Vec::new(),
        keepouts: Vec::new(),
        models: Vec::new(),
        pin_types: Default::default(),
        pin_names: BTreeMap::new(),
    }
}

fn place_pin1(refdes: &str, fp: &str, x_mm: f64, y_mm: f64, side: Side, net: &str) -> Placement {
    let mut pin_nets = BTreeMap::new();
    if !net.is_empty() {
        pin_nets.insert("1".into(), net.into());
    }
    Placement {
        refdes: refdes.into(),
        footprint: fp.into(),
        value: String::new(),
        pos: Pt::new(mm(x_mm), mm(y_mm)),
        rot_deg: 0.0,
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

fn base_board() -> BoardIr {
    let spec = BoardSpec {
        name: "drc_test".into(),
        outline: Vec::new(),
        size_nm: Some([mm(40.0), mm(30.0)]),
        cutouts: Vec::new(),
        layers: vec![
            LayerDef {
                name: "F.Cu".into(),
                kind: LayerKind::Signal,
                side: Some(Side::Top),
                thickness_nm: None,
            },
            LayerDef {
                name: "B.Cu".into(),
                kind: LayerKind::Signal,
                side: Some(Side::Bottom),
                thickness_nm: None,
            },
        ],
        footprints: BTreeMap::new(),
        placements: Vec::new(),
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
fn continuous_nm_clearance_zero_slack_and_net_class() {
    let mut board = base_board();
    board.rules.clearance_nm = um(150.0);
    board.rules.track_width_nm = um(150.0);

    // 200um tracks: center-to-center 350um => edge-to-edge 150um (exact pass).
    board.tracks = vec![
        Track {
            net: "A".into(),
            layer: "F.Cu".into(),
            a: Pt::new(mm(5.0), mm(10.0)),
            b: Pt::new(mm(15.0), mm(10.0)),
            width: um(200.0),
            locked: false,
        },
        Track {
            net: "B".into(),
            layer: "F.Cu".into(),
            a: Pt::new(mm(5.0), mm(10.0) + um(350.0)),
            b: Pt::new(mm(15.0), mm(10.0) + um(350.0)),
            width: um(200.0),
            locked: false,
        },
    ];
    let rep_pass = check_board_drc(&board);
    assert!(
        rep_pass
            .violations
            .iter()
            .all(|v| v.kind != ViolationKind::Clearance),
        "exact 150um clearance must pass, got {:?}",
        rep_pass.violations
    );

    // Move track B 1 nm closer => 149,999 nm edge-to-edge => MUST fail (zero fudge slack).
    board.tracks[1].a.y -= 1;
    board.tracks[1].b.y -= 1;
    let rep_fail = check_board_drc(&board);
    let clrs: Vec<&Violation> = rep_fail
        .violations
        .iter()
        .filter(|v| v.kind == ViolationKind::Clearance)
        .collect();
    assert_eq!(clrs.len(), 1);
    assert_eq!(clrs[0].measured_nm, 149_999);
    assert_eq!(clrs[0].required_nm, 150_000);
    assert_eq!(clrs[0].item_a, "track#0");
    assert_eq!(clrs[0].item_b.as_deref(), Some("track#1"));

    // Net-class clearance escalation: HV net class requires 400um.
    board.tracks[1].a.y = mm(10.0) + um(350.0) + um(50.0); // 200um edge-to-edge
    board.tracks[1].b.y = board.tracks[1].a.y;
    board.net_classes.insert(
        "HV".into(),
        NetClass {
            name: "HV".into(),
            track_width_nm: um(200.0),
            clearance_nm: um(400.0),
            via_dia_nm: um(500.0),
            via_drill_nm: um(250.0),
            diff_pair_width_nm: None,
            diff_pair_gap_nm: None,
        },
    );
    board.nets.insert(
        "A".into(),
        NetDef {
            name: "A".into(),
            class: "HV".into(),
            ..Default::default()
        },
    );
    let rep_hv = check_board_drc(&board);
    assert!(
        rep_hv
            .violations
            .iter()
            .any(|v| v.kind == ViolationKind::Clearance && v.required_nm == um(400.0))
    );
}

#[test]
fn copper_clearances_and_filled_pour_clearances() {
    let mut footprints = BTreeMap::new();
    footprints.insert(
        "PAD".into(),
        single_pad_fp(1.0, PadKind::Smd, vec!["F.Cu"], None),
    );
    let mut spec = BoardSpec {
        name: "copper_clr".into(),
        outline: Vec::new(),
        size_nm: Some([mm(30.0), mm(20.0)]),
        cutouts: Vec::new(),
        layers: vec![LayerDef {
            name: "F.Cu".into(),
            kind: LayerKind::Signal,
            side: Some(Side::Top),
            thickness_nm: None,
        }],
        footprints,
        placements: vec![
            place_pin1("U1", "PAD", 5.0, 5.0, Side::Top, "N1"),
            place_pin1("U2", "PAD", 6.1, 5.0, Side::Top, "N2"), // 0.1mm pad-pad < 0.15mm
        ],
        keepouts: Vec::new(),
        pours: Vec::new(),
        tracks: vec![Track {
            net: "N3".into(),
            layer: "F.Cu".into(),
            a: Pt::new(mm(5.0), mm(5.58)), // 0.08mm from U1 pad + 0.1mm half-width = 0.02mm gap
            b: Pt::new(mm(8.0), mm(5.58)),
            width: um(200.0),
            locked: false,
        }],
        vias: vec![
            Via {
                net: "N4".into(),
                pos: Pt::new(mm(12.0), mm(5.0)),
                diameter: um(500.0),
                drill: um(200.0),
                layers: ("F.Cu".into(), "F.Cu".into()),
                locked: false,
                tented: true,
            },
            Via {
                net: "N5".into(),
                pos: Pt::new(mm(12.6), mm(5.0)), // 0.1mm via-via < 0.15mm
                diameter: um(500.0),
                drill: um(200.0),
                layers: ("F.Cu".into(), "F.Cu".into()),
                locked: false,
                tented: true,
            },
        ],
        net_classes: Default::default(),
        nets: Default::default(),
        rules: DesignRules {
            smd_to_smd_nm: Some(um(150.0)),
            via_to_via_nm: Some(um(150.0)),
            via_to_track_nm: Some(um(150.0)),
            ..Default::default()
        },
        constraints: Default::default(),
        graphics: Vec::new(),
        stackup: Default::default(),
        ..Default::default()
    };
    // Add track violating via#0
    spec.tracks.push(Track {
        net: "N6".into(),
        layer: "F.Cu".into(),
        a: Pt::new(mm(12.0), mm(5.4)), // dist 400 - 250 - 100 = 50um < 150um
        b: Pt::new(mm(14.0), mm(5.4)),
        width: um(200.0),
        locked: false,
    });
    let mut board = BoardIr::from_spec(&spec).unwrap();
    // Add a filled pour violating track#1
    board.filled_pours.push(FilledPour {
        net: "GND".into(),
        layer: "F.Cu".into(),
        priority: 0,
        polys: vec![PourPoly {
            outer: rect_pts(13.0, 5.2, 18.0, 8.0),
            holes: Vec::new(),
        }],
    });

    let rep = check_board_drc(&board);
    let pairs: Vec<(&str, &str)> = rep
        .violations
        .iter()
        .filter(|v| v.kind == ViolationKind::Clearance)
        .map(|v| (v.item_a.as_str(), v.item_b.as_deref().unwrap_or("")))
        .collect();
    assert!(
        pairs.contains(&("U1.1", "U2.1")),
        "pad-pad missing: {pairs:?}"
    );
    assert!(
        pairs.contains(&("track#0", "U1.1")),
        "track-pad missing: {pairs:?}"
    );
    assert!(
        pairs.contains(&("via#0", "via#1")),
        "via-via missing: {pairs:?}"
    );
    assert!(
        pairs.contains(&("track#1", "via#0")),
        "track-via missing: {pairs:?}"
    );
    assert!(
        pairs.contains(&("track#1", "pour#0")),
        "track-pour missing: {pairs:?}"
    );
}

#[test]
fn mechanical_drill_edge_keepout_and_courtyard_checks() {
    let mut footprints = BTreeMap::new();
    footprints.insert(
        "THT".into(),
        single_pad_fp(0.5, PadKind::Tht, vec!["*.Cu"], Some(0.4)),
    ); // annular 0.05 < 0.1
    footprints.insert(
        "NPTH".into(),
        single_pad_fp(0.6, PadKind::Npth, vec!["*.Mask"], Some(0.6)),
    );
    footprints.insert(
        "SMD".into(),
        single_pad_fp(1.0, PadKind::Smd, vec!["F.Cu"], None),
    );

    let mut spec = BoardSpec {
        name: "mech".into(),
        outline: Vec::new(),
        size_nm: Some([mm(30.0), mm(20.0)]),
        cutouts: Vec::new(),
        layers: vec![
            LayerDef {
                name: "F.Cu".into(),
                kind: LayerKind::Signal,
                side: Some(Side::Top),
                thickness_nm: None,
            },
            LayerDef {
                name: "B.Cu".into(),
                kind: LayerKind::Signal,
                side: Some(Side::Bottom),
                thickness_nm: None,
            },
        ],
        footprints,
        placements: vec![
            place_pin1("J1", "THT", 5.0, 5.0, Side::Top, "A"),
            place_pin1("H1", "NPTH", 5.6, 5.0, Side::Top, ""), // hole-hole: 0.6 - 0.2 - 0.3 = 0.1 < 0.2
            place_pin1("U1", "SMD", 10.0, 10.0, Side::Top, "VCC"),
            place_pin1("U2", "SMD", 10.8, 10.0, Side::Top, "VCC"), // courtyard overlap on same side
            place_pin1("U3", "SMD", 29.85, 10.0, Side::Top, "SIG"), // edge clearance + component to edge
        ],
        keepouts: vec![
            Keepout {
                kind: KeepoutKind::Track,
                layers: vec!["F.Cu".into()],
                poly: rect_pts(15.0, 14.0, 18.0, 17.0),
            },
            Keepout {
                kind: KeepoutKind::Placement,
                layers: vec!["F.Cu".into()],
                poly: rect_pts(19.0, 4.0, 22.0, 7.0),
            },
        ],
        pours: Vec::new(),
        tracks: vec![
            Track {
                net: "SIG".into(),
                layer: "F.Cu".into(),
                a: Pt::new(mm(5.6), mm(5.38)), // 0.08mm from NPTH drill + 0.05 half-width => 0.03mm < 0.15mm
                b: Pt::new(mm(8.0), mm(5.38)),
                width: um(100.0), // < 150um min trace width
                locked: false,
            },
            Track {
                net: "SIG".into(),
                layer: "F.Cu".into(),
                a: Pt::new(mm(15.5), mm(15.5)),
                b: Pt::new(mm(17.5), mm(15.5)),
                width: um(200.0),
                locked: false,
            },
        ],
        vias: vec![Via {
            net: "VCC".into(),
            pos: Pt::new(mm(10.0), mm(10.0)), // via_at_smd = false!
            diameter: um(350.0),
            drill: um(250.0), // annular 50um < 100um
            layers: ("F.Cu".into(), "B.Cu".into()),
            locked: false,
            tented: true,
        }],
        net_classes: Default::default(),
        nets: Default::default(),
        rules: DesignRules {
            courtyard_clearance_nm: um(100.0),
            via_at_smd: false,
            ..Default::default()
        },
        constraints: Default::default(),
        graphics: Vec::new(),
        stackup: Default::default(),
        ..Default::default()
    };
    spec.placements
        .push(place_pin1("U4", "SMD", 20.5, 5.5, Side::Top, "SIG"));

    let board = BoardIr::from_spec(&spec).unwrap();
    let rep = check_board_drc(&board);
    let kinds: Vec<ViolationKind> = rep.violations.iter().map(|v| v.kind).collect();
    assert!(kinds.contains(&ViolationKind::MinAnnularRing));
    assert!(kinds.contains(&ViolationKind::HoleToHole));
    assert!(kinds.contains(&ViolationKind::DrillToCopper));
    assert!(kinds.contains(&ViolationKind::MinTraceWidth));
    assert!(kinds.contains(&ViolationKind::EdgeClearance));
    assert!(kinds.contains(&ViolationKind::Keepout));
    assert!(kinds.contains(&ViolationKind::ViaAtSmd));
    assert!(kinds.contains(&ViolationKind::CourtyardOverlap));
    assert!(kinds.contains(&ViolationKind::ComponentToEdge));
}

#[test]
fn connectivity_through_filled_pour_and_via_and_open_net() {
    let mut footprints = BTreeMap::new();
    footprints.insert(
        "SMD_TOP".into(),
        single_pad_fp(0.8, PadKind::Smd, vec!["F.Cu"], None),
    );
    footprints.insert(
        "SMD_BOT".into(),
        single_pad_fp(0.8, PadKind::Smd, vec!["B.Cu"], None),
    );

    let spec = BoardSpec {
        name: "pour_via_conn".into(),
        outline: Vec::new(),
        size_nm: Some([mm(30.0), mm(20.0)]),
        cutouts: Vec::new(),
        layers: vec![
            LayerDef {
                name: "F.Cu".into(),
                kind: LayerKind::Signal,
                side: Some(Side::Top),
                thickness_nm: None,
            },
            LayerDef {
                name: "B.Cu".into(),
                kind: LayerKind::Plane,
                side: Some(Side::Bottom),
                thickness_nm: None,
            },
        ],
        footprints,
        placements: vec![
            place_pin1("U1", "SMD_TOP", 5.0, 10.0, Side::Top, "GND"),
            place_pin1("U2", "SMD_BOT", 25.0, 10.0, Side::Top, "GND"), // B.Cu pad via layer def
            place_pin1("U3", "SMD_TOP", 25.0, 15.0, Side::Top, "SIG"),
            place_pin1("U4", "SMD_TOP", 5.0, 15.0, Side::Top, "SIG"),
        ],
        keepouts: Vec::new(),
        pours: vec![PourSpec {
            name: "GND_BOT".into(),
            net: "GND".into(),
            layer: "B.Cu".into(),
            outline: rect_pts(0.0, 0.0, 30.0, 20.0),
            priority: 0,
            clearance_nm: None,
            min_width_nm: mm(0.2),
            thermal_relief: true,
            thermal_gap_nm: mm(0.25),
            thermal_spoke_nm: mm(0.3),
            remove_islands: true,
        }],
        tracks: vec![Track {
            net: "GND".into(),
            layer: "F.Cu".into(),
            a: Pt::new(mm(5.0), mm(10.0)),
            b: Pt::new(mm(7.0), mm(10.0)),
            width: um(250.0),
            locked: false,
        }],
        vias: vec![Via {
            net: "GND".into(),
            pos: Pt::new(mm(7.0), mm(10.0)),
            diameter: um(500.0),
            drill: um(250.0),
            layers: ("F.Cu".into(), "B.Cu".into()),
            locked: false,
            tented: true,
        }],
        net_classes: Default::default(),
        nets: Default::default(),
        rules: DesignRules::default(),
        constraints: Default::default(),
        graphics: Vec::new(),
        stackup: Default::default(),
        ..Default::default()
    };

    let mut board = BoardIr::from_spec(&spec).unwrap();
    fill_zones(&mut board);

    let rep = check_board_drc(&board);
    let opens: Vec<&Violation> = rep
        .violations
        .iter()
        .filter(|v| v.kind == ViolationKind::OpenNet)
        .collect();
    // GND is fully connected (U1.1 -> track -> via -> B.Cu thermal-relieved pour -> U2.1), so only SIG is open!
    assert_eq!(opens.len(), 1, "expected only SIG open, got {opens:?}");
    assert_eq!(opens[0].net_a, "SIG");
    assert_eq!(opens[0].item_a, "U3.1");
    assert_eq!(opens[0].item_b.as_deref(), Some("U4.1"));
}

#[test]
fn constraints_max_length_match_group_cap_proximity_min_vias_bga_shadow() {
    let mut footprints = BTreeMap::new();
    footprints.insert(
        "C0402".into(),
        FootprintSpec::Chip2Pin {
            package: "0402".into(),
            pin_types: Default::default(),
            pin_names: BTreeMap::new(),
        },
    );
    footprints.insert(
        "BGA".into(),
        FootprintSpec::Bga {
            rows: 4,
            cols: 4,
            pitch_nm: mm(0.8),
            ball_dia_nm: mm(0.4),
            pin_types: Default::default(),
            pin_names: BTreeMap::new(),
        },
    );
    footprints.insert(
        "IC".into(),
        FootprintSpec::Qfn {
            pins_per_side: 2,
            pitch_nm: mm(0.5),
            body_nm: mm(2.0),
            ep_nm: 0,
            pin_types: Default::default(),
            pin_names: BTreeMap::new(),
        },
    );

    let mut u1_nets = BTreeMap::new();
    u1_nets.insert("A1".into(), "VCC".into());
    let mut c1_nets = BTreeMap::new();
    c1_nets.insert("1".into(), "VCC".into());
    c1_nets.insert("2".into(), "GND".into());

    let spec = BoardSpec {
        name: "constraints".into(),
        outline: Vec::new(),
        size_nm: Some([mm(40.0), mm(30.0)]),
        cutouts: Vec::new(),
        layers: vec![
            LayerDef {
                name: "F.Cu".into(),
                kind: LayerKind::Signal,
                side: Some(Side::Top),
                thickness_nm: None,
            },
            LayerDef {
                name: "B.Cu".into(),
                kind: LayerKind::Signal,
                side: Some(Side::Bottom),
                thickness_nm: None,
            },
        ],
        footprints,
        placements: vec![
            Placement {
                refdes: "U1".into(),
                footprint: "BGA".into(),
                value: String::new(),
                pos: Pt::new(mm(15.0), mm(15.0)),
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
            Placement {
                refdes: "C1".into(),
                footprint: "C0402".into(),
                value: "100n".into(),
                pos: Pt::new(mm(25.0), mm(15.0)), // > 3mm from U1
                rot_deg: 0.0,
                side: Side::Top,
                locked: false,
                pin_nets: c1_nets,
                mpn: None,
                lcsc: None,
                dnp: false,
                exclude_from_bom: false,
                exclude_from_pos: false,
                cpl_rotation_offset_deg: 0.0,
                ..Default::default()
            },
            // Passive on Bottom under U1 (allowed when allow_passives=true)
            Placement {
                refdes: "C2".into(),
                footprint: "C0402".into(),
                value: "100n".into(),
                pos: Pt::new(mm(15.0), mm(15.0)),
                rot_deg: 0.0,
                side: Side::Bottom,
                locked: false,
                pin_nets: Default::default(),
                mpn: None,
                lcsc: None,
                dnp: false,
                exclude_from_bom: false,
                exclude_from_pos: false,
                cpl_rotation_offset_deg: 0.0,
                ..Default::default()
            },
            // Active IC on Bottom under U1 (must violate BgaShadow)
            Placement {
                refdes: "U2".into(),
                footprint: "IC".into(),
                value: String::new(),
                pos: Pt::new(mm(15.5), mm(15.0)),
                rot_deg: 0.0,
                side: Side::Bottom,
                locked: false,
                pin_nets: Default::default(),
                mpn: None,
                lcsc: None,
                dnp: false,
                exclude_from_bom: false,
                exclude_from_pos: false,
                cpl_rotation_offset_deg: 0.0,
                ..Default::default()
            },
        ],
        keepouts: Vec::new(),
        pours: Vec::new(),
        tracks: vec![
            Track {
                net: "D0".into(),
                layer: "F.Cu".into(),
                a: Pt::new(mm(5.0), mm(5.0)),
                b: Pt::new(mm(20.0), mm(5.0)), // 15mm > 10mm max_length
                width: um(200.0),
                locked: false,
            },
            Track {
                net: "D1".into(),
                layer: "F.Cu".into(),
                a: Pt::new(mm(5.0), mm(6.0)),
                b: Pt::new(mm(10.0), mm(6.0)), // 5mm => 10mm skew > 1mm tolerance
                width: um(200.0),
                locked: false,
            },
        ],
        vias: vec![Via {
            net: "GND".into(),
            pos: Pt::new(mm(15.0), mm(15.0)),
            diameter: um(500.0),
            drill: um(250.0),
            layers: ("F.Cu".into(), "B.Cu".into()),
            locked: false,
            tented: true,
        }],
        net_classes: Default::default(),
        nets: Default::default(),
        rules: DesignRules::default(),
        constraints: Constraints {
            max_lengths: vec![MaxLengthRule {
                nets: vec!["D0".into()],
                max_nm: mm(10.0),
            }],
            match_groups: vec![MatchGroupRule {
                name: "BUS".into(),
                nets: vec!["D0".into(), "D1".into()],
                tolerance_nm: mm(1.0),
                target_nm: None,
            }],
            cap_proximity: vec![CapProximityRule {
                cap_refdes: "C1".into(),
                ic_refdes: "U1".into(),
                net: "VCC".into(),
                max_dist_nm: mm(3.0),
            }],
            min_vias: vec![MinViasRule {
                net: "GND".into(),
                refdes: None,
                min_vias: 2,
            }],
            bga_shadows: vec![BgaShadowRule {
                bga_refdes: "U1".into(),
                min_clearance_nm: mm(0.5),
                allow_passives: true,
            }],
        },
        graphics: Vec::new(),
        stackup: Default::default(),
        ..Default::default()
    };

    let board = BoardIr::from_spec(&spec).unwrap();
    let rep = check_board_drc(&board);
    let kinds: Vec<ViolationKind> = rep.violations.iter().map(|v| v.kind).collect();
    assert!(kinds.contains(&ViolationKind::MaxLength));
    assert!(kinds.contains(&ViolationKind::MatchGroup));
    assert!(kinds.contains(&ViolationKind::CapProximity));
    assert!(kinds.contains(&ViolationKind::MinVias));
    let bga_viols: Vec<&Violation> = rep
        .violations
        .iter()
        .filter(|v| v.kind == ViolationKind::BgaShadow)
        .collect();
    assert_eq!(bga_viols.len(), 1);
    assert_eq!(bga_viols[0].item_a, "U2"); // C2 passive was allowed
}

#[test]
fn hotspot_clustering_collapses_six_violations_around_one_via() {
    let mut violations = Vec::new();
    for i in 0..5 {
        violations.push(Violation {
            kind: ViolationKind::Clearance,
            severity: Severity::Error,
            layer: "F.Cu".into(),
            pos_nm: Pt::new(mm(10.0) + um(50.0 * i as f64), mm(10.0)),
            measured_nm: um(50.0),
            required_nm: um(150.0),
            item_a: "via#0".into(),
            item_b: Some(format!("track#{i}")),
            net_a: "GND".into(),
            net_b: format!("SIG{i}"),
            message: "clearance".into(),
        });
    }
    violations.push(Violation {
        kind: ViolationKind::MinAnnularRing,
        severity: Severity::Error,
        layer: "F.Cu".into(),
        pos_nm: Pt::new(mm(10.0), mm(10.0)),
        measured_nm: um(50.0),
        required_nm: um(100.0),
        item_a: "via#0".into(),
        item_b: None,
        net_a: "GND".into(),
        net_b: String::new(),
        message: "annular".into(),
    });
    // Distant unrelated violation
    violations.push(Violation {
        kind: ViolationKind::MinTraceWidth,
        severity: Severity::Error,
        layer: "B.Cu".into(),
        pos_nm: Pt::new(mm(30.0), mm(25.0)),
        measured_nm: um(80.0),
        required_nm: um(150.0),
        item_a: "track#99".into(),
        item_b: None,
        net_a: "CLK".into(),
        net_b: String::new(),
        message: "width".into(),
    });

    let hotspots = cluster_hotspots(&violations, mm(1.5));
    assert_eq!(hotspots.len(), 2);
    assert_eq!(hotspots[0].violation_count, 6);
    assert_eq!(hotspots[0].root_cause_item, "via#0");
    assert_eq!(hotspots[0].root_cause_net, "GND");
    assert!(hotspots[0].affected_nets.contains(&"SIG0".to_string()));
    assert!(!hotspots[0].action_hint.is_empty());
    assert_eq!(hotspots[1].violation_count, 1);
    assert_eq!(hotspots[1].root_cause_item, "track#99");
}

#[test]
fn cluster_kicad_drc_json_maps_coordinates_and_clusters_hotspots() {
    let mut footprints = BTreeMap::new();
    footprints.insert(
        "PAD".into(),
        single_pad_fp(1.0, PadKind::Smd, vec!["F.Cu"], None),
    );
    let mut spec = BoardSpec {
        name: "kdrc".into(),
        outline: Vec::new(),
        size_nm: Some([mm(20.0), mm(20.0)]),
        cutouts: Vec::new(),
        layers: vec![LayerDef {
            name: "F.Cu".into(),
            kind: LayerKind::Signal,
            side: Some(Side::Top),
            thickness_nm: None,
        }],
        footprints,
        // Note: BoardIr Y-up y = +16.0 corresponds to KiCad Y-down y = -16.0!
        placements: vec![place_pin1("R1", "PAD", 18.0, -16.0, Side::Top, "VCC")],
        keepouts: Vec::new(),
        pours: Vec::new(),
        tracks: vec![Track {
            net: "GND".into(),
            layer: "F.Cu".into(),
            a: Pt::new(mm(15.0), mm(-16.0)),
            b: Pt::new(mm(19.0), mm(-16.0)),
            width: um(250.0),
            locked: false,
        }],
        vias: vec![Via {
            net: "VCC".into(),
            pos: Pt::new(mm(5.0), mm(-5.0)),
            diameter: um(400.0),
            drill: um(350.0),
            layers: ("F.Cu".into(), "B.Cu".into()),
            locked: false,
            tented: true,
        }],
        net_classes: Default::default(),
        nets: Default::default(),
        rules: DesignRules::default(),
        constraints: Default::default(),
        graphics: Vec::new(),
        stackup: Default::default(),
        ..Default::default()
    };
    spec.vias.push(Via {
        net: "GND".into(),
        pos: Pt::new(mm(5.5), mm(-5.0)),
        diameter: um(500.0),
        drill: um(350.0),
        layers: ("F.Cu".into(), "B.Cu".into()),
        locked: false,
        tented: true,
    });
    let board = BoardIr::from_spec(&spec).unwrap();

    let kdrc_json = r#"{
        "coordinate_units": "mm",
        "violations": [
            {
                "type": "annular_width",
                "severity": "error",
                "description": "Annular width (board setup constraints min annular width 0.1000 mm; actual 0.0250 mm)",
                "items": [{ "description": "Via [VCC] on F.Cu - B.Cu", "pos": { "x": 5.0, "y": 5.0 } }]
            },
            {
                "type": "hole_to_hole",
                "severity": "warning",
                "description": "Drilled hole too close to other hole (board setup constraints min 0.2495 mm; actual 0.1500 mm)",
                "items": [
                    { "description": "Via [GND] on F.Cu - B.Cu", "pos": { "x": 5.5, "y": 5.0 } },
                    { "description": "Via [VCC] on F.Cu - B.Cu", "pos": { "x": 5.0, "y": 5.0 } }
                ]
            },
            {
                "type": "clearance",
                "severity": "error",
                "description": "Clearance violation ( clearance 0.2000 mm; actual 0.0500 mm)",
                "items": [
                    { "description": "Via [GND] on F.Cu - B.Cu", "pos": { "x": 5.5, "y": 5.0 } },
                    { "description": "Via [VCC] on F.Cu - B.Cu", "pos": { "x": 5.0, "y": 5.0 } }
                ]
            }
        ],
        "unconnected_items": [
            {
                "type": "unconnected_items",
                "severity": "error",
                "description": "Missing connection between items",
                "items": [
                    { "description": "Pad 1 [VCC] of R1 on F.Cu", "pos": { "x": 18.0, "y": 16.0 } },
                    { "description": "Track [GND] on F.Cu, length 4.0000 mm", "pos": { "x": 15.0, "y": 16.0 } }
                ]
            }
        ]
    }"#;

    let hotspots = cluster_kicad_drc_json(&board, kdrc_json).unwrap();
    assert_eq!(hotspots.len(), 2);
    assert_eq!(hotspots[0].violation_count, 3);
    assert_eq!(hotspots[0].root_cause_item, "via#0");
    assert_eq!(hotspots[0].root_cause_net, "VCC");
    assert_eq!(hotspots[0].centroid_nm.y, -mm(5.0));

    // Also test CLI `boardc drc --kicad-drc ... --json` and `boardc drc --json`
    let tmp_dir = std::env::temp_dir().join("boardc_test_drc_cli");
    std::fs::create_dir_all(&tmp_dir).unwrap();
    let board_path = tmp_dir.join("board.json");
    let kdrc_path = tmp_dir.join("kdrc.json");
    std::fs::write(&board_path, serde_json::to_string_pretty(&board).unwrap()).unwrap();
    std::fs::write(&kdrc_path, kdrc_json).unwrap();

    let out_native = Command::new(env!("CARGO_BIN_EXE_boardc"))
        .args(["drc", board_path.to_str().unwrap(), "--json"])
        .output()
        .unwrap();
    assert!(out_native.status.success());
    let rep: DrcReport = serde_json::from_slice(&out_native.stdout).unwrap();
    assert!(!rep.passed);
    assert!(rep.error_count > 0);

    let out_kdrc = Command::new(env!("CARGO_BIN_EXE_boardc"))
        .args([
            "drc",
            board_path.to_str().unwrap(),
            "--kicad-drc",
            kdrc_path.to_str().unwrap(),
            "--json",
        ])
        .output()
        .unwrap();
    assert!(out_kdrc.status.success());
    let cli_hs: Vec<Hotspot> = serde_json::from_slice(&out_kdrc.stdout).unwrap();
    assert_eq!(cli_hs.len(), 2);
    assert_eq!(cli_hs[0].root_cause_item, "via#0");

    let _ = std::fs::remove_dir_all(&tmp_dir);
}

#[test]
fn regression_inner_via_paste_pad_cutout_edge_flush_pour_clr_and_drc_units() {
    let mut footprints = BTreeMap::new();
    footprints.insert(
        "SMD".into(),
        single_pad_fp(1.0, PadKind::Smd, vec!["F.Cu", "F.Mask"], None),
    );
    footprints.insert(
        "PASTE_ONLY".into(),
        single_pad_fp(1.0, PadKind::Smd, vec!["F.Paste", "*.Mask"], None),
    );
    footprints.insert(
        "BOX".into(),
        FootprintSpec::Custom {
            pads: vec![],
            courtyard: rect_pts(-1.0, -1.0, 1.0, 1.0),
            graphics: vec![],
            keepouts: vec![],
            models: vec![],
            pin_types: Default::default(),
            pin_names: BTreeMap::new(),
        },
    );

    let place = |refdes: &str, fp: &str, x: f64, y: f64, side: Side, net: &str| Placement {
        refdes: refdes.into(),
        footprint: fp.into(),
        value: String::new(),
        pos: Pt::new(mm(x), mm(y)),
        rot_deg: 0.0,
        side,
        locked: true,
        pin_nets: if net.is_empty() {
            BTreeMap::new()
        } else {
            BTreeMap::from([("1".into(), net.into())])
        },
        ..Default::default()
    };

    // 1. Inner-layer via copper pad collides with In1.Cu track, paste-only pad causes no copper DRC,
    //    courtyard overlaps cutout with courtyard_clearance_nm=0, and pour covers cutout.
    let spec = BoardSpec {
        name: "drc_regressions".into(),
        outline: rect_pts(0.0, 0.0, 30.0, 20.0),
        size_nm: None,
        cutouts: vec![rect_pts(14.0, 9.0, 16.0, 11.0)],
        layers: vec![
            LayerDef {
                name: "F.Cu".into(),
                kind: LayerKind::Signal,
                side: Some(Side::Top),
                thickness_nm: None,
            },
            LayerDef {
                name: "In1.Cu".into(),
                kind: LayerKind::Signal,
                side: None,
                thickness_nm: None,
            },
            LayerDef {
                name: "B.Cu".into(),
                kind: LayerKind::Signal,
                side: Some(Side::Bottom),
                thickness_nm: None,
            },
        ],
        footprints: footprints.clone(),
        placements: vec![
            // Overlaps P_REAL on F.Cu, but PASTE_ONLY has no copper layers -> zero clearance error.
            place("P_REAL", "SMD", 5.0, 5.0, Side::Top, "SIG_A"),
            place("P_PASTE", "PASTE_ONLY", 5.0, 5.0, Side::Bottom, "SIG_B"),
            // Courtyard overlaps the (14..16, 9..11) cutout with courtyard_clearance_nm == 0.
            place("U_CUT", "BOX", 14.5, 10.0, Side::Top, ""),
        ],
        keepouts: Vec::new(),
        pours: Vec::new(),
        tracks: vec![Track {
            net: "INNER_A".into(),
            layer: "In1.Cu".into(),
            a: Pt::new(mm(20.0), mm(5.0)),
            b: Pt::new(mm(24.0), mm(5.0)),
            width: mm(0.2),
            locked: true,
        }],
        vias: vec![Via {
            net: "INNER_B".into(),
            pos: Pt::new(mm(22.0), mm(5.32)),
            diameter: mm(0.5),
            drill: mm(0.2),
            layers: ("F.Cu".into(), "B.Cu".into()),
            locked: true,
            tented: true,
        }],
        rules: DesignRules {
            clearance_nm: mm(0.15),
            edge_clearance_nm: mm(0.3),
            courtyard_clearance_nm: 0,
            ..Default::default()
        },
        ..Default::default()
    };
    let mut board = BoardIr::from_spec(&spec).unwrap();
    board.filled_pours.push(FilledPour {
        net: "GND".into(),
        layer: "B.Cu".into(),
        priority: 0,
        polys: vec![PourPoly {
            outer: rect_pts(12.0, 7.0, 18.0, 13.0),
            holes: vec![],
        }],
    });
    let rep = check_board_drc(&board);
    assert!(
        rep.violations
            .iter()
            .any(|v| v.kind == ViolationKind::Clearance && v.layer == "In1.Cu"),
        "expected inner-layer via-to-track clearance violation, got {:?}",
        rep.violations
    );
    assert!(
        !rep.violations.iter().any(|v| v.item_a.contains("P_PASTE")
            || v.item_b.as_deref().is_some_and(|b| b.contains("P_PASTE"))),
        "paste/mask-only pad must not trigger copper DRC: {:?}",
        rep.violations
    );
    assert!(
        rep.violations
            .iter()
            .any(|v| v.kind == ViolationKind::ComponentToEdge && v.item_a == "U_CUT"),
        "expected courtyard-vs-cutout ComponentToEdge violation, got {:?}",
        rep.violations
    );
    assert!(
        rep.violations
            .iter()
            .any(|v| v.kind == ViolationKind::EdgeClearance && v.item_a == "pour#0"),
        "expected pour-covering-cutout EdgeClearance violation, got {:?}",
        rep.violations
    );

    // 2. Edge-flush pad with edge_clearance_nm == 0 passes; lower PourSpec::clearance_nm passes both fill_zones and DRC.
    let mut flush_fps = footprints;
    flush_fps.insert(
        "FLUSH_SMD".into(),
        FootprintSpec::Custom {
            pads: vec![PadDef {
                name: "1".into(),
                kind: PadKind::Smd,
                shape: PadShape::Circle { diameter: mm(1.0) },
                pos: Pt::new(0, 0),
                rot_deg: 0.0,
                drill_nm: None,
                layers: vec!["F.Cu".into(), "F.Mask".into()],
                mask_margin_nm: None,
                paste_margin_nm: None,
            }],
            courtyard: rect_pts(-0.5, -0.5, 0.5, 0.5),
            graphics: vec![],
            keepouts: vec![],
            models: vec![],
            pin_types: Default::default(),
            pin_names: BTreeMap::new(),
        },
    );
    let flush_spec = BoardSpec {
        name: "flush_and_pour_clr".into(),
        outline: rect_pts(0.0, 0.0, 20.0, 20.0),
        layers: vec![LayerDef {
            name: "F.Cu".into(),
            kind: LayerKind::Signal,
            side: Some(Side::Top),
            thickness_nm: None,
        }],
        footprints: flush_fps,
        placements: vec![
            place("EDGE_PAD", "FLUSH_SMD", 0.5, 10.0, Side::Top, "SIG_E"),
            place("SIG_E_END", "SMD", 6.0, 10.0, Side::Top, "SIG_E"),
            place("GND_PAD", "SMD", 10.0, 10.0, Side::Top, "GND"),
        ],
        pours: vec![PourSpec {
            name: "GND_TIGHT".into(),
            net: "GND".into(),
            layer: "F.Cu".into(),
            outline: rect_pts(1.0, 1.0, 19.0, 19.0),
            clearance_nm: Some(mm(0.10)),
            thermal_relief: false,
            ..Default::default()
        }],
        tracks: vec![Track {
            net: "SIG_E".into(),
            layer: "F.Cu".into(),
            a: Pt::new(mm(0.5), mm(10.0)),
            b: Pt::new(mm(6.0), mm(10.0)),
            width: mm(0.2),
            locked: true,
        }],
        rules: DesignRules {
            clearance_nm: mm(0.20),
            edge_clearance_nm: 0,
            ..Default::default()
        },
        ..Default::default()
    };
    let mut flush_board = BoardIr::from_spec(&flush_spec).unwrap();
    fill_zones(&mut flush_board);
    let flush_drc = check_board_drc(&flush_board);
    assert!(
        flush_drc.passed,
        "edge-flush copper and tighter pour clearance should pass DRC, got {:?}",
        flush_drc.violations
    );

    // 3. `cluster_kicad_drc_json` must not mistake "minimum" for unit "um".
    let kdrc = r#"{
        "coordinate_units": "mm",
        "violations": [{
            "type": "clearance",
            "severity": "error",
            "description": "Clearance violation (minimum 0.1500 mm; actual 0.0500 mm)",
            "items": [{ "description": "Track [SIG_E] on F.Cu", "pos": { "x": 3.0, "y": -10.0 } }]
        }]
    }"#;
    let hs = cluster_kicad_drc_json(&flush_board, kdrc).unwrap();
    assert_eq!(hs.len(), 1);
    assert_eq!(hs[0].violations[0].required_nm, mm(0.15));
    assert_eq!(hs[0].violations[0].measured_nm, mm(0.05));
}

fn mask_viols(rep: &DrcReport) -> Vec<&Violation> {
    rep.violations
        .iter()
        .filter(|v| v.kind == ViolationKind::SolderMaskToCopper)
        .collect()
}

fn mask_pairs(rep: &DrcReport) -> Vec<(&str, &str, &str)> {
    mask_viols(rep)
        .into_iter()
        .map(|v| {
            (
                v.layer.as_str(),
                v.item_a.as_str(),
                v.item_b.as_deref().unwrap_or(""),
            )
        })
        .collect()
}

fn mk_track(net: &str, layer: &str, ax: Nm, ay: Nm, bx: Nm, by: Nm, width: Nm) -> Track {
    Track {
        net: net.into(),
        layer: layer.into(),
        a: Pt::new(ax, ay),
        b: Pt::new(bx, by),
        width,
        locked: true,
    }
}

fn mk_via(net: &str, x: Nm, y: Nm, diameter: Nm, drill: Nm, tented: bool) -> Via {
    Via {
        net: net.into(),
        pos: Pt::new(x, y),
        diameter,
        drill,
        layers: ("F.Cu".into(), "B.Cu".into()),
        locked: true,
        tented,
    }
}

#[test]
fn solder_mask_to_copper_bga_margin_via_pour_inner_bottom_and_zero_intrusion() {
    // 1. 0.4 mm-pitch NSMD BGA scenario: 0.23 mm pad + 0.03 mm mask expansion,
    //    trace passes 90 um copper clearance while failing 80 um mask_to_copper_nm.
    let mut fps = BTreeMap::new();
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
    fps.insert(
        "BGA".into(),
        FootprintSpec::Custom {
            pads: vec![bga_pad("A1", -200.0, 200.0), bga_pad("A2", 200.0, 200.0)],
            courtyard: vec![],
            graphics: vec![],
            keepouts: vec![],
            models: vec![],
            pin_types: Default::default(),
            pin_names: BTreeMap::new(),
        },
    );
    let bga_nets = BTreeMap::from([("A1".into(), "DQ0".into()), ("A2".into(), "DQ1".into())]);

    let mut spec = BoardSpec {
        name: "bga_mask".into(),
        outline: Vec::new(),
        size_nm: Some([mm(20.0), mm(20.0)]),
        cutouts: Vec::new(),
        layers: vec![
            LayerDef {
                name: "F.Cu".into(),
                kind: LayerKind::Signal,
                side: Some(Side::Top),
                thickness_nm: None,
            },
            LayerDef {
                name: "In1.Cu".into(),
                kind: LayerKind::Signal,
                side: None,
                thickness_nm: None,
            },
            LayerDef {
                name: "B.Cu".into(),
                kind: LayerKind::Signal,
                side: Some(Side::Bottom),
                thickness_nm: None,
            },
        ],
        footprints: fps,
        placements: vec![Placement {
            refdes: "U1".into(),
            footprint: "BGA".into(),
            pos: Pt::new(mm(10.0), mm(10.0)),
            side: Side::Top,
            locked: true,
            pin_nets: bga_nets,
            ..Default::default()
        }],
        rules: DesignRules {
            track_width_nm: um(60.0),
            clearance_nm: um(90.0),
            mask_expansion_nm: um(30.0),
            mask_to_copper_nm: Some(um(80.0)),
            ..Default::default()
        },
        ..Default::default()
    };

    let mut board = BoardIr::from_spec(&spec).unwrap();
    let a1_y = board.placements[0].pos.y + um(200.0);
    let a1_x = board.placements[0].pos.x - um(200.0);
    let a2_x = board.placements[0].pos.x + um(200.0);
    // Copper gap: 240 - 115 - 30 = 95 um (>= 90 um). Mask gap: 240 - 145 - 30 = 65 um (< 80 um).
    board.tracks = vec![
        mk_track(
            "SIG",
            "F.Cu",
            a1_x - mm(1.0),
            a1_y + um(240.0),
            a1_x + um(50.0),
            a1_y + um(240.0),
            um(60.0),
        ),
        // Same-net track at identical offset on A2 must NOT trigger SolderMaskToCopper.
        mk_track("DQ1", "F.Cu", a2_x, a1_y, a2_x, a1_y + um(300.0), um(60.0)),
        // Inner-layer track directly under A1 pad must NEVER trigger SolderMaskToCopper.
        mk_track(
            "INNER",
            "In1.Cu",
            a1_x - mm(1.0),
            a1_y,
            a1_x + mm(1.0),
            a1_y,
            um(100.0),
        ),
    ];

    let rep = check_board_drc(&board);
    assert!(
        rep.violations
            .iter()
            .all(|v| v.kind != ViolationKind::Clearance),
        "95 um copper gap should pass 90 um clearance: {:?}",
        rep.violations
    );
    let mv = mask_viols(&rep);
    assert_eq!(mv.len(), 1, "expected 1 mask violation: {mv:?}");
    assert_eq!(mv[0].layer, "F.Cu");
    assert_eq!(mv[0].item_a, "U1.A1");
    assert_eq!(mv[0].item_b.as_deref(), Some("track#0"));
    assert_eq!(mv[0].measured_nm, um(65.0));
    assert_eq!(mv[0].required_nm, um(80.0));
    assert!(
        serde_json::to_string(&mv[0].kind)
            .unwrap()
            .contains("solder_mask_to_copper")
    );
    assert!(!rep.hotspots[0].action_hint.is_empty());

    // Disabled when mask_to_copper_nm is None.
    let mut disabled = board.clone();
    disabled.rules.mask_to_copper_nm = None;
    assert!(mask_viols(&check_board_drc(&disabled)).is_empty());

    // 2. Per-pad mask_margin_nm override: negative margin shrinks opening to pass; collapsed pad skipped;
    //    positive override causes violation even with global mask_expansion_nm = 0.
    let mut shrunk = board.clone();
    shrunk.footprints.get_mut("BGA").unwrap().pads[0].mask_margin_nm = Some(-um(20.0));
    assert!(mask_viols(&check_board_drc(&shrunk)).is_empty());

    let mut collapsed = board.clone();
    collapsed.footprints.get_mut("BGA").unwrap().pads[0].mask_margin_nm = Some(-um(150.0));
    assert!(mask_viols(&check_board_drc(&collapsed)).is_empty());

    let mut pos_ov = board.clone();
    pos_ov.rules.mask_expansion_nm = 0;
    pos_ov.footprints.get_mut("BGA").unwrap().pads[0].mask_margin_nm = Some(um(50.0));
    let pos_rep = check_board_drc(&pos_ov);
    let pos_viols = mask_viols(&pos_rep);
    assert_eq!(pos_viols.len(), 1);
    assert_eq!(pos_viols[0].measured_nm, um(45.0));

    // 3. Untented vs tented vias + blind via + filled pour + bottom-side check.
    let mut via_board = base_board();
    via_board.rules.clearance_nm = um(90.0);
    via_board.rules.mask_expansion_nm = um(50.0);
    via_board.rules.mask_to_copper_nm = Some(um(80.0));
    via_board.layers = vec![
        LayerDef {
            name: "F.Cu".into(),
            kind: LayerKind::Signal,
            side: Some(Side::Top),
            thickness_nm: None,
        },
        LayerDef {
            name: "In1.Cu".into(),
            kind: LayerKind::Signal,
            side: None,
            thickness_nm: None,
        },
        LayerDef {
            name: "B.Cu".into(),
            kind: LayerKind::Signal,
            side: Some(Side::Bottom),
            thickness_nm: None,
        },
    ];
    via_board.footprints.insert(
        "BOT_SMD".into(),
        single_pad_fp(0.4, PadKind::Smd, vec!["F.Cu", "F.Mask"], None)
            .resolve("BOT_SMD")
            .unwrap(),
    );
    via_board.placements.push(place_pin1(
        "U_BOT",
        "BOT_SMD",
        5.0,
        5.0,
        Side::Bottom,
        "NET_B1",
    ));
    via_board.tracks = vec![
        mk_track(
            "NET_B2",
            "B.Cu",
            mm(4.0),
            mm(5.0) + um(360.0),
            mm(6.0),
            mm(5.0) + um(360.0),
            um(100.0),
        ),
        mk_track(
            "SIG_V",
            "F.Cu",
            mm(14.0),
            mm(15.0) + um(360.0),
            mm(16.0),
            mm(15.0) + um(360.0),
            um(100.0),
        ),
        mk_track(
            "SIG_T",
            "F.Cu",
            mm(24.0),
            mm(15.0) + um(360.0),
            mm(26.0),
            mm(15.0) + um(360.0),
            um(100.0),
        ),
        // Blind via#3 only spans F.Cu..In1.Cu: B.Cu track near it must pass, F.Cu track must fail.
        mk_track(
            "SIG_BLIND_B",
            "B.Cu",
            mm(34.0),
            mm(15.0) + um(360.0),
            mm(36.0),
            mm(15.0) + um(360.0),
            um(100.0),
        ),
        mk_track(
            "SIG_BLIND_F",
            "F.Cu",
            mm(34.0),
            mm(15.0) - um(360.0),
            mm(36.0),
            mm(15.0) - um(360.0),
            um(100.0),
        ),
    ];
    via_board.vias = vec![
        mk_via("V_UNTENT", mm(15.0), mm(15.0), um(400.0), um(200.0), false),
        mk_via("V_TENT", mm(25.0), mm(15.0), um(400.0), um(200.0), true),
        mk_via(
            "V_COPPER",
            mm(15.0) + um(410.0),
            mm(15.0),
            um(200.0),
            um(100.0),
            true,
        ),
        Via {
            net: "V_BLIND".into(),
            pos: Pt::new(mm(35.0), mm(15.0)),
            diameter: um(400.0),
            drill: um(200.0),
            layers: ("F.Cu".into(), "In1.Cu".into()),
            locked: true,
            tented: false,
        },
    ];
    via_board.filled_pours.push(FilledPour {
        net: "GND".into(),
        layer: "F.Cu".into(),
        priority: 0,
        polys: vec![PourPoly {
            outer: rect_pts(15.3, 14.0, 18.0, 16.0),
            holes: vec![],
        }],
    });

    let vrep = check_board_drc(&via_board);
    let mut pairs = mask_pairs(&vrep);
    pairs.sort();
    let expected = vec![
        ("B.Cu", "U_BOT.1", "track#0"),
        ("B.Cu", "via#0", "via#2"),
        ("F.Cu", "via#0", "pour#0"),
        ("F.Cu", "via#0", "track#1"),
        ("F.Cu", "via#0", "via#2"),
        ("F.Cu", "via#3", "track#4"),
    ];
    assert_eq!(pairs, expected, "exact mask violations mismatch: {pairs:?}");

    // 4. Negative mask_to_copper_nm rejected cleanly in BoardSpec and BoardIr validation.
    spec.rules.mask_to_copper_nm = Some(-1);
    assert!(BoardIr::from_spec(&spec).is_err());

    let mut bad_ir = board.clone();
    bad_ir.rules.mask_to_copper_nm = Some(-10);
    assert!(
        boardc::spec::validate_board_ir_pins(&bad_ir, std::path::Path::new("mem.json")).is_err()
    );
}

#[test]
fn solder_mask_zero_clearance_sagitta_shapes_npth_and_mask_only_pads() {
    // 1. req == 0: track intrusion, 0 nm tangent touch, sub-micron Circle & Oval sagitta into pour,
    //    rotated RoundRect pad, named NPTH inheriting pin_nets, and unnamed mask-only pad on same footprint.
    let mut board = base_board();
    board.rules.clearance_nm = um(50.0);
    board.rules.mask_expansion_nm = um(50.0);
    board.rules.mask_to_copper_nm = Some(0);

    board.footprints.insert(
        "CIRC".into(),
        single_pad_fp(0.9, PadKind::Smd, vec!["F.Cu", "F.Mask"], None)
            .resolve("CIRC")
            .unwrap(),
    );
    board.footprints.insert(
        "OVAL_ROT".into(),
        FootprintSpec::Custom {
            pads: vec![PadDef {
                name: "1".into(),
                kind: PadKind::Smd,
                shape: PadShape::Oval {
                    width: mm(1.9),
                    height: mm(0.9),
                },
                pos: Pt::new(0, 0),
                rot_deg: 0.0,
                drill_nm: None,
                layers: vec!["F.Cu".into(), "F.Mask".into()],
                mask_margin_nm: None,
                paste_margin_nm: None,
            }],
            courtyard: vec![],
            graphics: vec![],
            keepouts: vec![],
            models: vec![],
            pin_types: Default::default(),
            pin_names: BTreeMap::new(),
        }
        .resolve("OVAL_ROT")
        .unwrap(),
    );
    board.footprints.insert(
        "ROUNDRECT_45".into(),
        FootprintSpec::Custom {
            pads: vec![PadDef {
                name: "1".into(),
                kind: PadKind::Smd,
                shape: PadShape::RoundRect {
                    width: um(400.0),
                    height: um(400.0),
                    corner_radius: um(50.0),
                },
                pos: Pt::new(0, 0),
                rot_deg: 45.0,
                drill_nm: None,
                layers: vec!["F.Cu".into(), "F.Mask".into()],
                mask_margin_nm: None,
                paste_margin_nm: None,
            }],
            courtyard: vec![],
            graphics: vec![],
            keepouts: vec![],
            models: vec![],
            pin_types: Default::default(),
            pin_names: BTreeMap::new(),
        }
        .resolve("ROUNDRECT_45")
        .unwrap(),
    );
    board.footprints.insert(
        "NPTH_MASK".into(),
        FootprintSpec::Custom {
            pads: vec![
                PadDef {
                    name: "1".into(),
                    kind: PadKind::Smd,
                    shape: PadShape::Circle { diameter: mm(0.4) },
                    pos: Pt::new(0, 0),
                    rot_deg: 0.0,
                    drill_nm: None,
                    layers: vec!["F.Cu".into(), "F.Mask".into()],
                    mask_margin_nm: None,
                    paste_margin_nm: None,
                },
                PadDef {
                    name: "M1".into(),
                    kind: PadKind::Npth,
                    shape: PadShape::Circle { diameter: mm(0.6) },
                    pos: Pt::new(um(450.0), 0),
                    rot_deg: 0.0,
                    drill_nm: Some(mm(0.4)),
                    layers: vec!["*.Mask".into()],
                    mask_margin_nm: None,
                    paste_margin_nm: None,
                },
            ],
            courtyard: vec![],
            graphics: vec![],
            keepouts: vec![],
            models: vec![],
            pin_types: Default::default(),
            pin_names: BTreeMap::new(),
        }
        .resolve("NPTH_MASK")
        .unwrap(),
    );
    board.footprints.insert(
        "MASK_WIN".into(),
        FootprintSpec::Custom {
            pads: vec![
                PadDef {
                    name: "1".into(),
                    kind: PadKind::Smd,
                    shape: PadShape::Rect {
                        width: mm(0.6),
                        height: mm(0.6),
                    },
                    pos: Pt::new(0, 0),
                    rot_deg: 0.0,
                    drill_nm: None,
                    layers: vec!["F.Cu".into(), "F.Mask".into()],
                    mask_margin_nm: None,
                    paste_margin_nm: None,
                },
                PadDef {
                    name: String::new(),
                    kind: PadKind::Smd,
                    shape: PadShape::Rect {
                        width: mm(0.8),
                        height: mm(0.8),
                    },
                    pos: Pt::new(0, 0),
                    rot_deg: 0.0,
                    drill_nm: None,
                    layers: vec!["F.Mask".into()],
                    mask_margin_nm: None,
                    paste_margin_nm: None,
                },
            ],
            courtyard: vec![],
            graphics: vec![],
            keepouts: vec![],
            models: vec![],
            pin_types: Default::default(),
            pin_names: BTreeMap::new(),
        }
        .resolve("MASK_WIN")
        .unwrap(),
    );

    board.placements = vec![
        place_pin1("C1", "CIRC", 5.0, 10.0, Side::Top, "NC1"),
        place_pin1("O1", "OVAL_ROT", 15.0, 10.0, Side::Top, "NO1"),
        place_pin1("RR1", "ROUNDRECT_45", 25.0, 10.0, Side::Top, "NRR"),
        Placement {
            refdes: "U_NPTH".into(),
            footprint: "NPTH_MASK".into(),
            pos: Pt::new(mm(5.0), mm(20.0)),
            side: Side::Top,
            locked: true,
            // Even if pin_nets assigns "GND" to NPTH M1, NPTH must not inherit net!
            pin_nets: BTreeMap::from([("1".into(), "GND".into()), ("M1".into(), "GND".into())]),
            ..Default::default()
        },
        place_pin1("U_WIN", "MASK_WIN", 15.0, 20.0, Side::Top, "VCC"),
    ];

    // C1 mask radius = 500 um at (5, 10) mm:
    // - track#0 at y = 10 mm + 550 um (width 100 um -> edge at +500 um) is exact 0 nm tangent touch -> PASS!
    // - track#1 at y = 10 mm - 549 um (edge at -499 um) intrudes 1 um -> FAIL!
    // RR1 rotated 45 deg at (25, 10) mm: half-diag = 250*sqrt(2) ~= 353.55 um; track#2 at x=25.30 mm intrudes -> FAIL!
    board.tracks = vec![
        mk_track(
            "T_TANGENT",
            "F.Cu",
            mm(4.0),
            mm(10.0) + um(550.0),
            mm(6.0),
            mm(10.0) + um(550.0),
            um(100.0),
        ),
        mk_track(
            "T_INTRUDE",
            "F.Cu",
            mm(4.0),
            mm(10.0) - um(549.0),
            mm(6.0),
            mm(10.0) - um(549.0),
            um(100.0),
        ),
        mk_track(
            "T_RR",
            "F.Cu",
            mm(25.32),
            mm(9.0),
            mm(25.32),
            mm(11.0),
            um(40.0),
        ),
    ];

    // Pour#0: 200 nm sagitta intrusion into C1's circular mask (r = 500_000 nm) at chord midpoint theta = pi/64,
    // where all 64 vertices of to_polygon(16) stay strictly outside the pour!
    let theta = std::f64::consts::PI / 64.0;
    let (nx, ny) = (theta.cos(), theta.sin());
    let cx = mm(5.0);
    let cy = mm(10.0);
    let d_in = 499_800.0; // 200 nm inside r=500_000, outside chord r*cos(pi/64)=499_397.7
    let p0 = Pt::new(
        cx + (d_in * nx - 200_000.0 * ny).round() as Nm,
        cy + (d_in * ny + 200_000.0 * nx).round() as Nm,
    );
    let p1 = Pt::new(
        cx + (d_in * nx + 200_000.0 * ny).round() as Nm,
        cy + (d_in * ny - 200_000.0 * nx).round() as Nm,
    );
    let p2 = Pt::new(
        p1.x + (500_000.0 * nx).round() as Nm,
        p1.y + (500_000.0 * ny).round() as Nm,
    );
    let p3 = Pt::new(
        p0.x + (500_000.0 * nx).round() as Nm,
        p0.y + (500_000.0 * ny).round() as Nm,
    );

    // Pour#1: Oval O1 at (15, 10) mm has cap b at (15.5, 10) mm, r=500_000; sagitta of 16-seg cap at pi/32 is 2407 nm.
    // Place pour edge at 498_500 nm from cap b along theta_ov = pi/32 (1500 nm inside true cap, outside 16-gon).
    let theta_ov = std::f64::consts::PI / 32.0;
    let (ox, oy) = (theta_ov.cos(), theta_ov.sin());
    let bx = mm(15.5);
    let by = mm(10.0);
    let od_in = 498_500.0;
    let q0 = Pt::new(
        bx + (od_in * ox - 200_000.0 * oy).round() as Nm,
        by + (od_in * oy + 200_000.0 * ox).round() as Nm,
    );
    let q1 = Pt::new(
        bx + (od_in * ox + 200_000.0 * oy).round() as Nm,
        by + (od_in * oy - 200_000.0 * ox).round() as Nm,
    );
    let q2 = Pt::new(
        q1.x + (500_000.0 * ox).round() as Nm,
        q1.y + (500_000.0 * oy).round() as Nm,
    );
    let q3 = Pt::new(
        q0.x + (500_000.0 * ox).round() as Nm,
        q0.y + (500_000.0 * oy).round() as Nm,
    );

    // Pour#2: Exact 0 nm tangent touch to C1's left edge x = 4.50 mm -> must PASS!
    // Pour#3: Surrounds C1_HOLE at (35, 10) mm with a circular/square hole of half-side 500 um (0 nm touch) -> PASS,
    //         while Pour#4 surrounds C2_HOLE at (35, 20) mm with hole half-side 499.8 um (200 nm intrusion) -> FAIL!
    // P1..P3: Rect pad pair (50 um intrusion vs 0 nm touch).
    board.footprints.insert(
        "RECT_PAD".into(),
        FootprintSpec::Custom {
            pads: vec![PadDef {
                name: "1".into(),
                kind: PadKind::Smd,
                shape: PadShape::Rect {
                    width: um(400.0),
                    height: um(400.0),
                },
                pos: Pt::new(0, 0),
                rot_deg: 0.0,
                drill_nm: None,
                layers: vec!["F.Cu".into(), "F.Mask".into()],
                mask_margin_nm: None,
                paste_margin_nm: None,
            }],
            courtyard: vec![],
            graphics: vec![],
            keepouts: vec![],
            models: vec![],
            pin_types: Default::default(),
            pin_names: BTreeMap::new(),
        }
        .resolve("RECT_PAD")
        .unwrap(),
    );
    board.placements.extend([
        place_pin1("P1", "RECT_PAD", 25.0, 20.0, Side::Top, "PA"),
        place_pin1("P2", "RECT_PAD", 25.4, 20.0, Side::Top, "PB"),
        place_pin1("P3", "RECT_PAD", 24.55, 20.0, Side::Top, "PC"),
        place_pin1("C_H_OK", "CIRC", 35.0, 10.0, Side::Top, "NH1"),
        place_pin1("C_H_IN", "CIRC", 35.0, 20.0, Side::Top, "NH2"),
    ]);

    board.filled_pours = vec![
        FilledPour {
            net: "GND".into(),
            layer: "F.Cu".into(),
            priority: 0,
            polys: vec![PourPoly {
                outer: vec![p0, p1, p2, p3],
                holes: vec![],
            }],
        },
        FilledPour {
            net: "GND".into(),
            layer: "F.Cu".into(),
            priority: 0,
            polys: vec![PourPoly {
                outer: vec![q0, q1, q2, q3],
                holes: vec![],
            }],
        },
        FilledPour {
            net: "GND".into(),
            layer: "F.Cu".into(),
            priority: 0,
            polys: vec![PourPoly {
                outer: rect_pts(3.5, 9.5, 4.5, 10.5),
                holes: vec![],
            }],
        },
        FilledPour {
            net: "GND".into(),
            layer: "F.Cu".into(),
            priority: 0,
            polys: vec![PourPoly {
                outer: rect_pts(34.0, 9.0, 36.0, 11.0),
                holes: vec![rect_pts(34.5, 9.5, 35.5, 10.5)],
            }],
        },
        FilledPour {
            net: "GND".into(),
            layer: "F.Cu".into(),
            priority: 0,
            polys: vec![PourPoly {
                outer: rect_pts(34.0, 19.0, 36.0, 21.0),
                holes: vec![vec![
                    Pt::new(mm(35.0) - 499_800, mm(20.0) - 499_800),
                    Pt::new(mm(35.0) + 499_800, mm(20.0) - 499_800),
                    Pt::new(mm(35.0) + 499_800, mm(20.0) + 499_800),
                    Pt::new(mm(35.0) - 499_800, mm(20.0) + 499_800),
                ]],
            }],
        },
    ];

    let rep = check_board_drc(&board);
    let mut pairs = mask_pairs(&rep);
    pairs.sort();
    // Exact set:
    // - C1.1 vs track#1 (not track#0), pour#0 (not pour#2)
    // - C_H_IN.1 vs pour#4 (200 nm into hole edge), C_H_OK.1 vs pour#3 (0 nm tangent) passes
    // - O1.1 vs pour#1
    // - P1.1 vs P2.1 (not P3.1)
    // - RR1.1 vs track#2
    // - U_NPTH.M1 vs U_NPTH.1 (both "GND" in pin_nets, yet NPTH forces net="")
    // - U_WIN. (unnamed mask-only) does NOT false-positive against U_WIN.1 (VCC)!
    assert_eq!(
        pairs,
        vec![
            ("F.Cu", "C1.1", "pour#0"),
            ("F.Cu", "C1.1", "track#1"),
            ("F.Cu", "C_H_IN.1", "pour#4"),
            ("F.Cu", "O1.1", "pour#1"),
            ("F.Cu", "P1.1", "P2.1"),
            ("F.Cu", "P2.1", "P1.1"),
            ("F.Cu", "RR1.1", "track#2"),
            ("F.Cu", "U_NPTH.M1", "U_NPTH.1"),
        ],
        "unexpected mask pairs: {pairs:?}"
    );
    assert!(
        mask_viols(&rep)
            .iter()
            .all(|v| v.message.contains("intrudes on")
                && v.message.contains("0 nm clearance required")),
        "req==0 violation message should use intrusion wording"
    );
}

#[test]
fn fill_zones_honors_mask_to_copper_and_kicad_roundtrip() {
    // 1. fill_zones + DRC passes when mask_expansion_nm + mask_to_copper_nm > clearance_nm
    //    for SMD pads, NPTH pads with mask openings, and untented vias on F.Cu & B.Cu.
    let mut board = base_board();
    board.rules.clearance_nm = um(100.0);
    board.rules.mask_expansion_nm = um(50.0);
    board.rules.mask_to_copper_nm = Some(um(80.0));
    board.footprints.insert(
        "SMD".into(),
        single_pad_fp(0.6, PadKind::Smd, vec!["F.Cu", "F.Mask"], None)
            .resolve("SMD")
            .unwrap(),
    );
    board.footprints.insert(
        "NPTH_M".into(),
        single_pad_fp(1.0, PadKind::Npth, vec!["*.Mask"], Some(0.8))
            .resolve("NPTH_M")
            .unwrap(),
    );
    board.placements = vec![
        place_pin1("U1", "SMD", 10.0, 10.0, Side::Top, "SIG_TOP"),
        place_pin1("U2", "SMD", 10.0, 20.0, Side::Bottom, "SIG_BOT"),
        place_pin1("H1", "NPTH_M", 20.0, 15.0, Side::Top, "GND"),
        place_pin1("UGND_F", "SMD", 30.0, 10.0, Side::Top, "GND"),
        place_pin1("UGND_B", "SMD", 30.0, 20.0, Side::Bottom, "GND"),
    ];
    board.vias = vec![mk_via(
        "V_SIG",
        mm(15.0),
        mm(15.0),
        um(450.0),
        um(200.0),
        false,
    )];
    board.pours = vec![
        PourSpec {
            name: "GND_F".into(),
            net: "GND".into(),
            layer: "F.Cu".into(),
            outline: rect_pts(1.0, 1.0, 39.0, 29.0),
            thermal_relief: false,
            ..Default::default()
        },
        PourSpec {
            name: "GND_B".into(),
            net: "GND".into(),
            layer: "B.Cu".into(),
            outline: rect_pts(1.0, 1.0, 39.0, 29.0),
            thermal_relief: false,
            ..Default::default()
        },
    ];
    fill_zones(&mut board);
    assert_eq!(board.filled_pours.len(), 2);
    let rep = check_board_drc(&board);
    assert!(
        mask_viols(&rep).is_empty(),
        "fill_zones should clear mask_to_copper_nm cleanly, got {:?}",
        mask_viols(&rep)
    );

    // 2. .kicad_pro roundtrip for None, Some(0), Some(80_000), ignore override,
    //    plus (pad_to_mask_clearance ...) and footprint-level (solder_mask_margin ...).
    let mut b_none = board.clone();
    b_none.rules.mask_to_copper_nm = None;
    let pro_none = boardc::kicad_export::render_kicad_pro(&b_none);
    assert!(pro_none.contains("\"solder_mask_bridge\": \"ignore\""));
    let imp_none = boardc::kicad_io::parse_kicad_pcb(
        &boardc::kicad_export::render_kicad_pcb(&b_none),
        Some(&pro_none),
    )
    .unwrap();
    assert_eq!(imp_none.rules.mask_to_copper_nm, None);

    let mut b_zero = board.clone();
    b_zero.rules.mask_to_copper_nm = Some(0);
    let pro_zero = boardc::kicad_export::render_kicad_pro(&b_zero);
    assert!(pro_zero.contains("\"solder_mask_bridge\": \"error\""));
    let imp_zero = boardc::kicad_io::parse_kicad_pcb(
        &boardc::kicad_export::render_kicad_pcb(&b_zero),
        Some(&pro_zero),
    )
    .unwrap();
    assert_eq!(imp_zero.rules.mask_to_copper_nm, Some(0));

    let pro_80 = boardc::kicad_export::render_kicad_pro(&board);
    assert!(pro_80.contains("\"solder_mask_bridge\": \"error\""));
    assert!(pro_80.contains("\"solder_mask_to_copper_clearance\": 0.08"));
    let imp_80 = boardc::kicad_io::parse_kicad_pcb(
        &boardc::kicad_export::render_kicad_pcb(&board),
        Some(&pro_80),
    )
    .unwrap();
    assert_eq!(imp_80.rules.mask_to_copper_nm, Some(um(80.0)));

    // Legacy .kicad_pro with 0.0 clearance and no solder_mask_bridge stays None;
    // explicit "ignore" suppresses even non-zero clearance.
    let pcb_src = boardc::kicad_export::render_kicad_pcb(&board);
    let legacy_pro =
        r#"{"board":{"design_settings":{"rules":{"solder_mask_to_copper_clearance":0.0}}}}"#;
    assert_eq!(
        boardc::kicad_io::parse_kicad_pcb(&pcb_src, Some(legacy_pro))
            .unwrap()
            .rules
            .mask_to_copper_nm,
        None
    );
    let ignored_pro = r#"{"board":{"design_settings":{"rule_severities":{"solder_mask_bridge":"ignore"},"rules":{"solder_mask_to_copper_clearance":0.08}}}}"#;
    assert_eq!(
        boardc::kicad_io::parse_kicad_pcb(&pcb_src, Some(ignored_pro))
            .unwrap()
            .rules
            .mask_to_copper_nm,
        None
    );

    // 3. Import (pad_to_mask_clearance ...) & footprint-level (solder_mask_margin -0.05) accurately.
    let kpcb = r#"(kicad_pcb (version 20241229)
        (layers (0 "F.Cu" signal) (2 "B.Cu" signal))
        (setup (pad_to_mask_clearance 0.03))
        (gr_rect (start 0 0) (end 10 10) (stroke (width 0.1)) (layer "Edge.Cuts"))
        (footprint "Pkg" (layer "F.Cu") (at 5 5 0)
            (property "Reference" "U1")
            (solder_mask_margin -0.05)
            (pad "1" smd rect (at -1 0) (size 0.5 0.5) (layers "F.Cu" "F.Mask"))
            (pad "2" smd rect (at 1 0) (size 0.5 0.5) (layers "F.Cu" "F.Mask") (solder_mask_margin 0.02))
        )
    )"#;
    let parsed = boardc::kicad_io::parse_kicad_pcb(kpcb, None).unwrap();
    assert_eq!(parsed.rules.mask_expansion_nm, um(30.0));
    let fp = &parsed.footprints["Pkg"];
    assert_eq!(fp.pads[0].mask_margin_nm, Some(-um(50.0)));
    assert_eq!(fp.pads[1].mask_margin_nm, Some(um(20.0)));
    let kpcb_zero = kpcb.replace("(pad_to_mask_clearance 0.03)", "(pad_to_mask_clearance 0)");
    assert_eq!(
        boardc::kicad_io::parse_kicad_pcb(&kpcb_zero, None)
            .unwrap()
            .rules
            .mask_expansion_nm,
        0
    );
    let kpcb_neg = kpcb.replace(
        "(pad_to_mask_clearance 0.03)",
        "(pad_to_mask_clearance -0.02)",
    );
    assert_eq!(
        boardc::kicad_io::parse_kicad_pcb(&kpcb_neg, None)
            .unwrap()
            .rules
            .mask_expansion_nm,
        -um(20.0)
    );

    // 4. Real KiCad DRC JSON "type": "solder_mask_bridge" clustering.
    let kdrc = r#"{
        "coordinate_units": "mm",
        "violations": [{
            "type": "solder_mask_bridge",
            "severity": "error",
            "description": "Solder mask bridge (clearance 0.0800 mm; actual 0.0650 mm)",
            "items": [{ "description": "Pad 1 [SIG_TOP] of U1 on F.Mask", "pos": { "x": 10.0, "y": -10.0 } }]
        }]
    }"#;
    let hs = cluster_kicad_drc_json(&board, kdrc).unwrap();
    assert_eq!(hs.len(), 1);
    assert_eq!(hs[0].kinds, vec![ViolationKind::SolderMaskToCopper]);
    assert_eq!(hs[0].violations[0].item_a, "U1.1");
}

#[test]
fn allow_edge_overhang_skips_component_to_edge_but_enforces_pads_courtyard_and_keepout() {
    // Connector with pad inside at local y=+2.0mm and courtyard overhanging bottom edge to y=-3.0mm.
    let mut footprints = BTreeMap::new();
    footprints.insert(
        "EDGE_CONN".into(),
        FootprintSpec::Custom {
            pads: vec![PadDef {
                name: "1".into(),
                kind: PadKind::Smd,
                shape: PadShape::Rect {
                    width: mm(1.0),
                    height: mm(1.0),
                },
                pos: Pt::new(0, mm(2.0)),
                rot_deg: 0.0,
                drill_nm: None,
                layers: vec!["F.Cu".into(), "F.Mask".into()],
                mask_margin_nm: None,
                paste_margin_nm: None,
            }],
            courtyard: rect_pts(-2.0, -3.0, 2.0, 3.0),
            graphics: vec![],
            keepouts: vec![],
            models: vec![],
            pin_types: Default::default(),
            pin_names: BTreeMap::new(),
        },
    );
    footprints.insert(
        "SMD".into(),
        single_pad_fp(0.8, PadKind::Smd, vec!["F.Cu"], None),
    );

    let mut base = BoardSpec {
        name: "overhang_drc".into(),
        outline: rect_pts(0.0, 0.0, 20.0, 20.0),
        cutouts: vec![rect_pts(14.0, 14.0, 16.0, 16.0)],
        layers: vec![LayerDef {
            name: "F.Cu".into(),
            kind: LayerKind::Signal,
            side: Some(Side::Top),
            thickness_nm: None,
        }],
        footprints,
        placements: vec![Placement {
            refdes: "J1".into(),
            footprint: "EDGE_CONN".into(),
            pos: Pt::new(mm(10.0), mm(0.0)), // courtyard y in [-3, +3], pad at y=+2.0 (edge clr 1.5mm >= 0.3mm)
            allow_edge_overhang: false,
            pin_nets: BTreeMap::from([("1".into(), "SIG".into())]),
            ..Default::default()
        }],
        rules: DesignRules {
            edge_clearance_nm: mm(0.3),
            courtyard_clearance_nm: mm(0.2),
            ..Default::default()
        },
        ..Default::default()
    };

    // 1. Default allow_edge_overhang = false raises ComponentToEdge.
    let drc_default = check_board_drc(&BoardIr::from_spec(&base).unwrap());
    assert!(
        drc_default
            .violations
            .iter()
            .any(|v| v.kind == ViolationKind::ComponentToEdge && v.item_a == "J1"),
        "expected ComponentToEdge when allow_edge_overhang=false: {:?}",
        drc_default.violations
    );

    // 2. allow_edge_overhang = true passes cleanly when pads are inside edge_clearance_nm.
    base.placements[0].allow_edge_overhang = true;
    let drc_ok = check_board_drc(&BoardIr::from_spec(&base).unwrap());
    assert!(
        drc_ok.passed,
        "expected clean DRC with allow_edge_overhang=true, got {:?}",
        drc_ok.violations
    );

    // 3. Pad violating outer edge_clearance_nm still triggers EdgeClearance.
    let mut bad_pad_edge = base.clone();
    bad_pad_edge.placements[0].pos = Pt::new(mm(10.0), mm(-1.4)); // pad center y=0.6mm, bottom edge at 0.1mm < 0.3mm
    let drc_edge = check_board_drc(&BoardIr::from_spec(&bad_pad_edge).unwrap());
    assert!(
        drc_edge
            .violations
            .iter()
            .any(|v| v.kind == ViolationKind::EdgeClearance && v.item_a.starts_with("J1.")),
        "expected EdgeClearance on pad near outer edge: {:?}",
        drc_edge.violations
    );

    // 4. Pad violating cutout edge_clearance_nm still triggers EdgeClearance.
    let mut bad_pad_cut = base.clone();
    bad_pad_cut.placements[0].pos = Pt::new(mm(15.0), mm(11.4)); // pad center at (15, 13.4), top at 13.9 -> 0.1mm from cutout y=14
    let drc_cut = check_board_drc(&BoardIr::from_spec(&bad_pad_cut).unwrap());
    assert!(
        drc_cut
            .violations
            .iter()
            .any(|v| v.kind == ViolationKind::EdgeClearance && v.item_a.starts_with("J1.")),
        "expected EdgeClearance on pad near cutout: {:?}",
        drc_cut.violations
    );

    // 5. CourtyardOverlap with another component and placement Keepout still fail.
    let mut bad_cy_ko = base.clone();
    bad_cy_ko
        .placements
        .push(place_pin1("U2", "SMD", 10.0, 2.9, Side::Top, "SIG"));
    bad_cy_ko.keepouts.push(Keepout {
        kind: KeepoutKind::Placement,
        layers: vec!["F.Cu".into()],
        poly: rect_pts(9.0, 0.5, 11.0, 1.5),
    });
    let drc_cy_ko = check_board_drc(&BoardIr::from_spec(&bad_cy_ko).unwrap());
    assert!(
        drc_cy_ko
            .violations
            .iter()
            .any(|v| v.kind == ViolationKind::CourtyardOverlap),
        "expected CourtyardOverlap even with allow_edge_overhang=true: {:?}",
        drc_cy_ko.violations
    );
    assert!(
        drc_cy_ko
            .violations
            .iter()
            .any(|v| v.kind == ViolationKind::Keepout && v.item_a == "J1"),
        "expected placement Keepout even with allow_edge_overhang=true: {:?}",
        drc_cy_ko.violations
    );

    // 6. NPTH drill edge clearance violation is still caught when allow_edge_overhang=true.
    let mut npth_spec = base.clone();
    npth_spec.footprints.insert(
        "EDGE_NPTH".into(),
        FootprintSpec::Custom {
            pads: vec![
                PadDef {
                    name: "1".into(),
                    kind: PadKind::Smd,
                    shape: PadShape::Rect {
                        width: mm(1.0),
                        height: mm(1.0),
                    },
                    pos: Pt::new(0, mm(2.0)),
                    rot_deg: 0.0,
                    drill_nm: None,
                    layers: vec!["F.Cu".into(), "F.Mask".into()],
                    mask_margin_nm: None,
                    paste_margin_nm: None,
                },
                PadDef {
                    name: "H1".into(),
                    kind: PadKind::Npth,
                    shape: PadShape::Circle { diameter: mm(1.0) },
                    pos: Pt::new(0, mm(0.5)),
                    rot_deg: 0.0,
                    drill_nm: Some(mm(1.0)),
                    layers: vec!["*.Mask".into()],
                    mask_margin_nm: None,
                    paste_margin_nm: None,
                },
            ],
            courtyard: rect_pts(-2.0, -3.0, 2.0, 3.0),
            graphics: vec![],
            keepouts: vec![],
            models: vec![],
            pin_types: Default::default(),
            pin_names: BTreeMap::new(),
        },
    );
    npth_spec.placements[0].footprint = "EDGE_NPTH".into();
    npth_spec.placements[0].pos = Pt::new(mm(10.0), mm(0.0));
    let npth_ir = BoardIr::from_spec(&npth_spec).unwrap();
    let drc_npth = check_board_drc(&npth_ir);
    assert!(
        drc_npth
            .violations
            .iter()
            .any(|v| v.kind == ViolationKind::EdgeClearance && v.item_a == "J1.H1"),
        "expected EdgeClearance on NPTH drill with allow_edge_overhang=true: {:?}",
        drc_npth.violations
    );

    // 7. Interior cutout courtyard overhang fails ComponentToEdge even when pads clear the cutout.
    let mut cutout_cy = base.clone();
    cutout_cy.placements[0].pos = Pt::new(mm(15.0), mm(11.2)); // pad top y=13.7 (0.3mm clear), courtyard top y=14.2 (> 14.0)
    let cutout_ir = BoardIr::from_spec(&cutout_cy).unwrap();
    let drc_cut_cy = check_board_drc(&cutout_ir);
    assert!(
        drc_cut_cy
            .violations
            .iter()
            .any(|v| v.kind == ViolationKind::ComponentToEdge && v.item_a == "J1"),
        "expected ComponentToEdge against interior cutout with allow_edge_overhang=true: {:?}",
        drc_cut_cy.violations
    );
    assert!(
        !drc_cut_cy
            .violations
            .iter()
            .any(|v| v.kind == ViolationKind::EdgeClearance),
        "pad clears cutout, only courtyard should fail: {:?}",
        drc_cut_cy.violations
    );

    // 8. Completely off-board placement with allow_edge_overhang=true fails ComponentToEdge.
    let mut off_board = base.clone();
    off_board.placements[0].pos = Pt::new(mm(10.0), mm(-4.0));
    let drc_off = check_board_drc(&BoardIr::from_spec(&off_board).unwrap());
    assert!(
        drc_off
            .violations
            .iter()
            .any(|v| v.kind == ViolationKind::ComponentToEdge && v.item_a == "J1"),
        "expected ComponentToEdge for completely off-board placement: {:?}",
        drc_off.violations
    );

    // 9. run_preflight agrees on clean overhang vs interior cutout / NPTH violations.
    let mut pre_base = base;
    pre_base.placements[0].pin_nets.clear();
    let pre_ok = boardc::preflight::run_preflight(
        &BoardIr::from_spec(&pre_base).unwrap(),
        &Default::default(),
    )
    .unwrap();
    assert!(pre_ok.ok, "expected clean preflight: {pre_ok:?}");
    let pre_cut = boardc::preflight::run_preflight(&cutout_ir, &Default::default()).unwrap();
    assert!(!pre_cut.ok);
    assert!(
        pre_cut
            .layout_violations
            .iter()
            .any(|v| v.kind == ViolationKind::ComponentToEdge && v.item_a == "J1")
    );
    let pre_npth = boardc::preflight::run_preflight(&npth_ir, &Default::default()).unwrap();
    assert!(!pre_npth.ok);
    assert!(
        pre_npth
            .layout_violations
            .iter()
            .any(|v| v.kind == ViolationKind::EdgeClearance && v.item_a == "J1.H1")
    );
}

fn smd_row_fp(pins: &[(&str, f64)], layers: &[&str]) -> FootprintSpec {
    FootprintSpec::Custom {
        pads: pins
            .iter()
            .map(|&(name, x_mm)| PadDef {
                name: name.into(),
                kind: PadKind::Smd,
                shape: PadShape::Circle { diameter: mm(0.8) },
                pos: Pt::new(mm(x_mm), 0),
                rot_deg: 0.0,
                drill_nm: None,
                layers: layers.iter().map(|&l| l.into()).collect(),
                mask_margin_nm: None,
                paste_margin_nm: None,
            })
            .collect(),
        courtyard: Vec::new(),
        graphics: Vec::new(),
        keepouts: Vec::new(),
        models: Vec::new(),
        pin_types: Default::default(),
        pin_names: BTreeMap::new(),
    }
}

#[test]
fn hotspot_filters_unconnected_and_single_pin_nets_and_reroutes_cleanly() {
    let mut footprints = BTreeMap::new();
    footprints.insert(
        "SMD".into(),
        single_pad_fp(0.8, PadKind::Smd, vec!["F.Cu", "F.Mask"], None),
    );
    footprints.insert(
        "U_PKG".into(),
        smd_row_fp(
            &[("J1", 0.0), ("J2", 4.0), ("J3", 8.0), ("J4", 12.0)],
            &["F.Cu", "F.Mask"],
        ),
    );

    // U1.J1 & U2.1 form 2-pin VDDSMPS_1V8; U1.J2 is unconnected-(U1-PadJ2);
    // U1.J3 is generic single-pin NC_UNUSED; U1.J4 & U3.1 form 2-pin UNCONNECTED-(SHARED) (multi-pin!).
    let u1_pin_nets = BTreeMap::from([
        ("J1".into(), "VDDSMPS_1V8".into()),
        ("J2".into(), "unconnected-(U1-PadJ2)".into()),
        ("J3".into(), "NC_UNUSED".into()),
        ("J4".into(), "UNCONNECTED-(SHARED)".into()),
    ]);
    let spec = BoardSpec {
        name: "issue_29_hotspot_filter".into(),
        size_nm: Some([mm(40.0), mm(25.0)]),
        layers: vec![
            LayerDef {
                name: "F.Cu".into(),
                kind: LayerKind::Signal,
                side: Some(Side::Top),
                thickness_nm: None,
            },
            LayerDef {
                name: "B.Cu".into(),
                kind: LayerKind::Signal,
                side: Some(Side::Bottom),
                thickness_nm: None,
            },
        ],
        footprints,
        placements: vec![
            Placement {
                refdes: "U1".into(),
                footprint: "U_PKG".into(),
                pos: Pt::new(mm(6.0), mm(10.0)),
                side: Side::Top,
                locked: true,
                pin_nets: u1_pin_nets,
                ..Default::default()
            },
            place_pin1("U2", "SMD", 26.0, 10.0, Side::Top, "VDDSMPS_1V8"),
            place_pin1("U3", "SMD", 26.0, 16.0, Side::Top, "UNCONNECTED-(SHARED)"),
            // Distant single-pin pad violating edge clearance alone (no routable net in cluster).
            place_pin1("U4", "SMD", 0.3, 20.0, Side::Top, "UNCONNECTED-(U4-Pad1)"),
        ],
        tracks: vec![
            // Track#0 on VDDSMPS_1V8 passes straight across U1.J2 (unconnected-(U1-PadJ2)) and U1.J3 (NC_UNUSED).
            Track {
                net: "VDDSMPS_1V8".into(),
                layer: "F.Cu".into(),
                a: Pt::new(mm(6.0), mm(10.0)),
                b: Pt::new(mm(26.0), mm(10.0)),
                width: um(200.0),
                locked: false,
            },
            Track {
                net: "UNCONNECTED-(SHARED)".into(),
                layer: "F.Cu".into(),
                a: Pt::new(mm(18.0), mm(10.0)),
                b: Pt::new(mm(26.0), mm(16.0)),
                width: um(200.0),
                locked: false,
            },
        ],
        rules: DesignRules {
            clearance_nm: um(150.0),
            mask_expansion_nm: um(50.0),
            mask_to_copper_nm: Some(um(80.0)),
            edge_clearance_nm: um(300.0),
            ..Default::default()
        },
        ..Default::default()
    };

    let mut board = BoardIr::from_spec(&spec).unwrap();
    let rep = check_board_drc(&board);

    // 1. Atomic violations MUST retain exact net_a / net_b diagnostics.
    let j2_viol = rep
        .violations
        .iter()
        .find(|v| {
            v.kind == ViolationKind::Clearance
                && v.item_a == "track#0"
                && v.item_b.as_deref() == Some("U1.J2")
        })
        .expect("expected clearance violation between track#0 and U1.J2");
    assert_eq!(j2_viol.net_a, "VDDSMPS_1V8");
    assert_eq!(j2_viol.net_b, "unconnected-(U1-PadJ2)");

    let j3_viol = rep
        .violations
        .iter()
        .find(|v| {
            v.kind == ViolationKind::Clearance
                && v.item_a == "track#0"
                && v.item_b.as_deref() == Some("U1.J3")
        })
        .expect("expected clearance violation between track#0 and U1.J3");
    assert_eq!(j3_viol.net_a, "VDDSMPS_1V8");
    assert_eq!(j3_viol.net_b, "NC_UNUSED");

    let j2_mask_viol = rep
        .violations
        .iter()
        .find(|v| {
            v.kind == ViolationKind::SolderMaskToCopper
                && v.item_a == "U1.J2"
                && v.item_b.as_deref() == Some("track#0")
        })
        .expect("expected mask violation between U1.J2 and track#0");
    assert_eq!(j2_mask_viol.net_a, "unconnected-(U1-PadJ2)");
    assert_eq!(j2_mask_viol.net_b, "VDDSMPS_1V8");

    // 2. Main copper hotspot around U1 omits single-pin unconnected-(...) and NC_UNUSED,
    //    keeps routable VDDSMPS_1V8 and multi-pin UNCONNECTED-(SHARED), and picks a routable root_cause_net.
    let main_hs = rep
        .hotspots
        .iter()
        .find(|h| h.affected_nets.contains(&"VDDSMPS_1V8".to_string()))
        .expect("expected hotspot containing VDDSMPS_1V8");
    assert!(
        !main_hs
            .affected_nets
            .iter()
            .any(|n| n.eq_ignore_ascii_case("unconnected-(U1-PadJ2)") || n == "NC_UNUSED"),
        "single-pin / unconnected nets must be omitted from affected_nets: {:?}",
        main_hs.affected_nets
    );
    assert!(
        main_hs
            .affected_nets
            .contains(&"UNCONNECTED-(SHARED)".to_string()),
        "multi-pin UNCONNECTED-(SHARED) is routable and must stay in affected_nets: {:?}",
        main_hs.affected_nets
    );
    assert!(
        main_hs.root_cause_net == "VDDSMPS_1V8" || main_hs.root_cause_net == "UNCONNECTED-(SHARED)",
        "root_cause_net must be a routable net, got {:?}",
        main_hs.root_cause_net
    );

    // 3. Isolated non-routable hotspot (U4.1 edge clearance) has empty affected_nets and falls back to its net for root_cause_net.
    let u4_hs = rep
        .hotspots
        .iter()
        .find(|h| h.violations.iter().any(|v| v.item_a == "U4.1"))
        .expect("expected U4.1 edge hotspot");
    assert!(
        u4_hs.affected_nets.is_empty(),
        "isolated single-pin unconnected hotspot must have empty affected_nets: {:?}",
        u4_hs.affected_nets
    );
    assert_eq!(u4_hs.root_cause_net, "UNCONNECTED-(U4-Pad1)");

    // Also verify generic single-pin pad net (not unconnected-(...)) is omitted.
    board.placements[0]
        .pin_nets
        .insert("J2".into(), "NC_J2_GENERIC".into());
    let gen_rep = check_board_drc(&board);
    let gen_hs = gen_rep
        .hotspots
        .iter()
        .find(|h| h.affected_nets.contains(&"VDDSMPS_1V8".to_string()))
        .unwrap();
    assert!(
        !gen_hs.affected_nets.contains(&"NC_J2_GENERIC".to_string())
            && !gen_hs.affected_nets.contains(&"NC_UNUSED".to_string()),
        "generic single-pin pad nets must be omitted: {:?}",
        gen_hs.affected_nets
    );

    // 4. Tiebreak case via check_board_drc on a focused 2-pad + 1-unconnected-pad board:
    //    U1.J2 < track#0 alphabetically, so root_cause_item is U1.J2, yet root_cause_net must be VDDSMPS_1V8.
    let mut tie_board = base_board();
    tie_board.footprints.insert(
        "U_TIE".into(),
        smd_row_fp(&[("J1", 0.0), ("J2", 4.0)], &["F.Cu", "F.Mask"])
            .resolve("U_TIE")
            .unwrap(),
    );
    tie_board.footprints.insert(
        "SMD".into(),
        single_pad_fp(0.8, PadKind::Smd, vec!["F.Cu", "F.Mask"], None)
            .resolve("SMD")
            .unwrap(),
    );
    tie_board.placements = vec![
        Placement {
            refdes: "U1".into(),
            footprint: "U_TIE".into(),
            pos: Pt::new(mm(6.0), mm(10.0)),
            side: Side::Top,
            locked: true,
            pin_nets: BTreeMap::from([
                ("J1".into(), "VDDSMPS_1V8".into()),
                ("J2".into(), "unconnected-(U1-PadJ2)".into()),
            ]),
            ..Default::default()
        },
        place_pin1("U2", "SMD", 20.0, 10.0, Side::Top, "VDDSMPS_1V8"),
    ];
    tie_board.tracks = vec![Track {
        net: "VDDSMPS_1V8".into(),
        layer: "F.Cu".into(),
        a: Pt::new(mm(6.0), mm(10.0)),
        b: Pt::new(mm(20.0), mm(10.0)),
        width: um(200.0),
        locked: false,
    }];
    tie_board.rules.clearance_nm = um(150.0);
    tie_board.rules.mask_to_copper_nm = None;
    let tie_rep = check_board_drc(&tie_board);
    assert_eq!(tie_rep.hotspots.len(), 1);
    assert_eq!(tie_rep.hotspots[0].root_cause_item, "U1.J2");
    assert_eq!(tie_rep.hotspots[0].root_cause_net, "VDDSMPS_1V8");
    assert_eq!(
        tie_rep.hotspots[0].affected_nets,
        vec!["VDDSMPS_1V8".to_string()]
    );
    assert_eq!(
        tie_rep.hotspots[0].action_hint,
        "Reposition or resize U1.J2 to restore clearance to VDDSMPS_1V8"
    );

    // 5. End-to-end: pass hotspot.affected_nets directly to route_board_ir --reroute-nets.
    let reroute_targets: HashSet<String> =
        tie_rep.hotspots[0].affected_nets.iter().cloned().collect();
    let route_cfg = RouteOptions {
        reroute_nets: Some(reroute_targets),
        max_passes: 20,
        multi_start: 2,
        ..Default::default()
    };
    let summary = route_board_ir(&mut tie_board, &route_cfg).unwrap();
    assert_eq!(summary.unconnected_pairs, 0);
    let post_drc = check_board_drc(&tie_board);
    assert!(
        post_drc.passed,
        "rerouting hotspot.affected_nets must resolve DRC cleanly: {:?}",
        post_drc.violations
    );
}

#[test]
fn cluster_hotspots_and_kicad_drc_json_filter_unconnected_nets() {
    // 1. Board-less cluster_hotspots filters case-insensitive unconnected-(...) and picks routable root_cause_net.
    let violations = vec![
        Violation {
            kind: ViolationKind::Clearance,
            severity: Severity::Error,
            layer: "F.Cu".into(),
            pos_nm: Pt::new(mm(10.0), mm(10.0)),
            measured_nm: um(40.0),
            required_nm: um(150.0),
            item_a: "U1.J2".into(),
            item_b: Some("track#0".into()),
            net_a: "unconnected-(U1-PadJ2)".into(),
            net_b: "VDDSMPS_1V8".into(),
            message: "clearance".into(),
        },
        Violation {
            kind: ViolationKind::SolderMaskToCopper,
            severity: Severity::Error,
            layer: "F.Cu".into(),
            pos_nm: Pt::new(mm(10.2), mm(10.0)),
            measured_nm: um(30.0),
            required_nm: um(80.0),
            item_a: "U1.J3".into(),
            item_b: Some("track#0".into()),
            net_a: "UNCONNECTED-(U1-PadJ3)".into(),
            net_b: "VDDSMPS_1V8".into(),
            message: "mask".into(),
        },
        // Isolated cluster with only an unconnected-(...) net.
        Violation {
            kind: ViolationKind::EdgeClearance,
            severity: Severity::Error,
            layer: "F.Cu".into(),
            pos_nm: Pt::new(mm(30.0), mm(20.0)),
            measured_nm: um(50.0),
            required_nm: um(300.0),
            item_a: "U9.1".into(),
            item_b: None,
            net_a: "unconnected-(U9-Pad1)".into(),
            net_b: String::new(),
            message: "edge".into(),
        },
    ];
    let hs = cluster_hotspots(&violations, mm(1.5));
    assert_eq!(hs.len(), 2);
    assert_eq!(hs[0].affected_nets, vec!["VDDSMPS_1V8".to_string()]);
    assert_eq!(hs[0].root_cause_net, "VDDSMPS_1V8");
    // Atomic violations remain intact.
    assert_eq!(hs[0].violations[0].net_a, "unconnected-(U1-PadJ2)");
    assert_eq!(hs[1].affected_nets, Vec::<String>::new());
    assert_eq!(hs[1].root_cause_net, "unconnected-(U9-Pad1)");

    // 2. cluster_kicad_drc_json filters unconnected-(...), single-pin nets, and [no net],
    //    while preserving 2-pad routable nets for --reroute-nets.
    let mut board = base_board();
    board.footprints.insert(
        "U_PAD".into(),
        smd_row_fp(&[("J1", 0.0), ("J2", 4.0), ("J3", 8.0)], &["F.Cu"])
            .resolve("U_PAD")
            .unwrap(),
    );
    board.footprints.insert(
        "SMD".into(),
        single_pad_fp(0.8, PadKind::Smd, vec!["F.Cu"], None)
            .resolve("SMD")
            .unwrap(),
    );
    board.placements = vec![
        Placement {
            refdes: "U1".into(),
            footprint: "U_PAD".into(),
            pos: Pt::new(mm(10.0), mm(10.0)),
            side: Side::Top,
            locked: true,
            pin_nets: BTreeMap::from([
                ("J1".into(), "VDDSMPS_1V8".into()),
                ("J2".into(), "unconnected-(U1-PadJ2)".into()),
                ("J3".into(), "NC_SINGLE".into()),
            ]),
            ..Default::default()
        },
        place_pin1("U2", "SMD", 24.0, 10.0, Side::Top, "VDDSMPS_1V8"),
    ];
    board.tracks = vec![Track {
        net: "VDDSMPS_1V8".into(),
        layer: "F.Cu".into(),
        a: Pt::new(mm(10.0), mm(10.0)),
        b: Pt::new(mm(24.0), mm(10.0)),
        width: um(200.0),
        locked: false,
    }];

    let kdrc = r#"{
        "coordinate_units": "mm",
        "violations": [
            {
                "type": "clearance",
                "severity": "error",
                "description": "Clearance violation (clearance 0.1500 mm; actual 0.0500 mm)",
                "items": [
                    { "description": "Pad J2 [unconnected-(U1-PadJ2)] of U1 on F.Cu", "pos": { "x": 14.0, "y": -10.0 } },
                    { "description": "Track [VDDSMPS_1V8] on F.Cu", "pos": { "x": 14.0, "y": -10.0 } }
                ]
            },
            {
                "type": "clearance",
                "severity": "error",
                "description": "Clearance violation (clearance 0.1500 mm; actual 0.0500 mm)",
                "items": [
                    { "description": "Pad J3 [NC_SINGLE] of U1 on F.Cu", "pos": { "x": 18.0, "y": -10.0 } },
                    { "description": "Track [VDDSMPS_1V8] on F.Cu", "pos": { "x": 18.0, "y": -10.0 } }
                ]
            },
            {
                "type": "clearance",
                "severity": "error",
                "description": "Clearance violation (clearance 0.1500 mm; actual 0.0500 mm)",
                "items": [
                    { "description": "Track [no net] on F.Cu", "pos": { "x": 14.0, "y": -10.0 } },
                    { "description": "Track [VDDSMPS_1V8] on F.Cu", "pos": { "x": 14.0, "y": -10.0 } }
                ]
            }
        ]
    }"#;
    let kdrc_hs = cluster_kicad_drc_json(&board, kdrc).unwrap();
    assert_eq!(kdrc_hs.len(), 1);
    assert_eq!(kdrc_hs[0].affected_nets, vec!["VDDSMPS_1V8".to_string()]);
    assert_eq!(kdrc_hs[0].root_cause_net, "VDDSMPS_1V8");
    assert_eq!(kdrc_hs[0].violations[0].net_a, "unconnected-(U1-PadJ2)");
    assert_eq!(kdrc_hs[0].violations[1].net_a, "NC_SINGLE");
    assert_eq!(kdrc_hs[0].violations[2].net_a, "");

    let summary = route_board_ir(
        &mut board,
        &RouteOptions {
            reroute_nets: Some(kdrc_hs[0].affected_nets.iter().cloned().collect()),
            max_passes: 20,
            multi_start: 2,
            ..Default::default()
        },
    )
    .unwrap();
    assert_eq!(summary.unconnected_pairs, 0);
    assert!(check_board_drc(&board).passed);
}

#[test]
fn hotspot_omits_ignore_route_and_zero_copper_pad_nets() {
    let mut board = base_board();
    board.footprints.insert(
        "SMD".into(),
        single_pad_fp(0.8, PadKind::Smd, vec!["F.Cu", "F.Mask"], None)
            .resolve("SMD")
            .unwrap(),
    );
    board.footprints.insert(
        "MASK_ONLY".into(),
        single_pad_fp(0.8, PadKind::Smd, vec!["F.Mask"], None)
            .resolve("MASK_ONLY")
            .unwrap(),
    );
    board.footprints.insert(
        "NPTH_FP".into(),
        single_pad_fp(1.0, PadKind::Npth, vec!["*.Cu", "*.Mask"], Some(0.6))
            .resolve("NPTH_FP")
            .unwrap(),
    );
    board.nets.insert(
        "PWR_IGNORED".into(),
        NetDef {
            ignore_route: true,
            ..Default::default()
        },
    );
    board.placements = vec![
        place_pin1("U1", "SMD", 10.0, 10.0, Side::Top, "SIG_REAL"),
        place_pin1("U2", "SMD", 20.0, 10.0, Side::Top, "SIG_REAL"),
        place_pin1("U3", "SMD", 15.0, 10.0, Side::Top, "PWR_IGNORED"),
        place_pin1("U4", "SMD", 15.0, 15.0, Side::Top, "PWR_IGNORED"),
        place_pin1("M1", "MASK_ONLY", 15.0, 10.5, Side::Top, "MASK_ONLY_NET"),
        place_pin1("M2", "MASK_ONLY", 25.0, 10.5, Side::Top, "MASK_ONLY_NET"),
        place_pin1("H1", "NPTH_FP", 15.0, 11.0, Side::Top, "NPTH_NET"),
        place_pin1("H2", "NPTH_FP", 25.0, 11.0, Side::Top, "NPTH_NET"),
    ];

    let kdrc = r#"{
        "coordinate_units": "mm",
        "violations": [
            {
                "type": "clearance",
                "severity": "error",
                "description": "Clearance violation (clearance 0.1500 mm; actual 0.0500 mm)",
                "items": [
                    { "description": "Pad 1 [PWR_IGNORED] of U3 on F.Cu", "pos": { "x": 15.0, "y": -10.0 } },
                    { "description": "Track [SIG_REAL] on F.Cu", "pos": { "x": 15.0, "y": -10.0 } }
                ]
            },
            {
                "type": "clearance",
                "severity": "error",
                "description": "Clearance violation (clearance 0.1500 mm; actual 0.0500 mm)",
                "items": [
                    { "description": "Pad 1 [PWR_IGNORED] of U3 on F.Cu", "pos": { "x": 15.0, "y": -10.0 } },
                    { "description": "Pad 1 [MASK_ONLY_NET] of M1 on F.Mask", "pos": { "x": 15.0, "y": -10.5 } }
                ]
            },
            {
                "type": "clearance",
                "severity": "error",
                "description": "Clearance violation (clearance 0.1500 mm; actual 0.0500 mm)",
                "items": [
                    { "description": "Pad 1 [NPTH_NET] of H1 on F.Cu", "pos": { "x": 15.0, "y": -11.0 } },
                    { "description": "Pad 1 [no net] of U3 on F.Cu", "pos": { "x": 15.0, "y": -10.0 } }
                ]
            }
        ]
    }"#;

    let hs = cluster_kicad_drc_json(&board, kdrc).unwrap();
    assert_eq!(hs.len(), 1);
    assert_eq!(hs[0].affected_nets, vec!["SIG_REAL".to_string()]);
    assert_eq!(hs[0].root_cause_net, "SIG_REAL");
    assert_eq!(hs[0].violations[2].net_b, "");
}

#[test]
fn hotspot_omits_one_pin_track_net_so_reroute_freezes_obstacle() {
    let mut board = base_board();
    board.footprints.insert(
        "SMD".into(),
        single_pad_fp(0.8, PadKind::Smd, vec!["F.Cu", "F.Mask"], None)
            .resolve("SMD")
            .unwrap(),
    );
    board.placements = vec![
        place_pin1("U1", "SMD", 15.0, 6.0, Side::Top, "ONE_PIN_STUB"),
        place_pin1("U2", "SMD", 10.0, 10.0, Side::Top, "TWO_PIN_NET"),
        place_pin1("U3", "SMD", 20.0, 10.0, Side::Top, "TWO_PIN_NET"),
    ];
    board.tracks = vec![
        Track {
            net: "ONE_PIN_STUB".into(),
            layer: "F.Cu".into(),
            a: Pt::new(mm(15.0), mm(6.0)),
            b: Pt::new(mm(15.0), mm(11.0)),
            width: um(200.0),
            locked: false,
        },
        Track {
            net: "TWO_PIN_NET".into(),
            layer: "F.Cu".into(),
            a: Pt::new(mm(10.0), mm(10.0)),
            b: Pt::new(mm(20.0), mm(10.0)),
            width: um(200.0),
            locked: false,
        },
    ];

    let rep = check_board_drc(&board);
    assert_eq!(rep.hotspots.len(), 1);
    assert_eq!(
        rep.hotspots[0].affected_nets,
        vec!["TWO_PIN_NET".to_string()]
    );
    assert_eq!(rep.hotspots[0].root_cause_net, "TWO_PIN_NET");

    let summary = route_board_ir(
        &mut board,
        &RouteOptions {
            reroute_nets: Some(rep.hotspots[0].affected_nets.iter().cloned().collect()),
            max_passes: 20,
            multi_start: 2,
            ..Default::default()
        },
    )
    .unwrap();
    assert_eq!(summary.unconnected_pairs, 0);
    let post = check_board_drc(&board);
    assert!(
        post.violations
            .iter()
            .all(|v| v.kind == ViolationKind::TrackDangling && v.item_a == "track#0"),
        "freezing 1-pin track obstacle and rerouting 2-pin net must only keep intentional stub dangling: {:?}",
        post.violations
    );
}

#[test]
fn hotspot_handles_multipin_same_name_and_net_def_pins_fallback() {
    let mut board = base_board();
    let pad = |name: &str, x_mm: f64, layers: &[&str]| PadDef {
        name: name.into(),
        kind: PadKind::Smd,
        shape: PadShape::Circle { diameter: mm(0.8) },
        pos: Pt::new(mm(x_mm), 0),
        rot_deg: 0.0,
        drill_nm: None,
        layers: layers.iter().map(|&l| l.into()).collect(),
        mask_margin_nm: None,
        paste_margin_nm: None,
    };
    let custom_fp = |pads: Vec<PadDef>, name: &str| {
        FootprintSpec::Custom {
            pads,
            courtyard: Vec::new(),
            graphics: Vec::new(),
            keepouts: Vec::new(),
            models: Vec::new(),
            pin_types: Default::default(),
            pin_names: BTreeMap::new(),
        }
        .resolve(name)
        .unwrap()
    };

    // (a) Paste/mask-only pad preceding copper pad on same pin name.
    board.footprints.insert(
        "PASTE_FIRST".into(),
        custom_fp(
            vec![
                pad("1", 0.0, &["F.Paste", "F.Mask"]),
                pad("1", 0.0, &["F.Cu", "F.Mask"]),
            ],
            "PASTE_FIRST",
        ),
    );
    // (b) Two separated copper pads with same pin name.
    board.footprints.insert(
        "SPLIT_TAB".into(),
        custom_fp(
            vec![pad("1", 0.0, &["F.Cu"]), pad("1", 5.0, &["F.Cu"])],
            "SPLIT_TAB",
        ),
    );
    // (c) Two overlapping copper pads on single-pin unconnected-(...).
    board.footprints.insert(
        "OVERLAP_PAD".into(),
        custom_fp(
            vec![pad("1", 0.0, &["F.Cu"]), pad("1", 0.2, &["F.Cu"])],
            "OVERLAP_PAD",
        ),
    );
    board.footprints.insert(
        "SMD".into(),
        single_pad_fp(0.8, PadKind::Smd, vec!["F.Cu"], None)
            .resolve("SMD")
            .unwrap(),
    );

    // Empty pl.pin_nets for U1, U2, F1, F2 to also test board.nets[net].pins fallback.
    let place_bare = |refdes: &str, fp: &str, x_mm: f64, y_mm: f64| Placement {
        refdes: refdes.into(),
        footprint: fp.into(),
        pos: Pt::new(mm(x_mm), mm(y_mm)),
        side: Side::Top,
        locked: true,
        ..Default::default()
    };
    board.placements = vec![
        place_bare("U1", "PASTE_FIRST", 10.0, 10.0),
        place_bare("U2", "SMD", 16.0, 10.0),
        place_pin1("S1", "SPLIT_TAB", 10.0, 12.0, Side::Top, "SPLIT_SHIELD"),
        place_pin1(
            "O1",
            "OVERLAP_PAD",
            10.0,
            14.0,
            Side::Top,
            "unconnected-(O1-Pad1)",
        ),
        place_bare("F1", "SMD", 10.0, 16.0),
        place_bare("F2", "SMD", 16.0, 16.0),
        place_bare("F3", "SMD", 10.0, 18.0),
    ];
    board.nets.insert(
        "PASTE_FIRST_NET".into(),
        NetDef {
            pins: vec![("U1".into(), "1".into()), ("U2".into(), "1".into())],
            ..Default::default()
        },
    );
    board.nets.insert(
        "FALLBACK_NET".into(),
        NetDef {
            pins: vec![("F1".into(), "1".into()), ("F2".into(), "1".into())],
            ..Default::default()
        },
    );
    board.nets.insert(
        "UNCONN_FALLBACK".into(),
        NetDef {
            pins: vec![("F3".into(), "1".into())],
            ..Default::default()
        },
    );

    let violations = [
        Violation {
            kind: ViolationKind::Clearance,
            severity: Severity::Error,
            layer: "F.Cu".into(),
            pos_nm: Pt::new(mm(10.0), mm(12.0)),
            measured_nm: um(50.0),
            required_nm: um(150.0),
            item_a: "U1.1".into(),
            item_b: Some("S1.1".into()),
            net_a: "PASTE_FIRST_NET".into(),
            net_b: "SPLIT_SHIELD".into(),
            message: "clearance".into(),
        },
        Violation {
            kind: ViolationKind::Clearance,
            severity: Severity::Error,
            layer: "F.Cu".into(),
            pos_nm: Pt::new(mm(10.0), mm(14.0)),
            measured_nm: um(50.0),
            required_nm: um(150.0),
            item_a: "O1.1".into(),
            item_b: Some("F1.1".into()),
            net_a: "unconnected-(O1-Pad1)".into(),
            net_b: "FALLBACK_NET".into(),
            message: "clearance".into(),
        },
        Violation {
            kind: ViolationKind::Clearance,
            severity: Severity::Error,
            layer: "F.Cu".into(),
            pos_nm: Pt::new(mm(10.0), mm(15.0)),
            measured_nm: um(50.0),
            required_nm: um(150.0),
            item_a: "F3.1".into(),
            item_b: Some("F1.1".into()),
            net_a: "UNCONN_FALLBACK".into(),
            net_b: "FALLBACK_NET".into(),
            message: "clearance".into(),
        },
    ];

    let hs = boardc::drc::cluster_kicad_drc_json(
        &board,
        &serde_json::json!({
            "coordinate_units": "mm",
            "violations": violations.iter().map(|v| serde_json::json!({
                "type": "clearance",
                "severity": "error",
                "description": "Clearance violation (clearance 0.1500 mm; actual 0.0500 mm)",
                "items": [
                    { "description": format!("Pad 1 [{}] of {} on F.Cu", v.net_a, v.item_a.split('.').next().unwrap()), "pos": { "x": v.pos_nm.x as f64 / 1e6, "y": -(v.pos_nm.y as f64) / 1e6 } },
                    { "description": format!("Pad 1 [{}] of {} on F.Cu", v.net_b, v.item_b.as_deref().unwrap().split('.').next().unwrap()), "pos": { "x": v.pos_nm.x as f64 / 1e6, "y": -(v.pos_nm.y as f64) / 1e6 } }
                ]
            })).collect::<Vec<_>>()
        }).to_string(),
    )
    .unwrap();
    assert_eq!(hs.len(), 1);
    assert_eq!(
        hs[0].affected_nets,
        vec![
            "FALLBACK_NET".to_string(),
            "PASTE_FIRST_NET".to_string(),
            "SPLIT_SHIELD".to_string(),
        ]
    );
}

#[test]
fn native_drc_detects_dangling_tracks_and_vias_and_clusters_kicad_drc() {
    let mk_layer = |name: &str, kind: LayerKind, side: Option<Side>| LayerDef {
        name: name.into(),
        kind,
        side,
        thickness_nm: None,
    };
    let mk_pour = |net: &str, layer: &str, x0: f64, y0: f64, x1: f64, y1: f64| FilledPour {
        net: net.into(),
        layer: layer.into(),
        priority: 0,
        polys: vec![PourPoly {
            outer: rect_pts(x0, y0, x1, y1),
            holes: Vec::new(),
        }],
    };
    let mk_t = |net: &str, layer: &str, ax: f64, ay: f64, bx: f64, by: f64| {
        mk_track(net, layer, mm(ax), mm(ay), mm(bx), mm(by), um(200.0))
    };

    let mut board = base_board();
    board.layers = vec![
        mk_layer("F.Cu", LayerKind::Signal, Some(Side::Top)),
        mk_layer("In1.Cu", LayerKind::Plane, None),
        mk_layer("B.Cu", LayerKind::Signal, Some(Side::Bottom)),
    ];
    board.footprints.insert(
        "SMD".into(),
        single_pad_fp(0.8, PadKind::Smd, vec!["F.Cu"], None)
            .resolve("SMD")
            .unwrap(),
    );
    // THT pad whose copper layers list only F.Cu/B.Cu; plated barrel bridges In1.Cu.
    board.footprints.insert(
        "THT_OB".into(),
        single_pad_fp(0.8, PadKind::Tht, vec!["F.Cu", "B.Cu"], Some(0.4))
            .resolve("THT_OB")
            .unwrap(),
    );
    board.nets.insert(
        "GND".into(),
        NetDef {
            name: "GND".into(),
            ignore_route: true,
            ..Default::default()
        },
    );
    board.placements = vec![
        place_pin1("U1", "SMD", 4.0, 4.0, Side::Top, "SIG_OK"),
        place_pin1("U2", "SMD", 10.0, 4.0, Side::Top, "SIG_OK"),
        place_pin1("U3", "SMD", 7.0, 7.0, Side::Top, "SIG_OK"),
        place_pin1("J1", "THT_OB", 14.0, 4.0, Side::Top, "SIG_OK"),
        place_pin1("U4", "SMD", 4.0, 12.0, Side::Top, "DANG_T"),
        place_pin1("U5", "SMD", 4.0, 18.0, Side::Top, "VIA_1L"),
        place_pin1("U6", "SMD", 16.0, 12.0, Side::Top, "VIA_POUR"),
    ];
    board.filled_pours = vec![
        mk_pour("SIG_OK", "F.Cu", 17.0, 2.0, 21.0, 6.0),
        mk_pour("VIA_POUR", "In1.Cu", 15.0, 10.0, 22.0, 14.0),
        mk_pour("GND", "F.Cu", 24.0, 10.0, 32.0, 16.0),
        mk_pour("GND", "B.Cu", 24.0, 10.0, 32.0, 16.0),
    ];
    board.tracks = vec![
        mk_t("SIG_OK", "F.Cu", 4.0, 4.0, 10.0, 4.0),
        mk_t("SIG_OK", "F.Cu", 7.0, 7.0, 7.0, 4.0),
        mk_t("SIG_OK", "F.Cu", 10.0, 4.0, 12.0, 4.0),
        mk_t("SIG_OK", "In1.Cu", 12.0, 4.0, 14.0, 4.0),
        mk_t("SIG_OK", "F.Cu", 14.0, 4.0, 18.0, 4.0),
        mk_t("SIG_OK", "F.Cu", 4.0, 4.0, 4.0, 8.0),
        mk_t("SIG_OK", "B.Cu", 4.0, 8.0, 10.0, 8.0),
        mk_t("SIG_OK", "F.Cu", 10.0, 8.0, 10.0, 4.0),
        mk_t("DANG_T", "F.Cu", 4.0, 12.0, 8.0, 12.0), // track#8: dangling
        mk_t("", "F.Cu", 4.0, 24.0, 8.0, 24.0),       // track#9: no-net dangling
        mk_t("VIA_1L", "F.Cu", 4.0, 18.0, 8.0, 18.0), // track#10: clean; via#1 dangling
    ];
    let mk_span_via = |net: &str, x: f64, y: f64, l0: &str, l1: &str| Via {
        net: net.into(),
        pos: Pt::new(mm(x), mm(y)),
        diameter: um(500.0),
        drill: um(250.0),
        layers: (l0.into(), l1.into()),
        locked: false,
        tented: true,
    };
    board.vias = vec![
        mk_via("SIG_OK", mm(12.0), mm(4.0), um(500.0), um(250.0), true),
        mk_via("VIA_1L", mm(8.0), mm(18.0), um(500.0), um(250.0), true), // via#1: 1L dangling
        mk_via("VIA_POUR", mm(16.0), mm(12.0), um(500.0), um(250.0), true),
        mk_via("GND", mm(28.0), mm(13.0), um(500.0), um(250.0), true),
        mk_span_via("SIG_OK", 4.0, 8.0, "F.Cu", "In1.Cu"), // via#4: stacked
        mk_span_via("SIG_OK", 4.0, 8.0, "In1.Cu", "B.Cu"), // via#5: stacked
        mk_via("SIG_OK", mm(10.0), mm(8.0), um(500.0), um(250.0), true),
        mk_via("", mm(20.0), mm(24.0), um(500.0), um(250.0), true), // via#7: orphan dangling
    ];

    let rep = check_board_drc(&board);
    assert!(!rep.passed, "dangling tracks/vias must fail DRC");
    let by_kind = |k: ViolationKind| -> Vec<&Violation> {
        rep.violations.iter().filter(|v| v.kind == k).collect()
    };
    let track_dangs = by_kind(ViolationKind::TrackDangling);
    let via_dangs = by_kind(ViolationKind::ViaDangling);

    assert_eq!(track_dangs.len(), 2, "got {track_dangs:?}");
    assert!(track_dangs.iter().all(|v| v.severity == Severity::Error));
    assert!(track_dangs.iter().any(|v| {
        v.item_a == "track#8"
            && v.net_a == "DANG_T"
            && v.pos_nm == Pt::new(mm(8.0), mm(12.0))
            && v.layer == "F.Cu"
    }));
    assert!(
        track_dangs
            .iter()
            .any(|v| v.item_a == "track#9" && v.net_a.is_empty())
    );

    assert_eq!(via_dangs.len(), 2, "got {via_dangs:?}");
    assert!(via_dangs.iter().all(|v| v.severity == Severity::Error));
    assert!(
        via_dangs
            .iter()
            .any(|v| v.item_a == "via#1" && v.measured_nm == 1 && v.required_nm == 2)
    );
    assert!(
        via_dangs
            .iter()
            .any(|v| v.item_a == "via#7" && v.measured_nm == 0 && v.required_nm == 2)
    );

    assert!(rep.hotspots.iter().any(|h| {
        h.kinds.contains(&ViolationKind::TrackDangling)
            && h.action_hint.starts_with("Trim or connect dangling track")
    }));
    assert!(rep.hotspots.iter().any(|h| {
        h.kinds.contains(&ViolationKind::ViaDangling)
            && h.action_hint.starts_with("Connect or remove dangling via")
    }));

    // KiCad DRC JSON mapping + error-beats-warning tiebreak for dominant_kind.
    let kdrc = r#"{
        "coordinate_units": "mm",
        "violations": [
            {
                "type": "track_dangling",
                "severity": "error",
                "description": "Track has unconnected end",
                "items": [{ "description": "Track [DANG_T] on F.Cu, length 4.0000 mm", "pos": { "x": 8.0, "y": -12.0 } }]
            },
            {
                "type": "via_dangling",
                "severity": "warning",
                "description": "Via is not connected or connected on only one layer",
                "items": [{ "description": "Via [<no net>] on F.Cu - B.Cu", "pos": { "x": 20.0, "y": -24.0 } }]
            },
            {
                "type": "clearance",
                "severity": "error",
                "description": "Clearance violation",
                "items": [{ "description": "Via [<no net>] on F.Cu - B.Cu", "pos": { "x": 20.0, "y": -24.0 } }]
            }
        ]
    }"#;
    let hs = cluster_kicad_drc_json(&board, kdrc).unwrap();
    assert_eq!(hs.len(), 2);
    let t_hs = hs
        .iter()
        .find(|h| h.kinds.contains(&ViolationKind::TrackDangling))
        .unwrap();
    assert_eq!(t_hs.violations[0].item_a, "track#8");
    assert_eq!(t_hs.violations[0].net_a, "DANG_T");
    let v_hs = hs
        .iter()
        .find(|h| h.kinds.contains(&ViolationKind::ViaDangling))
        .unwrap();
    assert_eq!(v_hs.violations[0].item_a, "via#7");
    assert_eq!(v_hs.violations[0].net_a, "");
    assert!(
        v_hs.action_hint.starts_with("Reposition or resize"),
        "Severity::Error must beat Severity::Warning on tiebreak, got {}",
        v_hs.action_hint
    );
}

#[test]
fn via_dangling_rejects_orphan_via_bootstrapping_and_allows_stacked_blind_vias() {
    let mk_layer = |name: &str, kind: LayerKind, side: Option<Side>| LayerDef {
        name: name.into(),
        kind,
        side,
        thickness_nm: None,
    };
    let mk_span_via = |net: &str, x: f64, y: f64, l0: &str, l1: &str| Via {
        net: net.into(),
        pos: Pt::new(mm(x), mm(y)),
        diameter: um(500.0),
        drill: um(250.0),
        layers: (l0.into(), l1.into()),
        locked: false,
        tented: true,
    };
    let mk_t = |net: &str, layer: &str, ax: f64, ay: f64, bx: f64, by: f64| {
        mk_track(net, layer, mm(ax), mm(ay), mm(bx), mm(by), um(200.0))
    };

    let mut board = base_board();
    board.layers = vec![
        mk_layer("F.Cu", LayerKind::Signal, Some(Side::Top)),
        mk_layer("In1.Cu", LayerKind::Plane, None),
        mk_layer("B.Cu", LayerKind::Signal, Some(Side::Bottom)),
    ];
    board.footprints.insert(
        "SMD_F".into(),
        single_pad_fp(0.8, PadKind::Smd, vec!["F.Cu"], None)
            .resolve("SMD_F")
            .unwrap(),
    );
    board.footprints.insert(
        "SMD_B".into(),
        single_pad_fp(0.8, PadKind::Smd, vec!["B.Cu"], None)
            .resolve("SMD_B")
            .unwrap(),
    );
    board.placements = vec![
        place_pin1("U1", "SMD_F", 4.0, 4.0, Side::Top, "DUP"),
        place_pin1("U2", "SMD_F", 4.0, 10.0, Side::Top, "STACK"),
        place_pin1("U3", "SMD_B", 14.0, 10.0, Side::Top, "STACK"),
        place_pin1("U4", "SMD_F", 4.0, 16.0, Side::Top, "BLIND_BAD"),
    ];
    board.tracks = vec![
        mk_t("DUP", "F.Cu", 4.0, 4.0, 8.0, 4.0),
        mk_t("STACK", "F.Cu", 4.0, 10.0, 9.0, 10.0),
        mk_t("STACK", "B.Cu", 9.0, 10.0, 14.0, 10.0),
        mk_t("BLIND_BAD", "F.Cu", 4.0, 16.0, 8.0, 16.0),
    ];
    board.vias = vec![
        // Overlapping duplicate THT vias with planar copper only on F.Cu -> BOTH dangling.
        mk_span_via("DUP", 8.0, 4.0, "F.Cu", "B.Cu"),
        mk_span_via("DUP", 8.05, 4.0, "F.Cu", "B.Cu"),
        // Overlapping orphan same-net vias with 0 planar copper -> BOTH dangling.
        mk_span_via("ORPH", 20.0, 4.0, "F.Cu", "B.Cu"),
        mk_span_via("ORPH", 20.05, 4.0, "F.Cu", "B.Cu"),
        // Stacked blind/buried vias bridging F.Cu and B.Cu -> NEITHER dangling.
        mk_span_via("STACK", 9.0, 10.0, "F.Cu", "In1.Cu"),
        mk_span_via("STACK", 9.0, 10.0, "In1.Cu", "B.Cu"),
        // Actually-dangling blind via (F.Cu..In1.Cu touching only F.Cu track) -> dangling.
        mk_span_via("BLIND_BAD", 8.0, 16.0, "F.Cu", "In1.Cu"),
    ];

    let rep = check_board_drc(&board);
    assert!(!rep.passed);
    let mut via_dangs: Vec<&str> = rep
        .violations
        .iter()
        .filter(|v| v.kind == ViolationKind::ViaDangling)
        .map(|v| v.item_a.as_str())
        .collect();
    via_dangs.sort();
    assert_eq!(
        via_dangs,
        vec!["via#0", "via#1", "via#2", "via#3", "via#6"],
        "got {:?}",
        rep.violations
    );
    assert!(
        rep.violations
            .iter()
            .filter(|v| v.kind == ViolationKind::ViaDangling)
            .all(|v| v.severity == Severity::Error)
    );
}

fn silk_viols(rep: &DrcReport, kind: ViolationKind) -> Vec<&Violation> {
    rep.violations.iter().filter(|v| v.kind == kind).collect()
}

fn smd_pad(name: &str, shape: PadShape, pos: Pt, layers: &[&str]) -> PadDef {
    PadDef {
        name: name.into(),
        kind: PadKind::Smd,
        shape,
        pos,
        rot_deg: 0.0,
        drill_nm: None,
        layers: layers.iter().map(|s| (*s).into()).collect(),
        mask_margin_nm: None,
        paste_margin_nm: None,
    }
}

fn custom_fp(
    name: &str,
    pads: Vec<PadDef>,
    courtyard: Vec<Pt>,
    graphics: Vec<boardc::ir::Graphic>,
) -> boardc::ir::FootprintDef {
    FootprintSpec::Custom {
        pads,
        courtyard,
        graphics,
        keepouts: Vec::new(),
        models: Vec::new(),
        pin_types: Default::default(),
        pin_names: BTreeMap::new(),
    }
    .resolve(name)
    .unwrap()
}

#[test]
fn silkscreen_rules_opt_in_toml_validation_and_kicad_clustering() {
    let toml_src = r#"
name = "silk_toml"
size_nm = [20_000_000, 20_000_000]

[rules]
silk_to_mask_nm = 100_000
silk_to_edge_nm = 120_000
"#;
    let spec: BoardSpec = toml::from_str(toml_src).unwrap();
    assert_eq!(spec.rules.silk_to_mask_nm, Some(100_000));
    assert_eq!(spec.rules.silk_to_edge_nm, Some(120_000));
    let ir = BoardIr::from_spec(&spec).unwrap();
    let json = serde_json::to_string(&ir).unwrap();
    let rt: BoardIr = serde_json::from_str(&json).unwrap();
    assert_eq!(rt.rules.silk_to_mask_nm, Some(100_000));
    assert_eq!(rt.rules.silk_to_edge_nm, Some(120_000));

    let default_json = serde_json::to_string(&DesignRules::default()).unwrap();
    assert!(!default_json.contains("silk_to_mask_nm"));
    assert!(!default_json.contains("silk_to_edge_nm"));

    let mut bad_spec = spec.clone();
    bad_spec.rules.silk_to_mask_nm = Some(-1);
    assert!(BoardIr::from_spec(&bad_spec).is_err());

    let mut bad_edge = spec.clone();
    bad_edge.rules.silk_to_edge_nm = Some(-5);
    assert!(BoardIr::from_spec(&bad_edge).is_err());

    let mut bad_ir = ir.clone();
    bad_ir.rules.silk_to_mask_nm = Some(-1);
    assert!(
        boardc::spec::validate_board_ir_pins(&bad_ir, std::path::Path::new("mem.json")).is_err()
    );
    bad_ir.rules.silk_to_mask_nm = None;
    bad_ir.rules.silk_to_edge_nm = Some(-1);
    assert!(
        boardc::spec::validate_board_ir_pins(&bad_ir, std::path::Path::new("mem.json")).is_err()
    );

    // When both rules are None, overlapping/off-board silkscreen emits zero silk violations.
    let mut off_board = base_board();
    off_board.footprints.insert(
        "FP".into(),
        single_pad_fp(1.0, PadKind::Smd, vec!["F.Cu", "F.Mask"], None)
            .resolve("FP")
            .unwrap(),
    );
    off_board
        .placements
        .push(place_pin1("U1", "FP", 10.0, 10.0, Side::Top, "SIG"));
    off_board.graphics.push(boardc::ir::Graphic::Line {
        layer: "F.SilkS".into(),
        a: Pt::new(mm(-1.0), mm(10.0)),
        b: Pt::new(mm(12.0), mm(10.0)),
        width: um(150.0),
    });
    let rep_disabled = check_board_drc(&off_board);
    assert!(silk_viols(&rep_disabled, ViolationKind::SilkOverCopper).is_empty());
    assert!(silk_viols(&rep_disabled, ViolationKind::SilkEdgeClearance).is_empty());

    // KiCad DRC JSON mapping and hotspot hints.
    let kdrc = r#"{
        "coordinate_units": "mm",
        "violations": [
            {
                "type": "silk_over_copper",
                "severity": "error",
                "description": "Silkscreen clipped by solder mask (clearance 0.1000 mm; actual 0.0200 mm)",
                "items": [
                    { "description": "Reference field of U1 on F.SilkS", "pos": { "x": 10.0, "y": -10.0 } },
                    { "description": "Pad 1 [SIG] of U1 on F.Mask", "pos": { "x": 10.0, "y": -10.0 } }
                ]
            },
            {
                "type": "silk_edge_clearance",
                "severity": "warning",
                "description": "Silkscreen too close to board edge (clearance 0.1500 mm; actual 0.0500 mm)",
                "items": [
                    { "description": "Graphic line on B.SilkS", "pos": { "x": 20.0, "y": -0.05 } }
                ]
            },
            {
                "type": "silk_over_copper",
                "severity": "error",
                "description": "Graphic silkscreen clipped by solder mask (clearance 0.1000 mm; actual 0.0100 mm)",
                "items": [
                    { "description": "Graphic line on F.SilkS", "pos": { "x": 10.0, "y": -10.0 } },
                    { "description": "Pad 1 [SIG] of U1 on F.Mask", "pos": { "x": 10.0, "y": -10.0 } }
                ]
            }
        ]
    }"#;
    let hs = cluster_kicad_drc_json(&off_board, kdrc).unwrap();
    assert_eq!(hs.len(), 2);
    let mask_hs = hs
        .iter()
        .find(|h| h.kinds.contains(&ViolationKind::SilkOverCopper))
        .unwrap();
    let ref_v = mask_hs
        .violations
        .iter()
        .find(|v| v.item_a == "U1.refdes")
        .expect("Reference field must map to U1.refdes");
    assert_eq!(ref_v.item_b.as_deref(), Some("U1.1"));
    assert_eq!(ref_v.required_nm, um(100.0));
    assert_eq!(ref_v.measured_nm, um(20.0));
    assert!(
        mask_hs
            .violations
            .iter()
            .any(|v| v.item_a == "Graphic line on F.SilkS"),
        "silkscreen graphic must not snap to overlapped U1.1 pad: {:?}",
        mask_hs.violations
    );
    assert!(!mask_hs.action_hint.is_empty());

    let edge_hs = hs
        .iter()
        .find(|h| h.kinds.contains(&ViolationKind::SilkEdgeClearance))
        .unwrap();
    assert_eq!(edge_hs.violations[0].layer, "B.SilkS");
    assert!(!edge_hs.action_hint.is_empty());
}

#[test]
fn silk_over_copper_sides_shapes_vias_zero_clearance_and_dedup() {
    use boardc::ir::{AutoRefdes, Graphic};

    let mut board = base_board();
    board.rules.mask_expansion_nm = um(50.0);
    board.rules.silk_to_mask_nm = Some(um(100.0));
    board.fab.auto_refdes = AutoRefdes::Off;

    // Footprint with 1.0x1.0 mm SMD pad (mask opening 1.1x1.1 mm) and an F.SilkS rect (4 strokes).
    board.footprints.insert(
        "SMD_BOX".into(),
        custom_fp(
            "SMD_BOX",
            vec![smd_pad(
                "1",
                PadShape::Rect {
                    width: mm(1.0),
                    height: mm(1.0),
                },
                Pt::new(0, 0),
                &["F.Cu", "F.Mask"],
            )],
            vec![],
            vec![Graphic::Rect {
                layer: "F.SilkS".into(),
                min: Pt::new(um(-550.0), um(-550.0)),
                max: Pt::new(um(550.0), um(550.0)),
                width: um(100.0),
                fill: false,
            }],
        ),
    );
    // NPTH pad with *.Mask only, and footprint explicit F.Mask graphic.
    board.footprints.insert(
        "MASK_ONLY".into(),
        custom_fp(
            "MASK_ONLY",
            vec![PadDef {
                name: "H1".into(),
                kind: PadKind::Npth,
                shape: PadShape::Circle { diameter: mm(1.0) },
                pos: Pt::new(0, 0),
                rot_deg: 0.0,
                drill_nm: Some(mm(0.8)),
                layers: vec!["*.Mask".into()],
                mask_margin_nm: Some(0),
                paste_margin_nm: None,
            }],
            vec![],
            vec![Graphic::Circle {
                layer: "F.Mask".into(),
                center: Pt::new(mm(3.0), 0),
                radius: mm(0.5),
                width: 0,
                fill: true,
            }],
        ),
    );

    board.placements = vec![
        place_pin1("U_TOP", "SMD_BOX", 6.0, 10.0, Side::Top, "SIG_T"),
        place_pin1("U_BOT", "SMD_BOX", 14.0, 10.0, Side::Bottom, "SIG_B"),
        place_pin1("M1", "MASK_ONLY", 24.0, 10.0, Side::Top, ""),
    ];
    board.vias = vec![
        mk_via("V_UNTENT", mm(10.0), mm(20.0), um(400.0), um(200.0), false), // mask r=250um
        mk_via("V_TENT", mm(18.0), mm(20.0), um(400.0), um(200.0), true),
    ];
    board.graphics = vec![
        // Board-level F.SilkS text overlapping V_UNTENT (multi-stroke -> 1 violation).
        Graphic::Text {
            layer: "F.SilkS".into(),
            text: "VIA".into(),
            pos: Pt::new(mm(10.0), mm(20.0)),
            height_nm: mm(1.0),
            stroke_nm: um(120.0),
            rot_deg: 0.0,
        },
        // Board-level B.SilkS line overlapping V_UNTENT (bottom side) and V_TENT (ignored).
        Graphic::Line {
            layer: "B.SilkS".into(),
            a: Pt::new(mm(9.0), mm(20.0)),
            b: Pt::new(mm(19.0), mm(20.0)),
            width: um(120.0),
        },
        // Board-level F.SilkS filled rect overlapping M1.H1 (NPTH mask) and M1.mask#0.
        Graphic::Rect {
            layer: "F.SilkS".into(),
            min: Pt::new(mm(23.8), mm(9.8)),
            max: Pt::new(mm(27.2), mm(10.2)),
            width: 0,
            fill: true,
        },
        // Board-level F.SilkS line at exact clearance 100 um from U_TOP pad edge (550 + 50 + 100 = 700 um): MUST pass.
        Graphic::Line {
            layer: "F.SilkS".into(),
            a: Pt::new(mm(6.0) - mm(1.0), mm(10.0) + um(700.0)),
            b: Pt::new(mm(6.0) + mm(1.0), mm(10.0) + um(700.0)),
            width: um(100.0),
        },
    ];

    let rep = check_board_drc(&board);
    let sv = silk_viols(&rep, ViolationKind::SilkOverCopper);
    let mut got: Vec<(&str, &str, &str)> = sv
        .iter()
        .map(|v| {
            (
                v.layer.as_str(),
                v.item_a.as_str(),
                v.item_b.as_deref().unwrap_or(""),
            )
        })
        .collect();
    got.sort();
    assert!(sv.iter().all(|v| v.severity == Severity::Error));
    assert_eq!(
        got,
        vec![
            ("B.SilkS", "U_BOT.silk#0", "U_BOT.1"),
            ("B.SilkS", "silk#1", "via#0"),
            ("F.SilkS", "U_TOP.silk#0", "U_TOP.1"),
            ("F.SilkS", "silk#0", "via#0"),
            ("F.SilkS", "silk#2", "M1.H1"),
            ("F.SilkS", "silk#2", "M1.mask#0"),
        ],
        "unexpected SilkOverCopper violations: {got:?}"
    );
    assert!(!rep.hotspots.is_empty());

    // Auto pin-1 dot & auto_refdes SilkOverCopper: triggered when active, suppressed when dnp or Off.
    let mut auto_board = base_board();
    auto_board.rules.mask_expansion_nm = um(50.0);
    auto_board.rules.silk_to_mask_nm = Some(um(100.0));
    auto_board.fab.auto_refdes = AutoRefdes::Rotate;
    auto_board.footprints.insert(
        "PIN1_TEST".into(),
        custom_fp(
            "PIN1_TEST",
            vec![
                smd_pad(
                    "1",
                    PadShape::Circle { diameter: mm(0.8) },
                    Pt::new(0, 0),
                    &["F.Cu", "F.Mask"],
                ),
                smd_pad(
                    "2",
                    PadShape::Circle { diameter: mm(1.0) },
                    Pt::new(0, mm(1.0)),
                    &["F.Cu", "F.Mask"],
                ),
            ],
            rect_pts(-0.2, -0.2, 0.2, 0.2),
            vec![],
        ),
    );
    auto_board.placements.push(place_pin1(
        "U_AUTO",
        "PIN1_TEST",
        10.0,
        10.0,
        Side::Top,
        "A",
    ));
    let auto_rep = check_board_drc(&auto_board);
    let mut auto_items: Vec<(&str, &str)> = silk_viols(&auto_rep, ViolationKind::SilkOverCopper)
        .into_iter()
        .map(|v| (v.item_a.as_str(), v.item_b.as_deref().unwrap_or("")))
        .collect();
    auto_items.sort();
    assert_eq!(
        auto_items,
        vec![
            ("U_AUTO.pin1_dot", "U_AUTO.1"),
            ("U_AUTO.refdes", "U_AUTO.1"),
            ("U_AUTO.refdes", "U_AUTO.2"),
        ],
        "expected pin1_dot and refdes SilkOverCopper: {auto_rep:?}"
    );
    // Explicit filled silk Graphic::Circle suppresses auto pin1_dot.
    let mut dot_suppressed = auto_board.clone();
    dot_suppressed
        .footprints
        .get_mut("PIN1_TEST")
        .unwrap()
        .graphics
        .push(Graphic::Circle {
            layer: "F.SilkS".into(),
            center: Pt::new(mm(-2.0), mm(-2.0)),
            radius: um(120.0),
            width: 0,
            fill: true,
        });
    let dot_rep = check_board_drc(&dot_suppressed);
    assert!(
        silk_viols(&dot_rep, ViolationKind::SilkOverCopper)
            .iter()
            .all(|v| v.item_a != "U_AUTO.pin1_dot"),
        "explicit filled silk Circle must suppress auto pin1_dot: {:?}",
        dot_rep.violations
    );

    let mut auto_off = auto_board.clone();
    auto_off.fab.auto_refdes = AutoRefdes::Off;
    assert!(silk_viols(&check_board_drc(&auto_off), ViolationKind::SilkOverCopper).is_empty());
    let mut auto_dnp = auto_board.clone();
    auto_dnp.placements[0].dnp = true;
    assert!(silk_viols(&check_board_drc(&auto_dnp), ViolationKind::SilkOverCopper).is_empty());

    // Tented pads (no F.Mask layer or negative mask_margin_nm collapsing opening) emit zero SilkOverCopper.
    let mut tent_board = base_board();
    tent_board.rules.silk_to_mask_nm = Some(um(100.0));
    tent_board.fab.auto_refdes = AutoRefdes::Off;
    let mut collapsed_pad = smd_pad(
        "1",
        PadShape::Circle { diameter: mm(1.0) },
        Pt::new(mm(2.0), 0),
        &["F.Cu", "F.Mask"],
    );
    collapsed_pad.mask_margin_nm = Some(-mm(0.6));
    tent_board.footprints.insert(
        "TENT_FP".into(),
        custom_fp(
            "TENT_FP",
            vec![
                smd_pad(
                    "1",
                    PadShape::Circle { diameter: mm(1.0) },
                    Pt::new(0, 0),
                    &["F.Cu"],
                ),
                collapsed_pad,
            ],
            vec![],
            vec![],
        ),
    );
    tent_board
        .placements
        .push(place_pin1("U_TENT", "TENT_FP", 10.0, 10.0, Side::Top, "A"));
    tent_board.graphics.push(Graphic::Line {
        layer: "F.SilkS".into(),
        a: Pt::new(mm(9.0), mm(10.0)),
        b: Pt::new(mm(13.0), mm(10.0)),
        width: um(150.0),
    });
    assert!(
        silk_viols(&check_board_drc(&tent_board), ViolationKind::SilkOverCopper).is_empty(),
        "tented pads must not trigger SilkOverCopper"
    );

    // Zero clearance (`Some(0)`): overlapping Poly/Capsule fails, exact tangent passes.
    let mut zero_board = base_board();
    zero_board.rules.mask_expansion_nm = 0;
    zero_board.rules.silk_to_mask_nm = Some(0);
    zero_board.fab.auto_refdes = AutoRefdes::Off;
    zero_board.footprints.insert(
        "SQ".into(),
        custom_fp(
            "SQ",
            vec![smd_pad(
                "1",
                PadShape::Rect {
                    width: mm(2.0),
                    height: mm(2.0),
                },
                Pt::new(0, 0),
                &["F.Cu", "F.Mask"],
            )],
            vec![],
            vec![],
        ),
    );
    zero_board
        .placements
        .push(place_pin1("U1", "SQ", 10.0, 10.0, Side::Top, "SIG"));
    zero_board.graphics = vec![
        // Tangent filled rect (x = 11.0..12.0 touches pad right edge x = 11.0): passes at req = 0.
        Graphic::Rect {
            layer: "F.SilkS".into(),
            min: Pt::new(mm(11.0), mm(9.0)),
            max: Pt::new(mm(12.0), mm(11.0)),
            width: 0,
            fill: true,
        },
        // Overlapping filled rect (x = 10.9..12.0 overlaps pad by 0.1 mm): fails at req = 0.
        Graphic::Rect {
            layer: "F.SilkS".into(),
            min: Pt::new(mm(10.9), mm(9.0)),
            max: Pt::new(mm(12.0), mm(11.0)),
            width: 0,
            fill: true,
        },
        // Tangent stroke line (y = 11.05 with width 0.10 => bottom edge y = 11.0): passes at req = 0.
        Graphic::Line {
            layer: "F.SilkS".into(),
            a: Pt::new(mm(9.0), mm(11.05)),
            b: Pt::new(mm(11.0), mm(11.05)),
            width: um(100.0),
        },
        // Overlapping stroke line (y = 11.04 with width 0.10 => intrudes 10 um): fails at req = 0.
        Graphic::Line {
            layer: "F.SilkS".into(),
            a: Pt::new(mm(9.0), mm(11.04)),
            b: Pt::new(mm(11.0), mm(11.04)),
            width: um(100.0),
        },
    ];
    let zrep = check_board_drc(&zero_board);
    let zsv = silk_viols(&zrep, ViolationKind::SilkOverCopper);
    let mut z_items: Vec<&str> = zsv.iter().map(|v| v.item_a.as_str()).collect();
    z_items.sort();
    assert_eq!(z_items, vec!["silk#1", "silk#3"], "got {zsv:?}");
}

#[test]
fn silk_edge_clearance_outline_cutout_overhang_and_auto_refdes() {
    use boardc::ir::{AutoRefdes, Graphic};

    let mut board = base_board(); // 40 x 30 mm
    board.cutouts.push(rect_pts(18.0, 13.0, 22.0, 17.0));
    board.rules.silk_to_edge_nm = Some(um(200.0));
    board.fab.auto_refdes = AutoRefdes::Rotate;

    board.footprints.insert(
        "EDGE_FP".into(),
        custom_fp(
            "EDGE_FP",
            vec![smd_pad(
                "1",
                PadShape::Circle { diameter: mm(0.8) },
                Pt::new(0, 0),
                &["F.Cu", "F.Mask"],
            )],
            rect_pts(-1.0, -1.0, 1.0, 1.0),
            vec![],
        ),
    );

    // U_TOP near top edge (y = 28.5): auto refdes at y = 28.5 + 1.45 = 29.95 violates edge.
    let mut u_top = place_pin1("U_TOP", "EDGE_FP", 10.0, 28.5, Side::Top, "A");
    u_top.allow_edge_overhang = true;
    // U_DNP at same edge height, marked DNP: auto refdes & pin1_dot are suppressed.
    let mut u_dnp = place_pin1("U_DNP", "EDGE_FP", 20.0, 28.5, Side::Top, "B");
    u_dnp.dnp = true;
    // U_BOT near bottom edge (y = 1.5, Side::Bottom): auto refdes at y = 1.5 - 1.45 = 0.05 violates B.SilkS edge.
    let u_bot = place_pin1("U_BOT", "EDGE_FP", 30.0, 1.5, Side::Bottom, "C");
    board.placements = vec![u_top, u_dnp, u_bot];

    board.graphics = vec![
        // Multi-stroke text near left board edge (x = 0.15 mm < 0.20 mm): deduplicated to 1 violation.
        Graphic::Text {
            layer: "F.SilkS".into(),
            text: "EDGE".into(),
            pos: Pt::new(um(450.0), mm(10.0)),
            height_nm: mm(0.8),
            stroke_nm: um(100.0),
            rot_deg: 0.0,
        },
        // Circle intruding into internal cutout (cutout x=[18..22], y=[13..17]).
        Graphic::Circle {
            layer: "F.SilkS".into(),
            center: Pt::new(mm(17.7), mm(15.0)),
            radius: um(250.0),
            width: um(100.0),
            fill: false,
        },
        // Off-board filled poly completely outside outline.
        Graphic::Rect {
            layer: "B.SilkS".into(),
            min: Pt::new(mm(41.0), mm(5.0)),
            max: Pt::new(mm(43.0), mm(7.0)),
            width: 0,
            fill: true,
        },
        // Safe line 250 um from edge (center 300 um - half width 50 um = 250 um >= 200 um): passes.
        Graphic::Line {
            layer: "F.SilkS".into(),
            a: Pt::new(mm(5.0), um(300.0)),
            b: Pt::new(mm(15.0), um(300.0)),
            width: um(100.0),
        },
        // Near-edge line strictly inside board at 100 um clearance (center 150 um - half width 50 um = 100 um < 200 um).
        Graphic::Line {
            layer: "F.SilkS".into(),
            a: Pt::new(mm(5.0), um(150.0)),
            b: Pt::new(mm(15.0), um(150.0)),
            width: um(100.0),
        },
    ];

    let rep = check_board_drc(&board);
    let ev = silk_viols(&rep, ViolationKind::SilkEdgeClearance);
    let mut items: Vec<(&str, &str)> = ev
        .iter()
        .map(|v| (v.layer.as_str(), v.item_a.as_str()))
        .collect();
    items.sort();
    assert_eq!(
        items,
        vec![
            ("B.SilkS", "U_BOT.refdes"),
            ("B.SilkS", "silk#2"),
            ("F.SilkS", "U_TOP.refdes"),
            ("F.SilkS", "silk#0"),
            ("F.SilkS", "silk#1"),
            ("F.SilkS", "silk#4"),
        ],
        "unexpected SilkEdgeClearance violations: {ev:?}"
    );
    assert!(ev.iter().all(|v| {
        v.severity == Severity::Error
            && v.item_b.as_deref() == Some("board_edge")
            && v.required_nm == um(200.0)
    }));
    let near_v = ev.iter().find(|v| v.item_a == "silk#4").unwrap();
    assert_eq!(near_v.measured_nm, um(100.0));

    // Switching auto_refdes = Off suppresses U_TOP.refdes and U_BOT.refdes.
    let mut no_refdes = board.clone();
    no_refdes.fab.auto_refdes = AutoRefdes::Off;
    let rep_off = check_board_drc(&no_refdes);
    let mut ev_off: Vec<&str> = silk_viols(&rep_off, ViolationKind::SilkEdgeClearance)
        .into_iter()
        .map(|v| v.item_a.as_str())
        .collect();
    ev_off.sort();
    assert_eq!(ev_off, vec!["silk#0", "silk#1", "silk#2", "silk#4"]);

    // Verify pipeline DRC stage invalidation when board.fab or board.graphics changes.
    let tmp = std::env::temp_dir().join(format!("boardc_silk_cache_{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&tmp);
    let cfg = boardc::pipeline::BuildOptions {
        cache_dir: tmp.join("cache"),
        use_cache: true,
        emit_outputs: false,
        ..Default::default()
    };
    let mut pipe_board = base_board();
    pipe_board.rules.silk_to_edge_nm = Some(um(200.0));
    pipe_board
        .footprints
        .insert("EDGE_FP".into(), board.footprints["EDGE_FP"].clone());
    pipe_board.placements.push(board.placements[0].clone()); // U_TOP with auto_refdes = Rotate
    let r1 = boardc::pipeline::compile_board(pipe_board.clone(), &cfg).unwrap();
    assert!(!r1.drc.passed);
    pipe_board.fab.auto_refdes = AutoRefdes::Off;
    let r2 = boardc::pipeline::compile_board(pipe_board.clone(), &cfg).unwrap();
    assert!(
        !r2.stage_hit(boardc::pipeline::StageKind::Drc),
        "DRC cache must invalidate when board.fab changes"
    );
    assert!(r2.drc.passed);
    pipe_board.graphics.push(Graphic::Line {
        layer: "F.SilkS".into(),
        a: Pt::new(mm(5.0), um(100.0)),
        b: Pt::new(mm(15.0), um(100.0)),
        width: um(100.0),
    });
    let r3 = boardc::pipeline::compile_board(pipe_board, &cfg).unwrap();
    assert!(
        !r3.stage_hit(boardc::pipeline::StageKind::Drc),
        "DRC cache must invalidate when board.graphics changes"
    );
    assert!(!r3.drc.passed);
    let _ = std::fs::remove_dir_all(&tmp);

    // silk_to_edge_nm == Some(0): tangent passes, off-board/cutout fails with clean message.
    let mut edge_zero = base_board();
    edge_zero.cutouts.push(rect_pts(18.0, 13.0, 22.0, 17.0));
    edge_zero.rules.silk_to_edge_nm = Some(0);
    edge_zero.fab.auto_refdes = AutoRefdes::Off;
    edge_zero.graphics = vec![
        // Tangent line (y = 0.05 with width 0.10 => bottom edge at y = 0.0): passes.
        Graphic::Line {
            layer: "F.SilkS".into(),
            a: Pt::new(mm(5.0), um(50.0)),
            b: Pt::new(mm(10.0), um(50.0)),
            width: um(100.0),
        },
        // Tangent filled rect (y = 0..1): passes.
        Graphic::Rect {
            layer: "F.SilkS".into(),
            min: Pt::new(mm(11.0), 0),
            max: Pt::new(mm(13.0), mm(1.0)),
            width: 0,
            fill: true,
        },
        // Off-board line: fails.
        Graphic::Line {
            layer: "F.SilkS".into(),
            a: Pt::new(mm(5.0), um(20.0)),
            b: Pt::new(mm(10.0), um(20.0)),
            width: um(100.0),
        },
        // Filled poly overlapping internal cutout: fails.
        Graphic::Poly {
            layer: "F.SilkS".into(),
            verts: rect_pts(17.5, 14.0, 18.5, 16.0),
            width: 0,
            fill: true,
        },
    ];
    let ez_rep = check_board_drc(&edge_zero);
    let ez_ev = silk_viols(&ez_rep, ViolationKind::SilkEdgeClearance);
    let mut ez_items: Vec<&str> = ez_ev.iter().map(|v| v.item_a.as_str()).collect();
    ez_items.sort();
    assert_eq!(ez_items, vec!["silk#2", "silk#3"], "got {ez_ev:?}");
    for v in ez_ev {
        assert!(v.message.contains("silk extends off board/cutout edge"));
        assert!(!v.message.contains("0 nm < 0 nm"));
    }
}

#[test]
fn silk_capsule_mask_dedup_hotspot_isolation_poly_and_upright() {
    use boardc::ir::{AutoRefdes, Graphic};

    // 1. Long F.Mask Capsule crossing + multi-stroke F.Mask graphic deduplication + Graphic::Poly.
    let mut board = base_board();
    board.rules.silk_to_mask_nm = Some(um(100.0));
    board.fab.auto_refdes = AutoRefdes::Off;
    board.graphics = vec![
        // mask#0: long horizontal F.Mask capsule centered at x = 15 mm.
        Graphic::Line {
            layer: "F.Mask".into(),
            a: Pt::new(mm(2.0), mm(10.0)),
            b: Pt::new(mm(28.0), mm(10.0)),
            width: um(200.0),
        },
        // silk#1: vertical silk line crossing mask#0 at x = 24 mm.
        Graphic::Line {
            layer: "F.SilkS".into(),
            a: Pt::new(mm(24.0), mm(9.0)),
            b: Pt::new(mm(24.0), mm(11.0)),
            width: um(100.0),
        },
        // mask#2: 4-stroke unfilled F.Mask rect.
        Graphic::Rect {
            layer: "F.Mask".into(),
            min: Pt::new(mm(10.0), mm(20.0)),
            max: Pt::new(mm(12.0), mm(22.0)),
            width: um(100.0),
            fill: false,
        },
        // silk#3: filled F.SilkS polygon overlapping multiple strokes of mask#2.
        Graphic::Poly {
            layer: "F.SilkS".into(),
            verts: rect_pts(10.5, 19.5, 11.5, 22.5),
            width: 0,
            fill: true,
        },
        // silk#4: unfilled F.SilkS triangle touching mask#2 at (12.0, 20.0) corner via an edge.
        Graphic::Poly {
            layer: "F.SilkS".into(),
            verts: vec![
                Pt::new(mm(12.0), mm(20.0)),
                Pt::new(mm(14.0), mm(20.0)),
                Pt::new(mm(13.0), mm(21.0)),
            ],
            width: um(100.0),
            fill: false,
        },
    ];
    let rep = check_board_drc(&board);
    let sv = silk_viols(&rep, ViolationKind::SilkOverCopper);
    let mut pairs: Vec<(&str, &str)> = sv
        .iter()
        .map(|v| (v.item_a.as_str(), v.item_b.as_deref().unwrap_or("")))
        .collect();
    pairs.sort();
    assert_eq!(
        pairs,
        vec![
            ("silk#1", "mask#0"),
            ("silk#3", "mask#2"),
            ("silk#4", "mask#2"),
        ],
        "got {sv:?}"
    );
    let cap_v = sv.iter().find(|v| v.item_a == "silk#1").unwrap();
    assert!(
        (cap_v.pos_nm.x - mm(24.0)).abs() <= um(50.0)
            && (cap_v.pos_nm.y - mm(10.0)).abs() <= um(50.0),
        "Capsule crossing pos_nm must be near (24mm, 10mm), got {:?}",
        cap_v.pos_nm
    );

    // 2. Board-wide silk#0 crossing two distant pads stays in 2 separate hotspots.
    let mut wide_board = base_board();
    wide_board.rules.silk_to_mask_nm = Some(um(100.0));
    wide_board.fab.auto_refdes = AutoRefdes::Off;
    wide_board.footprints.insert(
        "PAD".into(),
        custom_fp(
            "PAD",
            vec![smd_pad(
                "1",
                PadShape::Circle { diameter: mm(1.0) },
                Pt::new(0, 0),
                &["F.Cu", "F.Mask"],
            )],
            vec![],
            vec![],
        ),
    );
    wide_board.placements = vec![
        place_pin1("U1", "PAD", 4.0, 20.0, Side::Top, "A"),
        place_pin1("U2", "PAD", 36.0, 20.0, Side::Top, "B"),
    ];
    wide_board.graphics.push(Graphic::Line {
        layer: "F.SilkS".into(),
        a: Pt::new(mm(2.0), mm(20.0)),
        b: Pt::new(mm(38.0), mm(20.0)),
        width: um(150.0),
    });
    let wide_rep = check_board_drc(&wide_board);
    assert_eq!(
        silk_viols(&wide_rep, ViolationKind::SilkOverCopper).len(),
        2
    );
    assert_eq!(
        wide_rep.hotspots.len(),
        2,
        "board-wide silk#0 must not merge distant hotspots: {:?}",
        wide_rep.hotspots
    );

    // 3. AutoRefdes::Upright with 180-deg rotated footprint flips glyph upright.
    let mut up_board = base_board();
    up_board.rules.mask_expansion_nm = 0;
    up_board.rules.silk_to_mask_nm = Some(um(50.0));
    up_board.fab.auto_refdes = AutoRefdes::Upright;
    up_board.footprints.insert(
        "ROT_FP".into(),
        custom_fp(
            "ROT_FP",
            vec![smd_pad(
                "1",
                PadShape::Circle { diameter: mm(0.2) },
                Pt::new(0, 0),
                &["F.Cu", "F.Mask"],
            )],
            rect_pts(-0.5, -0.5, 0.5, 0.5),
            vec![],
        ),
    );
    up_board.footprints.insert(
        "PROBE_PAD".into(),
        custom_fp(
            "PROBE_PAD",
            vec![smd_pad(
                "1",
                PadShape::Circle { diameter: mm(0.1) },
                Pt::new(0, 0),
                &["F.Cu", "F.Mask"],
            )],
            vec![],
            vec![],
        ),
    );
    let mut rot_pl = place_pin1("L", "ROT_FP", 10.0, 10.0, Side::Top, "A");
    rot_pl.rot_deg = 180.0;
    // Refdes center at (10.0, 9.05); upright 'L' bottom bar sits at y = 8.70, rotated-180 'L' sits at y = 9.40.
    up_board.placements = vec![
        rot_pl,
        place_pin1("P1", "PROBE_PAD", 10.0, 8.70, Side::Top, "B"),
    ];
    let up_rep = check_board_drc(&up_board);
    assert!(
        silk_viols(&up_rep, ViolationKind::SilkOverCopper)
            .iter()
            .any(|v| v.item_a == "L.refdes" && v.item_b.as_deref() == Some("P1.1")),
        "expected Upright 'L.refdes' bottom stroke to hit P1.1: {:?}",
        up_rep.violations
    );
    up_board.fab.auto_refdes = AutoRefdes::Rotate;
    let rot_rep = check_board_drc(&up_board);
    assert!(
        silk_viols(&rot_rep, ViolationKind::SilkOverCopper).is_empty(),
        "rotated-180 'L.refdes' must clear P1.1: {:?}",
        rot_rep.violations
    );
}
