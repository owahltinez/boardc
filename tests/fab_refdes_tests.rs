use std::fs;
use std::io::BufReader;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};

use boardc::fab::{FabOptions, export_fab};
use boardc::ir::{
    AutoRefdes, BoardIr, BoardSpec, DEFAULT_REFDES_STROKE_NM, FabConfig, MIN_REFDES_STROKE_NM,
    Side, load_board_json, load_spec_file, save_board_json,
};
use boardc::svg::{SvgOptions, SvgView, render_board_svg};

type PtMm = (f64, f64);
type SegMm = (PtMm, PtMm);

fn fresh_tmp_dir(name: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("boardc_fab_refdes_{name}_{}", std::process::id()));
    let _ = fs::remove_dir_all(&dir);
    fs::create_dir_all(&dir).unwrap();
    dir
}

fn run_boardc(args: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_boardc"))
        .args(args)
        .output()
        .unwrap()
}

fn parse_gerber_silk(path: &Path) -> (Vec<SegMm>, Vec<PtMm>) {
    use gerber_types::{Command as GCmd, DCode, FunctionCode, GCode, Operation};
    let file = fs::File::open(path).unwrap();
    let doc = gerber_parser::parse(BufReader::new(file)).unwrap();
    assert!(doc.errors().is_empty(), "{:?}", doc.errors());
    let mut cur = (0.0_f64, 0.0_f64);
    let mut in_region = false;
    let mut segs = Vec::new();
    let mut flashes = Vec::new();
    for cmd in doc.commands() {
        match cmd {
            GCmd::FunctionCode(FunctionCode::GCode(GCode::RegionMode(on))) => {
                in_region = *on;
            }
            GCmd::FunctionCode(FunctionCode::DCode(DCode::Operation(op))) => {
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
                } else if !in_region && matches!(op, Operation::Flash(_)) {
                    flashes.push(cur);
                }
            }
            _ => {}
        }
    }
    (segs, flashes)
}

fn has_seg(segs: &[SegMm], a: PtMm, b: PtMm) -> bool {
    let near = |p: PtMm, q: PtMm| (p.0 - q.0).abs() < 0.02 && (p.1 - q.1).abs() < 0.02;
    segs.iter()
        .any(|&(p, q)| (near(p, a) && near(q, b)) || (near(p, b) && near(q, a)))
}

fn parse_svg_silk_lines(svg: &str) -> Vec<(SegMm, String)> {
    let section = svg.split("<g id=\"silkscreen\">").nth(1).unwrap_or("");
    let section = section.split("</g>").next().unwrap_or("");
    let attr = |line: &str, key: &str| -> Option<String> {
        let pat = format!("{key}=\"");
        let start = line.find(&pat)? + pat.len();
        let end = start + line[start..].find('"')?;
        Some(line[start..end].to_string())
    };
    section
        .lines()
        .filter(|l| l.contains("<line "))
        .map(|l| {
            let x1: f64 = attr(l, "x1").unwrap().parse().unwrap();
            let y1: f64 = attr(l, "y1").unwrap().parse().unwrap();
            let x2: f64 = attr(l, "x2").unwrap().parse().unwrap();
            let y2: f64 = attr(l, "y2").unwrap().parse().unwrap();
            let sw = attr(l, "stroke-width").unwrap();
            (((x1, y1), (x2, y2)), sw)
        })
        .collect()
}

#[test]
fn fab_config_serde_roundtrip_and_validation() {
    let min_toml = "name = \"compat\"\nsize_nm = [20000000, 10000000]\n";
    let spec: BoardSpec = toml::from_str(min_toml).unwrap();
    assert_eq!(spec.fab, FabConfig::default());
    assert_eq!(spec.fab.auto_refdes, AutoRefdes::Rotate);
    assert_eq!(spec.fab.refdes_stroke_nm, DEFAULT_REFDES_STROKE_NM);
    assert!(!serde_json::to_string(&spec).unwrap().contains("\"fab\""));
    assert!(!toml::to_string(&spec).unwrap().contains("[fab]"));

    let board = BoardIr::from_spec(&spec).unwrap();
    let board_json = serde_json::to_string(&board).unwrap();
    assert!(!board_json.contains("\"fab\""));
    assert_eq!(
        serde_json::from_str::<BoardIr>(&board_json).unwrap().fab,
        FabConfig::default()
    );

    // Non-default FabConfig round-trips through TOML and JSON on BoardSpec and BoardIr.
    let custom_toml = format!(
        "{min_toml}[fab]\nauto_refdes = \"upright\"\nrefdes_stroke_nm = {MIN_REFDES_STROKE_NM}\n"
    );
    let custom_spec: BoardSpec = toml::from_str(&custom_toml).unwrap();
    assert_eq!(custom_spec.fab.auto_refdes, AutoRefdes::Upright);
    assert_eq!(custom_spec.fab.refdes_stroke_nm, MIN_REFDES_STROKE_NM);
    let round_toml: BoardSpec = toml::from_str(&toml::to_string(&custom_spec).unwrap()).unwrap();
    assert_eq!(round_toml.fab, custom_spec.fab);
    let custom_board = BoardIr::from_spec(&custom_spec).unwrap();
    let round_board: BoardIr =
        serde_json::from_str(&serde_json::to_string(&custom_board).unwrap()).unwrap();
    assert_eq!(round_board.fab, custom_spec.fab);

    // FabOptions JSON deserialization honors bool/string auto_refdes and refdes_stroke_nm.
    let opt_a: FabOptions =
        serde_json::from_str(r#"{"auto_refdes": false, "refdes_stroke_nm": 150000}"#).unwrap();
    assert_eq!(opt_a.auto_refdes, Some(AutoRefdes::Off));
    assert_eq!(opt_a.refdes_stroke_nm, Some(150_000));
    let opt_b: FabOptions = serde_json::from_str(r#"{"auto_refdes": "upright"}"#).unwrap();
    assert_eq!(opt_b.auto_refdes, Some(AutoRefdes::Upright));
    assert_eq!(opt_b.refdes_stroke_nm, None);
    assert_eq!(
        serde_json::from_str::<FabOptions>("{}").unwrap(),
        FabOptions::default()
    );

    for (val, expected) in [
        ("false", AutoRefdes::Off),
        ("true", AutoRefdes::Rotate),
        ("\"off\"", AutoRefdes::Off),
        ("\"rotate\"", AutoRefdes::Rotate),
        ("\"upright\"", AutoRefdes::Upright),
    ] {
        let s: BoardSpec =
            toml::from_str(&format!("{min_toml}[fab]\nauto_refdes = {val}\n")).unwrap();
        assert_eq!(s.fab.auto_refdes, expected, "TOML {val}");
        let sj: BoardSpec = serde_json::from_str(&format!(
            "{{\"name\":\"compat\",\"size_nm\":[20000000,10000000],\"fab\":{{\"auto_refdes\":{val}}}}}"
        ))
        .unwrap();
        assert_eq!(sj.fab.auto_refdes, expected, "JSON {val}");
    }

    for invalid in ["\"auto\"", "\"bogus\""] {
        assert!(
            toml::from_str::<BoardSpec>(&format!("{min_toml}[fab]\nauto_refdes = {invalid}\n"))
                .is_err(),
            "{invalid} must be rejected"
        );
    }

    // Validation rejects <= 0 and < MIN_REFDES_STROKE_NM (e.g. 150 um/nm typo), accepts 10_000 and 150_000.
    let tmp = fresh_tmp_dir("cfg_validate");
    for good in [MIN_REFDES_STROKE_NM, 150_000] {
        let good_spec: BoardSpec =
            toml::from_str(&format!("{min_toml}[fab]\nrefdes_stroke_nm = {good}\n")).unwrap();
        assert!(BoardIr::from_spec(&good_spec).is_ok(), "good={good}");
    }
    for bad_stroke in [0_i64, -1, 150] {
        let bad_spec: BoardSpec = toml::from_str(&format!(
            "{min_toml}[fab]\nrefdes_stroke_nm = {bad_stroke}\n"
        ))
        .unwrap();
        let err = BoardIr::from_spec(&bad_spec).unwrap_err();
        assert!(err.contains("10000"), "expected >= 10000 hint, got: {err}");

        let mut bad_board = board.clone();
        bad_board.fab.refdes_stroke_nm = bad_stroke;
        let bad_ir_path = tmp.join(format!("bad_{bad_stroke}.json"));
        save_board_json(&bad_board, &bad_ir_path).unwrap();
        assert!(load_board_json(&bad_ir_path).is_err());
        assert!(export_fab(&bad_board, &tmp.join("out"), &FabOptions::default()).is_err());
    }
    let _ = fs::remove_dir_all(&tmp);
}

fn map_local_mm(lx: f64, ly: f64, cx: f64, cy: f64, rot_deg: f64, side: Side) -> PtMm {
    let (s, c) = rot_deg.to_radians().sin_cos();
    let sy = if side == Side::Bottom { -ly } else { ly };
    (cx + lx * c - sy * s, cy + lx * s + sy * c)
}

#[test]
fn upright_all_cardinal_negative_noncardinal_and_multichar_svg() {
    let tmp = fresh_tmp_dir("upright_angles");
    let cases: &[(&str, Side, f64, f64, f64, bool)] = &[
        ("F", Side::Top, 10.0, 10.0, 0.0, false),
        ("F", Side::Top, 20.0, 10.0, 90.0, false),
        ("F", Side::Top, 30.0, 10.0, 180.0, true),
        ("F", Side::Top, 40.0, 10.0, 270.0, true),
        ("F", Side::Top, 50.0, 10.0, -90.0, true),
        ("F", Side::Top, 60.0, 10.0, 45.0, false),
        ("F", Side::Top, 70.0, 10.0, 135.0, true),
        ("F", Side::Bottom, 10.0, 25.0, 0.0, true),
        ("F", Side::Bottom, 20.0, 25.0, 90.0, false),
        ("F", Side::Bottom, 30.0, 25.0, 180.0, false),
        ("F", Side::Bottom, 40.0, 25.0, 270.0, true),
        ("F", Side::Bottom, 50.0, 25.0, -90.0, true),
        ("F", Side::Bottom, 60.0, 25.0, 45.0, true),
        ("F", Side::Bottom, 70.0, 25.0, 135.0, false),
    ];

    let make_board = |refdes: &str,
                      side: Side,
                      cx_mm: f64,
                      cy_mm: f64,
                      rot: f64,
                      mode: AutoRefdes,
                      stroke_nm: i64| {
        let side_str = if side == Side::Top { "top" } else { "bottom" };
        let mode_str = match mode {
            AutoRefdes::Rotate => "\"rotate\"",
            AutoRefdes::Upright => "\"upright\"",
            AutoRefdes::Off => "\"off\"",
        };
        let spec_toml = format!(
            r#"
name = "ang"
size_nm = [80000000, 40000000]
[fab]
auto_refdes = {mode_str}
refdes_stroke_nm = {stroke_nm}
[footprints.FP]
kind = "custom"
courtyard = [
  {{ x = -500000, y = -500000 }},
  {{ x = 500000, y = -500000 }},
  {{ x = 500000, y = 500000 }},
  {{ x = -500000, y = 500000 }},
]
[[footprints.FP.pads]]
name = "1"
kind = "smd"
shape = {{ shape = "rect", width = 400000, height = 400000 }}
pos = {{ x = 0, y = 0 }}
layers = ["F.Cu", "F.Mask"]
[[placements]]
refdes = "{refdes}"
footprint = "FP"
pos = {{ x = {}, y = {} }}
rot_deg = {rot}
side = "{side_str}"
"#,
            (cx_mm * 1_000_000.0).round() as i64,
            (cy_mm * 1_000_000.0).round() as i64,
        );
        BoardIr::from_spec(&toml::from_str::<BoardSpec>(&spec_toml).unwrap()).unwrap()
    };

    let hw = 0.70 * 0.65 * 0.5;
    let hh = 0.35;
    let mid_r = 0.70 * 0.65 * (3.0 / 4.0) - hw;
    for &(refdes, side, cx, cy, rot, flipped) in cases {
        let board = make_board(refdes, side, cx, cy, rot, AutoRefdes::Upright, 150_000);
        let out = tmp.join(format!("{side:?}_{rot}"));
        export_fab(&board, &out, &FabOptions::default()).unwrap();
        let silk_file = if side == Side::Top {
            "F_Silkscreen.gto"
        } else {
            "B_Silkscreen.gbo"
        };
        let (segs, _) = parse_gerber_silk(&out.join(silk_file));
        let wc = map_local_mm(0.0, 0.95, cx, cy, rot, side);
        let eff_rot = if flipped { rot + 180.0 } else { rot };
        let tl = map_local_mm(-hw, hh, wc.0, wc.1, eff_rot, side);
        let tr = map_local_mm(hw, hh, wc.0, wc.1, eff_rot, side);
        let bl = map_local_mm(-hw, -hh, wc.0, wc.1, eff_rot, side);
        let ml = map_local_mm(-hw, 0.0, wc.0, wc.1, eff_rot, side);
        let mr = map_local_mm(mid_r, 0.0, wc.0, wc.1, eff_rot, side);
        assert!(
            has_seg(&segs, tl, tr) && has_seg(&segs, tl, bl) && has_seg(&segs, ml, mr),
            "{side:?} rot={rot} flipped={flipped} expected 'F' strokes ({tl:?}->{tr:?}, {tl:?}->{bl:?}, {ml:?}->{mr:?}), got: {segs:?}"
        );

        // Verify SVG renders identical transformed endpoints & stroke width for upright mode.
        let view = if side == Side::Top {
            SvgView::Top
        } else {
            SvgView::Bottom
        };
        let svg = render_board_svg(
            &board,
            None,
            &SvgOptions {
                view,
                ..Default::default()
            },
        );
        let svg_lines = parse_svg_silk_lines(&svg);
        assert_eq!(svg_lines.len(), 3);
        assert!(svg_lines.iter().all(|(_, sw)| sw == "0.1500"));
        let svg_segs: Vec<SegMm> = svg_lines.into_iter().map(|(s, _)| s).collect();
        let sx = if side == Side::Bottom { -1.0 } else { 1.0 };
        let svg_pt = |p: PtMm| (p.0 * sx, -p.1);
        assert!(
            has_seg(&svg_segs, svg_pt(tr), svg_pt(tl))
                && has_seg(&svg_segs, svg_pt(tl), svg_pt(bl))
                && has_seg(&svg_segs, svg_pt(ml), svg_pt(mr)),
            "{side:?} rot={rot} SVG lines mismatch: {svg_segs:?}"
        );
    }

    // Multi-char "R1" on top 180 deg and bottom 0 deg verifies left-to-right character ordering (R left, 1 right).
    let scale_x = 0.70 * 0.65 / 4.0;
    let char_w = 4.0 * scale_x;
    let advance = char_w * 1.45;
    let x0 = -(advance + char_w) * 0.5;
    for (side, rot) in [(Side::Top, 180.0), (Side::Bottom, 0.0)] {
        let b = make_board("R1", side, 20.0, 15.0, rot, AutoRefdes::Upright, 120_000);
        let out = tmp.join(format!("r1_{side:?}"));
        export_fab(&b, &out, &FabOptions::default()).unwrap();
        let silk_file = if side == Side::Top {
            "F_Silkscreen.gto"
        } else {
            "B_Silkscreen.gbo"
        };
        let (segs, _) = parse_gerber_silk(&out.join(silk_file));
        let wc = map_local_mm(0.0, 0.95, 20.0, 15.0, rot, side);
        let eff_rot = rot + 180.0;
        // 'R' left stem at local x0, '1' bottom serif at [x0 + advance + scale_x .. x0 + advance + 3*scale_x].
        let r_bot = map_local_mm(x0, -hh, wc.0, wc.1, eff_rot, side);
        let r_top = map_local_mm(x0, hh, wc.0, wc.1, eff_rot, side);
        let one_base_l = map_local_mm(x0 + advance + scale_x, -hh, wc.0, wc.1, eff_rot, side);
        let one_base_r = map_local_mm(x0 + advance + 3.0 * scale_x, -hh, wc.0, wc.1, eff_rot, side);
        assert!(
            has_seg(&segs, r_bot, r_top) && has_seg(&segs, one_base_l, one_base_r),
            "{side:?} upright 'R1' order wrong: {segs:?}"
        );
    }

    // Off mode emits zero auto refdes <line> elements in SVG while keeping explicit silkscreen graphics.
    let off_board = make_board("F", Side::Top, 20.0, 15.0, 0.0, AutoRefdes::Off, 120_000);
    let off_svg = render_board_svg(&off_board, None, &SvgOptions::default());
    assert!(
        !off_svg.contains("<line "),
        "auto_refdes=off should emit zero auto refdes <line> elements:\n{off_svg}"
    );
    let _ = fs::remove_dir_all(&tmp);
}

#[test]
fn pin1_dot_suppression_cli_overrides_and_cached_build() {
    let tmp = fresh_tmp_dir("cli_cache");
    let spec_toml = r#"
name = "refdes_modes"
size_nm = [40000000, 30000000]

[fab]
auto_refdes = "upright"
refdes_stroke_nm = 150000

[footprints.FP_SILK]
kind = "custom"
courtyard = [
  { x = -500000, y = -500000 },
  { x = 500000, y = -500000 },
  { x = 500000, y = 500000 },
  { x = -500000, y = 500000 },
]
[[footprints.FP_SILK.pads]]
name = "1"
kind = "smd"
shape = { shape = "rect", width = 400000, height = 400000 }
pos = { x = 0, y = 0 }
layers = ["F.Cu", "F.Mask"]
[[footprints.FP_SILK.graphics]]
kind = "line"
layer = "F.Silk"
a = { x = -400000, y = -600000 }
b = { x = 400000, y = -600000 }
width = 140000
[[footprints.FP_SILK.graphics]]
kind = "circle"
layer = "F.Silk"
center = { x = -600000, y = 0 }
radius = 100000
width = 0
fill = true

[footprints.FP_NODOT]
kind = "custom"
courtyard = [
  { x = -500000, y = -500000 },
  { x = 500000, y = -500000 },
  { x = 500000, y = 500000 },
  { x = -500000, y = 500000 },
]
[[footprints.FP_NODOT.pads]]
name = "1"
kind = "smd"
shape = { shape = "rect", width = 400000, height = 400000 }
pos = { x = 0, y = 0 }
layers = ["F.Cu", "F.Mask"]

[[placements]]
refdes = "L"
footprint = "FP_SILK"
pos = { x = 10000000, y = 15000000 }
rot_deg = 180.0
side = "top"
locked = true

[[placements]]
refdes = "F"
footprint = "FP_NODOT"
pos = { x = 25000000, y = 15000000 }
rot_deg = 0.0
side = "bottom"
locked = true

[[graphics]]
kind = "line"
layer = "F.Silk"
a = { x = 2000000, y = 2000000 }
b = { x = 6000000, y = 2000000 }
width = 160000
"#;
    let spec_path = tmp.join("board.toml");
    fs::write(&spec_path, spec_toml).unwrap();
    let spec_str = spec_path.to_str().unwrap();

    // 1. Initial cached `boardc build` emits 0.15 mm refdes strokes and auto pin-1 dot on FP_NODOT.
    let cache_dir = tmp.join("cache");
    let build_out = tmp.join("build_out");
    let build_res = run_boardc(&[
        "build",
        spec_str,
        "-o",
        build_out.to_str().unwrap(),
        "--cache-dir",
        cache_dir.to_str().unwrap(),
        "--json",
    ]);
    assert!(
        build_res.status.success(),
        "{}",
        String::from_utf8_lossy(&build_res.stderr)
    );
    let fab_dir = build_out.join("fab");
    assert!(
        fs::read_to_string(fab_dir.join("F_Silkscreen.gto"))
            .unwrap()
            .contains("C,0.15*")
    );
    let (b_segs, b_flashes) = parse_gerber_silk(&fab_dir.join("B_Silkscreen.gbo"));
    assert!(!b_segs.is_empty());
    assert_eq!(
        b_flashes.len(),
        1,
        "upright mode should emit auto pin-1 dot on FP_NODOT"
    );

    // 2. Modifying [fab] updates emitted silkscreen even with cache enabled (all stages hit cache).
    let off_toml = spec_toml
        .replace("auto_refdes = \"upright\"", "auto_refdes = false")
        .replace("refdes_stroke_nm = 150000", "refdes_stroke_nm = 180000");
    fs::write(&spec_path, &off_toml).unwrap();
    let build_out2 = tmp.join("build_out_cached");
    let build_res2 = run_boardc(&[
        "build",
        spec_str,
        "-o",
        build_out2.to_str().unwrap(),
        "--cache-dir",
        cache_dir.to_str().unwrap(),
        "--json",
    ]);
    assert!(build_res2.status.success());
    let rep2: serde_json::Value = serde_json::from_slice(&build_res2.stdout).unwrap();
    let stages2 = rep2["stages"].as_array().unwrap();
    assert!(!stages2.is_empty());
    assert!(
        stages2.iter().all(|s| s["cache_hit"] == true),
        "changing [fab] should reuse all pipeline stage caches: {stages2:?}"
    );
    let (b_off_segs, b_off_flashes) =
        parse_gerber_silk(&build_out2.join("fab").join("B_Silkscreen.gbo"));
    assert!(
        b_off_segs.is_empty(),
        "auto_refdes=false must suppress bottom refdes"
    );
    assert!(
        b_off_flashes.is_empty(),
        "auto_refdes=false must suppress auto pin-1 dot on FP_NODOT"
    );
    let (f_off_segs, f_off_flashes) =
        parse_gerber_silk(&build_out2.join("fab").join("F_Silkscreen.gto"));
    assert_eq!(
        f_off_flashes.len(),
        1,
        "explicit filled circle on FP_SILK must be preserved"
    );
    assert!(has_seg(&f_off_segs, (2.0, 2.0), (6.0, 2.0)));
    assert!(has_seg(&f_off_segs, (9.6, 15.6), (10.4, 15.6)));

    // 3. CLI `fab --no-auto-refdes` over upright spec also suppresses auto refdes & auto pin-1 dot while keeping explicit circle.
    fs::write(&spec_path, spec_toml).unwrap();
    let fab_no_ref = tmp.join("fab_no_ref");
    assert!(
        run_boardc(&[
            "fab",
            spec_str,
            "-o",
            fab_no_ref.to_str().unwrap(),
            "--no-auto-refdes",
        ])
        .status
        .success()
    );
    let (_, f_cli_flashes) = parse_gerber_silk(&fab_no_ref.join("F_Silkscreen.gto"));
    let (b_cli_segs, b_cli_flashes) = parse_gerber_silk(&fab_no_ref.join("B_Silkscreen.gbo"));
    assert_eq!(f_cli_flashes.len(), 1);
    assert!(b_cli_segs.is_empty());
    assert!(b_cli_flashes.is_empty());

    // 4. CLI `--auto-refdes rotate --refdes-stroke-nm 180000` overrides [fab], and invalid flags fail.
    let fab_ov = tmp.join("fab_ov");
    assert!(
        run_boardc(&[
            "fab",
            spec_str,
            "-o",
            fab_ov.to_str().unwrap(),
            "--auto-refdes",
            "rotate",
            "--refdes-stroke-nm",
            "180000",
        ])
        .status
        .success()
    );
    assert!(
        fs::read_to_string(fab_ov.join("F_Silkscreen.gto"))
            .unwrap()
            .contains("C,0.18*")
    );
    for bad_arg in ["0", "-10000", "150"] {
        assert!(
            !run_boardc(&[
                "fab",
                spec_str,
                "-o",
                tmp.join("fab_bad").to_str().unwrap(),
                "--refdes-stroke-nm",
                bad_arg,
            ])
            .status
            .success(),
            "--refdes-stroke-nm {bad_arg} must fail"
        );
    }
    assert!(
        !run_boardc(&[
            "fab",
            spec_str,
            "-o",
            tmp.join("fab_bad").to_str().unwrap(),
            "--no-auto-refdes",
            "--auto-refdes",
            "upright",
        ])
        .status
        .success()
    );

    let built_board = BoardIr::from_spec(&load_spec_file(&spec_path).unwrap()).unwrap();
    assert!(
        boardc::verify::verify_fab_outputs(&built_board, &fab_dir)
            .unwrap()
            .passed
    );

    let _ = fs::remove_dir_all(&tmp);
}
