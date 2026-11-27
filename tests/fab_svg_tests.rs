use std::collections::BTreeMap;
use std::fs;
use std::io::BufReader;
use std::path::PathBuf;
use std::process::Command;

use boardc::drc::{DrcReport, Hotspot, Severity, Violation, ViolationKind};
use boardc::fab::{FabOptions, export_fab};
use boardc::ir::{
    BoardIr, BoardSpec, DesignRules, FootprintSpec, LayerDef, LayerKind, PadDef, PadKind, PadShape,
    Placement, PourSpec, Pt, Side, Track, Via, mm,
};
use boardc::pipeline::{BuildOptions, compile_board};
use boardc::svg::{SvgOptions, SvgView, render_board_svg, render_layer_svg};
use boardc::zones::fill_zones;

fn fresh_tmp_dir(name: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("boardc_fab_svg_{name}_{}", std::process::id()));
    let _ = fs::remove_dir_all(&dir);
    fs::create_dir_all(&dir).unwrap();
    dir
}

fn layer(name: &str, kind: LayerKind, side: Option<Side>) -> LayerDef {
    LayerDef {
        name: name.into(),
        kind,
        side,
        thickness_nm: None,
    }
}

fn make_4layer_board() -> BoardIr {
    let mut footprints = BTreeMap::new();
    footprints.insert(
        "C_0402".into(),
        FootprintSpec::Chip2Pin {
            package: "0402".into(),
            pin_types: Default::default(),
        },
    );
    footprints.insert(
        "U_QFN16".into(),
        FootprintSpec::Qfn {
            pins_per_side: 4,
            pitch_nm: mm(0.5),
            body_nm: mm(3.0),
            ep_nm: mm(1.4),
            pin_types: Default::default(),
        },
    );
    footprints.insert(
        "HDR_2".into(),
        FootprintSpec::PinHeader {
            pins: 2,
            pitch_nm: mm(2.54),
            pin_types: Default::default(),
        },
    );
    footprints.insert(
        "NPTH_3MM".into(),
        FootprintSpec::Custom {
            pads: vec![PadDef {
                name: "1".into(),
                kind: PadKind::Npth,
                shape: PadShape::Circle { diameter: mm(3.0) },
                pos: Pt::new(0, 0),
                rot_deg: 0.0,
                drill_nm: Some(mm(3.0)),
                layers: vec!["*.Mask".into()],
                mask_margin_nm: Some(mm(0.1)),
                paste_margin_nm: None,
            }],
            courtyard: Vec::new(),
            graphics: Vec::new(),
            keepouts: Vec::new(),
            pin_types: Default::default(),
        },
    );

    let spec = BoardSpec {
        name: "fab_4layer".into(),
        outline: Vec::new(),
        size_nm: Some([mm(40.0), mm(30.0)]),
        cutouts: vec![vec![
            Pt::new(mm(34.0), mm(24.0)),
            Pt::new(mm(37.0), mm(24.0)),
            Pt::new(mm(37.0), mm(27.0)),
            Pt::new(mm(34.0), mm(27.0)),
        ]],
        layers: vec![
            layer("F.Cu", LayerKind::Signal, Some(Side::Top)),
            layer("In1.Cu", LayerKind::Plane, None),
            layer("In2.Cu", LayerKind::Plane, None),
            layer("B.Cu", LayerKind::Signal, Some(Side::Bottom)),
        ],
        footprints,
        placements: vec![
            Placement {
                refdes: "U1".into(),
                footprint: "U_QFN16".into(),
                value: "MCU_QFN16".into(),
                pos: Pt::new(mm(15.0), mm(15.0)),
                rot_deg: 0.0,
                side: Side::Top,
                locked: true,
                pin_nets: BTreeMap::from([
                    ("1".into(), "SIG0".into()),
                    ("EP".into(), "GND".into()),
                ]),
                mpn: Some("STM32TEST".into()),
                lcsc: Some("C12345".into()),
                dnp: false,
                exclude_from_bom: false,
                exclude_from_pos: false,
                cpl_rotation_offset_deg: 0.0,
                ..Default::default()
            },
            Placement {
                refdes: "C1".into(),
                footprint: "C_0402".into(),
                value: "100nF".into(),
                pos: Pt::new(mm(8.0), mm(15.0)),
                rot_deg: 90.0,
                side: Side::Top,
                locked: true,
                pin_nets: BTreeMap::from([("1".into(), "VCC".into()), ("2".into(), "GND".into())]),
                mpn: Some("CAP100N".into()),
                lcsc: Some("C1525".into()),
                dnp: false,
                exclude_from_bom: false,
                exclude_from_pos: false,
                cpl_rotation_offset_deg: 0.0,
                ..Default::default()
            },
            Placement {
                refdes: "C2".into(),
                footprint: "C_0402".into(),
                value: "100nF".into(),
                pos: Pt::new(mm(25.0), mm(15.0)),
                rot_deg: 30.0,
                side: Side::Bottom,
                locked: true,
                pin_nets: BTreeMap::from([("1".into(), "VCC".into()), ("2".into(), "GND".into())]),
                mpn: Some("CAP100N".into()),
                lcsc: Some("C1525".into()),
                dnp: false,
                exclude_from_bom: false,
                exclude_from_pos: false,
                cpl_rotation_offset_deg: 90.0,
                ..Default::default()
            },
            Placement {
                refdes: "R_DNP".into(),
                footprint: "C_0402".into(),
                value: "0R".into(),
                pos: Pt::new(mm(25.0), mm(22.0)),
                rot_deg: 0.0,
                side: Side::Top,
                locked: true,
                pin_nets: BTreeMap::new(),
                mpn: Some("RES0".into()),
                lcsc: Some("C9999".into()),
                dnp: true,
                exclude_from_bom: false,
                exclude_from_pos: false,
                cpl_rotation_offset_deg: 0.0,
                ..Default::default()
            },
            Placement {
                refdes: "J1".into(),
                footprint: "HDR_2".into(),
                value: "CONN_2".into(),
                pos: Pt::new(mm(5.0), mm(22.0)),
                rot_deg: 0.0,
                side: Side::Top,
                locked: true,
                pin_nets: BTreeMap::from([("1".into(), "SIG0".into()), ("2".into(), "GND".into())]),
                mpn: None,
                lcsc: None,
                dnp: false,
                exclude_from_bom: false,
                exclude_from_pos: false,
                cpl_rotation_offset_deg: 0.0,
                ..Default::default()
            },
            Placement {
                refdes: "H1".into(),
                footprint: "NPTH_3MM".into(),
                value: "M3_HOLE".into(),
                pos: Pt::new(mm(3.0), mm(3.0)),
                rot_deg: 0.0,
                side: Side::Top,
                locked: true,
                pin_nets: BTreeMap::new(),
                mpn: None,
                lcsc: None,
                dnp: true,
                exclude_from_bom: false,
                exclude_from_pos: false,
                cpl_rotation_offset_deg: 0.0,
                ..Default::default()
            },
        ],
        keepouts: Vec::new(),
        pours: vec![
            PourSpec {
                name: "GND_F".into(),
                net: "GND".into(),
                layer: "F.Cu".into(),
                outline: vec![
                    Pt::new(mm(1.0), mm(1.0)),
                    Pt::new(mm(39.0), mm(1.0)),
                    Pt::new(mm(39.0), mm(29.0)),
                    Pt::new(mm(1.0), mm(29.0)),
                ],
                priority: 0,
                clearance_nm: Some(mm(0.25)),
                min_width_nm: mm(0.2),
                thermal_relief: true,
                thermal_gap_nm: mm(0.25),
                thermal_spoke_nm: mm(0.3),
                remove_islands: false,
            },
            PourSpec {
                name: "GND_IN1".into(),
                net: "GND".into(),
                layer: "In1.Cu".into(),
                outline: vec![
                    Pt::new(mm(1.0), mm(1.0)),
                    Pt::new(mm(39.0), mm(1.0)),
                    Pt::new(mm(39.0), mm(29.0)),
                    Pt::new(mm(1.0), mm(29.0)),
                ],
                priority: 0,
                clearance_nm: None,
                min_width_nm: mm(0.2),
                thermal_relief: false,
                thermal_gap_nm: mm(0.25),
                thermal_spoke_nm: mm(0.3),
                remove_islands: true,
            },
            PourSpec {
                name: "VCC_IN2".into(),
                net: "VCC".into(),
                layer: "In2.Cu".into(),
                outline: vec![
                    Pt::new(mm(1.0), mm(1.0)),
                    Pt::new(mm(39.0), mm(1.0)),
                    Pt::new(mm(39.0), mm(29.0)),
                    Pt::new(mm(1.0), mm(29.0)),
                ],
                priority: 0,
                clearance_nm: None,
                min_width_nm: mm(0.2),
                thermal_relief: false,
                thermal_gap_nm: mm(0.25),
                thermal_spoke_nm: mm(0.3),
                remove_islands: true,
            },
        ],
        tracks: vec![Track {
            net: "SIG0".into(),
            layer: "F.Cu".into(),
            a: Pt::new(mm(5.0), mm(22.0)),
            b: Pt::new(mm(13.5), mm(15.75)),
            width: mm(0.2),
            locked: true,
        }],
        vias: vec![
            Via {
                net: "GND".into(),
                pos: Pt::new(mm(20.0), mm(10.0)),
                diameter: mm(0.5),
                drill: mm(0.25),
                layers: ("F.Cu".into(), "B.Cu".into()),
                locked: true,
                tented: true,
            },
            Via {
                net: "VCC".into(),
                pos: Pt::new(mm(10.0), mm(10.0)),
                diameter: mm(0.6),
                drill: mm(0.3),
                layers: ("F.Cu".into(), "B.Cu".into()),
                locked: true,
                tented: false,
            },
        ],
        net_classes: Default::default(),
        nets: Default::default(),
        rules: DesignRules {
            mask_expansion_nm: mm(0.05),
            paste_margin_nm: -mm(0.02),
            ..Default::default()
        },
        constraints: Default::default(),
        graphics: Vec::new(),
        stackup: Default::default(),
        ..Default::default()
    };
    let mut board = BoardIr::from_spec(&spec).unwrap();
    fill_zones(&mut board);
    board
}

#[test]
fn fab_export_4layer_gerbers_excellon_bom_cpl_and_svg_are_valid() {
    let tmp = fresh_tmp_dir("full_export");
    let fab_dir = tmp.join("fab");
    let board = make_4layer_board();

    let manifest = export_fab(&board, &fab_dir, &FabOptions::default()).unwrap();
    // 4 copper + 2 mask + 2 paste + 2 silk + Edge_Cuts = 11 Gerbers.
    assert_eq!(manifest.gerber_files.len(), 11);
    assert_eq!(manifest.drill_files.len(), 2);

    // Every Gerber file must be non-empty, end with M02*, and parse cleanly with gerber_parser.
    for gf in &manifest.gerber_files {
        assert!(gf.exists(), "missing gerber {}", gf.display());
        let txt = fs::read_to_string(gf).unwrap();
        assert!(!txt.is_empty());
        assert!(
            txt.contains("%MOMM*%"),
            "missing mm unit in {}",
            gf.display()
        );
        assert!(
            txt.contains("%FSLAX46Y46*%"),
            "missing format in {}",
            gf.display()
        );
        assert!(
            txt.trim_end().ends_with("M02*"),
            "missing M02* in {}",
            gf.display()
        );
        let file = fs::File::open(gf).unwrap();
        let doc = gerber_parser::parse(BufReader::new(file)).unwrap();
        assert!(
            doc.errors().is_empty(),
            "gerber_parser errors in {}: {:?}",
            gf.display(),
            doc.errors()
        );
    }

    // Critical polarity check on F_Cu.gtl: pours + %LPC*% holes must precede %LPD*% tracks/pads/vias.
    let f_cu = fs::read_to_string(fab_dir.join("F_Cu.gtl")).unwrap();
    assert!(f_cu.contains("G36*"), "expected pour region in F_Cu.gtl");
    assert!(
        f_cu.contains("%LPC*%"),
        "expected clear hole polarity in F_Cu.gtl"
    );
    let last_lpc = f_cu.rfind("%LPC*%").unwrap();
    let last_lpd = f_cu.rfind("%LPD*%").unwrap();
    assert!(
        last_lpd > last_lpc,
        "F_Cu.gtl must restore %LPD*% after %LPC*% before tracks/pads/vias"
    );
    // The SIG0 track endpoint (5.0, 22.0) -> (13.5, 15.75) must appear AFTER the final %LPD*%.
    assert!(
        f_cu[last_lpd..].contains("X5000000Y22000000D02*"),
        "expected SIG0 track after final %LPD*% so pour holes never erase it"
    );

    // Check tented vs untented via on F_Mask.gts: (10.0, 10.0) untented present, (20.0, 10.0) tented absent.
    let f_mask = fs::read_to_string(fab_dir.join("F_Mask.gts")).unwrap();
    assert!(
        f_mask.contains("X10000000Y10000000D03*"),
        "untented via must open solder mask"
    );
    assert!(
        !f_mask.contains("X20000000Y10000000D03*"),
        "tented via must NOT open solder mask"
    );

    // Check Excellon PTH and NPTH drills (Y-up mm matching Gerbers).
    let pth = fs::read_to_string(&manifest.drill_files[0]).unwrap();
    assert!(pth.contains("M48") && pth.contains("FMAT,2") && pth.contains("METRIC,TZ"));
    assert!(pth.contains("T01C") && pth.trim_end().ends_with("M30"));
    assert!(pth.contains("X10.0000Y10.0000"));
    assert!(pth.contains("X5.0000Y22.0000"));

    let npth = fs::read_to_string(&manifest.drill_files[1]).unwrap();
    assert!(npth.contains("M48") && npth.contains("FMAT,2") && npth.contains("METRIC,TZ"));
    assert!(npth.contains("T01C3.0000") && npth.contains("X3.0000Y3.0000"));
    assert!(npth.trim_end().ends_with("M30"));

    // Check BOM CSV: C1 & C2 grouped (Qty=2), R_DNP & H1 excluded.
    let bom = fs::read_to_string(&manifest.bom_csv).unwrap();
    assert!(bom.starts_with("Comment,Designator,Footprint,LCSC Part #,MPN,Qty\n"));
    assert!(bom.contains("100nF,\"C1,C2\",C_0402,C1525,CAP100N,2"));
    assert!(!bom.contains("R_DNP"));
    assert!(!bom.contains("H1"));

    // Check CPL CSV: DNP excluded, bottom rotation (180 - 30).rem_euclid(360) + 90 = 240.00.
    let cpl = fs::read_to_string(&manifest.cpl_csv).unwrap();
    assert!(cpl.starts_with("Designator,Mid X,Mid Y,Layer,Rotation\n"));
    assert!(cpl.contains("C1,8.0000,15.0000,Top,90.00"));
    assert!(cpl.contains("C2,25.0000,15.0000,Bottom,240.00"));
    assert!(!cpl.contains("R_DNP"));

    // Check SVG rendering (Top, Bottom, Layer with DRC hotspot overlay).
    let fake_drc = DrcReport {
        violations: vec![Violation {
            kind: ViolationKind::Clearance,
            severity: Severity::Error,
            layer: "F.Cu".into(),
            pos_nm: Pt::new(mm(12.0), mm(14.0)),
            measured_nm: mm(0.05),
            required_nm: mm(0.15),
            item_a: "U1:1".into(),
            item_b: Some("C1:1".into()),
            net_a: "SIG0".into(),
            net_b: "VCC".into(),
            message: "clearance".into(),
        }],
        hotspots: vec![Hotspot {
            centroid_nm: Pt::new(mm(12.0), mm(14.0)),
            radius_nm: mm(1.0),
            violation_count: 1,
            error_count: 1,
            warning_count: 0,
            root_cause_item: "U1:1".into(),
            root_cause_net: "SIG0".into(),
            affected_nets: vec!["SIG0".into(), "VCC".into()],
            layers: vec!["F.Cu".into()],
            kinds: vec![ViolationKind::Clearance],
            action_hint: "move".into(),
            violations: Vec::new(),
        }],
        error_count: 1,
        warning_count: 0,
        passed: false,
    };

    let top_svg = render_board_svg(&board, Some(&fake_drc), &SvgOptions::default());
    assert!(top_svg.starts_with("<svg "));
    assert!(top_svg.trim_end().ends_with("</svg>"));
    assert!(top_svg.contains("id=\"drc_hotspots\""));
    assert!(top_svg.contains("U1:1"));

    let bot_svg = render_board_svg(
        &board,
        None,
        &SvgOptions {
            view: SvgView::Bottom,
            ..Default::default()
        },
    );
    assert!(bot_svg.starts_with("<svg ") && bot_svg.trim_end().ends_with("</svg>"));

    let in1_svg = render_layer_svg(&board, "In1.Cu", None);
    assert!(in1_svg.starts_with("<svg ") && in1_svg.contains("layer_In1.Cu"));

    let _ = fs::remove_dir_all(&tmp);
}

#[test]
fn cli_fab_svg_and_build_emit_outputs() {
    let tmp = fresh_tmp_dir("cli_fab_svg_build");
    let board_path = tmp.join("board.json");
    let fab_out = tmp.join("cli_fab");
    let svg_file = tmp.join("cli_top.svg");
    let build_out = tmp.join("build_out");
    let cache_dir = tmp.join("cache");

    let board = make_4layer_board();
    boardc::ir::save_board_json(&board, &board_path).unwrap();

    let fab_res = Command::new(env!("CARGO_BIN_EXE_boardc"))
        .args([
            "fab",
            board_path.to_str().unwrap(),
            "-o",
            fab_out.to_str().unwrap(),
            "--json",
        ])
        .output()
        .unwrap();
    assert!(
        fab_res.status.success(),
        "boardc fab failed: {}",
        String::from_utf8_lossy(&fab_res.stderr)
    );
    let fab_json: serde_json::Value = serde_json::from_slice(&fab_res.stdout).unwrap();
    assert_eq!(fab_json["gerber_files"].as_array().unwrap().len(), 11);
    assert_eq!(fab_json["drill_files"].as_array().unwrap().len(), 2);
    assert!(
        fab_json["gbrjob"]
            .as_str()
            .unwrap()
            .ends_with("fab_4layer-job.gbrjob")
    );

    let svg_res = Command::new(env!("CARGO_BIN_EXE_boardc"))
        .args([
            "svg",
            board_path.to_str().unwrap(),
            "-o",
            svg_file.to_str().unwrap(),
        ])
        .output()
        .unwrap();
    assert!(
        svg_res.status.success(),
        "boardc svg failed: {}",
        String::from_utf8_lossy(&svg_res.stderr)
    );
    let svg_text = fs::read_to_string(&svg_file).unwrap();
    assert!(svg_text.starts_with("<svg ") && svg_text.trim_end().ends_with("</svg>"));

    // Verify `compile_board` with default BuildOptions emits board.json + fab/ + svg/.
    let mut cfg = BuildOptions {
        out_dir: Some(build_out.clone()),
        cache_dir,
        use_cache: false,
        ..Default::default()
    };
    cfg.route.max_passes = 2;
    cfg.route.multi_start = 1;
    let rep = compile_board(board, &cfg).unwrap();
    assert!(rep.emitted_files.iter().any(|p| p.ends_with("F_Cu.gtl")));
    assert!(
        rep.emitted_files
            .iter()
            .any(|p| p.ends_with("fab_4layer_bom.csv"))
    );
    assert!(
        rep.emitted_files
            .iter()
            .any(|p| p.ends_with("fab_4layer-job.gbrjob"))
    );
    assert!(
        rep.emitted_files
            .iter()
            .any(|p| p.ends_with("fab_4layer_top.svg"))
    );
    assert!(rep.emitted_files.iter().all(|p| p.exists()));

    let _ = fs::remove_dir_all(&tmp);
}

#[test]
fn regression_cw_poly_mask_expands_and_blind_via_splits_drill_file() {
    let tmp = fresh_tmp_dir("cw_poly_and_blind");
    let fab_dir = tmp.join("fab");

    // Clockwise polygon pad (negative signed area) with +0.2 mm mask margin must expand, not shrink.
    let cw_poly = vec![
        Pt::new(mm(-0.5), mm(-0.5)),
        Pt::new(mm(-0.5), mm(0.5)),
        Pt::new(mm(0.5), mm(0.5)),
        Pt::new(mm(0.5), mm(-0.5)),
    ];
    let mut footprints = BTreeMap::new();
    footprints.insert(
        "CW_PAD".into(),
        FootprintSpec::Custom {
            pads: vec![PadDef {
                name: "1".into(),
                kind: PadKind::Smd,
                shape: PadShape::Poly { verts: cw_poly },
                pos: Pt::new(0, 0),
                rot_deg: 0.0,
                drill_nm: None,
                layers: vec!["F.Cu".into(), "F.Mask".into()],
                mask_margin_nm: Some(mm(0.2)),
                paste_margin_nm: None,
            }],
            courtyard: Vec::new(),
            graphics: Vec::new(),
            keepouts: Vec::new(),
            pin_types: Default::default(),
        },
    );

    let spec = BoardSpec {
        name: "cw_blind".into(),
        outline: Vec::new(),
        size_nm: Some([mm(20.0), mm(20.0)]),
        layers: vec![
            layer("F.Cu", LayerKind::Signal, Some(Side::Top)),
            layer("In1.Cu", LayerKind::Signal, None),
            layer("In2.Cu", LayerKind::Signal, None),
            layer("B.Cu", LayerKind::Signal, Some(Side::Bottom)),
        ],
        footprints,
        placements: vec![Placement {
            refdes: "P1".into(),
            footprint: "CW_PAD".into(),
            pos: Pt::new(mm(10.0), mm(10.0)),
            pin_nets: BTreeMap::from([("1".into(), "SIG".into())]),
            ..Default::default()
        }],
        tracks: vec![
            Track {
                net: "SIG".into(),
                layer: "F.Cu".into(),
                a: Pt::new(mm(10.0), mm(10.0)),
                b: Pt::new(mm(13.0), mm(10.0)),
                width: mm(0.2),
                locked: true,
            },
            Track {
                net: "SIG".into(),
                layer: "In1.Cu".into(),
                a: Pt::new(mm(13.0), mm(10.0)),
                b: Pt::new(mm(16.0), mm(10.0)),
                width: mm(0.2),
                locked: true,
            },
        ],
        vias: vec![
            Via {
                net: "SIG".into(),
                pos: Pt::new(mm(13.0), mm(10.0)),
                diameter: mm(0.45),
                drill: mm(0.2),
                layers: ("F.Cu".into(), "In1.Cu".into()),
                locked: true,
                tented: true,
            },
            Via {
                net: "SIG".into(),
                pos: Pt::new(mm(16.0), mm(10.0)),
                diameter: mm(0.45),
                drill: mm(0.2),
                layers: ("F.Cu".into(), "B.Cu".into()),
                locked: true,
                tented: true,
            },
        ],
        rules: DesignRules::default(),
        ..Default::default()
    };

    let board = BoardIr::from_spec(&spec).unwrap();
    let manifest = export_fab(&board, &fab_dir, &FabOptions::default()).unwrap();

    // Expanded CW polygon (1.0 mm + 2*0.2 mm = 1.4 mm -> [-0.7, +0.7] mm) in the %AM outline macro.
    let mask_path = manifest
        .gerber_files
        .iter()
        .find(|p| p.ends_with("F_Mask.gts"))
        .expect("missing F_Mask.gts");
    let mask_text = fs::read_to_string(mask_path).unwrap();
    let am_start = mask_text
        .find("4,1,")
        .expect("expected polygon aperture macro outline primitive in F_Mask.gts");
    let am_end = mask_text[am_start..].find('*').unwrap() + am_start;
    let nums: Vec<f64> = mask_text[am_start..am_end]
        .split([',', '\n', '\r'])
        .skip(3)
        .filter_map(|tok| tok.trim().parse::<f64>().ok())
        .collect();
    // Last token is rotation angle; preceding tokens are interleaved (x, y) coordinates in mm.
    let coords = &nums[..nums.len() - 1];
    let max_abs = coords.iter().copied().map(f64::abs).fold(0.0_f64, f64::max);
    assert!(
        max_abs >= 0.69,
        "CW pad mask should expand to +/-0.7 mm, got max_abs={max_abs} in {}",
        &mask_text[am_start..am_end]
    );

    // Blind via F.Cu..In1.Cu must be split into its own span drill file with LAYER_SPAN comment.
    let blind_drl = manifest
        .drill_files
        .iter()
        .find(|p| {
            p.file_name()
                .unwrap()
                .to_str()
                .unwrap()
                .contains("F_Cu-In1_Cu")
        })
        .expect("expected dedicated blind via drill file");
    let blind_text = fs::read_to_string(blind_drl).unwrap();
    assert!(blind_text.contains("; LAYER_SPAN=F.Cu..In1.Cu"));
    assert!(blind_text.contains("X13.0000Y10.0000"));
    assert!(!blind_text.contains("X16.0000Y10.0000"));

    // Through-hole PTH drill file must contain only the through via at x=16.
    let pth_drl = manifest
        .drill_files
        .iter()
        .find(|p| p.ends_with("cw_blind-PTH.drl"))
        .expect("expected through PTH drill file");
    let pth_text = fs::read_to_string(pth_drl).unwrap();
    assert!(pth_text.contains("X16.0000Y10.0000"));
    assert!(!pth_text.contains("X13.0000Y10.0000"));

    let fab_rep = boardc::verify::verify_fab_outputs(&board, &fab_dir).unwrap();
    assert!(
        fab_rep.passed,
        "blind via fab verification failed: {fab_rep:?}"
    );

    let _ = fs::remove_dir_all(&tmp);
}

fn gerber_op_coords_mm(path: &std::path::Path) -> Vec<(f64, f64)> {
    use gerber_types::{Command, DCode, FunctionCode, Operation};
    let file = fs::File::open(path).unwrap();
    let doc = gerber_parser::parse(BufReader::new(file)).unwrap();
    assert!(doc.errors().is_empty(), "{:?}", doc.errors());
    let mut cur = (0.0_f64, 0.0_f64);
    let mut pts = Vec::new();
    for cmd in doc.commands() {
        if let Command::FunctionCode(FunctionCode::DCode(DCode::Operation(op))) = cmd {
            let coords = match op {
                Operation::Move(c) | Operation::Interpolate(c, _) | Operation::Flash(c) => c,
            };
            if let Some(c) = coords {
                if let Some(x) = c.x {
                    cur.0 = f64::from(x);
                }
                if let Some(y) = c.y {
                    cur.1 = f64::from(y);
                }
            }
            if !matches!(op, Operation::Move(_)) {
                pts.push(cur);
            }
        }
    }
    pts
}

#[test]
fn dnp_skips_paste_and_exclude_flags_filter_bom_cpl_without_hiding_silk_or_svg() {
    let tmp = fresh_tmp_dir("dnp_paste_virtual");
    let fab_dir = tmp.join("fab");

    let toml_spec = r#"
name = "dnp_virtual"
size_nm = [40000000, 30000000]

[footprints.R_0402]
kind = "chip2_pin"
package = "0402"

[footprints.DNP_WITH_PASTE_GFX]
kind = "custom"
[[footprints.DNP_WITH_PASTE_GFX.pads]]
name = "1"
kind = "smd"
shape = { shape = "rect", width = 1000000, height = 1000000 }
pos = { x = 0, y = 0 }
layers = ["F.Cu", "F.Mask", "F.Paste"]
[[footprints.DNP_WITH_PASTE_GFX.graphics]]
kind = "rect"
layer = "F.Paste"
min = { x = -400000, y = -400000 }
max = { x = 400000, y = 400000 }
width = 0
fill = true

[footprints.PASTE_GFX_ONLY]
kind = "custom"
[[footprints.PASTE_GFX_ONLY.pads]]
name = "1"
kind = "smd"
shape = { shape = "rect", width = 1000000, height = 1000000 }
pos = { x = 0, y = 0 }
layers = ["F.Cu", "F.Mask"]
[[footprints.PASTE_GFX_ONLY.graphics]]
kind = "rect"
layer = "F.Paste"
min = { x = -350000, y = -350000 }
max = { x = 350000, y = 350000 }
width = 0
fill = true

[footprints.TP_PAD]
kind = "custom"
[[footprints.TP_PAD.pads]]
name = "1"
kind = "smd"
shape = { shape = "circle", diameter = 1000000 }
pos = { x = 0, y = 0 }
layers = ["F.Cu", "F.Mask", "F.Paste"]

[[placements]]
refdes = "R1"
footprint = "R_0402"
value = "10k"
pos = { x = 10000000, y = 8000000 }
side = "top"

[[placements]]
refdes = "R_DNP_TOP"
footprint = "DNP_WITH_PASTE_GFX"
value = "0R"
pos = { x = 10000000, y = 20000000 }
side = "top"
dnp = true

[[placements]]
refdes = "R_DNP_BOT"
footprint = "R_0402"
value = "0R"
pos = { x = 20000000, y = 8000000 }
side = "bottom"
dnp = true

[[placements]]
refdes = "U_PASTE_GFX"
footprint = "PASTE_GFX_ONLY"
value = "STENCIL"
pos = { x = 20000000, y = 14000000 }
side = "top"

[[placements]]
refdes = "TP1"
footprint = "TP_PAD"
value = "TEST"
pos = { x = 30000000, y = 20000000 }
side = "top"
exclude_from_bom = true
exclude_from_pos = true

[[placements]]
refdes = "J_NO_BOM"
footprint = "R_0402"
value = "1k"
pos = { x = 20000000, y = 20000000 }
side = "top"
exclude_from_bom = true

[[placements]]
refdes = "J_NO_POS"
footprint = "R_0402"
value = "2k"
pos = { x = 30000000, y = 8000000 }
side = "top"
exclude_from_pos = true
"#;

    let spec: BoardSpec = toml::from_str(toml_spec).unwrap();
    let board = BoardIr::from_spec(&spec).unwrap();
    let manifest = export_fab(&board, &fab_dir, &FabOptions::default()).unwrap();

    // F_Paste must include R1 and TP1, and have zero pads or paste graphics around R_DNP_TOP (10, 20).
    let f_paste_pts = gerber_op_coords_mm(&fab_dir.join("F_Paste.gtp"));
    let near = |pts: &[(f64, f64)], cx: f64, cy: f64, r: f64| {
        pts.iter()
            .any(|&(x, y)| (x - cx).abs() <= r && (y - cy).abs() <= r)
    };
    assert!(
        near(&f_paste_pts, 10.0, 8.0, 2.0),
        "R1 missing from F_Paste"
    );
    assert!(
        near(&f_paste_pts, 20.0, 14.0, 1.0),
        "populated footprint F.Paste graphic (no paste pad) missing from F_Paste"
    );
    assert!(
        near(&f_paste_pts, 30.0, 20.0, 2.0),
        "TP1 missing from F_Paste"
    );
    assert!(
        !near(&f_paste_pts, 10.0, 20.0, 2.5),
        "DNP part R_DNP_TOP must not emit paste pads or paste graphics: {f_paste_pts:?}"
    );

    // B_Paste must be completely empty because the only bottom part is R_DNP_BOT.
    let b_paste_pts = gerber_op_coords_mm(&fab_dir.join("B_Paste.gbp"));
    assert!(
        b_paste_pts.is_empty(),
        "DNP bottom part must not emit B_Paste: {b_paste_pts:?}"
    );

    // Copper and solder mask openings remain on DNP pads for optional hand population.
    assert!(near(
        &gerber_op_coords_mm(&fab_dir.join("F_Cu.gtl")),
        10.0,
        20.0,
        2.0
    ));
    assert!(near(
        &gerber_op_coords_mm(&fab_dir.join("F_Mask.gts")),
        10.0,
        20.0,
        2.0
    ));
    assert!(near(
        &gerber_op_coords_mm(&fab_dir.join("B_Cu.gbl")),
        20.0,
        8.0,
        2.0
    ));
    assert!(near(
        &gerber_op_coords_mm(&fab_dir.join("B_Mask.gbs")),
        20.0,
        8.0,
        2.0
    ));

    // BOM omits dnp and exclude_from_bom, keeps R1 and J_NO_POS.
    let bom = fs::read_to_string(&manifest.bom_csv).unwrap();
    assert!(bom.contains("R1"));
    assert!(bom.contains("J_NO_POS"));
    assert!(!bom.contains("R_DNP_TOP"));
    assert!(!bom.contains("R_DNP_BOT"));
    assert!(!bom.contains("TP1"));
    assert!(!bom.contains("J_NO_BOM"));

    // CPL omits dnp and exclude_from_pos, keeps R1 and J_NO_BOM.
    let cpl = fs::read_to_string(&manifest.cpl_csv).unwrap();
    assert!(cpl.contains("R1,"));
    assert!(cpl.contains("J_NO_BOM,"));
    assert!(!cpl.contains("R_DNP_TOP"));
    assert!(!cpl.contains("R_DNP_BOT"));
    assert!(!cpl.contains("TP1"));
    assert!(!cpl.contains("J_NO_POS"));

    // Silkscreen and SVG keep TP1 refdes (non-DNP virtual footprint) while omitting R_DNP_TOP refdes.
    let f_silk_pts = gerber_op_coords_mm(&fab_dir.join("F_Silkscreen.gto"));
    assert!(
        near(&f_silk_pts, 30.0, 20.5, 2.5),
        "virtual footprint TP1 should keep silkscreen refdes"
    );
    assert!(
        !near(&f_silk_pts, 10.0, 20.5, 2.5),
        "DNP part R_DNP_TOP should omit silkscreen refdes"
    );

    let top_svg = render_board_svg(&board, None, &SvgOptions::default());
    let silk_start = top_svg.find("id=\"silkscreen\"").unwrap();
    let silk_end = top_svg[silk_start..].find("</g>").unwrap() + silk_start;
    let svg_attr = |line: &str, name: &str| {
        line.split_once(name)?
            .1
            .split('"')
            .next()?
            .parse::<f64>()
            .ok()
    };
    // Top SVG uses SVG Y = -board_y; negate y1 back to board coordinates to reuse `near`.
    let svg_silk_pts: Vec<(f64, f64)> = top_svg[silk_start..silk_end]
        .lines()
        .filter_map(|line| Some((svg_attr(line, "x1=\"")?, -svg_attr(line, "y1=\"")?)))
        .collect();
    assert!(
        near(&svg_silk_pts, 30.0, 20.5, 2.5),
        "TP1 should appear on SVG silkscreen"
    );
    assert!(
        !near(&svg_silk_pts, 10.0, 20.5, 2.5),
        "R_DNP_TOP should be hidden on SVG silkscreen"
    );

    // Independent verifier must pass cleanly (including empty B_Paste.gbp).
    let fab_rep = boardc::verify::verify_fab_outputs(&board, &fab_dir).unwrap();
    assert!(
        fab_rep.passed,
        "fab verification failed: {:?}",
        fab_rep.errors
    );

    let _ = fs::remove_dir_all(&tmp);
}

#[test]
fn stackup_toml_emits_gbrjob_with_thickness_finish_pofv_and_dielectrics() {
    let tmp = fresh_tmp_dir("stackup_gbrjob");
    let fab_6l = tmp.join("fab_6l");
    let fab_2l = tmp.join("fab_2l");

    let spec_6l_toml = r#"
name = "six_layer_pofv"
size_nm = [40000000, 30000000]

[[layers]]
name = "F.Cu"
kind = "signal"
side = "top"

[[layers]]
name = "In1.Cu"
kind = "plane"

[[layers]]
name = "In2.Cu"
kind = "signal"

[[layers]]
name = "In3.Cu"
kind = "signal"

[[layers]]
name = "In4.Cu"
kind = "plane"

[[layers]]
name = "B.Cu"
kind = "signal"
side = "bottom"

[stackup]
thickness_nm = 1000000
finish = "ENIG"
pofv = true
dielectric_nm = [95000, 240000, 170000, 240000, 95000]

[footprints.R0402]
kind = "chip2_pin"
package = "0402"

[[placements]]
refdes = "R1"
footprint = "R0402"
value = "10k"
pos = { x = 15000000, y = 15000000 }
pin_nets = { "1" = "SIG", "2" = "GND" }
"#;
    let spec_6l: BoardSpec = toml::from_str(spec_6l_toml).unwrap();
    assert_eq!(spec_6l.stackup.thickness_nm, 1_000_000);
    assert_eq!(spec_6l.stackup.finish.as_deref(), Some("ENIG"));
    assert!(spec_6l.stackup.pofv);
    assert_eq!(
        spec_6l.stackup.dielectric_nm,
        vec![95_000, 240_000, 170_000, 240_000, 95_000]
    );

    let board_6l = BoardIr::from_spec(&spec_6l).unwrap();
    let manifest_6l = export_fab(&board_6l, &fab_6l, &FabOptions::default()).unwrap();
    assert!(manifest_6l.gbrjob.ends_with("six_layer_pofv-job.gbrjob"));
    assert!(manifest_6l.gbrjob.exists());
    assert!(manifest_6l.all_files().contains(&manifest_6l.gbrjob));

    let job: serde_json::Value =
        serde_json::from_str(&fs::read_to_string(&manifest_6l.gbrjob).unwrap()).unwrap();
    assert_eq!(job["Header"]["GenerationSoftware"]["Vendor"], "boardc");
    assert_eq!(job["GeneralSpecs"]["ProjectId"]["Name"], "six_layer_pofv");
    assert_eq!(job["GeneralSpecs"]["Size"]["X"], 40.0);
    assert_eq!(job["GeneralSpecs"]["Size"]["Y"], 30.0);
    assert_eq!(job["GeneralSpecs"]["LayerNumber"], 6);
    assert_eq!(job["GeneralSpecs"]["BoardThickness"], 1.0);
    assert_eq!(job["GeneralSpecs"]["Finish"], "ENIG");
    assert_eq!(
        job["GeneralSpecs"]["ViaProtection"],
        serde_json::json!(["VII"])
    );

    let files = job["FilesAttributes"].as_array().unwrap();
    assert_eq!(files.len(), manifest_6l.gerber_files.len());
    for f in files {
        let rel = f["Path"].as_str().unwrap();
        assert!(
            fab_6l.join(rel).exists(),
            "FilesAttributes path {rel} missing"
        );
    }
    let find_file = |name: &str| files.iter().find(|f| f["Path"] == name).unwrap();
    assert_eq!(find_file("F_Cu.gtl")["FileFunction"], "Copper,L1,Top");
    assert_eq!(find_file("In1_Cu.g1")["FileFunction"], "Copper,L2,Inr");
    assert_eq!(find_file("In4_Cu.g4")["FileFunction"], "Copper,L5,Inr");
    assert_eq!(find_file("B_Cu.gbl")["FileFunction"], "Copper,L6,Bot");
    assert_eq!(find_file("F_Mask.gts")["FileFunction"], "SolderMask,Top");
    assert_eq!(find_file("F_Mask.gts")["FilePolarity"], "Negative");
    assert_eq!(find_file("Edge_Cuts.gm1")["FileFunction"], "Profile");
    // Emitted solder mask Gerbers must declare Negative polarity matching FilesAttributes.
    let f_mask_txt = fs::read_to_string(fab_6l.join("F_Mask.gts")).unwrap();
    let b_mask_txt = fs::read_to_string(fab_6l.join("B_Mask.gbs")).unwrap();
    assert!(f_mask_txt.contains("%TF.FilePolarity,Negative*%"));
    assert!(b_mask_txt.contains("%TF.FilePolarity,Negative*%"));
    // `[stackup] pofv = true` enables `rules.via_at_smd` consistently.
    assert!(board_6l.rules.via_at_smd);

    let stack = job["MaterialStackup"].as_array().unwrap();
    let dielectrics: Vec<f64> = stack
        .iter()
        .filter(|e| e["Type"] == "Dielectric")
        .map(|e| e["Thickness"].as_f64().unwrap())
        .collect();
    assert_eq!(dielectrics, vec![0.095, 0.24, 0.17, 0.24, 0.095]);
    let coppers: Vec<(&str, f64)> = stack
        .iter()
        .filter(|e| e["Type"] == "Copper")
        .map(|e| {
            (
                e["Name"].as_str().unwrap(),
                e["Thickness"].as_f64().unwrap(),
            )
        })
        .collect();
    assert_eq!(
        coppers,
        vec![
            ("F.Cu", 0.035),
            ("In1.Cu", 0.0175),
            ("In2.Cu", 0.0175),
            ("In3.Cu", 0.0175),
            ("In4.Cu", 0.0175),
            ("B.Cu", 0.035),
        ]
    );

    let fab_rep = boardc::verify::verify_fab_outputs(&board_6l, &fab_6l).unwrap();
    assert!(fab_rep.passed, "6-layer fab verify failed: {fab_rep:?}");

    // Negative verifier checks: corrupted thickness, finish, ViaProtection, malformed JSON, and missing gbrjob all fail.
    let orig_job = fs::read_to_string(&manifest_6l.gbrjob).unwrap();
    fs::write(
        &manifest_6l.gbrjob,
        orig_job.replace("\"BoardThickness\": 1.0", "\"BoardThickness\": 1.6"),
    )
    .unwrap();
    assert!(boardc::verify::verify_fab_outputs(&board_6l, &fab_6l).is_err());

    fs::write(
        &manifest_6l.gbrjob,
        orig_job.replace("\"Finish\": \"ENIG\"", "\"Finish\": \"None\""),
    )
    .unwrap();
    assert!(boardc::verify::verify_fab_outputs(&board_6l, &fab_6l).is_err());

    fs::write(&manifest_6l.gbrjob, "{not json").unwrap();
    assert!(boardc::verify::verify_fab_outputs(&board_6l, &fab_6l).is_err());

    fs::remove_file(&manifest_6l.gbrjob).unwrap();
    assert!(boardc::verify::verify_fab_outputs(&board_6l, &fab_6l).is_err());
    fs::write(&manifest_6l.gbrjob, &orig_job).unwrap();

    // Default stackup (omitted in TOML) on 2-layer board: 1.6 mm, Finish="None", no ViaProtection, 1.51 mm core.
    let spec_2l: BoardSpec =
        toml::from_str("name = \"def_2l\"\nsize_nm = [20000000, 10000000]\n").unwrap();
    let board_2l = BoardIr::from_spec(&spec_2l).unwrap();
    let manifest_2l = export_fab(&board_2l, &fab_2l, &FabOptions::default()).unwrap();
    let job_2l: serde_json::Value =
        serde_json::from_str(&fs::read_to_string(&manifest_2l.gbrjob).unwrap()).unwrap();
    assert_eq!(job_2l["GeneralSpecs"]["LayerNumber"], 2);
    assert_eq!(job_2l["GeneralSpecs"]["BoardThickness"], 1.6);
    assert_eq!(job_2l["GeneralSpecs"]["Finish"], "None");
    assert!(job_2l["GeneralSpecs"].get("ViaProtection").is_none());
    let diels_2l: Vec<f64> = job_2l["MaterialStackup"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|e| e["Type"] == "Dielectric")
        .map(|e| e["Thickness"].as_f64().unwrap())
        .collect();
    assert_eq!(diels_2l, vec![1.51]);

    // 4-layer default dielectric split with remainder (1_475_000 / 3) preserves exact total thickness.
    let board_4l = make_4layer_board();
    let (cu_4l, diel_4l) = board_4l.resolved_stackup_thicknesses();
    assert_eq!(diel_4l.len(), 3);
    let total_4l: i64 = cu_4l.iter().map(|(_, t)| *t).sum::<i64>()
        + diel_4l.iter().sum::<i64>()
        + 2 * boardc::ir::SOLDER_MASK_THICKNESS_NM;
    assert_eq!(total_4l, board_4l.effective_board_thickness_nm());

    // 1-layer board emits a single substrate dielectric (1.545 mm) and `[rules] via_at_smd = true` enables POFV metadata.
    let fab_1l = tmp.join("fab_1l");
    let spec_1l: BoardSpec = toml::from_str(
        "name = \"one_layer\"\nsize_nm = [20000000, 10000000]\n[[layers]]\nname = \"F.Cu\"\nkind = \"signal\"\nside = \"top\"\n[rules]\nvia_at_smd = true\n",
    )
    .unwrap();
    let board_1l = BoardIr::from_spec(&spec_1l).unwrap();
    assert!(board_1l.stackup.pofv);
    let manifest_1l = export_fab(&board_1l, &fab_1l, &FabOptions::default()).unwrap();
    let job_1l: serde_json::Value =
        serde_json::from_str(&fs::read_to_string(&manifest_1l.gbrjob).unwrap()).unwrap();
    assert_eq!(job_1l["GeneralSpecs"]["LayerNumber"], 1);
    assert_eq!(
        job_1l["GeneralSpecs"]["ViaProtection"],
        serde_json::json!(["VII"])
    );
    let diels_1l: Vec<f64> = job_1l["MaterialStackup"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|e| e["Type"] == "Dielectric")
        .map(|e| e["Thickness"].as_f64().unwrap())
        .collect();
    assert_eq!(diels_1l, vec![1.545]);
    assert!(
        boardc::verify::verify_fab_outputs(&board_1l, &fab_1l)
            .unwrap()
            .passed
    );

    // Spec validation catches invalid thickness_nm and mismatched/non-positive dielectric_nm.
    let mut bad_thick = spec_6l.clone();
    bad_thick.stackup.thickness_nm = 0;
    assert!(BoardIr::from_spec(&bad_thick).is_err());

    let mut bad_len = spec_6l.clone();
    bad_len.stackup.dielectric_nm = vec![95_000, 240_000];
    assert!(BoardIr::from_spec(&bad_len).is_err());

    let mut bad_entry = spec_6l.clone();
    bad_entry.stackup.dielectric_nm[2] = 0;
    assert!(BoardIr::from_spec(&bad_entry).is_err());

    let _ = fs::remove_dir_all(&tmp);
}

fn gerber_stroke_segments_mm(path: &std::path::Path) -> Vec<((f64, f64), (f64, f64))> {
    use gerber_types::{Command, DCode, FunctionCode, GCode, Operation};
    let file = fs::File::open(path).unwrap();
    let doc = gerber_parser::parse(BufReader::new(file)).unwrap();
    assert!(doc.errors().is_empty(), "{:?}", doc.errors());
    let mut cur = (0.0_f64, 0.0_f64);
    let mut in_region = false;
    let mut segs = Vec::new();
    for cmd in doc.commands() {
        match cmd {
            Command::FunctionCode(FunctionCode::GCode(GCode::RegionMode(on))) => {
                in_region = *on;
            }
            Command::FunctionCode(FunctionCode::DCode(DCode::Operation(op))) => {
                let prev = cur;
                let coords = match op {
                    Operation::Move(c) | Operation::Interpolate(c, _) | Operation::Flash(c) => c,
                };
                if let Some(c) = coords {
                    if let Some(x) = c.x {
                        cur.0 = f64::from(x);
                    }
                    if let Some(y) = c.y {
                        cur.1 = f64::from(y);
                    }
                }
                if !in_region && matches!(op, Operation::Interpolate(_, _)) {
                    segs.push((prev, cur));
                }
            }
            _ => {}
        }
    }
    segs
}

#[test]
fn silkscreen_exact_mask_clipping_and_cli_subtract_soldermask() {
    use boardc::ir::Graphic;

    let tmp = fresh_tmp_dir("silk_clip");
    let mut footprints = BTreeMap::new();
    footprints.insert(
        "PAD_2MM".into(),
        FootprintSpec::Custom {
            pads: vec![PadDef {
                name: "1".into(),
                kind: PadKind::Smd,
                shape: PadShape::Rect {
                    width: mm(2.0),
                    height: mm(2.0),
                },
                pos: Pt::new(0, 0),
                rot_deg: 0.0,
                drill_nm: None,
                layers: vec!["F.Cu".into(), "F.Mask".into()],
                mask_margin_nm: None,
                paste_margin_nm: None,
            }],
            courtyard: Vec::new(),
            graphics: Vec::new(),
            keepouts: Vec::new(),
            pin_types: Default::default(),
        },
    );
    footprints.insert(
        "PAD_CIRC".into(),
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
            courtyard: Vec::new(),
            graphics: Vec::new(),
            keepouts: Vec::new(),
            pin_types: Default::default(),
        },
    );
    footprints.insert(
        "PAD_OVAL".into(),
        FootprintSpec::Custom {
            pads: vec![PadDef {
                name: "1".into(),
                kind: PadKind::Smd,
                shape: PadShape::Oval {
                    width: mm(2.4),
                    height: mm(1.0),
                },
                pos: Pt::new(0, 0),
                rot_deg: 0.0,
                drill_nm: None,
                layers: vec!["F.Cu".into(), "F.Mask".into()],
                mask_margin_nm: None,
                paste_margin_nm: None,
            }],
            courtyard: Vec::new(),
            graphics: Vec::new(),
            keepouts: Vec::new(),
            pin_types: Default::default(),
        },
    );

    let spec = BoardSpec {
        name: "silk_clip".into(),
        size_nm: Some([mm(40.0), mm(30.0)]),
        footprints,
        placements: vec![
            Placement {
                refdes: String::new(),
                footprint: "PAD_2MM".into(),
                pos: Pt::new(mm(10.0), mm(10.0)),
                dnp: true,
                ..Default::default()
            },
            Placement {
                refdes: String::new(),
                footprint: "PAD_CIRC".into(),
                pos: Pt::new(mm(11.0), mm(21.0)),
                dnp: true,
                ..Default::default()
            },
            Placement {
                refdes: String::new(),
                footprint: "PAD_OVAL".into(),
                pos: Pt::new(mm(25.0), mm(21.0)),
                rot_deg: 45.0,
                dnp: true,
                ..Default::default()
            },
        ],
        vias: vec![
            Via {
                net: "VCC".into(),
                pos: Pt::new(mm(20.0), mm(10.0)),
                diameter: mm(1.0),
                drill: mm(0.4),
                layers: ("F.Cu".into(), "B.Cu".into()),
                locked: true,
                tented: false,
            },
            Via {
                net: "GND".into(),
                pos: Pt::new(mm(25.0), mm(10.0)),
                diameter: mm(1.0),
                drill: mm(0.4),
                layers: ("F.Cu".into(), "B.Cu".into()),
                locked: true,
                tented: true,
            },
        ],
        graphics: vec![
            // Long 10 mm stroke crossing 2x2 pad at (10, 10).
            Graphic::Line {
                layer: "F.SilkS".into(),
                a: Pt::new(mm(5.0), mm(10.0)),
                b: Pt::new(mm(15.0), mm(10.0)),
                width: mm(0.15),
            },
            // Stroke crossing untented via at (20, 10) and tented via at (25, 10).
            Graphic::Line {
                layer: "F.SilkS".into(),
                a: Pt::new(mm(17.0), mm(10.0)),
                b: Pt::new(mm(28.0), mm(10.0)),
                width: mm(0.15),
            },
            // Board F.Mask graphic opening [30, 9]..[32, 11] crossed by vertical silk line.
            Graphic::Rect {
                layer: "F.Mask".into(),
                min: Pt::new(mm(30.0), mm(9.0)),
                max: Pt::new(mm(32.0), mm(11.0)),
                width: 0,
                fill: true,
            },
            Graphic::Line {
                layer: "F.SilkS".into(),
                a: Pt::new(mm(31.0), mm(6.0)),
                b: Pt::new(mm(31.0), mm(14.0)),
                width: mm(0.15),
            },
            // Filled silkscreen rect wrapping circular pad at (11, 21).
            Graphic::Rect {
                layer: "F.SilkS".into(),
                min: Pt::new(mm(8.0), mm(18.0)),
                max: Pt::new(mm(14.0), mm(24.0)),
                width: 0,
                fill: true,
            },
            // Diagonal X-crossing strokes over rotated oval pad at (25, 21).
            Graphic::Line {
                layer: "F.SilkS".into(),
                a: Pt::new(mm(21.0), mm(17.0)),
                b: Pt::new(mm(29.0), mm(25.0)),
                width: mm(0.15),
            },
            Graphic::Line {
                layer: "F.SilkS".into(),
                a: Pt::new(mm(21.0), mm(25.0)),
                b: Pt::new(mm(29.0), mm(17.0)),
                width: mm(0.15),
            },
            // Bottom-side B.Mask graphic [14, 4]..[16, 6] crossed by B.SilkS horizontal stroke.
            Graphic::Rect {
                layer: "B.Mask".into(),
                min: Pt::new(mm(14.0), mm(4.0)),
                max: Pt::new(mm(16.0), mm(6.0)),
                width: 0,
                fill: true,
            },
            Graphic::Line {
                layer: "B.SilkS".into(),
                a: Pt::new(mm(11.0), mm(5.0)),
                b: Pt::new(mm(19.0), mm(5.0)),
                width: mm(0.15),
            },
        ],
        rules: DesignRules {
            mask_expansion_nm: mm(0.05),
            silk_clearance_nm: mm(0.15),
            ..Default::default()
        },
        ..Default::default()
    };

    let board = BoardIr::from_spec(&spec).unwrap();
    let board_path = tmp.join("board.json");
    boardc::ir::save_board_json(&board, &board_path).unwrap();

    // CLI `--subtract-soldermask` must succeed and produce exact clipped silkscreen.
    let fab_clipped = tmp.join("fab_clipped");
    let res_sub = Command::new(env!("CARGO_BIN_EXE_boardc"))
        .args([
            "fab",
            board_path.to_str().unwrap(),
            "-o",
            fab_clipped.to_str().unwrap(),
            "--subtract-soldermask",
        ])
        .output()
        .unwrap();
    assert!(
        res_sub.status.success(),
        "boardc fab --subtract-soldermask failed: {}",
        String::from_utf8_lossy(&res_sub.stderr)
    );

    let segs = gerber_stroke_segments_mm(&fab_clipped.join("F_Silkscreen.gto"));
    let find_hseg = |y_mm: f64, x_lo: f64, x_hi: f64| {
        segs.iter().any(|&((x0, y0), (x1, y1))| {
            let (a, b) = if x0 <= x1 { (x0, x1) } else { (x1, x0) };
            (y0 - y_mm).abs() < 0.005
                && (y1 - y_mm).abs() < 0.005
                && (a - x_lo).abs() < 0.01
                && (b - x_hi).abs() < 0.01
        })
    };

    // 2x2 pad + 0.05 mask + 0.075 half-width + 0.15 silk_clearance => gap [8.725, 11.275].
    assert!(
        find_hseg(10.0, 5.0, 8.725) && find_hseg(10.0, 11.275, 15.0),
        "expected exact clipped pad segments [5, 8.725] and [11.275, 15], got: {segs:?}"
    );
    // Untented via r=0.5 + 0.05 + 0.075 + 0.15 = 0.775 around x=20 => [17, 19.225] & [20.775, 28]; tented via at x=25 unclipped.
    assert!(
        find_hseg(10.0, 17.0, 19.225) && find_hseg(10.0, 20.775, 28.0),
        "expected exact clipped untented-via segments (tented via untouched), got: {segs:?}"
    );
    // Board F.Mask rect [30, 9]..[32, 11] + 0.075 + 0.15 => vertical gap [8.775, 11.225].
    let has_vseg = |y_lo: f64, y_hi: f64| {
        segs.iter().any(|&((x0, y0), (x1, y1))| {
            let (a, b) = if y0 <= y1 { (y0, y1) } else { (y1, y0) };
            (x0 - 31.0).abs() < 0.005
                && (x1 - 31.0).abs() < 0.005
                && (a - y_lo).abs() < 0.01
                && (b - y_hi).abs() < 0.01
        })
    };
    assert!(
        has_vseg(6.0, 8.775) && has_vseg(11.225, 14.0),
        "expected vertical stroke clipped against F.Mask board graphic, got: {segs:?}"
    );

    // Crossing X-strokes over 45-deg rotated 2.4x1.0 oval pad at (25, 21) split into 4 pieces clearing pad.
    let diag_segs: Vec<_> = segs
        .iter()
        .copied()
        .filter(|&((x0, y0), (x1, y1))| {
            x0 >= 20.5 && x1 <= 29.5 && y0 >= 16.5 && y1 <= 25.5 && (x1 - x0).abs() > 0.1
        })
        .collect();
    assert_eq!(
        diag_segs.len(),
        4,
        "expected X-crossing strokes split into 4 pieces by rotated oval pad, got: {diag_segs:?}"
    );
    // Verify every clipped diagonal endpoint stays outside oval pad + mask(0.05) + half-w(0.075) + clr(0.15).
    for &((x0, y0), (x1, y1)) in &diag_segs {
        for (px, py) in [(x0, y0), (x1, y1)] {
            let (rx, ry) = (px - 25.0, py - 21.0);
            let along = ((rx + ry) * std::f64::consts::FRAC_1_SQRT_2).abs();
            let perp = ((-rx + ry) * std::f64::consts::FRAC_1_SQRT_2).abs();
            let dx = (along - 0.7).max(0.0);
            let dist_to_capsule_axis = (dx * dx + perp * perp).sqrt();
            assert!(
                dist_to_capsule_axis >= 0.5 + 0.05 + 0.075 + 0.15 - 0.002,
                "diagonal endpoint ({px:.4}, {py:.4}) violates oval pad silk clearance: d={dist_to_capsule_axis:.4}"
            );
        }
    }

    // Bottom-side B.SilkS stroke must clip cleanly against B.Mask rect [14, 4]..[16, 6] => [11, 13.775] & [16.225, 19].
    let b_segs = gerber_stroke_segments_mm(&fab_clipped.join("B_Silkscreen.gbo"));
    let find_b_hseg = |x_lo: f64, x_hi: f64| {
        b_segs.iter().any(|&((x0, y0), (x1, y1))| {
            let (a, b) = if x0 <= x1 { (x0, x1) } else { (x1, x0) };
            (y0 - 5.0).abs() < 0.005
                && (y1 - 5.0).abs() < 0.005
                && (a - x_lo).abs() < 0.01
                && (b - x_hi).abs() < 0.01
        })
    };
    assert!(
        find_b_hseg(11.0, 13.775) && find_b_hseg(16.225, 19.0),
        "expected B.SilkS clipped against B.Mask graphic, got: {b_segs:?}"
    );

    // Filled silkscreen rect wrapping circular pad must survive as G36* region with %LPC*% cutout hole.
    let silk_txt = fs::read_to_string(fab_clipped.join("F_Silkscreen.gto")).unwrap();
    assert!(
        silk_txt.contains("G36*") && silk_txt.contains("%LPC*%"),
        "expected clipped filled silkscreen region with cutout hole"
    );
    let last_lpc = silk_txt.rfind("%LPC*%").unwrap();
    let last_lpd = silk_txt.rfind("%LPD*%").unwrap();
    assert!(
        last_lpd > last_lpc,
        "strokes must be emitted after filled silkscreen %LPC*% holes"
    );

    // Regression: `silk_clearance_nm == 0` must still subtract Poly mask openings from Poly filled silkscreen.
    let mut zero_clr_spec = spec.clone();
    zero_clr_spec.rules.silk_clearance_nm = 0;
    zero_clr_spec.graphics = vec![Graphic::Rect {
        layer: "F.SilkS".into(),
        min: Pt::new(mm(7.0), mm(7.0)),
        max: Pt::new(mm(13.0), mm(13.0)),
        width: 0,
        fill: true,
    }];
    let zero_clr_board = BoardIr::from_spec(&zero_clr_spec).unwrap();
    let fab_zero = tmp.join("fab_zero_clr");
    export_fab(&zero_clr_board, &fab_zero, &FabOptions::default()).unwrap();
    let zero_silk = fs::read_to_string(fab_zero.join("F_Silkscreen.gto")).unwrap();
    assert!(
        zero_silk.contains("G36*") && zero_silk.contains("%LPC*%"),
        "filled Poly silkscreen over Poly SMD pad must clip even when silk_clearance_nm == 0"
    );
    assert!(
        zero_silk.contains("X8950000Y8950000") && zero_silk.contains("X11050000Y11050000"),
        "expected exact 2.1x2.1 mm mask cutout hole coordinates in zero-clearance filled silkscreen"
    );

    let rep = boardc::verify::verify_fab_outputs(&board, &fab_clipped).unwrap();
    assert!(rep.passed, "clipped fab verify failed: {:?}", rep.errors);

    // CLI `--no-clip-silk` preserves full unclipped stroke (5, 10) -> (15, 10).
    let fab_raw = tmp.join("fab_raw");
    let res_raw = Command::new(env!("CARGO_BIN_EXE_boardc"))
        .args([
            "fab",
            board_path.to_str().unwrap(),
            "-o",
            fab_raw.to_str().unwrap(),
            "--no-clip-silk",
        ])
        .output()
        .unwrap();
    assert!(res_raw.status.success());
    let raw_segs = gerber_stroke_segments_mm(&fab_raw.join("F_Silkscreen.gto"));
    assert!(
        raw_segs.iter().any(|&((x0, y0), (x1, y1))| {
            (y0 - 10.0).abs() < 0.005
                && (y1 - 10.0).abs() < 0.005
                && (x0 - 5.0).abs() < 0.01
                && (x1 - 15.0).abs() < 0.01
        }),
        "expected unclipped (5,10)->(15,10) segment with --no-clip-silk, got: {raw_segs:?}"
    );

    // Passing both `--subtract-soldermask` and `--no-clip-silk` must fail with a CLI conflict error.
    let res_conflict = Command::new(env!("CARGO_BIN_EXE_boardc"))
        .args([
            "fab",
            board_path.to_str().unwrap(),
            "-o",
            fab_raw.to_str().unwrap(),
            "--subtract-soldermask",
            "--no-clip-silk",
        ])
        .output()
        .unwrap();
    assert!(!res_conflict.status.success());

    let _ = fs::remove_dir_all(&tmp);
}

#[test]
fn graphic_text_and_revision_render_across_gbrjob_gerbers_and_svg() {
    use boardc::ir::Graphic;

    let tmp = fresh_tmp_dir("graphic_text_rev");
    let spec = BoardSpec {
        name: "draco".into(),
        title: Some("Draco Rev A1".into()),
        revision: Some("A1".into()),
        size_nm: Some([mm(60.0), mm(50.0)]),
        graphics: vec![
            // Multi-line top silkscreen text partially crossed by an F.Mask opening.
            Graphic::Text {
                layer: "F.SilkS".into(),
                text: "DRACO\nREV A1".into(),
                pos: Pt::new(30_600_000, 32_000_000),
                height_nm: 1_200_000,
                stroke_nm: 150_000,
                rot_deg: 0.0,
            },
            Graphic::Rect {
                layer: "F.Mask".into(),
                min: Pt::new(29_500_000, 30_000_000),
                max: Pt::new(31_700_000, 34_000_000),
                width: 0,
                fill: true,
            },
            // Multi-line bottom silkscreen text: top line 'L' sits on left in bottom view (world x > 15).
            Graphic::Text {
                layer: "B.SilkS".into(),
                text: "L1\nAB".into(),
                pos: Pt::new(mm(15.0), mm(15.0)),
                height_nm: mm(1.2),
                stroke_nm: mm(0.15),
                rot_deg: 0.0,
            },
            // Non-silk copper and fab Graphic::Text.
            Graphic::Text {
                layer: "F.Cu".into(),
                text: "CU".into(),
                pos: Pt::new(mm(45.0), mm(10.0)),
                height_nm: mm(1.0),
                stroke_nm: mm(0.15),
                rot_deg: 90.0,
            },
            Graphic::Text {
                layer: "F.Fab".into(),
                text: "FAB".into(),
                pos: Pt::new(mm(10.0), mm(40.0)),
                height_nm: mm(1.0),
                stroke_nm: mm(0.12),
                rot_deg: 0.0,
            },
        ],
        rules: DesignRules {
            silk_clearance_nm: mm(0.1),
            ..Default::default()
        },
        ..Default::default()
    };

    let board = BoardIr::from_spec(&spec).unwrap();
    let fab_dir = tmp.join("fab");
    let manifest = export_fab(
        &board,
        &fab_dir,
        &FabOptions {
            clip_silk_to_mask: true,
            assembly: true,
            ..Default::default()
        },
    )
    .unwrap();

    // gbrjob emits custom Revision "A1" and passes verification; tampering Revision fails.
    let job_orig = fs::read_to_string(&manifest.gbrjob).unwrap();
    let job_json: serde_json::Value = serde_json::from_str(&job_orig).unwrap();
    assert_eq!(job_json["GeneralSpecs"]["ProjectId"]["Revision"], "A1");
    let rep = boardc::verify::verify_fab_outputs(&board, &fab_dir).unwrap();
    assert!(rep.passed, "verify failed: {:?}", rep.errors);

    let mut bad_job = job_json.clone();
    bad_job["GeneralSpecs"]["ProjectId"]["Revision"] = serde_json::json!("99");
    fs::write(&manifest.gbrjob, serde_json::to_string(&bad_job).unwrap()).unwrap();
    assert!(boardc::verify::verify_fab_outputs(&board, &fab_dir).is_err());
    fs::write(&manifest.gbrjob, &job_orig).unwrap();

    // Board without revision falls back to "1" in gbrjob.
    let default_board = BoardIr::from_spec(&BoardSpec {
        name: "def_rev".into(),
        revision: Some("   ".into()),
        size_nm: Some([mm(20.0), mm(20.0)]),
        ..Default::default()
    })
    .unwrap();
    let fab_def = tmp.join("fab_def");
    let man_def = export_fab(&default_board, &fab_def, &FabOptions::default()).unwrap();
    let def_job: serde_json::Value =
        serde_json::from_str(&fs::read_to_string(&man_def.gbrjob).unwrap()).unwrap();
    assert_eq!(def_job["GeneralSpecs"]["ProjectId"]["Revision"], "1");

    // Top silkscreen Graphic::Text emits strokes and gets clipped against F.Mask [29.5, 31.7].
    let f_silk_segs = gerber_stroke_segments_mm(&fab_dir.join("F_Silkscreen.gto"));
    assert!(
        !f_silk_segs.is_empty(),
        "expected strokes on F_Silkscreen.gto for Graphic::Text"
    );
    assert!(
        f_silk_segs.iter().all(|&((x0, y0), (x1, y1))| {
            let inside = |x: f64, y: f64| x > 29.35 && x < 31.85 && y > 29.85 && y < 34.15;
            !(inside(x0, y0) && inside(x1, y1))
        }),
        "expected F.SilkS Graphic::Text strokes inside F.Mask opening to be clipped: {f_silk_segs:?}"
    );

    // Bottom silkscreen Graphic::Text is mirrored horizontally (x > 15 on left char, y > 15 on top line).
    let b_silk_segs = gerber_stroke_segments_mm(&fab_dir.join("B_Silkscreen.gbo"));
    assert!(!b_silk_segs.is_empty());
    assert!(
        b_silk_segs
            .iter()
            .any(|&((x0, y0), (_, y1))| y0 > 15.1 && y1 > 15.1 && x0 > 15.1),
        "expected top line 'L' mirrored to x > 15 in world coords: {b_silk_segs:?}"
    );
    assert!(
        b_silk_segs
            .iter()
            .any(|&((_, y0), (_, y1))| y0 < 14.9 && y1 < 14.9),
        "expected second line 'AB' below center (y < 15): {b_silk_segs:?}"
    );

    // Copper layer Graphic::Text emits strokes on F_Cu.gtl.
    let f_cu_segs = gerber_stroke_segments_mm(&fab_dir.join("F_Cu.gtl"));
    assert!(
        f_cu_segs
            .iter()
            .any(|&((x0, y0), (x1, y1))| x0 > 43.0 && x1 < 47.0 && y0 > 8.0 && y1 < 12.0),
        "expected copper Graphic::Text strokes near (45, 10) on F_Cu.gtl, got: {f_cu_segs:?}"
    );

    // Top, Bottom, and Assembly SVGs render Graphic::Text strokes.
    let top_svg = render_board_svg(&board, None, &SvgOptions::default());
    assert!(
        top_svg
            .split("id=\"silkscreen\"")
            .nth(1)
            .unwrap()
            .contains("<line "),
        "expected Top SVG silkscreen lines for Graphic::Text"
    );
    let bot_svg = render_board_svg(
        &board,
        None,
        &SvgOptions {
            view: SvgView::Bottom,
            ..Default::default()
        },
    );
    assert!(
        bot_svg
            .split("id=\"silkscreen\"")
            .nth(1)
            .unwrap()
            .contains("<line "),
        "expected Bottom SVG silkscreen lines for Graphic::Text"
    );
    let assy_top = fs::read_to_string(&manifest.assembly_svgs[0]).unwrap();
    let fab_group = assy_top.split("id=\"fab\"").nth(1).unwrap();
    assert!(
        fab_group.contains("<line "),
        "expected Assembly Top SVG fab group to render F.Fab Graphic::Text"
    );

    let _ = fs::remove_dir_all(&tmp);
}
