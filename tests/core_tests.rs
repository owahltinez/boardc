use std::collections::BTreeMap;

use clipper2_rust::{FillRule, intersect_64};

use boardc::footprints::{bga, chip_2pin, pin_header, qfn, sot23};
use boardc::geom::{
    ResolvedShape, courtyard_world_polygon, local_to_board, oval_poly, pad_to_world_shape,
    pad_world_layers, pad_world_polygon, path64_to_pts, paths64_to_polys, point_in_polygon,
    point_to_polygon_dist, point_to_segment_dist, polygon_area, polygon_to_polygon_dist,
    polys_to_paths64, pts_to_path64, rect_poly, remap_layer_for_side, roundrect_poly,
    segment_to_polygon_dist, segment_to_segment_dist, shape_to_path64,
};
use boardc::ir::{
    BgaShadowRule, BoardIr, BoardSpec, CapProximityRule, Constraints, DesignRules, FootprintSpec,
    LayerDef, LayerKind, MatchGroupRule, MaxLengthRule, MinViasRule, NetClass, NetDef, PadDef,
    PadKind, PadShape, Placement, PourSpec, Pt, Side, load_board_json, load_spec_file, mm,
    save_board_json, um,
};

#[test]
fn top_and_bottom_placement_transforms_0_90_180_270() {
    let cx = mm(10.0);
    let cy = mm(20.0);
    let local = Pt::new(mm(2.0), mm(3.0));

    // Top: (lx, ly) rotated CCW around (0,0) then translated to (cx, cy).
    assert_eq!(
        local_to_board(local, cx, cy, 0.0, Side::Top),
        Pt::new(cx + mm(2.0), cy + mm(3.0))
    );
    assert_eq!(
        local_to_board(local, cx, cy, 90.0, Side::Top),
        Pt::new(cx - mm(3.0), cy + mm(2.0))
    );
    assert_eq!(
        local_to_board(local, cx, cy, 180.0, Side::Top),
        Pt::new(cx - mm(2.0), cy - mm(3.0))
    );
    assert_eq!(
        local_to_board(local, cx, cy, 270.0, Side::Top),
        Pt::new(cx + mm(3.0), cy - mm(2.0))
    );

    // Bottom: flips local Y -> (lx, -ly) = (2, -3), then rotates CCW.
    assert_eq!(
        local_to_board(local, cx, cy, 0.0, Side::Bottom),
        Pt::new(cx + mm(2.0), cy - mm(3.0))
    );
    assert_eq!(
        local_to_board(local, cx, cy, 90.0, Side::Bottom),
        Pt::new(cx + mm(3.0), cy + mm(2.0))
    );
    assert_eq!(
        local_to_board(local, cx, cy, 180.0, Side::Bottom),
        Pt::new(cx - mm(2.0), cy + mm(3.0))
    );
    assert_eq!(
        local_to_board(local, cx, cy, 270.0, Side::Bottom),
        Pt::new(cx - mm(3.0), cy - mm(2.0))
    );

    // Layer remapping for Top vs Bottom.
    let pad = PadDef {
        name: "1".into(),
        kind: PadKind::Smd,
        shape: PadShape::Rect {
            width: mm(1.0),
            height: mm(0.5),
        },
        pos: local,
        rot_deg: 0.0,
        drill_nm: None,
        layers: vec!["F.Cu".into(), "F.Mask".into(), "F.Paste".into()],
        mask_margin_nm: None,
        paste_margin_nm: None,
    };
    let copper = vec!["F.Cu".into(), "B.Cu".into()];
    assert_eq!(
        pad_world_layers(&pad, Side::Top, &copper),
        vec!["F.Cu", "F.Mask", "F.Paste"]
    );
    assert_eq!(
        pad_world_layers(&pad, Side::Bottom, &copper),
        vec!["B.Cu", "B.Mask", "B.Paste"]
    );
    assert_eq!(remap_layer_for_side("F.Silk", Side::Bottom), "B.Silk");
    assert_eq!(remap_layer_for_side("B.Silk", Side::Bottom), "F.Silk");
    assert_eq!(remap_layer_for_side("F.CrtYd", Side::Bottom), "B.CrtYd");
    assert_eq!(remap_layer_for_side("B.CrtYd", Side::Bottom), "F.CrtYd");
    assert_eq!(remap_layer_for_side("In1.Cu", Side::Bottom), "In1.Cu");
}

#[test]
fn pad_world_shape_and_courtyard_transform() {
    let fp = chip_2pin("0603").unwrap();
    let pl = Placement {
        refdes: "R1".into(),
        footprint: "C_0603".into(),
        value: "10k".into(),
        pos: Pt::new(mm(5.0), mm(8.0)),
        rot_deg: 90.0,
        side: Side::Bottom,
        locked: false,
        pin_nets: BTreeMap::new(),
        pin_types: BTreeMap::new(),
        mpn: None,
        lcsc: None,
        dnp: false,
        exclude_from_bom: false,
        exclude_from_pos: false,
        cpl_rotation_offset_deg: 0.0,
        schematic: None,
    };
    let p1 = pad_to_world_shape(&fp.pads[0], &pl, 8);
    let b = p1.bbox();
    // Pin 1 at (-0.75, 0) rotated 90 deg on Bottom -> (5.0, 8.0 - 0.75) = (5.0, 7.25).
    assert_eq!((b.min_x + b.max_x) / 2, mm(5.0));
    assert_eq!((b.min_y + b.max_y) / 2, mm(7.25));
    let poly = pad_world_polygon(&fp.pads[0], &pl, 8);
    assert!(poly.len() >= 8);
    let crtyd = courtyard_world_polygon(&fp, &pl);
    assert_eq!(crtyd.len(), 4);
    assert!(polygon_area(&crtyd) > 0.0);
}

#[test]
fn parametric_ipc7351_footprints() {
    for pkg in ["0201", "0402", "0603", "0805"] {
        let fp = chip_2pin(pkg).unwrap();
        assert_eq!(fp.pads.len(), 2);
        assert_eq!(fp.courtyard.len(), 4);
    }
    assert!(chip_2pin("1206").is_err());

    let fp_bga = bga(4, 4, mm(0.8), mm(0.4)).unwrap();
    assert_eq!(fp_bga.pads.len(), 16);
    assert_eq!(fp_bga.pads[0].name, "A1");
    assert_eq!(fp_bga.pads[15].name, "D4");
    assert!(fp_bga.pads[0].pos.x < 0 && fp_bga.pads[0].pos.y > 0);

    let fp_qfn = qfn(4, mm(0.5), mm(3.0), mm(1.5)).unwrap();
    assert_eq!(fp_qfn.pads.len(), 17);
    assert_eq!(fp_qfn.pads[16].name, "EP");

    for pins in [3, 5, 6] {
        assert_eq!(sot23(pins).unwrap().pads.len(), pins);
    }
    assert!(sot23(4).is_err());

    let hdr = pin_header(4, mm(2.54)).unwrap();
    assert_eq!(hdr.pads.len(), 4);
    assert_eq!(hdr.pads[0].kind, PadKind::Tht);
    assert!(hdr.pads[0].drill_nm.is_some());

    // Invalid or huge parametric dimensions must be rejected cleanly.
    assert!(bga(1000, 1000, mm(0.8), mm(0.4)).is_err());
    assert!(bga(2, 2, 0, mm(0.4)).is_err());
    assert!(bga(2, 2, mm(0.8), 0).is_err());
    assert!(qfn(4, 0, mm(3.0), mm(1.5)).is_err());
    assert!(qfn(20_000, mm(0.5), mm(3.0), mm(1.5)).is_err());
    assert!(pin_header(100_000, mm(2.54)).is_err());
}

#[test]
fn json_and_toml_spec_roundtrip_and_board_ir_io() {
    let mut footprints = BTreeMap::new();
    footprints.insert(
        "R_0402".into(),
        FootprintSpec::Chip2Pin {
            package: "0402".into(),
            pin_types: Default::default(),
        },
    );
    footprints.insert(
        "U_BGA".into(),
        FootprintSpec::Bga {
            rows: 3,
            cols: 3,
            pitch_nm: mm(0.8),
            ball_dia_nm: mm(0.4),
            pin_types: Default::default(),
        },
    );
    footprints.insert(
        "U_QFN".into(),
        FootprintSpec::Qfn {
            pins_per_side: 4,
            pitch_nm: mm(0.5),
            body_nm: mm(3.0),
            ep_nm: mm(1.5),
            pin_types: Default::default(),
        },
    );
    footprints.insert(
        "U_SOT".into(),
        FootprintSpec::Sot23 {
            pin_count: 5,
            pin_types: Default::default(),
        },
    );
    footprints.insert(
        "J_HDR".into(),
        FootprintSpec::PinHeader {
            pins: 3,
            pitch_nm: mm(2.54),
            pin_types: Default::default(),
        },
    );
    let mut net_classes = BTreeMap::new();
    net_classes.insert(
        "Power".into(),
        NetClass {
            name: "Power".into(),
            track_width_nm: mm(0.4),
            clearance_nm: mm(0.2),
            via_dia_nm: mm(0.6),
            via_drill_nm: mm(0.3),
            diff_pair_width_nm: None,
            diff_pair_gap_nm: None,
        },
    );
    let mut nets = BTreeMap::new();
    nets.insert(
        "VCC".into(),
        NetDef {
            name: "VCC".into(),
            class: "Power".into(),
            pins: vec![("R1".into(), "1".into())],
            ignore_route: false,
            power_driven: false,
        },
    );
    let mut r1_nets = BTreeMap::new();
    r1_nets.insert("2".into(), "GND".into());

    let spec = BoardSpec {
        name: "roundtrip_board".into(),
        outline: vec![
            Pt::new(0, 0),
            Pt::new(mm(40.0), 0),
            Pt::new(mm(40.0), mm(30.0)),
            Pt::new(0, mm(30.0)),
        ],
        size_nm: None,
        cutouts: vec![],
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
                thickness_nm: Some(um(35.0)),
            },
            LayerDef {
                name: "B.Cu".into(),
                kind: LayerKind::Signal,
                side: Some(Side::Bottom),
                thickness_nm: None,
            },
        ],
        footprint_libs: vec![],
        netlist: None,
        footprints,
        placements: vec![Placement {
            refdes: "R1".into(),
            footprint: "R_0402".into(),
            value: "1k".into(),
            pos: Pt::new(mm(10.0), mm(10.0)),
            rot_deg: 0.0,
            side: Side::Top,
            locked: false,
            pin_nets: r1_nets,
            pin_types: BTreeMap::new(),
            mpn: None,
            lcsc: None,
            dnp: false,
            exclude_from_bom: false,
            exclude_from_pos: false,
            cpl_rotation_offset_deg: 0.0,
            schematic: None,
        }],
        keepouts: vec![],
        pours: vec![PourSpec {
            name: "GND_PLANE".into(),
            net: "GND".into(),
            layer: "In1.Cu".into(),
            outline: vec![
                Pt::new(0, 0),
                Pt::new(mm(40.0), 0),
                Pt::new(mm(40.0), mm(30.0)),
                Pt::new(0, mm(30.0)),
            ],
            priority: 1,
            clearance_nm: Some(mm(0.25)),
            min_width_nm: mm(0.2),
            thermal_relief: true,
            thermal_gap_nm: mm(0.25),
            thermal_spoke_nm: mm(0.3),
            remove_islands: true,
        }],
        tracks: vec![],
        vias: vec![],
        net_classes,
        nets,
        rules: DesignRules {
            track_width_nm: mm(0.15),
            clearance_nm: mm(0.15),
            via_at_smd: true,
            ..Default::default()
        },
        constraints: Constraints {
            max_lengths: vec![MaxLengthRule {
                nets: vec!["VCC".into()],
                max_nm: mm(20.0),
            }],
            match_groups: vec![MatchGroupRule {
                name: "SPI".into(),
                nets: vec!["VCC".into(), "GND".into()],
                tolerance_nm: mm(0.5),
                target_nm: Some(mm(12.0)),
            }],
            cap_proximity: vec![CapProximityRule {
                cap_refdes: "R1".into(),
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
        graphics: vec![],
        stackup: Default::default(),
    };

    let tmp_dir = std::env::temp_dir().join("boardc_test_spec");
    std::fs::create_dir_all(&tmp_dir).unwrap();
    let json_path = tmp_dir.join("board.json");
    let toml_path = tmp_dir.join("board.toml");
    let ir_path = tmp_dir.join("board_ir.json");

    std::fs::write(&json_path, serde_json::to_string_pretty(&spec).unwrap()).unwrap();
    std::fs::write(&toml_path, toml::to_string_pretty(&spec).unwrap()).unwrap();

    let loaded_json = load_spec_file(&json_path).unwrap();
    let loaded_toml = load_spec_file(&toml_path).unwrap();
    assert_eq!(loaded_json, spec);
    assert_eq!(loaded_toml, spec);

    let ir = BoardIr::from_spec(&loaded_toml).unwrap();
    assert_eq!(ir.footprints.len(), 5);
    assert_eq!(ir.layers.len(), 3);
    assert!(ir.nets["VCC"].pins.contains(&("R1".into(), "1".into())));
    assert!(ir.nets["GND"].pins.contains(&("R1".into(), "2".into())));
    assert_eq!(
        ir.placements[0].pin_nets.get("1").map(String::as_str),
        Some("VCC")
    );

    save_board_json(&ir, &ir_path).unwrap();
    let ir2 = load_board_json(&ir_path).unwrap();
    assert_eq!(ir, ir2);

    let _ = std::fs::remove_dir_all(&tmp_dir);
}

#[test]
fn exact_geometry_distances_and_clipper_roundtrip() {
    // Point to segment
    let d_ps = point_to_segment_dist(Pt::new(500, 300), Pt::new(0, 0), Pt::new(1000, 0));
    assert!((d_ps - 300.0).abs() < 1e-9);
    let d_ps_end = point_to_segment_dist(Pt::new(-400, 300), Pt::new(0, 0), Pt::new(1000, 0));
    assert!((d_ps_end - 500.0).abs() < 1e-9);

    // Segment to segment (crossing vs parallel vs skew)
    assert_eq!(
        segment_to_segment_dist(
            Pt::new(0, 0),
            Pt::new(100, 100),
            Pt::new(0, 100),
            Pt::new(100, 0)
        ),
        0.0
    );
    let d_ss = segment_to_segment_dist(
        Pt::new(0, 0),
        Pt::new(100, 0),
        Pt::new(0, 250),
        Pt::new(100, 250),
    );
    assert!((d_ss - 250.0).abs() < 1e-9);

    // Polygon area and point-in-polygon
    let sq = rect_poly(1000, 1000);
    assert!((polygon_area(&sq) - 1_000_000.0).abs() < 1e-6);
    assert!(point_in_polygon(Pt::new(0, 0), &sq));
    assert!(point_in_polygon(Pt::new(500, 0), &sq));
    assert!(!point_in_polygon(Pt::new(501, 0), &sq));
    assert!((point_to_polygon_dist(Pt::new(800, 0), &sq) - 300.0).abs() < 1e-9);

    // Segment to polygon and polygon to polygon
    assert!(
        (segment_to_polygon_dist(Pt::new(700, -100), Pt::new(700, 100), &sq) - 200.0).abs() < 1e-9
    );
    let sq2: Vec<Pt> = sq.iter().map(|p| Pt::new(p.x + 1400, p.y)).collect();
    assert!((polygon_to_polygon_dist(&sq, &sq2) - 400.0).abs() < 1e-9);

    // Capsule and shape distances
    let cap = ResolvedShape::Capsule {
        a: Pt::new(0, 0),
        b: Pt::new(1000, 0),
        radius: 100,
    };
    let circ = ResolvedShape::Circle {
        center: Pt::new(500, 350),
        radius: 50,
    };
    assert!((cap.dist_to_shape(&circ) - 200.0).abs() < 1e-9);

    // RoundRect and Oval discretization
    let rr = roundrect_poly(1000, 600, 100, 8);
    assert!(rr.len() >= 16);
    let ov = oval_poly(1200, 600, 8);
    assert!(ov.len() >= 16);

    // Clipper2 Path64 conversion and boolean intersection
    let p1 = pts_to_path64(&[
        Pt::new(0, 0),
        Pt::new(1000, 0),
        Pt::new(1000, 1000),
        Pt::new(0, 1000),
    ]);
    let p2 = shape_to_path64(
        &ResolvedShape::polygon(vec![
            Pt::new(500, 500),
            Pt::new(1500, 500),
            Pt::new(1500, 1500),
            Pt::new(500, 1500),
        ]),
        8,
    );
    let inter = intersect_64(
        &polys_to_paths64(&[path64_to_pts(&p1)]),
        &vec![p2],
        FillRule::NonZero,
    );
    let out_polys = paths64_to_polys(&inter);
    assert_eq!(out_polys.len(), 1);
    assert!((polygon_area(&out_polys[0]) - 250_000.0).abs() < 1.0);
}

#[test]
fn cli_skill_install_show_and_uninstall() {
    let tmp = std::env::temp_dir().join(format!("boardc_skill_test_{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&tmp);

    let show = std::process::Command::new(env!("CARGO_BIN_EXE_boardc"))
        .args(["skill", "show"])
        .output()
        .unwrap();
    assert!(show.status.success());
    let shown = String::from_utf8(show.stdout).unwrap();
    let head: Vec<&str> = shown.lines().take(2).collect();
    assert_eq!(head, vec!["---", "name: boardc"]);

    let install = std::process::Command::new(env!("CARGO_BIN_EXE_boardc"))
        .args(["skill", "install", "--to", tmp.to_str().unwrap()])
        .output()
        .unwrap();
    assert!(
        install.status.success(),
        "{}",
        String::from_utf8_lossy(&install.stderr)
    );
    let installed = tmp.join("boardc").join("SKILL.md");
    assert_eq!(std::fs::read_to_string(&installed).unwrap(), shown);

    let uninstall = std::process::Command::new(env!("CARGO_BIN_EXE_boardc"))
        .args(["skill", "uninstall", "--to", tmp.to_str().unwrap()])
        .output()
        .unwrap();
    assert!(uninstall.status.success());
    assert!(!installed.exists());

    let _ = std::fs::remove_dir_all(&tmp);
}
