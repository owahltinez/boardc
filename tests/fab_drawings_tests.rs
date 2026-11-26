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

    // Uppercase `.SVG` extension works with `--view`; `--view` + `--layer` conflict fails.
    let upper_svg = tmp.join("UPPER.SVG");
    let upper_res = Command::new(env!("CARGO_BIN_EXE_boardc"))
        .args([
            "svg",
            board_path.to_str().unwrap(),
            "-o",
            upper_svg.to_str().unwrap(),
            "--view",
            "assembly-top",
        ])
        .output()
        .unwrap();
    assert!(upper_res.status.success());
    assert!(
        fs::read_to_string(&upper_svg)
            .unwrap()
            .contains("id=\"refdes\"")
    );

    let conflict_res = Command::new(env!("CARGO_BIN_EXE_boardc"))
        .args([
            "svg",
            board_path.to_str().unwrap(),
            "-o",
            tmp.join("conflict.svg").to_str().unwrap(),
            "--layer",
            "F.Cu",
            "--view",
            "assembly-top",
        ])
        .output()
        .unwrap();
    assert!(!conflict_res.status.success());

    // Singular "1 assembly SVG" in text stdout for a 1-sided board.
    let one_path = tmp.join("one.json");
    boardc::ir::save_board_json(&make_one_sided_smd_board(), &one_path).unwrap();
    let one_res = Command::new(env!("CARGO_BIN_EXE_boardc"))
        .args([
            "fab",
            one_path.to_str().unwrap(),
            "-o",
            tmp.join("one_fab").to_str().unwrap(),
            "--assembly",
        ])
        .output()
        .unwrap();
    assert!(one_res.status.success());
    let stdout = String::from_utf8_lossy(&one_res.stdout);
    assert!(
        stdout.contains("1 assembly SVG") && !stdout.contains("1 assembly SVGs"),
        "expected singular '1 assembly SVG', got: {stdout}"
    );

    // FabOptions deserializes missing fields using Default.
    let parsed_opts: FabOptions = serde_json::from_str("{}").unwrap();
    assert_eq!(parsed_opts, FabOptions::default());

    let _ = fs::remove_dir_all(&tmp);
}

fn parse_attr_f64(line: &str, attr: &str) -> f64 {
    let needle = format!("{attr}=\"");
    let start = line.find(&needle).unwrap() + needle.len();
    let rest = &line[start..];
    let end = rest.find('"').unwrap();
    rest[..end].parse().unwrap()
}

fn parse_viewbox(svg: &str) -> [f64; 4] {
    let start = svg.find("viewBox=\"").unwrap() + "viewBox=\"".len();
    let end = svg[start..].find('"').unwrap() + start;
    let nums: Vec<f64> = svg[start..end]
        .split_whitespace()
        .map(|s| s.parse().unwrap())
        .collect();
    [nums[0], nums[1], nums[2], nums[3]]
}

#[test]
fn roast_regressions_tools_bounds_asymmetric_mirror_and_edge_cases() {
    let tmp = fresh_tmp_dir("roast");

    // 1. 14 distinct drill tools (>12), sub-50nm diameter coalescing, buried long-span layout,
    //    and exact .drl <-> .rpt <-> -drl_map.svg tool table matching.
    let mut layers = vec![layer("F.Cu", LayerKind::Signal, Some(Side::Top))];
    for i in 1..=10 {
        layers.push(layer(&format!("In{i}.Cu"), LayerKind::Signal, None));
    }
    layers.push(layer("B.Cu", LayerKind::Signal, Some(Side::Bottom)));

    let mut vias = Vec::new();
    for i in 0..13 {
        vias.push(Via {
            net: format!("N{i}"),
            pos: Pt::new(mm(2.0 + i as f64 * 1.5), mm(5.0)),
            diameter: mm(0.45 + i as f64 * 0.05),
            drill: mm(0.20 + i as f64 * 0.05),
            layers: ("F.Cu".into(), "B.Cu".into()),
            locked: true,
            tented: true,
        });
    }
    // Differs by 20 nm from tool #3 (0.3000 mm = 300_000 nm); must coalesce into T03C0.3000.
    vias.push(Via {
        net: "N2B".into(),
        pos: Pt::new(mm(5.0), mm(8.0)),
        diameter: mm(0.55),
        drill: 300_020,
        layers: ("F.Cu".into(), "B.Cu".into()),
        locked: true,
        tented: true,
    });
    // 14th distinct tool on long buried span In1.Cu..In10.Cu.
    vias.push(Via {
        net: "BUR".into(),
        pos: Pt::new(mm(8.0), mm(8.0)),
        diameter: mm(0.35),
        drill: mm(0.15),
        layers: ("In1.Cu".into(), "In10.Cu".into()),
        locked: true,
        tented: true,
    });

    let long_name = "long_board_name_exceeding_default_legend_width_1234567890";
    let multi_board = BoardIr::from_spec(&BoardSpec {
        name: long_name.into(),
        size_nm: Some([mm(24.0), mm(12.0)]),
        layers,
        vias,
        ..Default::default()
    })
    .unwrap();

    let fab_multi = tmp.join("fab_multi");
    let manifest = export_fab(
        &multi_board,
        &fab_multi,
        &FabOptions {
            drill_map: true,
            drill_report: true,
            ..Default::default()
        },
    )
    .unwrap();

    // Check emitted PTH .drl has exactly 13 tools (300_020 coalesced into T03C0.3000).
    let pth_drl = fs::read_to_string(fab_multi.join(format!("{long_name}-PTH.drl"))).unwrap();
    let drl_tools: Vec<&str> = pth_drl
        .lines()
        .filter(|l| l.starts_with('T') && l.contains('C'))
        .collect();
    assert_eq!(
        drl_tools.len(),
        13,
        "expected 13 coalesced PTH tools: {drl_tools:?}"
    );
    assert_eq!(drl_tools[2], "T03C0.3000");

    let rpt = fs::read_to_string(manifest.drill_report.as_ref().unwrap()).unwrap();
    assert!(rpt.contains("T03  0.3000mm  0.0118\"  (2 holes)"));
    assert!(rpt.contains("T13  0.8000mm  0.0315\"  (1 hole)"));
    assert!(rpt.contains("In1.Cu to In10.Cu"));

    let map_svg = fs::read_to_string(manifest.drill_map_svg.as_ref().unwrap()).unwrap();
    let [vx, _, vw, _] = parse_viewbox(&map_svg);
    // Tool #13 (sym_idx 12, T13) and tool #1 (sym_idx 0, T01) share shape 0 but must differ in color.
    let t01_line = map_svg
        .lines()
        .find(|l| l.contains("data-tool=\"T01\" data-type=\"PTH\""))
        .unwrap();
    let t13_line = map_svg
        .lines()
        .find(|l| l.contains("data-tool=\"T13\" data-type=\"PTH\""))
        .unwrap();
    assert_ne!(
        t01_line
            .split("stroke=\"")
            .nth(1)
            .unwrap()
            .split('"')
            .next(),
        t13_line
            .split("stroke=\"")
            .nth(1)
            .unwrap()
            .split('"')
            .next(),
        "tool #13 must not collide with tool #1 color"
    );
    // Buried span "PTH (In1.Cu..In10.Cu)" Type column must not overlap Qty column, and title fits viewBox.
    let legend = map_svg.split("id=\"drill_legend\"").nth(1).unwrap();
    let type_hdr = legend.lines().find(|l| l.contains(">Type<")).unwrap();
    let qty_hdr = legend.lines().find(|l| l.contains(">Qty<")).unwrap();
    let legend_fs = parse_attr_f64(
        map_svg
            .lines()
            .find(|l| l.contains("id=\"drill_legend\""))
            .unwrap(),
        "font-size",
    );
    let type_x = parse_attr_f64(type_hdr, "x");
    let qty_x = parse_attr_f64(qty_hdr, "x");
    let buried_text_w = "PTH (In1.Cu..In10.Cu)".len() as f64 * legend_fs * 0.62;
    assert!(
        type_x + buried_text_w < qty_x,
        "Type column ({type_x} + {buried_text_w}) overlaps Qty ({qty_x})"
    );
    let title_line = legend.lines().find(|l| l.contains("DRILL MAP:")).unwrap();
    let title_x = parse_attr_f64(title_line, "x");
    let title_fs = parse_attr_f64(title_line, "font-size");
    assert!(
        title_x + (11 + long_name.len()) as f64 * title_fs * 0.62 <= vx + vw,
        "drill map title overflows viewBox"
    );

    // 2. Asymmetric bottom placement, bottom DNP, non-vertical portrait RefDes, vertical RefDes,
    //    F.CrtYd-only graphic, pad-only DNP bbox, invalid circle radius<=0, SMD drill ignored,
    //    and negative/NaN margin_mm safety.
    let mut footprints = BTreeMap::new();
    footprints.insert(
        "HDR_2".into(),
        FootprintSpec::PinHeader {
            pins: 2,
            pitch_nm: mm(2.54),
        },
    );
    footprints.insert(
        "PORTRAIT_13".into(),
        FootprintSpec::Custom {
            pads: vec![PadDef {
                name: "1".into(),
                kind: PadKind::Smd,
                shape: PadShape::Rect {
                    width: mm(1.0),
                    height: mm(1.0),
                },
                pos: Pt::new(0, 0),
                rot_deg: 0.0,
                drill_nm: Some(mm(0.5)), // Bogus drill on SMD pad must be ignored in assembly.
                layers: vec!["F.Cu".into()],
                mask_margin_nm: None,
                paste_margin_nm: None,
            }],
            courtyard: Vec::new(),
            graphics: vec![
                Graphic::Rect {
                    layer: "F.CrtYd".into(),
                    min: Pt::new(mm(-1.0), mm(-1.375)),
                    max: Pt::new(mm(1.0), mm(1.375)),
                    width: mm(0.05),
                    fill: false,
                },
                Graphic::Circle {
                    layer: "F.Fab".into(),
                    center: Pt::new(0, 0),
                    radius: -mm(0.1),
                    width: mm(0.1),
                    fill: false,
                },
            ],
            keepouts: Vec::new(),
        },
    );
    footprints.insert(
        "PAD_ONLY_3MM".into(),
        FootprintSpec::Custom {
            pads: vec![PadDef {
                name: "1".into(),
                kind: PadKind::Npth,
                shape: PadShape::Circle { diameter: mm(3.0) },
                pos: Pt::new(0, 0),
                rot_deg: 0.0,
                drill_nm: Some(mm(3.0)),
                layers: vec!["*.Mask".into()],
                mask_margin_nm: None,
                paste_margin_nm: None,
            }],
            courtyard: Vec::new(),
            graphics: Vec::new(),
            keepouts: Vec::new(),
        },
    );

    let edge_board = BoardIr::from_spec(&BoardSpec {
        name: "tiny_board_with_long_name".into(),
        size_nm: Some([mm(18.0), mm(14.0)]),
        footprints,
        placements: vec![
            // Asymmetric HDR_2 on Bottom at (10, 8): pin1=(10,8), pin2=(10, 8+2.54=10.54) because ly'=-ly.
            // Local courtyard center=(0, -1.27) -> board (10, 9.27) -> mirrored SVG (-10.0, -9.27).
            Placement {
                refdes: "J_BOT".into(),
                footprint: "HDR_2".into(),
                pos: Pt::new(mm(10.0), mm(8.0)),
                side: Side::Bottom,
                dnp: true,
                locked: true,
                ..Default::default()
            },
            // Portrait 2.0 x 2.75 mm (sh/sw = 1.375 <= 1.4): horizontal R101 must fit within 2.0 mm.
            Placement {
                refdes: "R101".into(),
                footprint: "PORTRAIT_13".into(),
                pos: Pt::new(mm(5.0), mm(7.0)),
                side: Side::Top,
                locked: true,
                ..Default::default()
            },
            // Pad-only 3mm NPTH marked DNP: X crossout must span 3mm, not 0.8mm.
            Placement {
                refdes: "MH1".into(),
                footprint: "PAD_ONLY_3MM".into(),
                pos: Pt::new(mm(14.0), mm(4.0)),
                side: Side::Top,
                dnp: true,
                locked: true,
                ..Default::default()
            },
        ],
        ..Default::default()
    })
    .unwrap();

    let top_svg = render_board_svg(
        &edge_board,
        None,
        &SvgOptions {
            view: SvgView::AssemblyTop,
            margin_mm: f64::NAN,
            ..Default::default()
        },
    );
    assert!(!top_svg.contains("NaN"), "NaN margin leaked into SVG");
    assert!(!top_svg.contains("r=\"-"), "negative circle radius emitted");
    // SMD pad at (5, 7) has bogus drill_nm=0.5mm; pads group must not draw a r=0.2500 drill circle.
    let pads_grp = top_svg
        .split("id=\"pads\"")
        .nth(1)
        .unwrap()
        .split("</g>")
        .next()
        .unwrap();
    assert!(
        !pads_grp.contains("r=\"0.2500\""),
        "SMD pad drill must not render"
    );
    // Portrait R101 stays horizontal and fits inside 2.0 mm width.
    let r101_line = top_svg.lines().find(|l| l.contains(">R101<")).unwrap();
    assert!(
        !r101_line.contains("rotate(-90"),
        "1.375 aspect should stay horizontal"
    );
    let r101_fs = parse_attr_f64(r101_line, "font-size");
    assert!(
        r101_fs * 4.0 * 0.62 <= 2.0,
        "R101 font-size {r101_fs} overflows 2.0 mm width"
    );
    // MH1 pad-only DNP crossout spans 3.0 mm (12.5..15.5 in X).
    let dnp_grp = top_svg
        .split("id=\"dnp\"")
        .nth(1)
        .unwrap()
        .split("</g>")
        .next()
        .unwrap();
    assert!(
        dnp_grp.contains("x1=\"12.5000\"") && dnp_grp.contains("x2=\"15.5000\""),
        "expected 3mm pad-only DNP crossout, got:\n{dnp_grp}"
    );

    // Bottom mirrored view: asymmetric J_BOT refdes sits at (-10.0000, -9.2700), rotated -90,
    // has bottom DNP crossout, and long title fits inside viewBox even with negative margin_mm.
    let bot_svg = render_board_svg(
        &edge_board,
        None,
        &SvgOptions {
            view: SvgView::AssemblyBottom,
            margin_mm: -5.0,
            ..Default::default()
        },
    );
    let [bvx, _, bvw, _] = parse_viewbox(&bot_svg);
    assert!(bvw >= 18.0, "negative margin cropped board width: {bvw}");
    let bot_title = bot_svg
        .lines()
        .find(|l| l.contains("ASSEMBLY BOTTOM (MIRRORED)") && l.contains("<text"))
        .unwrap();
    let bt_x = parse_attr_f64(bot_title, "x");
    let bt_fs = parse_attr_f64(bot_title, "font-size");
    let bt_chars = "tiny_board_with_long_name - ASSEMBLY BOTTOM (MIRRORED)".len() as f64;
    assert!(
        bt_x + bt_chars * bt_fs * 0.62 <= bvx + bvw + 0.01,
        "bottom assembly title overflows viewBox"
    );
    let jbot_line = bot_svg.lines().find(|l| l.contains(">J_BOT<")).unwrap();
    assert!(
        jbot_line.contains("x=\"-10.0000\" y=\"-9.2700\"")
            && jbot_line.contains("rotate(-90 -10.0000 -9.2700)"),
        "expected mirrored vertical J_BOT refdes, got: {jbot_line}"
    );
    assert!(
        bot_svg.contains("id=\"dnp\""),
        "expected bottom DNP crossout"
    );

    // 3. 1-layer B.Cu board with side: None reports 'back' and singular '1 hole'.
    let bcu_only = BoardIr::from_spec(&BoardSpec {
        name: "single_bot".into(),
        size_nm: Some([mm(20.0), mm(15.0)]),
        layers: vec![layer("B.Cu", LayerKind::Signal, None)],
        vias: vec![Via {
            net: "GND".into(),
            pos: Pt::new(mm(5.0), mm(5.0)),
            diameter: mm(0.6),
            drill: mm(0.3),
            layers: ("B.Cu".into(), "B.Cu".into()),
            locked: true,
            tented: true,
        }],
        graphics: vec![Graphic::Line {
            layer: "B.Fab".into(),
            a: Pt::new(mm(1.0), mm(1.0)),
            b: Pt::new(mm(4.0), mm(1.0)),
            width: mm(0.1),
        }],
        ..Default::default()
    })
    .unwrap();
    let bcu_rpt = render_drill_report(&bcu_only);
    assert!(
        bcu_rpt.contains("B.Cu") && bcu_rpt.contains("back") && !bcu_rpt.contains("front"),
        "1-layer B.Cu with side=None should report 'back':\n{bcu_rpt}"
    );
    let bcu_map = render_board_svg(
        &bcu_only,
        None,
        &SvgOptions {
            view: SvgView::DrillMap,
            ..Default::default()
        },
    );
    assert!(bcu_map.contains("Total: 1 hole (1 plated, 0 unplated)"));
    // Board with B.Fab graphic only (no bottom placements) still triggers bottom assembly emission.
    let fab_bcu = tmp.join("fab_bcu");
    let m_bcu = export_fab(
        &bcu_only,
        &fab_bcu,
        &FabOptions {
            assembly: true,
            ..Default::default()
        },
    )
    .unwrap();
    assert_eq!(m_bcu.assembly_svgs.len(), 2);

    let _ = fs::remove_dir_all(&tmp);
}
