use std::collections::{BTreeMap, HashSet};
use std::fs;
use std::path::PathBuf;
use std::process::{Command, Output};

use boardc::drc::Severity;
use boardc::erc::{ErcReport, ErcViolationKind, check_board_erc};
use boardc::ir::{
    BoardIr, BoardSpec, FootprintSpec, NetDef, PadDef, PadKind, PadShape, PinType, Placement,
    PourSpec, Pt, load_board_json, load_spec_file, mm, save_board_json,
};
use boardc::pipeline::{BuildOptions, compile_board};
use boardc::preflight::{PreflightSummary, run_preflight};

fn test_dir(name: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("boardc_issue6_{name}_{}", std::process::id()));
    let _ = fs::remove_dir_all(&dir);
    fs::create_dir_all(&dir).unwrap();
    dir
}

fn smd(name: &str, x_mm: f64, y_mm: f64) -> PadDef {
    PadDef {
        name: name.into(),
        kind: PadKind::Smd,
        shape: PadShape::Rect {
            width: mm(0.5),
            height: mm(0.5),
        },
        pos: Pt::new(mm(x_mm), mm(y_mm)),
        rot_deg: 0.0,
        drill_nm: None,
        layers: vec!["F.Cu".into(), "F.Mask".into()],
        mask_margin_nm: None,
        paste_margin_nm: None,
    }
}

fn run_cli(args: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_boardc"))
        .args(args)
        .output()
        .expect("run boardc")
}

fn clean_ldo_board_toml() -> &'static str {
    r#"
name = "ldo_clean"
size_nm = [40000000, 30000000]

[footprints.U_LDO]
kind = "sot23"
pin_count = 5

[footprints.U_LDO.pin_types]
1 = "power_in"
2 = "power_in"
3 = "in"
4 = "nc"
5 = "power_out"

[footprints.J_PWR]
kind = "pin_header"
pins = 2
pitch_nm = 2540000

[footprints.J_PWR.pin_types]
1 = "power_out"
2 = "power_out"

[footprints.R_0402]
kind = "chip2_pin"
package = "0402"

[footprints.R_0402.pin_types]
1 = "passive"
2 = "passive"

[footprints.U_LOAD]
kind = "sot23"
pin_count = 3

[footprints.U_LOAD.pin_types]
1 = "power_in"
2 = "power_in"
3 = "bidir"

[[placements]]
refdes = "J1"
footprint = "J_PWR"
pos = { x = 5000000, y = 15000000 }
pin_nets = { "1" = "VIN", "2" = "GND" }

[[placements]]
refdes = "U1"
footprint = "U_LDO"
pos = { x = 15000000, y = 15000000 }
pin_nets = { "1" = "VIN", "2" = "GND", "3" = "EN", "5" = "V3V3" }

[[placements]]
refdes = "R1"
footprint = "R_0402"
pos = { x = 15000000, y = 20000000 }
pin_nets = { "1" = "VIN", "2" = "EN" }

[[placements]]
refdes = "U2"
footprint = "U_LOAD"
pos = { x = 25000000, y = 15000000 }
pin_nets = { "1" = "V3V3", "2" = "GND", "3" = "EN" }
"#
}

#[test]
fn erc_catches_all_issue6_violation_kinds_and_passes_clean_circuit() {
    let spec: BoardSpec = toml::from_str(clean_ldo_board_toml()).expect("parse clean TOML");
    let clean = BoardIr::from_spec(&spec).expect("from_spec clean");
    let report = check_board_erc(&clean);
    assert!(report.passed, "expected clean ERC: {:?}", report.violations);
    assert_eq!(
        (
            report.error_count,
            report.warning_count,
            report.checked_nets,
            report.typed_pins
        ),
        (0, 0, 4, 12)
    );

    // 1. Single-pin net typo on V3V3_TYPO + undriven power_in on U2.1.
    let mut typo_spec = spec.clone();
    typo_spec.placements[1]
        .pin_nets
        .insert("5".into(), "V3V3_TYPO".into());
    let typo_rep = check_board_erc(&BoardIr::from_spec(&typo_spec).unwrap());
    assert!(!typo_rep.passed);
    assert!(
        typo_rep
            .violations
            .iter()
            .any(|v| v.kind == ErcViolationKind::SinglePinNet && v.net == "V3V3_TYPO")
    );
    assert!(
        typo_rep
            .violations
            .iter()
            .any(|v| v.kind == ErcViolationKind::UndrivenPowerInput && v.net == "V3V3")
    );

    // 2. Output, power_out, tri-state, and combined multi-driver conflicts.
    let conflict_toml = r#"
name = "conflicts"
size_nm = [40000000, 30000000]

[footprints.IC3]
kind = "sot23"
pin_count = 3

[[placements]]
refdes = "U1"
footprint = "IC3"
pos = { x = 8000000, y = 15000000 }
pin_nets = { "1" = "N_OO", "2" = "N_PP", "3" = "N_OP" }
pin_types = { "1" = "out", "2" = "power_out", "3" = "out" }

[[placements]]
refdes = "U2"
footprint = "IC3"
pos = { x = 16000000, y = 15000000 }
pin_nets = { "1" = "N_OO", "2" = "N_PP", "3" = "N_OP" }
pin_types = { "1" = "out", "2" = "power_out", "3" = "power_out" }

[[placements]]
refdes = "U3"
footprint = "IC3"
pos = { x = 24000000, y = 15000000 }
pin_nets = { "1" = "N_OT", "2" = "N_TT", "3" = "N_OP" }
pin_types = { "1" = "out", "2" = "tri", "3" = "out" }

[[placements]]
refdes = "U4"
footprint = "IC3"
pos = { x = 32000000, y = 15000000 }
pin_nets = { "1" = "N_OT", "2" = "N_TT", "3" = "N_PT" }
pin_types = { "1" = "tri", "2" = "tri", "3" = "power_out" }

[[placements]]
refdes = "U5"
footprint = "IC3"
pos = { x = 35000000, y = 20000000 }
pin_nets = { "1" = "N_PT", "2" = "N_OB", "3" = "N_OB" }
pin_types = { "1" = "tri", "2" = "out", "3" = "bidir" }
"#;
    let conf_rep =
        check_board_erc(&BoardIr::from_spec(&toml::from_str(conflict_toml).unwrap()).unwrap());
    assert!(!conf_rep.passed);
    assert!(conf_rep.violations.iter().any(|v| {
        v.kind == ErcViolationKind::OutputConflict
            && v.severity == Severity::Error
            && v.net == "N_OO"
            && v.pins == vec!["U1.1", "U2.1"]
    }));
    assert!(conf_rep.violations.iter().any(|v| {
        v.kind == ErcViolationKind::PowerOutputConflict
            && v.severity == Severity::Error
            && v.net == "N_PP"
            && v.pins == vec!["U1.2", "U2.2"]
    }));
    // 2 out + 1 power_out on N_OP must emit exactly one OutputConflict (no duplicate).
    let op_errs: Vec<_> = conf_rep
        .violations
        .iter()
        .filter(|v| v.net == "N_OP")
        .collect();
    assert_eq!(op_errs.len(), 1, "unexpected N_OP violations: {op_errs:?}");
    assert_eq!(op_errs[0].kind, ErcViolationKind::OutputConflict);
    assert_eq!(op_errs[0].pins, vec!["U1.3", "U2.3", "U3.3"]);
    // power_out + tri must be Error OutputConflict, while out + tri is Warning OutputTriStateConflict.
    assert!(conf_rep.violations.iter().any(|v| {
        v.kind == ErcViolationKind::OutputConflict
            && v.severity == Severity::Error
            && v.net == "N_PT"
            && v.pins == vec!["U4.3", "U5.1"]
    }));
    assert!(conf_rep.violations.iter().any(|v| {
        v.kind == ErcViolationKind::OutputTriStateConflict
            && v.severity == Severity::Warning
            && v.net == "N_OT"
    }));
    assert!(
        conf_rep
            .violations
            .iter()
            .all(|v| v.net != "N_TT" && v.net != "N_OB")
    );

    // 3. Undriven in/power_in, unconnected non-nc, connected nc.
    let misc_toml = r#"
name = "misc"
size_nm = [40000000, 30000000]

[footprints.IC3]
kind = "sot23"
pin_count = 3

[footprints.C_0402]
kind = "chip2_pin"
package = "0402"
pin_types = { "1" = "passive", "2" = "passive" }

[[placements]]
refdes = "U1"
footprint = "IC3"
pos = { x = 8000000, y = 15000000 }
pin_nets = { "1" = "DEAD_IN", "2" = "DEAD_PWR" }
pin_types = { "1" = "in", "2" = "power_in", "3" = "in" }

[[placements]]
refdes = "U2"
footprint = "IC3"
pos = { x = 18000000, y = 15000000 }
pin_nets = { "1" = "DEAD_IN", "2" = "NC_SHORT", "3" = "NC_SHORT" }
pin_types = { "1" = "in", "2" = "nc", "3" = "out" }

[[placements]]
refdes = "C1"
footprint = "C_0402"
pos = { x = 28000000, y = 15000000 }
pin_nets = { "1" = "DEAD_PWR", "2" = "DEAD_PWR" }
"#;
    let misc_rep =
        check_board_erc(&BoardIr::from_spec(&toml::from_str(misc_toml).unwrap()).unwrap());
    assert!(
        misc_rep
            .violations
            .iter()
            .any(|v| v.kind == ErcViolationKind::UndrivenInput && v.net == "DEAD_IN")
    );
    assert!(
        misc_rep
            .violations
            .iter()
            .any(|v| v.kind == ErcViolationKind::UndrivenPowerInput && v.net == "DEAD_PWR")
    );
    assert!(
        misc_rep
            .violations
            .iter()
            .any(|v| v.kind == ErcViolationKind::UnconnectedPin && v.pins == vec!["U1.3"])
    );
    assert!(misc_rep.violations.iter().any(|v| {
        v.kind == ErcViolationKind::ConnectedNcPin && v.net == "NC_SHORT" && v.pins == vec!["U2.2"]
    }));
}

#[test]
fn erc_partial_annotation_dnp_copper_unconnected_and_order_invariance() {
    // Partial annotation (untyped peer suppresses undriven), KiCad unconnected-(...), DNP peer, copper pour, multi-pad pin.
    let fp = FootprintSpec::Custom {
        pads: vec![smd("1", -0.5, 0.0), smd("1", 0.5, 0.0), smd("2", 0.0, 1.0)],
        courtyard: vec![],
        graphics: vec![],
        keepouts: vec![],
        pin_types: BTreeMap::new(),
    };
    let spec = BoardSpec {
        name: "edge_cases".into(),
        size_nm: Some([mm(30.0), mm(20.0)]),
        footprints: BTreeMap::from([("DUAL".into(), fp)]),
        placements: vec![
            Placement {
                refdes: "U1".into(),
                footprint: "DUAL".into(),
                pos: Pt::new(mm(5.0), mm(10.0)),
                pin_nets: BTreeMap::from([
                    ("1".into(), "PARTIAL_IN".into()),
                    ("2".into(), "PARTIAL_PWR".into()),
                ]),
                pin_types: BTreeMap::from([
                    ("1".into(), PinType::In),
                    ("2".into(), PinType::PowerIn),
                ]),
                ..Default::default()
            },
            Placement {
                refdes: "U2".into(),
                footprint: "DUAL".into(),
                pos: Pt::new(mm(12.0), mm(10.0)),
                pin_nets: BTreeMap::from([
                    ("1".into(), "PARTIAL_IN".into()),
                    ("2".into(), "PARTIAL_PWR".into()),
                ]),
                ..Default::default() // Untyped peer pins.
            },
            Placement {
                refdes: "U3".into(),
                footprint: "DUAL".into(),
                pos: Pt::new(mm(18.0), mm(10.0)),
                pin_nets: BTreeMap::from([
                    ("1".into(), "DNP_NET".into()),
                    ("2".into(), "POUR_ONLY".into()),
                ]),
                pin_types: BTreeMap::from([
                    ("1".into(), PinType::Out),
                    ("2".into(), PinType::PowerOut),
                ]),
                ..Default::default()
            },
            Placement {
                refdes: "U_DNP".into(),
                footprint: "DUAL".into(),
                pos: Pt::new(mm(24.0), mm(10.0)),
                dnp: true,
                pin_nets: BTreeMap::from([("1".into(), "DNP_NET".into())]),
                pin_types: BTreeMap::from([("1".into(), PinType::Out), ("2".into(), PinType::In)]),
                ..Default::default()
            },
            Placement {
                refdes: "U4".into(),
                footprint: "DUAL".into(),
                pos: Pt::new(mm(10.0), mm(15.0)),
                pin_nets: BTreeMap::from([
                    ("1".into(), "unconnected-(U4-IN-Pad1)".into()),
                    ("2".into(), "unconnected-(U4-NC-Pad2)".into()),
                ]),
                pin_types: BTreeMap::from([("1".into(), PinType::In), ("2".into(), PinType::Nc)]),
                ..Default::default()
            },
            Placement {
                refdes: "U5".into(),
                footprint: "DUAL".into(),
                pos: Pt::new(mm(20.0), mm(15.0)),
                pin_nets: BTreeMap::from([("1".into(), "LONE_OUT".into())]),
                pin_types: BTreeMap::from([("1".into(), PinType::Out), ("2".into(), PinType::Nc)]),
                ..Default::default()
            },
        ],
        pours: vec![PourSpec {
            name: "P1".into(),
            net: "POUR_ONLY".into(),
            layer: "B.Cu".into(),
            outline: vec![
                Pt::new(0, 0),
                Pt::new(mm(30.0), 0),
                Pt::new(mm(30.0), mm(20.0)),
                Pt::new(0, mm(20.0)),
            ],
            ..Default::default()
        }],
        ..Default::default()
    };
    let ir = BoardIr::from_spec(&spec).unwrap();
    let rep = check_board_erc(&ir);
    // Only U4.1 (unconnected-(...) on non-nc) and U5.1 (LONE_OUT single-pin) should fail.
    assert_eq!(
        rep.violations.len(),
        2,
        "unexpected violations: {:?}",
        rep.violations
    );
    assert!(rep.violations.iter().any(|v| {
        v.kind == ErcViolationKind::UnconnectedPin && v.pins == vec!["U4.1"] && v.net.is_empty()
    }));
    assert!(rep.violations.iter().any(|v| {
        v.kind == ErcViolationKind::SinglePinNet && v.net == "LONE_OUT" && v.pins == vec!["U5.1"]
    }));

    // Placement ordering invariance + board.nets fallback when pl.pin_nets is empty.
    let mut rev_ir = BoardIr::from_spec(&toml::from_str(clean_ldo_board_toml()).unwrap()).unwrap();
    rev_ir.placements[1]
        .pin_types
        .insert("3".into(), PinType::Out);
    rev_ir.placements[3]
        .pin_types
        .insert("3".into(), PinType::Out);
    let rep_fwd = check_board_erc(&rev_ir);
    rev_ir.placements.reverse();
    let rep_rev = check_board_erc(&rev_ir);
    assert_eq!(rep_fwd, rep_rev);

    for p in &mut rev_ir.placements {
        p.pin_nets.clear();
    }
    rev_ir.nets = BTreeMap::from([(
        "EN".into(),
        NetDef {
            name: "EN".into(),
            class: String::new(),
            pins: vec![("U1".into(), "3".into()), ("U2".into(), "3".into())],
            ignore_route: false,
        },
    )]);
    let nets_only = check_board_erc(&rev_ir);
    assert!(nets_only.violations.iter().any(|v| {
        v.kind == ErcViolationKind::OutputConflict
            && v.net == "EN"
            && v.pins == vec!["U1.3", "U2.3"]
    }));
}

#[test]
fn erc_lib_overlays_placement_overrides_and_validation() {
    let dir = test_dir("overrides");
    let lib = dir.join("custom.pretty");
    fs::create_dir_all(&lib).unwrap();
    fs::write(
        lib.join("U_LIB.kicad_mod"),
        r#"(footprint "U_LIB"
  (version 20241229) (layer "F.Cu")
  (pad "1" smd rect (at -1 0) (size 0.5 0.5) (layers "F.Cu" "F.Mask" "F.Paste"))
  (pad "2" smd rect (at 1 0) (size 0.5 0.5) (layers "F.Cu" "F.Mask" "F.Paste"))
  (pad "MH1" np_thru_hole circle (at 0 1.5) (size 1.0 1.0) (drill 1.0) (layers "*.Cu" "*.Mask"))
)"#,
    )
    .unwrap();
    let net_path = dir.join("board.net");
    fs::write(
        &net_path,
        r#"(export (version "D")
  (components
    (comp (ref "U1") (value "IC") (footprint "custom:U_LIB"))
    (comp (ref "U2") (value "IC") (footprint "custom:U_LIB")))
  (nets
    (net (code "1") (name "SIG") (node (ref "U1") (pin "1")) (node (ref "U2") (pin "2")))
    (net (code "2") (name "SIG2") (node (ref "U1") (pin "2")) (node (ref "U2") (pin "1")))))"#,
    )
    .unwrap();

    // Qualified [footprints."custom:U_LIB".pin_types] matched via netlist schematic.footprint.
    let qual_toml = r#"
name = "qual_overlay"
size_nm = [30000000, 20000000]
footprint_libs = ["custom.pretty"]
netlist = "board.net"

[footprints."custom:U_LIB".pin_types]
1 = "in"
2 = "out"

[[placements]]
refdes = "U1"
footprint = "U_LIB"
pos = { x = 8000000, y = 10000000 }

[[placements]]
refdes = "U2"
footprint = "U_LIB"
pos = { x = 18000000, y = 10000000 }
"#;
    let spec: BoardSpec = toml::from_str(qual_toml).unwrap();
    let clean_ir = BoardIr::from_spec_with_base(&spec, Some(&dir)).unwrap();
    assert!(check_board_erc(&clean_ir).passed);

    // Placement pin_types override flips U1.1 from in -> out, causing OutputConflict on SIG.
    let mut ov_spec = spec.clone();
    ov_spec.placements[0]
        .pin_types
        .insert("1".into(), PinType::Out);
    let ov_rep = check_board_erc(&BoardIr::from_spec_with_base(&ov_spec, Some(&dir)).unwrap());
    assert!(ov_rep.violations.iter().any(|v| {
        v.kind == ErcViolationKind::OutputConflict
            && v.net == "SIG"
            && v.pins == vec!["U1.1", "U2.2"]
    }));

    // Validation rejections: unknown lib/NPTH/whitespace pin, unmatched overlay, bad lib field, unknown non-lib/placement pin, padless fp.
    for bad in [
        qual_toml.replace("2 = \"out\"", "2 = \"out\"\n99 = \"in\""),
        qual_toml.replace("2 = \"out\"", "2 = \"out\"\nMH1 = \"passive\""),
        qual_toml.replace("1 = \"in\"", "\" 1 \" = \"in\""),
        format!("{qual_toml}\n[footprints.TYPO_FP.pin_types]\n1 = \"in\"\n"),
    ] {
        let s: BoardSpec = toml::from_str(&bad).expect("valid TOML");
        assert!(
            BoardIr::from_spec_with_base(&s, Some(&dir)).is_err(),
            "expected error for:\n{bad}"
        );
    }
    let bad_fp_pin: BoardSpec = toml::from_str(
        r#"name = "bad_fp"
size_nm = [20000000, 20000000]
[footprints.U1]
kind = "sot23"
pin_count = 3
pin_types = { "99" = "in" }"#,
    )
    .unwrap();
    assert!(BoardIr::from_spec(&bad_fp_pin).is_err());
    let bad_lib_extra = r#"
name = "bad_lib"
size_nm = [20000000, 20000000]
[footprints.U_LIB]
kind = "lib"
package = "0402"
pin_types = { "1" = "in" }
"#;
    assert!(toml::from_str::<BoardSpec>(bad_lib_extra).is_err());

    let mut bad_pl_spec = spec.clone();
    bad_pl_spec.placements[0]
        .pin_types
        .insert("99".into(), PinType::In);
    assert!(BoardIr::from_spec_with_base(&bad_pl_spec, Some(&dir)).is_err());

    let padless = BoardSpec {
        name: "padless".into(),
        size_nm: Some([mm(20.0), mm(20.0)]),
        footprints: BTreeMap::from([(
            "EMPTY".into(),
            FootprintSpec::Custom {
                pads: vec![],
                courtyard: vec![],
                graphics: vec![],
                keepouts: vec![],
                pin_types: BTreeMap::from([("1".into(), PinType::In)]),
            },
        )]),
        ..Default::default()
    };
    assert!(BoardIr::from_spec(&padless).is_err());

    // Direct BoardIr JSON validation rejects invalid footprint or placement pin_types.
    let bad_ir_path = dir.join("bad_ir.json");
    let mut bad_ir = clean_ir.clone();
    bad_ir.placements[0]
        .pin_types
        .insert("99".into(), PinType::In);
    save_board_json(&bad_ir, &bad_ir_path).unwrap();
    assert!(load_board_json(&bad_ir_path).is_err());
    assert!(
        !run_cli(&["erc", bad_ir_path.to_str().unwrap()])
            .status
            .success()
    );

    let _ = fs::remove_dir_all(&dir);
}

#[test]
fn preflight_pipeline_and_cli_erc_report_and_gate_on_erc_errors() {
    let dir = test_dir("cli");
    let clean_path = dir.join("clean.toml");
    let bad_path = dir.join("bad.toml");
    let warn_path = dir.join("warn.toml");
    fs::write(&clean_path, clean_ldo_board_toml()).unwrap();

    let bad_toml = clean_ldo_board_toml()
        .replace(
            "pin_nets = { \"1\" = \"V3V3\", \"2\" = \"GND\", \"3\" = \"EN\" }",
            "pin_nets = { \"1\" = \"V3V3\", \"2\" = \"GND\", \"3\" = \"EN\" }\npin_types = { \"3\" = \"out\" }",
        )
        .replace("3 = \"in\"", "3 = \"out\"");
    fs::write(&bad_path, &bad_toml).unwrap();

    let warn_toml = clean_ldo_board_toml()
        .replace(
            "pin_nets = { \"1\" = \"V3V3\", \"2\" = \"GND\", \"3\" = \"EN\" }",
            "pin_nets = { \"1\" = \"V3V3\", \"2\" = \"GND\", \"3\" = \"EN\" }\npin_types = { \"3\" = \"tri\" }",
        )
        .replace("3 = \"in\"", "3 = \"out\"");
    fs::write(&warn_path, &warn_toml).unwrap();

    let clean_ir = BoardIr::from_spec(&load_spec_file(&clean_path).unwrap()).unwrap();
    let bad_ir = BoardIr::from_spec(&load_spec_file(&bad_path).unwrap()).unwrap();
    let warn_ir = BoardIr::from_spec(&load_spec_file(&warn_path).unwrap()).unwrap();

    assert!(run_preflight(&clean_ir, &HashSet::new()).unwrap().ok);
    let bad_pf = run_preflight(&bad_ir, &HashSet::new()).unwrap();
    assert!(!bad_pf.ok);
    assert!(
        bad_pf
            .erc_violations
            .iter()
            .any(|v| v.kind == ErcViolationKind::OutputConflict && v.net == "EN")
    );
    let warn_pf = run_preflight(&warn_ir, &HashSet::new()).unwrap();
    assert!(warn_pf.ok, "warning-only ERC must not fail preflight");
    assert_eq!(warn_pf.erc_violations.len(), 1);

    // fail_on_preflight = true blocks on ERC error, passes on warning-only; fail_on_preflight = false still marks report.ok = false on ERC error.
    let cfg_gate = BuildOptions {
        use_cache: false,
        fail_on_preflight: true,
        ..Default::default()
    };
    let err = compile_board(bad_ir.clone(), &cfg_gate).unwrap_err();
    let blockers: serde_json::Value = serde_json::from_str(&err[err.find('{').unwrap()..]).unwrap();
    assert!(!blockers["erc_violations"].as_array().unwrap().is_empty());

    let warn_build = compile_board(warn_ir, &cfg_gate).unwrap();
    assert!(warn_build.ok, "warning-only ERC must allow build ok");

    let cfg_nogate = BuildOptions {
        use_cache: false,
        fail_on_preflight: false,
        ..Default::default()
    };
    let bad_build = compile_board(bad_ir, &cfg_nogate).unwrap();
    assert!(!bad_build.ok, "ERC error must mark BuildReport.ok = false");

    // CLI `boardc erc` on TOML, exported .kicad_pcb, JSON BoardIr, and `boardc preflight`.
    let out_clean = run_cli(&["erc", clean_path.to_str().unwrap(), "--json"]);
    assert!(out_clean.status.success());
    assert!(
        serde_json::from_slice::<ErcReport>(&out_clean.stdout)
            .unwrap()
            .passed
    );

    let out_bad = run_cli(&["erc", bad_path.to_str().unwrap(), "--json"]);
    assert!(out_bad.status.success());
    let rep_bad: ErcReport = serde_json::from_slice(&out_bad.stdout).unwrap();
    assert!(!rep_bad.passed && rep_bad.error_count >= 1);

    let out_bad_txt = run_cli(&["erc", bad_path.to_str().unwrap()]);
    assert!(
        String::from_utf8(out_bad_txt.stdout)
            .unwrap()
            .contains("ERC FAILED")
    );

    let out_warn_pf = run_cli(&["preflight", warn_path.to_str().unwrap()]);
    assert!(out_warn_pf.status.success());
    let warn_pf_txt = String::from_utf8(out_warn_pf.stdout).unwrap();
    assert!(
        warn_pf_txt.contains("0 ERC errors (1 warnings)"),
        "unexpected preflight output:\n{warn_pf_txt}"
    );

    let out_pf = run_cli(&["preflight", bad_path.to_str().unwrap(), "--json"]);
    let pf_cli: PreflightSummary = serde_json::from_slice(&out_pf.stdout).unwrap();
    assert!(!pf_cli.ok && !pf_cli.erc_violations.is_empty());

    let kicad_out = dir.join("clean.kicad_pcb");
    assert!(
        run_cli(&[
            "export-kicad",
            clean_path.to_str().unwrap(),
            "--out",
            kicad_out.to_str().unwrap(),
            "--no-pro",
        ])
        .status
        .success()
    );
    let out_kicad = run_cli(&["erc", kicad_out.to_str().unwrap(), "--json"]);
    assert!(out_kicad.status.success());
    assert!(
        serde_json::from_slice::<ErcReport>(&out_kicad.stdout)
            .unwrap()
            .passed
    );

    let ir_path = dir.join("clean.board.json");
    save_board_json(&clean_ir, &ir_path).unwrap();
    let out_ir = run_cli(&["erc", ir_path.to_str().unwrap(), "--json"]);
    assert!(out_ir.status.success());
    assert!(
        serde_json::from_slice::<ErcReport>(&out_ir.stdout)
            .unwrap()
            .passed
    );

    let _ = fs::remove_dir_all(&dir);
}
