use std::collections::BTreeMap;
use std::process::Command;

use boardc::drc::{
    DrcReport, Hotspot, Severity, Violation, ViolationKind, check_board_drc, cluster_hotspots,
    cluster_kicad_drc_json,
};
use boardc::ir::{
    BgaShadowRule, BoardIr, BoardSpec, CapProximityRule, Constraints, DesignRules, FilledPour,
    FootprintSpec, Keepout, KeepoutKind, LayerDef, LayerKind, MatchGroupRule, MaxLengthRule,
    MinViasRule, NetClass, NetDef, PadDef, PadKind, PadShape, Placement, PourPoly, PourSpec, Pt,
    Side, Track, Via, mm, um,
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
        },
    );
    footprints.insert(
        "BGA".into(),
        FootprintSpec::Bga {
            rows: 4,
            cols: 4,
            pitch_nm: mm(0.8),
            ball_dia_nm: mm(0.4),
        },
    );
    footprints.insert(
        "IC".into(),
        FootprintSpec::Qfn {
            pins_per_side: 2,
            pitch_nm: mm(0.5),
            body_nm: mm(2.0),
            ep_nm: 0,
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
