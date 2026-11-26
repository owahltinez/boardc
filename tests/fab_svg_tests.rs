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
        },
    );
    footprints.insert(
        "U_QFN16".into(),
        FootprintSpec::Qfn {
            pins_per_side: 4,
            pitch_nm: mm(0.5),
            body_nm: mm(3.0),
            ep_nm: mm(1.4),
        },
    );
    footprints.insert(
        "HDR_2".into(),
        FootprintSpec::PinHeader {
            pins: 2,
            pitch_nm: mm(2.54),
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
                cpl_rotation_offset_deg: 0.0,
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
                cpl_rotation_offset_deg: 0.0,
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
                cpl_rotation_offset_deg: 90.0,
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
                cpl_rotation_offset_deg: 0.0,
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
                cpl_rotation_offset_deg: 0.0,
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
                cpl_rotation_offset_deg: 0.0,
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
