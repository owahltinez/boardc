use std::collections::BTreeMap;
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
        pin_types: Default::default(),
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
            pins: Vec::new(),
            ignore_route: false,
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
            pin_types: Default::default(),
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
            pin_types: Default::default(),
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
            pin_types: Default::default(),
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
            pin_types: Default::default(),
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
            pin_types: Default::default(),
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
            pin_types: Default::default(),
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
            pin_types: Default::default(),
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
            pin_types: Default::default(),
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
