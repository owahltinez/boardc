use std::collections::BTreeMap;
use std::fs;
use std::path::PathBuf;
use std::process::Command;

use boardc::fab::{FabOptions, export_fab, render_drill_report};
use boardc::ir::{
    BoardIr, BoardSpec, FootprintSpec, Graphic, LayerDef, LayerKind, PadDef, PadKind, PadShape,
    Placement, Pt, Side, Via, mm,
};
use boardc::svg::{SvgOptions, SvgView, render_board_svg};
use boardc::verify::verify_fab_outputs;

fn fresh_tmp_dir(name: &str) -> PathBuf {
    let dir =
        std::env::temp_dir().join(format!("boardc_fab_drawings_{name}_{}", std::process::id()));
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

fn make_two_sided_4layer_board() -> BoardIr {
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
            graphics: vec![Graphic::Circle {
                layer: "F.Fab".into(),
                center: Pt::new(0, 0),
                radius: mm(1.5),
                width: mm(0.1),
                fill: false,
            }],
            keepouts: Vec::new(),
        },
    );

    let spec = BoardSpec {
        name: "assy_4l".into(),
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
                value: "MCU".into(),
                pos: Pt::new(mm(15.0), mm(15.0)),
                side: Side::Top,
                locked: true,
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
                ..Default::default()
            },
            Placement {
                refdes: "R_DNP".into(),
                footprint: "C_0402".into(),
                value: "0R".into(),
                pos: Pt::new(mm(25.0), mm(22.0)),
                side: Side::Top,
                dnp: true,
                locked: true,
                ..Default::default()
            },
            Placement {
                refdes: "J1".into(),
                footprint: "HDR_2".into(),
                value: "CONN_2".into(),
                pos: Pt::new(mm(5.0), mm(22.0)),
                side: Side::Top,
                locked: true,
                ..Default::default()
            },
            Placement {
                refdes: "H1".into(),
                footprint: "NPTH_3MM".into(),
                value: "M3".into(),
                pos: Pt::new(mm(3.0), mm(3.0)),
                side: Side::Top,
                dnp: true,
                locked: true,
                ..Default::default()
            },
        ],
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
                drill: mm(0.30),
                layers: ("F.Cu".into(), "B.Cu".into()),
                locked: true,
                tented: false,
            },
            Via {
                net: "SIG".into(),
                pos: Pt::new(mm(12.0), mm(10.0)),
                diameter: mm(0.4),
                drill: mm(0.15),
                layers: ("F.Cu".into(), "In1.Cu".into()),
                locked: true,
                tented: true,
            },
        ],
        graphics: vec![
            Graphic::Line {
                layer: "F.Fab".into(),
                a: Pt::new(mm(1.0), mm(1.0)),
                b: Pt::new(mm(5.0), mm(1.0)),
                width: mm(0.1),
            },
            Graphic::Rect {
                layer: "B.CrtYd".into(),
                min: Pt::new(mm(28.0), mm(2.0)),
                max: Pt::new(mm(32.0), mm(5.0)),
                width: mm(0.05),
                fill: false,
            },
        ],
        ..Default::default()
    };
    BoardIr::from_spec(&spec).unwrap()
}

fn make_one_sided_smd_board() -> BoardIr {
    let mut footprints = BTreeMap::new();
    footprints.insert(
        "C_0402".into(),
        FootprintSpec::Chip2Pin {
            package: "0402".into(),
        },
    );
    let spec = BoardSpec {
        name: "top_only & <v1>".into(),
        size_nm: Some([mm(20.0), mm(15.0)]),
        footprints,
        placements: vec![Placement {
            refdes: "R1".into(),
            footprint: "C_0402".into(),
            value: "10k".into(),
            pos: Pt::new(mm(10.0), mm(7.5)),
            side: Side::Top,
            locked: true,
            ..Default::default()
        }],
        ..Default::default()
    };
    BoardIr::from_spec(&spec).unwrap()
}

#[test]
fn assembly_drawings_top_and_mirrored_bottom_and_one_sided_behavior() {
    let tmp = fresh_tmp_dir("assembly");
    let two_sided = make_two_sided_4layer_board();

    let fab_two = tmp.join("fab_two");
    let manifest = export_fab(
        &two_sided,
        &fab_two,
        &FabOptions {
            assembly: true,
            ..Default::default()
        },
    )
    .unwrap();
    assert_eq!(manifest.assembly_svgs.len(), 2);
    assert!(manifest.assembly_svgs[0].ends_with("assy_4l-assembly-top.svg"));
    assert!(manifest.assembly_svgs[1].ends_with("assy_4l-assembly-bottom.svg"));
    assert!(manifest.all_files().contains(&manifest.assembly_svgs[0]));
    assert!(manifest.all_files().contains(&manifest.assembly_svgs[1]));

    let top_svg = fs::read_to_string(&manifest.assembly_svgs[0]).unwrap();
    assert!(top_svg.starts_with("<svg ") && top_svg.trim_end().ends_with("</svg>"));
    assert!(top_svg.contains("id=\"board_outline\""));
    assert!(top_svg.contains("id=\"courtyard\""));
    assert!(top_svg.contains("id=\"fab\""));
    assert!(top_svg.contains("id=\"refdes\""));
    assert!(top_svg.contains("id=\"dnp\""));
    let refdes_section = top_svg.split("id=\"refdes\"").nth(1).unwrap();
    assert!(refdes_section.contains(">U1<"));
    assert!(refdes_section.contains(">C1<"));
    assert!(refdes_section.contains(">J1<"));
    assert!(refdes_section.contains(">R_DNP<"));
    assert!(!refdes_section.contains(">C2<"));
    // J1 header pins sit at y=22.0 and y=19.46, so its centered label sits at y=-20.73 in SVG.
    assert!(
        refdes_section.contains("y=\"-20.7300\""),
        "expected J1 refdes centered on header body, got:\n{refdes_section}"
    );

    let bot_svg = fs::read_to_string(&manifest.assembly_svgs[1]).unwrap();
    assert!(bot_svg.starts_with("<svg ") && bot_svg.trim_end().ends_with("</svg>"));
    assert!(bot_svg.contains("id=\"board_outline\""));
    assert!(bot_svg.contains("id=\"courtyard\""));
    assert!(bot_svg.contains("id=\"fab\""));
    let bot_refdes = bot_svg.split("id=\"refdes\"").nth(1).unwrap();
    assert!(bot_refdes.contains(">C2<"));
    assert!(!bot_refdes.contains(">U1<"));
    assert!(!bot_refdes.contains(">C1<"));
    // Bottom assembly is X-mirrored: C2 at board (25, 15) appears at SVG (-25.0000, -15.0000).
    assert!(
        bot_refdes.contains("x=\"-25.0000\" y=\"-15.0000\""),
        "expected mirrored C2 refdes at (-25, -15), got:\n{bot_refdes}"
    );
    assert!(
        bot_svg.contains("viewBox=\"-42.0000"),
        "expected negative X viewBox on mirrored bottom assembly"
    );

    // 1-sided board (no bottom placements or B.Fab/B.CrtYd graphics) emits only top assembly.
    let one_sided = make_one_sided_smd_board();
    let fab_one = tmp.join("fab_one");
    let manifest_one = export_fab(
        &one_sided,
        &fab_one,
        &FabOptions {
            assembly: true,
            ..Default::default()
        },
    )
    .unwrap();
    assert_eq!(manifest_one.assembly_svgs.len(), 1);
    assert!(
        manifest_one.assembly_svgs[0]
            .to_string_lossy()
            .ends_with("-assembly-top.svg")
    );
    let one_top = fs::read_to_string(&manifest_one.assembly_svgs[0]).unwrap();
    assert!(one_top.contains("top_only &amp; &lt;v1&gt;"));
    assert!(one_top.contains(">R1<"));

    // Direct SvgView::AssemblyBottom still renders cleanly for a 1-sided board.
    let empty_bot = render_board_svg(
        &one_sided,
        None,
        &SvgOptions {
            view: SvgView::AssemblyBottom,
            ..Default::default()
        },
    );
    assert!(empty_bot.starts_with("<svg ") && empty_bot.contains("id=\"board_outline\""));

    let _ = fs::remove_dir_all(&tmp);
}

#[test]
fn drill_map_and_drill_report_match_excellon_tools_and_handle_empty_drills() {
    let tmp = fresh_tmp_dir("drills");
    let board = make_two_sided_4layer_board();
    let fab_dir = tmp.join("fab");

    let manifest = export_fab(
        &board,
        &fab_dir,
        &FabOptions {
            drill_map: true,
            drill_report: true,
            ..Default::default()
        },
    )
    .unwrap();

    let map_path = manifest
        .drill_map_svg
        .as_ref()
        .expect("expected drill_map_svg");
    let rpt_path = manifest
        .drill_report
        .as_ref()
        .expect("expected drill_report");
    assert!(map_path.ends_with("assy_4l-drl_map.svg"));
    assert!(rpt_path.ends_with("assy_4l-drl.rpt"));
    assert!(manifest.all_files().contains(map_path));
    assert!(manifest.all_files().contains(rpt_path));

    // Drill report matches Excellon files (PTH through, NPTH, and blind span F.Cu..In1.Cu).
    let rpt = fs::read_to_string(rpt_path).unwrap();
    assert_eq!(rpt, render_drill_report(&board));
    assert!(rpt.contains("Copper Layer Stackup:"));
    assert!(rpt.contains("L1") && rpt.contains("F.Cu") && rpt.contains("front"));
    assert!(rpt.contains("L4") && rpt.contains("B.Cu") && rpt.contains("back"));
    assert!(rpt.contains("assy_4l-PTH.drl"));
    assert!(rpt.contains("T01  0.2500mm  0.0098\"  (1 hole)"));
    assert!(rpt.contains("T02  0.3000mm  0.0118\"  (1 hole)"));
    assert!(rpt.contains("T03  1.0160mm  0.0400\"  (2 holes)"));
    assert!(rpt.contains("Total plated holes count 4"));
    assert!(rpt.contains("assy_4l-NPTH.drl"));
    assert!(rpt.contains("T01  3.0000mm  0.1181\"  (1 hole)"));
    assert!(rpt.contains("Total unplated holes count 1"));
    assert!(rpt.contains("assy_4l-PTH-F_Cu-In1_Cu.drl"));
    assert!(rpt.contains("F.Cu to In1.Cu"));
    assert!(rpt.contains("T01  0.1500mm  0.0059\"  (1 hole)"));

    // Drill map SVG has board outline, per-tool symbol groups, and legend matching every tool.
    let map_svg = fs::read_to_string(map_path).unwrap();
    assert!(map_svg.starts_with("<svg ") && map_svg.trim_end().ends_with("</svg>"));
    assert!(map_svg.contains("id=\"board_outline\""));
    assert!(map_svg.contains("id=\"drill_hits\""));
    assert!(map_svg.contains("id=\"drill_legend\""));
    assert!(map_svg.contains("data-tool=\"T01\" data-type=\"PTH\" data-diameter-mm=\"0.2500\""));
    assert!(map_svg.contains("data-tool=\"T02\" data-type=\"PTH\" data-diameter-mm=\"0.3000\""));
    assert!(map_svg.contains("data-tool=\"T03\" data-type=\"PTH\" data-diameter-mm=\"1.0160\""));
    assert!(map_svg.contains("data-tool=\"T01\" data-type=\"NPTH\" data-diameter-mm=\"3.0000\""));
    assert!(map_svg.contains(
        "data-tool=\"T01\" data-type=\"PTH (F.Cu..In1.Cu)\" data-diameter-mm=\"0.1500\""
    ));
    assert!(map_svg.contains("Total: 6 holes (5 plated, 1 unplated)"));

    // Verify independent Gerber/Excellon verification still passes with extra drawings in fab/.
    let vrep = verify_fab_outputs(&board, &fab_dir).unwrap();
    assert!(vrep.passed, "verify failed: {:?}", vrep.errors);

    // Pure SMD board with 0 drill hits must render report and drill map cleanly.
    let smd_only = make_one_sided_smd_board();
    let smd_rpt = render_drill_report(&smd_only);
    assert!(smd_rpt.contains("Total plated holes count 0"));
    assert!(smd_rpt.contains("Total unplated holes count 0"));
    let smd_map = render_board_svg(
        &smd_only,
        None,
        &SvgOptions {
            view: SvgView::DrillMap,
            ..Default::default()
        },
    );
    assert!(smd_map.starts_with("<svg ") && smd_map.contains("No drill hits"));

    let _ = fs::remove_dir_all(&tmp);
}

#[test]
fn cli_fab_and_svg_emit_assembly_drill_map_and_drill_report() {
    let tmp = fresh_tmp_dir("cli");
    let board = make_two_sided_4layer_board();
    let board_path = tmp.join("board.json");
    boardc::ir::save_board_json(&board, &board_path).unwrap();

    // `boardc fab` with `--assembly --drill-map --drill-report --json`
    let fab_dir = tmp.join("fab");
    let fab_res = Command::new(env!("CARGO_BIN_EXE_boardc"))
        .args([
            "fab",
            board_path.to_str().unwrap(),
            "-o",
            fab_dir.to_str().unwrap(),
            "--assembly",
            "--drill-map",
            "--drill-report",
            "--json",
        ])
        .output()
        .unwrap();
    assert!(
        fab_res.status.success(),
        "boardc fab failed: {}",
        String::from_utf8_lossy(&fab_res.stderr)
    );
    let json: serde_json::Value = serde_json::from_slice(&fab_res.stdout).unwrap();
    assert_eq!(json["assembly_svgs"].as_array().unwrap().len(), 2);
    assert!(
        json["drill_map_svg"]
            .as_str()
            .unwrap()
            .ends_with("assy_4l-drl_map.svg")
    );
    assert!(
        json["drill_report"]
            .as_str()
            .unwrap()
            .ends_with("assy_4l-drl.rpt")
    );

    // Default `boardc fab` without those flags omits them from JSON and disk.
    let fab_default = tmp.join("fab_default");
    let def_res = Command::new(env!("CARGO_BIN_EXE_boardc"))
        .args([
            "fab",
            board_path.to_str().unwrap(),
            "-o",
            fab_default.to_str().unwrap(),
            "--json",
        ])
        .output()
        .unwrap();
    assert!(def_res.status.success());
    let def_json: serde_json::Value = serde_json::from_slice(&def_res.stdout).unwrap();
    assert!(def_json.get("assembly_svgs").is_none());
    assert!(def_json.get("drill_map_svg").is_none());
    assert!(def_json.get("drill_report").is_none());

    // `boardc svg` views: assembly-top, assembly-bottom, drill-map.
    for (view, file, needle) in [
        ("assembly-top", "a_top.svg", "id=\"refdes\""),
        ("assembly-bottom", "a_bot.svg", "x=\"-25.0000\""),
        ("drill-map", "d_map.svg", "id=\"drill_legend\""),
    ] {
        let out_svg = tmp.join(file);
        let res = Command::new(env!("CARGO_BIN_EXE_boardc"))
            .args([
                "svg",
                board_path.to_str().unwrap(),
                "-o",
                out_svg.to_str().unwrap(),
                "--view",
                view,
            ])
            .output()
            .unwrap();
        assert!(
            res.status.success(),
            "boardc svg --view {view} failed: {}",
            String::from_utf8_lossy(&res.stderr)
        );
        let text = fs::read_to_string(&out_svg).unwrap();
        assert!(text.contains(needle), "missing {needle} in {view}");
    }

    // Invalid `--view` fails cleanly.
    let bad_view = Command::new(env!("CARGO_BIN_EXE_boardc"))
        .args([
            "svg",
            board_path.to_str().unwrap(),
            "-o",
            tmp.join("bad.svg").to_str().unwrap(),
            "--view",
            "bogus",
        ])
        .output()
        .unwrap();
    assert!(!bad_view.status.success());

    // Passing `--view` with a directory path (not `.svg`) fails cleanly.
    let bad_dir = Command::new(env!("CARGO_BIN_EXE_boardc"))
        .args([
            "svg",
            board_path.to_str().unwrap(),
            "-o",
            tmp.join("dir_out").to_str().unwrap(),
            "--view",
            "assembly-top",
        ])
        .output()
        .unwrap();
    assert!(!bad_dir.status.success());

    let _ = fs::remove_dir_all(&tmp);
}
