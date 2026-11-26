use std::collections::BTreeMap;
use std::process::Command;

use boardc::geom::{point_in_polygon, point_to_polygon_edge_dist};
use boardc::ir::{
    BoardIr, BoardSpec, DesignRules, FootprintSpec, Keepout, KeepoutKind, LayerDef, LayerKind,
    PadDef, PadKind, PadShape, Placement, PourPoly, PourSpec, Pt, Side, Track, Via,
    load_board_json, mm,
};
use boardc::zones::{BlockerKind, ChokeKind, ZoneReport, analyze_zone_chokes, fill_zones};

fn point_in_pour_poly(p: Pt, poly: &PourPoly) -> bool {
    point_in_polygon(p, &poly.outer) && !poly.holes.iter().any(|h| point_in_polygon(p, h))
}

fn point_in_filled_pour(p: Pt, polys: &[PourPoly]) -> bool {
    polys.iter().any(|poly| point_in_pour_poly(p, poly))
}

fn rect_pts(x0: f64, y0: f64, x1: f64, y1: f64) -> Vec<Pt> {
    vec![
        Pt::new(mm(x0), mm(y0)),
        Pt::new(mm(x1), mm(y0)),
        Pt::new(mm(x1), mm(y1)),
        Pt::new(mm(x0), mm(y1)),
    ]
}

fn single_pad_fp(size_mm: f64, kind: PadKind, layers: Vec<&str>) -> FootprintSpec {
    FootprintSpec::Custom {
        pads: vec![PadDef {
            name: "1".into(),
            kind,
            shape: PadShape::Rect {
                width: mm(size_mm),
                height: mm(size_mm),
            },
            pos: Pt::new(0, 0),
            rot_deg: 0.0,
            drill_nm: if kind == PadKind::Tht {
                Some(mm(0.4))
            } else {
                None
            },
            layers: layers.into_iter().map(String::from).collect(),
            mask_margin_nm: None,
            paste_margin_nm: None,
        }],
        courtyard: Vec::new(),
        graphics: Vec::new(),
        keepouts: Vec::new(),
    }
}

fn pad_place(refdes: &str, fp: &str, x_mm: f64, y_mm: f64, net: &str) -> Placement {
    let mut pin_nets = BTreeMap::new();
    pin_nets.insert("1".into(), net.into());
    Placement {
        refdes: refdes.into(),
        footprint: fp.into(),
        value: String::new(),
        pos: Pt::new(mm(x_mm), mm(y_mm)),
        rot_deg: 0.0,
        side: Side::Top,
        locked: false,
        pin_nets,
        mpn: None,
        lcsc: None,
        dnp: false,
        cpl_rotation_offset_deg: 0.0,
    }
}

#[test]
fn priority_pour_overlap_and_edge_cutout_keepout_clearance() {
    let mut footprints = BTreeMap::new();
    footprints.insert("PAD".into(), single_pad_fp(0.8, PadKind::Smd, vec!["F.Cu"]));

    let spec = BoardSpec {
        name: "priority_test".into(),
        outline: Vec::new(),
        size_nm: Some([mm(20.0), mm(20.0)]),
        cutouts: vec![rect_pts(1.5, 1.5, 3.0, 3.0)],
        layers: vec![LayerDef {
            name: "F.Cu".into(),
            kind: LayerKind::Signal,
            side: Some(Side::Top),
            thickness_nm: None,
        }],
        footprints,
        placements: vec![
            pad_place("P_GND", "PAD", 2.0, 18.0, "GND"),
            pad_place("P_PWR", "PAD", 10.0, 10.0, "PWR"),
        ],
        keepouts: vec![Keepout {
            kind: KeepoutKind::Copper,
            layers: vec!["F.Cu".into()],
            poly: rect_pts(12.0, 12.0, 14.0, 14.0),
        }],
        pours: vec![
            PourSpec {
                name: "GND_LOW".into(),
                net: "GND".into(),
                layer: "F.Cu".into(),
                outline: rect_pts(0.0, 0.0, 20.0, 20.0),
                priority: 0,
                clearance_nm: Some(mm(0.2)),
                min_width_nm: mm(0.2),
                thermal_relief: false,
                thermal_gap_nm: mm(0.25),
                thermal_spoke_nm: mm(0.3),
                remove_islands: true,
            },
            PourSpec {
                name: "PWR_HIGH".into(),
                net: "PWR".into(),
                layer: "F.Cu".into(),
                outline: rect_pts(6.0, 6.0, 16.0, 16.0),
                priority: 5,
                clearance_nm: Some(mm(0.25)),
                min_width_nm: mm(0.2),
                thermal_relief: false,
                thermal_gap_nm: mm(0.25),
                thermal_spoke_nm: mm(0.3),
                remove_islands: true,
            },
        ],
        tracks: Vec::new(),
        vias: Vec::new(),
        net_classes: Default::default(),
        nets: Default::default(),
        rules: DesignRules {
            edge_clearance_nm: mm(0.3),
            clearance_nm: mm(0.15),
            ..Default::default()
        },
        constraints: Default::default(),
        graphics: Vec::new(),
    };

    let mut ir = BoardIr::from_spec(&spec).unwrap();
    let report = fill_zones(&mut ir);
    assert_eq!(ir.filled_pours.len(), 2);

    // Higher-priority PWR pour comes first and claims (6..16, 6..16) minus keepout (12..14).
    let pwr = &ir.filled_pours[0];
    let gnd = &ir.filled_pours[1];
    assert_eq!(pwr.net, "PWR");
    assert_eq!(gnd.net, "GND");
    assert_eq!(pwr.polys.len(), 1);
    assert_eq!(gnd.polys.len(), 1);

    // PWR pad point is inside PWR pour, not GND pour; keepout center is excluded from both.
    assert!(point_in_filled_pour(Pt::new(mm(8.0), mm(8.0)), &pwr.polys));
    assert!(!point_in_filled_pour(Pt::new(mm(8.0), mm(8.0)), &gnd.polys));
    assert!(!point_in_filled_pour(
        Pt::new(mm(13.0), mm(13.0)),
        &pwr.polys
    ));
    assert!(!point_in_filled_pour(
        Pt::new(mm(13.0), mm(13.0)),
        &gnd.polys
    ));

    // Edge clearance >= 0.3 mm and cutout clearance >= 0.3 mm on GND pour.
    for pt in &gnd.polys[0].outer {
        assert!(pt.x >= mm(0.3) - 1 && pt.x <= mm(19.7) + 1);
        assert!(pt.y >= mm(0.3) - 1 && pt.y <= mm(19.7) + 1);
    }
    assert!(!point_in_filled_pour(Pt::new(mm(3.2), mm(2.2)), &gnd.polys));
    assert!(point_in_filled_pour(Pt::new(mm(3.4), mm(2.2)), &gnd.polys));

    // Clearance gap between PWR_HIGH and GND_LOW is >= 0.25 mm.
    assert!(!point_in_filled_pour(
        Pt::new(mm(5.76), mm(10.0)),
        &gnd.polys
    ));
    assert!(point_in_filled_pour(
        Pt::new(mm(5.74), mm(10.0)),
        &gnd.polys
    ));
    let pwr_hole = gnd.polys[0]
        .holes
        .iter()
        .find(|h| point_in_polygon(Pt::new(mm(8.0), mm(8.0)), h))
        .expect("expected hole in GND around PWR_HIGH");
    let gap = pwr.polys[0]
        .outer
        .iter()
        .map(|&p| point_to_polygon_edge_dist(p, pwr_hole))
        .fold(f64::INFINITY, f64::min);
    assert!(gap >= mm(0.249) as f64, "expected >= 0.25mm gap, got {gap}");
    assert_eq!(report.total_islands, 2);
}

#[test]
fn antipad_subtraction_around_other_net_via_outer_and_inner_layers() {
    let mut footprints = BTreeMap::new();
    footprints.insert("THT".into(), single_pad_fp(1.0, PadKind::Tht, vec!["*.Cu"]));

    let spec = BoardSpec {
        name: "antipad_test".into(),
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
                kind: LayerKind::Plane,
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
        footprints,
        placements: vec![pad_place("J1", "THT", 3.0, 3.0, "GND")],
        keepouts: Vec::new(),
        pours: vec![
            PourSpec {
                name: "GND_TOP".into(),
                net: "GND".into(),
                layer: "F.Cu".into(),
                outline: rect_pts(0.0, 0.0, 20.0, 20.0),
                priority: 0,
                clearance_nm: Some(mm(0.15)),
                min_width_nm: mm(0.2),
                thermal_relief: false,
                thermal_gap_nm: mm(0.25),
                thermal_spoke_nm: mm(0.3),
                remove_islands: true,
            },
            PourSpec {
                name: "GND_IN1".into(),
                net: "GND".into(),
                layer: "In1.Cu".into(),
                outline: rect_pts(0.0, 0.0, 20.0, 20.0),
                priority: 0,
                clearance_nm: Some(mm(0.15)),
                min_width_nm: mm(0.2),
                thermal_relief: false,
                thermal_gap_nm: mm(0.25),
                thermal_spoke_nm: mm(0.3),
                remove_islands: true,
            },
        ],
        tracks: Vec::new(),
        vias: vec![
            // Pad-dominated on inner layer: dia/2 + clr = 0.30 + 0.15 = 0.45 mm > drill/2 + hole_clr (0.35 mm).
            Via {
                net: "SIG".into(),
                pos: Pt::new(mm(10.0), mm(10.0)),
                diameter: mm(0.6),
                drill: mm(0.2),
                layers: ("F.Cu".into(), "B.Cu".into()),
                locked: false,
                tented: true,
            },
            // Hole-dominated on inner layer: drill/2 + hole_clr = 0.10 + 0.25 = 0.35 mm > dia/2 + clr (0.30 mm).
            Via {
                net: "SIG2".into(),
                pos: Pt::new(mm(15.0), mm(10.0)),
                diameter: mm(0.3),
                drill: mm(0.2),
                layers: ("F.Cu".into(), "B.Cu".into()),
                locked: false,
                tented: true,
            },
        ],
        net_classes: Default::default(),
        nets: Default::default(),
        rules: DesignRules {
            clearance_nm: mm(0.15),
            hole_to_hole_nm: mm(0.25),
            hole_clearance_nm: Some(mm(0.25)),
            ..Default::default()
        },
        constraints: Default::default(),
        graphics: Vec::new(),
    };

    let mut ir = BoardIr::from_spec(&spec).unwrap();
    fill_zones(&mut ir);

    // Outer layer F.Cu: via copper radius (0.30) + clearance (0.15) = 0.45 mm.
    let top_polys = &ir.filled_pours[0].polys;
    assert!(!point_in_filled_pour(
        Pt::new(mm(10.43), mm(10.0)),
        top_polys
    ));
    assert!(point_in_filled_pour(
        Pt::new(mm(10.47), mm(10.0)),
        top_polys
    ));

    // Inner layer In1.Cu: full via pad is flashed, so antipad is max(dia/2+clr, drill/2+hole_clr).
    let in1_polys = &ir.filled_pours[1].polys;
    // Pad-dominated via at x=10.0: max(0.30+0.15=0.45, 0.10+0.25=0.35) = 0.45 mm.
    assert!(!point_in_filled_pour(
        Pt::new(mm(10.43), mm(10.0)),
        in1_polys
    ));
    assert!(point_in_filled_pour(
        Pt::new(mm(10.47), mm(10.0)),
        in1_polys
    ));
    // Hole-dominated via at x=15.0: max(0.15+0.15=0.30, 0.10+0.25=0.35) = 0.35 mm.
    assert!(!point_in_filled_pour(
        Pt::new(mm(15.33), mm(10.0)),
        in1_polys
    ));
    assert!(point_in_filled_pour(
        Pt::new(mm(15.37), mm(10.0)),
        in1_polys
    ));
}

#[test]
fn dead_island_removal_prunes_unconnected_components() {
    let mut footprints = BTreeMap::new();
    footprints.insert("PAD".into(), single_pad_fp(1.0, PadKind::Smd, vec!["F.Cu"]));

    let mut spec = BoardSpec {
        name: "island_test".into(),
        outline: Vec::new(),
        size_nm: Some([mm(20.0), mm(10.0)]),
        cutouts: Vec::new(),
        layers: vec![LayerDef {
            name: "F.Cu".into(),
            kind: LayerKind::Signal,
            side: Some(Side::Top),
            thickness_nm: None,
        }],
        footprints,
        placements: vec![pad_place("P1", "PAD", 4.0, 5.0, "GND")],
        keepouts: Vec::new(),
        pours: vec![PourSpec {
            name: "GND_TOP".into(),
            net: "GND".into(),
            layer: "F.Cu".into(),
            outline: rect_pts(0.0, 0.0, 20.0, 10.0),
            priority: 0,
            clearance_nm: Some(mm(0.2)),
            min_width_nm: mm(0.2),
            thermal_relief: false,
            thermal_gap_nm: mm(0.25),
            thermal_spoke_nm: mm(0.3),
            remove_islands: true,
        }],
        tracks: vec![Track {
            net: "SIG".into(),
            layer: "F.Cu".into(),
            a: Pt::new(mm(10.0), mm(0.0)),
            b: Pt::new(mm(10.0), mm(10.0)),
            width: mm(0.4),
            locked: false,
        }],
        vias: Vec::new(),
        net_classes: Default::default(),
        nets: Default::default(),
        rules: DesignRules::default(),
        constraints: Default::default(),
        graphics: Vec::new(),
    };

    let mut ir = BoardIr::from_spec(&spec).unwrap();
    let report = fill_zones(&mut ir);
    assert_eq!(ir.filled_pours[0].polys.len(), 1);
    assert_eq!(report.total_removed_dead_islands, 1);
    assert!(point_in_filled_pour(
        Pt::new(mm(4.0), mm(5.0)),
        &ir.filled_pours[0].polys
    ));
    assert!(!point_in_filled_pour(
        Pt::new(mm(15.0), mm(5.0)),
        &ir.filled_pours[0].polys
    ));

    spec.pours[0].remove_islands = false;
    let mut ir_keep = BoardIr::from_spec(&spec).unwrap();
    let report_keep = fill_zones(&mut ir_keep);
    assert_eq!(ir_keep.filled_pours[0].polys.len(), 2);
    assert_eq!(report_keep.total_removed_dead_islands, 0);
}

#[test]
fn thermal_relief_connection_has_four_spokes_and_moat() {
    let mut footprints = BTreeMap::new();
    footprints.insert("PAD".into(), single_pad_fp(1.0, PadKind::Smd, vec!["F.Cu"]));

    let spec = BoardSpec {
        name: "thermal_test".into(),
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
        placements: vec![pad_place("P1", "PAD", 10.0, 10.0, "GND")],
        keepouts: Vec::new(),
        pours: vec![PourSpec {
            name: "GND_THERM".into(),
            net: "GND".into(),
            layer: "F.Cu".into(),
            outline: rect_pts(0.0, 0.0, 20.0, 20.0),
            priority: 0,
            clearance_nm: Some(mm(0.2)),
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
        rules: DesignRules::default(),
        constraints: Default::default(),
        graphics: Vec::new(),
    };

    let mut ir = BoardIr::from_spec(&spec).unwrap();
    let report = fill_zones(&mut ir);
    assert_eq!(ir.filled_pours[0].polys.len(), 1);
    let polys = &ir.filled_pours[0].polys;

    // Pad center and all 4 orthogonal spokes across the 0.25 mm moat are connected.
    for pt in [
        Pt::new(mm(10.0), mm(10.0)),
        Pt::new(mm(10.65), mm(10.0)),
        Pt::new(mm(9.35), mm(10.0)),
        Pt::new(mm(10.0), mm(10.65)),
        Pt::new(mm(10.0), mm(9.35)),
    ] {
        assert!(
            point_in_filled_pour(pt, polys),
            "expected spoke copper at {pt:?}"
        );
    }

    // Corners inside the thermal gap moat (outside pad 0.5mm half-width, within 0.75mm) are cleared.
    for pt in [
        Pt::new(mm(10.6), mm(10.6)),
        Pt::new(mm(9.4), mm(10.6)),
        Pt::new(mm(10.6), mm(9.4)),
        Pt::new(mm(9.4), mm(9.4)),
    ] {
        assert!(
            !point_in_filled_pour(pt, polys),
            "expected moat clearance at {pt:?}"
        );
    }

    assert!(
        report.chokes.is_empty(),
        "thermal spokes must not cause false-positive chokes: {:?}",
        report.chokes
    );
}

#[test]
fn via_wall_chokes_power_pour_into_split_and_narrow_neck_naming_blockers() {
    let mut footprints = BTreeMap::new();
    footprints.insert("THT".into(), single_pad_fp(0.8, PadKind::Tht, vec!["*.Cu"]));

    // 1. Full via-wall splits VCC inner plane into two disconnected islands.
    let mut wall_vias = Vec::new();
    for i in 0..=15 {
        let y = 0.5 + i as f64 * 0.6;
        wall_vias.push(Via {
            net: "SIG_WALL".into(),
            pos: Pt::new(mm(15.0), mm(y)),
            diameter: mm(0.45),
            drill: mm(0.2),
            layers: ("F.Cu".into(), "B.Cu".into()),
            locked: false,
            tented: true,
        });
    }
    let mut split_spec = BoardSpec {
        name: "via_wall_split".into(),
        outline: Vec::new(),
        size_nm: Some([mm(30.0), mm(10.0)]),
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
                kind: LayerKind::Plane,
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
            pad_place("U_LEFT", "THT", 4.0, 5.0, "VCC"),
            pad_place("U_RIGHT", "THT", 26.0, 5.0, "VCC"),
        ],
        keepouts: Vec::new(),
        pours: vec![PourSpec {
            name: "VCC_PLANE".into(),
            net: "VCC".into(),
            layer: "In1.Cu".into(),
            outline: rect_pts(0.0, 0.0, 30.0, 10.0),
            priority: 0,
            clearance_nm: Some(mm(0.2)),
            min_width_nm: mm(0.15),
            thermal_relief: false,
            thermal_gap_nm: mm(0.25),
            thermal_spoke_nm: mm(0.3),
            remove_islands: true,
        }],
        tracks: Vec::new(),
        vias: wall_vias,
        net_classes: Default::default(),
        nets: Default::default(),
        rules: DesignRules {
            clearance_nm: mm(0.2),
            hole_to_hole_nm: mm(0.25),
            hole_clearance_nm: Some(mm(0.25)),
            ..Default::default()
        },
        constraints: Default::default(),
        graphics: Vec::new(),
    };

    let mut ir_split = BoardIr::from_spec(&split_spec).unwrap();
    let rep_split = fill_zones(&mut ir_split);
    assert_eq!(ir_split.filled_pours[0].polys.len(), 2);
    let split_chokes: Vec<_> = rep_split
        .chokes
        .iter()
        .filter(|c| c.kind == ChokeKind::SplitPour)
        .collect();
    assert_eq!(split_chokes.len(), 1);
    assert_eq!(split_chokes[0].island_count, 2);
    assert_eq!(split_chokes[0].separated_groups.len(), 2);
    assert!(
        split_chokes[0]
            .blockers
            .iter()
            .any(|b| b.kind == BlockerKind::Via && b.net == "SIG_WALL")
    );

    // 2. Partial via-wall leaves a single ~0.24 mm copper neck at y = 5.0 mm.
    // On In1.Cu: antipad r = max(dia/2 + clr = 0.425, drill/2 + hole_clr = 0.35) = 0.425 mm.
    // Vias at y = 4.455 and y = 5.545 leave 1.09 - 0.85 = 0.24 mm neck (< 0.40 mm threshold).
    let mut neck_vias = Vec::new();
    for &y in &[
        0.5, 1.15, 1.8, 2.45, 3.1, 3.75, 4.455, 5.545, 6.25, 6.9, 7.55, 8.2, 8.85, 9.5,
    ] {
        neck_vias.push(Via {
            net: "SIG_PINCH".into(),
            pos: Pt::new(mm(15.0), mm(y)),
            diameter: mm(0.45),
            drill: mm(0.2),
            layers: ("F.Cu".into(), "B.Cu".into()),
            locked: false,
            tented: true,
        });
    }
    split_spec.vias = neck_vias;
    split_spec.pours[0].min_width_nm = mm(0.18);
    split_spec.pours[0].thermal_relief = true;

    let mut ir_neck = BoardIr::from_spec(&split_spec).unwrap();
    fill_zones(&mut ir_neck);
    assert_eq!(ir_neck.filled_pours[0].polys.len(), 1);
    let chokes = analyze_zone_chokes(&ir_neck);
    let neck_chokes: Vec<_> = chokes
        .iter()
        .filter(|c| c.kind == ChokeKind::NarrowNeck)
        .collect();
    assert_eq!(
        neck_chokes.len(),
        1,
        "expected 1 narrow neck choke, got {chokes:?}"
    );
    let nc = neck_chokes[0];
    assert!(
        nc.min_neck_width_nm >= mm(0.20) && nc.min_neck_width_nm <= mm(0.28),
        "expected ~0.24mm neck width, got {} nm",
        nc.min_neck_width_nm
    );
    assert_eq!(nc.separated_groups.len(), 2);
    let blocker_ys: Vec<i64> = nc
        .blockers
        .iter()
        .filter(|b| b.kind == BlockerKind::Via && b.net == "SIG_PINCH")
        .map(|b| b.pos.y)
        .collect();
    assert!(
        blocker_ys.contains(&mm(4.455)) && blocker_ys.contains(&mm(5.545)),
        "expected pinching vias at y=4.455 and y=5.545 in {:?}",
        nc.blockers
    );
}

#[test]
fn cli_zones_writes_filled_board_and_report_json() {
    let tmp_dir = std::env::temp_dir().join("boardc_test_zones_cli");
    std::fs::create_dir_all(&tmp_dir).unwrap();
    let spec_path = tmp_dir.join("board.toml");
    let out_path = tmp_dir.join("filled.json");
    let report_path = tmp_dir.join("report.json");

    let mut footprints = BTreeMap::new();
    footprints.insert(
        "C_0402".into(),
        FootprintSpec::Chip2Pin {
            package: "0402".into(),
        },
    );
    let mut c1_nets = BTreeMap::new();
    c1_nets.insert("1".into(), "VCC".into());
    c1_nets.insert("2".into(), "GND".into());

    let spec = BoardSpec {
        name: "cli_zones".into(),
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
        placements: vec![Placement {
            refdes: "C1".into(),
            footprint: "C_0402".into(),
            value: "100n".into(),
            pos: Pt::new(mm(10.0), mm(10.0)),
            rot_deg: 0.0,
            side: Side::Top,
            locked: false,
            pin_nets: c1_nets,
            mpn: None,
            lcsc: None,
            dnp: false,
            cpl_rotation_offset_deg: 0.0,
        }],
        keepouts: Vec::new(),
        pours: vec![PourSpec {
            name: "GND_F".into(),
            net: "GND".into(),
            layer: "F.Cu".into(),
            outline: rect_pts(0.0, 0.0, 20.0, 20.0),
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
        rules: DesignRules::default(),
        constraints: Default::default(),
        graphics: Vec::new(),
    };
    std::fs::write(&spec_path, toml::to_string_pretty(&spec).unwrap()).unwrap();

    let status = Command::new(env!("CARGO_BIN_EXE_boardc"))
        .args([
            "zones",
            spec_path.to_str().unwrap(),
            "-o",
            out_path.to_str().unwrap(),
            "--report",
            report_path.to_str().unwrap(),
        ])
        .status()
        .unwrap();
    assert!(status.success());

    let ir = load_board_json(&out_path).unwrap();
    assert_eq!(ir.filled_pours.len(), 1);
    assert_eq!(ir.filled_pours[0].polys.len(), 1);

    let rep: ZoneReport =
        serde_json::from_str(&std::fs::read_to_string(&report_path).unwrap()).unwrap();
    assert_eq!(rep.total_islands, 1);
    assert_eq!(rep.pours[0].connected_terminals, 1);

    let _ = std::fs::remove_dir_all(&tmp_dir);
}
