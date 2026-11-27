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
use boardc::pipeline::{BuildOptions, StageKind, compile_board};
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
        models: vec![],
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
            Placement {
                refdes: "U6".into(),
                footprint: "DUAL".into(),
                pos: Pt::new(mm(25.0), mm(15.0)),
                pin_types: BTreeMap::from([("1".into(), PinType::In), ("2".into(), PinType::Nc)]),
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
    // U4.1/U4.2 on unconnected-(...) are intentional nc; only U6.1 (genuinely unassigned) and U5.1 (LONE_OUT) fail.
    assert_eq!(rep.checked_nets, 5);
    assert_eq!(
        rep.violations.len(),
        2,
        "unexpected violations: {:?}",
        rep.violations
    );
    assert!(rep.violations.iter().any(|v| {
        v.kind == ErcViolationKind::UnconnectedPin && v.pins == vec!["U6.1"] && v.net.is_empty()
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
            pins: vec![("U1".into(), "3".into()), ("U2".into(), "3".into())],
            ..Default::default()
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
                models: vec![],
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

#[test]
fn erc_power_driven_net_suppresses_undriven_and_invalidates_preflight_cache() {
    let dir = test_dir("power_driven");
    // power_driven rail driving power_in + in (EN strapped to rail) with NO passive/other driver on VDD_1V8_IO.
    let base_toml = r#"
name = "power_driven_board"
size_nm = [40000000, 30000000]

[nets.VDD_1V8_IO]
class = "Power"
power_driven = true

[nets.GND]
class = "Power"
power_driven = true

[footprints.U_LOAD]
kind = "sot23"
pin_count = 3
pin_types = { "1" = "power_in", "2" = "power_in", "3" = "in" }

[[placements]]
refdes = "U1"
footprint = "U_LOAD"
pos = { x = 15000000, y = 15000000 }
locked = true
pin_nets = { "1" = "VDD_1V8_IO", "2" = "GND", "3" = "VDD_1V8_IO" }

[[placements]]
refdes = "U2"
footprint = "U_LOAD"
pos = { x = 25000000, y = 15000000 }
locked = true
pin_nets = { "1" = "VDD_1V8_IO", "2" = "GND", "3" = "VDD_1V8_IO" }
"#;
    let spec: BoardSpec = toml::from_str(base_toml).unwrap();
    let ir = BoardIr::from_spec(&spec).unwrap();
    assert_eq!(ir.nets["VDD_1V8_IO"].name, "VDD_1V8_IO");
    assert_eq!(ir.nets["VDD_1V8_IO"].class, "Power");
    assert!(ir.nets["VDD_1V8_IO"].power_driven);

    let rep = check_board_erc(&ir);
    assert!(rep.passed, "expected clean ERC: {:?}", rep.violations);
    assert_eq!(rep.error_count, 0);

    // JSON round-trip preserves power_driven = true and omits false.
    let ir_path = dir.join("power_driven.board.json");
    save_board_json(&ir, &ir_path).unwrap();
    let raw_json = fs::read_to_string(&ir_path).unwrap();
    assert!(raw_json.contains("\"power_driven\": true"));
    let rt = load_board_json(&ir_path).unwrap();
    assert!(rt.nets["VDD_1V8_IO"].power_driven);

    let mut all_false_ir = rt.clone();
    for nd in all_false_ir.nets.values_mut() {
        nd.power_driven = false;
    }
    let false_json_path = dir.join("no_power_driven.board.json");
    save_board_json(&all_false_ir, &false_json_path).unwrap();
    let false_raw_json = fs::read_to_string(&false_json_path).unwrap();
    assert!(!false_raw_json.contains("\"power_driven\""));

    // power_driven = true + 1 power_out pin on the same net must NOT raise PowerOutputConflict.
    let mut with_pwr_out = rt.clone();
    with_pwr_out.placements[0]
        .pin_types
        .insert("1".into(), PinType::PowerOut);
    let rep_po = check_board_erc(&with_pwr_out);
    assert!(
        rep_po.passed,
        "unexpected violations: {:?}",
        rep_po.violations
    );

    // power_driven = true MUST catch OutputConflict against out, tri, and open_collector.
    for pt in [PinType::Out, PinType::Tri, PinType::OpenCollector] {
        let mut conflict_ir = rt.clone();
        conflict_ir.placements[0].pin_types.insert("3".into(), pt);
        let rep_c = check_board_erc(&conflict_ir);
        assert!(
            rep_c.violations.iter().any(|v| {
                v.kind == ErcViolationKind::OutputConflict
                    && v.severity == Severity::Error
                    && v.net == "VDD_1V8_IO"
                    && v.pins == vec!["U1.3"]
                    && v.message.contains("power_driven rail 'VDD_1V8_IO'")
            }),
            "expected OutputConflict for {pt:?} on power_driven rail, got: {:?}",
            rep_c.violations
        );
    }

    // Toggling power_driven invalidates the pipeline preflight cache cleanly (catches both UndrivenPowerInput and UndrivenInput).
    let mut undriven_spec = spec.clone();
    undriven_spec
        .nets
        .get_mut("VDD_1V8_IO")
        .unwrap()
        .power_driven = false;
    let undriven_ir = BoardIr::from_spec(&undriven_spec).unwrap();
    let undriven_rep = check_board_erc(&undriven_ir);
    assert!(
        undriven_rep
            .violations
            .iter()
            .any(|v| v.kind == ErcViolationKind::UndrivenPowerInput && v.net == "VDD_1V8_IO")
    );
    assert!(
        undriven_rep
            .violations
            .iter()
            .any(|v| v.kind == ErcViolationKind::UndrivenInput && v.net == "VDD_1V8_IO")
    );

    let cache_dir = dir.join("cache");
    let cfg = BuildOptions {
        cache_dir,
        use_cache: true,
        ..Default::default()
    };
    let r1 = compile_board(undriven_ir, &cfg).unwrap();
    assert!(!r1.ok);
    let r2 = compile_board(ir, &cfg).unwrap();
    assert!(r2.ok);
    assert!(
        !r2.stage_hit(StageKind::Preflight),
        "toggling power_driven must invalidate preflight cache"
    );

    let _ = fs::remove_dir_all(&dir);
}

#[test]
fn erc_open_collector_and_open_drain_alias_matrix() {
    assert_eq!(PinType::OpenCollector.as_str(), "open_collector");
    let parsed_json: BTreeMap<String, PinType> =
        serde_json::from_str(r#"{"a":"open_collector","b":"open_drain"}"#).unwrap();
    assert_eq!(parsed_json["a"], PinType::OpenCollector);
    assert_eq!(parsed_json["b"], PinType::OpenCollector);

    // Clean wired-OR I2C/INT bus + bidir/tri/passive/in compatibility.
    let clean_toml = r#"
name = "open_drain_clean"
size_nm = [40000000, 30000000]

[footprints.U_PMIC]
kind = "sot23"
pin_count = 5
pin_types = { "1" = "open_collector", "2" = "open_drain", "3" = "in", "4" = "bidir", "5" = "tri" }

[footprints.R_PU]
kind = "chip2_pin"
package = "0402"
pin_types = { "1" = "passive", "2" = "passive" }

[[placements]]
refdes = "U1"
footprint = "U_PMIC"
pos = { x = 10000000, y = 15000000 }
pin_nets = { "1" = "SCL", "2" = "INT_N", "3" = "INT_N", "4" = "SCL", "5" = "INT_N" }

[[placements]]
refdes = "U2"
footprint = "U_PMIC"
pos = { x = 22000000, y = 15000000 }
pin_nets = { "1" = "SCL", "2" = "INT_N", "3" = "SCL", "4" = "INT_N", "5" = "INT_N" }

[[placements]]
refdes = "R1"
footprint = "R_PU"
pos = { x = 16000000, y = 22000000 }
pin_nets = { "1" = "SCL", "2" = "INT_N" }
"#;
    let clean_ir = BoardIr::from_spec(&toml::from_str(clean_toml).unwrap()).unwrap();
    let clean_rep = check_board_erc(&clean_ir);
    assert!(
        clean_rep.passed && clean_rep.warning_count == 0,
        "unexpected violations: {:?}",
        clean_rep.violations
    );

    // Conflict matrix:
    // - N_OC_PO: open_collector + power_out -> OutputConflict (Error)
    // - N_OC_OUT: open_collector + out -> OutputConflict (Error)
    // - N_OC_2OUT: open_collector + 2x out -> single OutputConflict (Error)
    // - N_OC_PWR: open_collector + power_in -> UndrivenPowerInput (Error; open_collector does NOT drive power_in)
    let conflict_toml = r#"
name = "open_drain_conflicts"
size_nm = [50000000, 30000000]

[footprints.IC3]
kind = "sot23"
pin_count = 3

[[placements]]
refdes = "U1"
footprint = "IC3"
pos = { x = 8000000, y = 15000000 }
pin_nets = { "1" = "N_OC_PO", "2" = "N_OC_OUT", "3" = "N_OC_2OUT" }
pin_types = { "1" = "open_collector", "2" = "open_drain", "3" = "open_collector" }

[[placements]]
refdes = "U2"
footprint = "IC3"
pos = { x = 18000000, y = 15000000 }
pin_nets = { "1" = "N_OC_PO", "2" = "N_OC_OUT", "3" = "N_OC_2OUT" }
pin_types = { "1" = "power_out", "2" = "out", "3" = "out" }

[[placements]]
refdes = "U3"
footprint = "IC3"
pos = { x = 28000000, y = 15000000 }
pin_nets = { "1" = "N_OC_PWR", "2" = "N_OC_PWR", "3" = "N_OC_2OUT" }
pin_types = { "1" = "open_collector", "2" = "power_in", "3" = "out" }
"#;
    let conf_rep =
        check_board_erc(&BoardIr::from_spec(&toml::from_str(conflict_toml).unwrap()).unwrap());
    assert!(!conf_rep.passed);
    assert!(conf_rep.violations.iter().any(|v| {
        v.kind == ErcViolationKind::OutputConflict
            && v.severity == Severity::Error
            && v.net == "N_OC_PO"
            && v.pins == vec!["U1.1", "U2.1"]
    }));
    assert!(conf_rep.violations.iter().any(|v| {
        v.kind == ErcViolationKind::OutputConflict
            && v.severity == Severity::Error
            && v.net == "N_OC_OUT"
            && v.pins == vec!["U1.2", "U2.2"]
    }));
    let two_out_errs: Vec<_> = conf_rep
        .violations
        .iter()
        .filter(|v| v.net == "N_OC_2OUT")
        .collect();
    assert_eq!(two_out_errs.len(), 1);
    assert_eq!(two_out_errs[0].kind, ErcViolationKind::OutputConflict);
    assert_eq!(two_out_errs[0].pins, vec!["U1.3", "U2.3", "U3.3"]);
    assert!(conf_rep.violations.iter().any(|v| {
        v.kind == ErcViolationKind::UndrivenPowerInput
            && v.severity == Severity::Error
            && v.net == "N_OC_PWR"
            && v.pins == vec!["U3.2"]
    }));
}

#[test]
fn erc_kicad_netlist_unconnected_nc_inference_and_power_driven_e2e() {
    let dir = test_dir("kicad_e2e");
    let lib = dir.join("custom.pretty");
    fs::create_dir_all(&lib).unwrap();
    fs::write(
        lib.join("U_PMIC.kicad_mod"),
        r#"(footprint "U_PMIC"
  (version 20241229) (layer "F.Cu")
  (pad "A1" smd rect (at -1.5 0) (size 0.4 0.4) (layers "F.Cu" "F.Mask" "F.Paste"))
  (pad "A2" smd rect (at -0.5 0) (size 0.4 0.4) (layers "F.Cu" "F.Mask" "F.Paste"))
  (pad "B1" smd rect (at 0.5 0) (size 0.4 0.4) (layers "F.Cu" "F.Mask" "F.Paste"))
  (pad "B2" smd rect (at 1.5 0) (size 0.4 0.4) (layers "F.Cu" "F.Mask" "F.Paste"))
  (pad "C1" smd rect (at 0 1.0) (size 0.4 0.4) (layers "F.Cu" "F.Mask" "F.Paste"))
)"#,
    )
    .unwrap();
    fs::write(
        lib.join("R_0402.kicad_mod"),
        r#"(footprint "R_0402"
  (version 20241229) (layer "F.Cu")
  (pad "1" smd rect (at -0.5 0) (size 0.5 0.5) (layers "F.Cu" "F.Mask" "F.Paste"))
  (pad "2" smd rect (at 0.5 0) (size 0.5 0.5) (layers "F.Cu" "F.Mask" "F.Paste"))
)"#,
    )
    .unwrap();

    // KiCad .net with passive-fed VDD_1V8_IO, shared open-drain /INT, and single-pin unconnected-(...) on typed pins.
    let net_path = dir.join("design.net");
    fs::write(
        &net_path,
        r#"(export (version "E")
  (components
    (comp (ref "U1") (value "PMIC") (footprint "custom:U_PMIC"))
    (comp (ref "U2") (value "MCU") (footprint "custom:U_PMIC"))
    (comp (ref "R1") (value "10k") (footprint "custom:R_0402"))
  )
  (nets
    (net (code "1") (name "VDD_1V8_IO") (class "Power")
      (node (ref "R1") (pin "1"))
      (node (ref "U1") (pin "A1"))
      (node (ref "U2") (pin "A1"))
    )
    (net (code "2") (name "INT_N") (class "Default")
      (node (ref "U1") (pin "B1"))
      (node (ref "U2") (pin "B1"))
      (node (ref "R1") (pin "2"))
    )
    (net (code "3") (name "unconnected-(U1-STAT-PadA2)") (class "Default")
      (node (ref "U1") (pin "A2"))
    )
    (net (code "4") (name "Unconnected-(U2-STAT-PadA2)") (class "Default")
      (node (ref "U2") (pin "A2"))
    )
    (net (code "5") (name "unconnected-(U1-IN-PadB2)") (class "Default")
      (node (ref "U1") (pin "B2"))
    )
    (net (code "6") (name "unconnected-(U2-IN-PadB2)") (class "Default")
      (node (ref "U2") (pin "B2"))
    )
    (net (code "7") (name "unconnected-(U1-NC-PadC1)") (class "Default")
      (node (ref "U1") (pin "C1"))
    )
    (net (code "8") (name "unconnected-(U2-NC-PadC1)") (class "Default")
      (node (ref "U2") (pin "C1"))
    )
  )
)"#,
    )
    .unwrap();

    let base_toml = r#"
name = "kicad_erc_e2e"
size_nm = [40000000, 30000000]
footprint_libs = ["custom.pretty"]
netlist = "design.net"

[nets.VDD_1V8_IO]
class = "Power"
power_driven = true

[footprints."custom:U_PMIC".pin_types]
A1 = "power_in"
A2 = "out"
B1 = "open_collector"
B2 = "in"
C1 = "nc"

[footprints."custom:R_0402".pin_types]
1 = "passive"
2 = "passive"

[[placements]]
refdes = "U1"
pos = { x = 10000000, y = 15000000 }
pin_types = { B1 = "open_drain" }

[[placements]]
refdes = "U2"
pos = { x = 22000000, y = 15000000 }

[[placements]]
refdes = "R1"
pos = { x = 16000000, y = 22000000 }
"#;
    let spec_path = dir.join("board.toml");
    fs::write(&spec_path, base_toml).unwrap();

    let spec = load_spec_file(&spec_path).unwrap();
    let mut ir = BoardIr::from_spec_with_base(&spec, Some(&dir)).unwrap();
    assert!(ir.nets["VDD_1V8_IO"].power_driven);
    let rep = check_board_erc(&ir);
    assert!(rep.passed, "expected clean ERC: {:?}", rep.violations);
    assert_eq!(rep.error_count, 0);
    assert_eq!(rep.checked_nets, 2);

    // CLI `boardc erc` and `boardc preflight` both pass cleanly.
    let cli_erc = run_cli(&["erc", spec_path.to_str().unwrap(), "--json"]);
    assert!(cli_erc.status.success());
    let cli_rep: ErcReport = serde_json::from_slice(&cli_erc.stdout).unwrap();
    assert!(cli_rep.passed, "CLI ERC failed: {:?}", cli_rep.violations);

    let cli_pf = run_cli(&["preflight", spec_path.to_str().unwrap(), "--json"]);
    assert!(cli_pf.status.success());
    let pf_rep: PreflightSummary = serde_json::from_slice(&cli_pf.stdout).unwrap();
    assert!(
        pf_rep.ok,
        "CLI preflight failed: {:?}",
        pf_rep.erc_violations
    );

    // board.nets fallback (with pl.pin_nets cleared) also treats single-pin unconnected-(...) as intentional nc.
    for p in &mut ir.placements {
        p.pin_nets.clear();
    }
    let fallback_rep = check_board_erc(&ir);
    assert!(
        fallback_rep.passed,
        "expected board.nets fallback to pass: {:?}",
        fallback_rep.violations
    );

    // Explicit pin_nets = { A2 = "" } disconnecting a netlist unconnected-(...) pin triggers UnconnectedPin.
    let mut disconn_spec = spec.clone();
    disconn_spec.placements[0]
        .pin_nets
        .insert("A2".into(), String::new());
    let disconn_ir = BoardIr::from_spec_with_base(&disconn_spec, Some(&dir)).unwrap();
    let disconn_rep = check_board_erc(&disconn_ir);
    assert!(!disconn_rep.passed);
    assert!(disconn_rep.violations.iter().any(|v| {
        v.kind == ErcViolationKind::UnconnectedPin && v.pins == vec!["U1.A2"] && v.net.is_empty()
    }));

    let _ = fs::remove_dir_all(&dir);
}

#[test]
fn erc_unconnected_guards_whitespace_typos_and_net_name_validation() {
    let dir = test_dir("roast_guards");

    // 1. Multi-pin, copper-bearing, power_driven, and DNP+active unconnected-(...) nets are NOT ignored.
    let multi_unconn_toml = r#"
name = "unconn_guards"
size_nm = [40000000, 30000000]

[nets."unconnected-(PWR)"]
power_driven = true

[footprints.IC3]
kind = "sot23"
pin_count = 3
pin_types = { "1" = "out", "2" = "nc", "3" = "in" }

[[placements]]
refdes = "U1"
footprint = "IC3"
pos = { x = 8000000, y = 15000000 }
pin_nets = { "1" = "unconnected-(SHARED)", "2" = "unconnected-(SHARED)", "3" = "unconnected-(SINGLE-IN)" }

[[placements]]
refdes = "U2"
footprint = "IC3"
pos = { x = 18000000, y = 15000000 }
pin_nets = { "1" = "unconnected-(SHARED)", "2" = "unconnected-(SINGLE-NC)", "3" = "unconnected-(COPPER)" }

[[placements]]
refdes = "U3"
footprint = "IC3"
pos = { x = 28000000, y = 15000000 }
pin_nets = { "1" = "unconnected-(PWR)", "2" = "unconnected-(DNP-PAIR)", "3" = "SIG " }

[[placements]]
refdes = "U4"
footprint = "IC3"
pos = { x = 28000000, y = 22000000 }
dnp = true
pin_nets = { "1" = "SIG", "2" = "unconnected-(DNP-PAIR)", "3" = "unconnected-(DNP-ONLY)" }

[[placements]]
refdes = "U5"
footprint = "IC3"
pos = { x = 8000000, y = 22000000 }
pin_nets = { "1" = "SIG", "2" = "unconnected-(OK-NC)", "3" = "SIG" }

[[pours]]
name = "P_UNCONN"
net = "unconnected-(COPPER)"
layer = "B.Cu"
outline = [{ x = 0, y = 0 }, { x = 40000000, y = 0 }, { x = 40000000, y = 30000000 }, { x = 0, y = 30000000 }]
"#;
    let spec: BoardSpec = toml::from_str(multi_unconn_toml).unwrap();
    let ir = BoardIr::from_spec(&spec).unwrap();
    let rep = check_board_erc(&ir);
    assert!(!rep.passed);
    // Multi-pin unconnected-(SHARED) catches OutputConflict and ConnectedNcPin.
    assert!(rep.violations.iter().any(|v| {
        v.kind == ErcViolationKind::OutputConflict
            && v.net == "unconnected-(SHARED)"
            && v.pins == vec!["U1.1", "U2.1"]
    }));
    assert!(rep.violations.iter().any(|v| {
        v.kind == ErcViolationKind::ConnectedNcPin
            && v.net == "unconnected-(SHARED)"
            && v.pins == vec!["U1.2"]
    }));
    // Copper-bearing unconnected-(COPPER) catches UndrivenInput; power_driven unconnected-(PWR) catches OutputConflict.
    assert!(rep.violations.iter().any(|v| {
        v.kind == ErcViolationKind::UndrivenInput
            && v.net == "unconnected-(COPPER)"
            && v.pins == vec!["U2.3"]
    }));
    assert!(rep.violations.iter().any(|v| {
        v.kind == ErcViolationKind::OutputConflict
            && v.net == "unconnected-(PWR)"
            && v.pins == vec!["U3.1"]
    }));
    // Active + DNP pin on unconnected-(DNP-PAIR) has physical_net_pins == 2, so ConnectedNcPin fires.
    assert!(rep.violations.iter().any(|v| {
        v.kind == ErcViolationKind::ConnectedNcPin
            && v.net == "unconnected-(DNP-PAIR)"
            && v.pins == vec!["U3.2"]
    }));
    // Single-pin unconnected-(SINGLE-IN), unconnected-(SINGLE-NC), unconnected-(OK-NC) suppress UnconnectedPin & SinglePinNet.
    assert!(rep.violations.iter().all(|v| {
        v.net != "unconnected-(SINGLE-IN)"
            && v.net != "unconnected-(SINGLE-NC)"
            && v.net != "unconnected-(OK-NC)"
            && v.net != "unconnected-(DNP-ONLY)"
    }));
    // Trailing-space typo "SIG " vs "SIG" is NOT silently merged in ERC (caught as SinglePinNet + UndrivenInput).
    assert!(rep.violations.iter().any(|v| {
        v.kind == ErcViolationKind::SinglePinNet && v.net == "SIG " && v.pins == vec!["U3.3"]
    }));
    assert!(rep.violations.iter().any(|v| {
        v.kind == ErcViolationKind::UndrivenInput && v.net == "SIG " && v.pins == vec!["U3.3"]
    }));

    // 2. Mismatched [nets.v3v3] + name = "+3V3" is rejected in both BoardSpec and BoardIr JSON.
    let bad_spec_toml = r#"
name = "bad_net_name"
size_nm = [20000000, 20000000]
[nets.v3v3]
name = "+3V3"
power_driven = true
"#;
    let bad_spec: BoardSpec = toml::from_str(bad_spec_toml).unwrap();
    let spec_err = BoardIr::from_spec(&bad_spec).unwrap_err();
    assert!(spec_err.contains("v3v3") && spec_err.contains("+3V3"));

    let mut bad_ir = BoardIr::from_spec(
        &toml::from_str::<BoardSpec>(
            r#"
name = "ok_ir"
size_nm = [20000000, 20000000]
[nets.v3v3]
power_driven = true
"#,
        )
        .unwrap(),
    )
    .unwrap();
    bad_ir.nets.get_mut("v3v3").unwrap().name = "+3V3".into();
    let bad_ir_path = dir.join("bad_net.board.json");
    save_board_json(&bad_ir, &bad_ir_path).unwrap();
    let ir_err = load_board_json(&bad_ir_path).unwrap_err();
    assert!(ir_err.contains("v3v3") && ir_err.contains("+3V3"));

    // 3. load_board_json backfills empty NetDef.name from map key.
    let backfill_path = dir.join("backfill.board.json");
    let mut backfill_ir = bad_ir;
    backfill_ir.nets.get_mut("v3v3").unwrap().name.clear();
    save_board_json(&backfill_ir, &backfill_path).unwrap();
    let loaded = load_board_json(&backfill_path).unwrap();
    assert_eq!(loaded.nets["v3v3"].name, "v3v3");

    let _ = fs::remove_dir_all(&dir);
}
