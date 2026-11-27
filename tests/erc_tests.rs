use std::collections::{BTreeMap, HashSet};
use std::fs;
use std::path::PathBuf;
use std::process::Command;

use boardc::drc::Severity;
use boardc::erc::{ErcReport, ErcViolationKind, check_board_erc};
use boardc::ir::{
    BoardIr, BoardSpec, FootprintSpec, PadDef, PadKind, PadShape, PinType, Placement, Pt,
    load_spec_file, mm,
};
use boardc::pipeline::{BuildOptions, compile_board};
use boardc::preflight::{PreflightSummary, run_preflight};

fn test_dir(name: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("boardc_issue6_{name}_{}", std::process::id()));
    let _ = fs::remove_dir_all(&dir);
    fs::create_dir_all(&dir).unwrap();
    dir
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
    assert!(
        report.passed,
        "expected clean ERC, got {:?}",
        report.violations
    );
    assert_eq!(report.error_count, 0);
    assert_eq!(report.warning_count, 0);
    assert_eq!(report.checked_nets, 4);
    assert_eq!(report.typed_pins, 12);

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

    // 2. Output-to-output conflict, power_out-to-power_out conflict, out-to-power_out conflict, and out-to-tri warning.
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
pos = { x = 18000000, y = 15000000 }
pin_nets = { "1" = "N_OO", "2" = "N_PP", "3" = "N_OP" }
pin_types = { "1" = "out", "2" = "power_out", "3" = "power_out" }

[[placements]]
refdes = "U3"
footprint = "IC3"
pos = { x = 28000000, y = 15000000 }
pin_nets = { "1" = "N_OT", "2" = "N_TT", "3" = "N_OB" }
pin_types = { "1" = "out", "2" = "tri", "3" = "out" }

[[placements]]
refdes = "U4"
footprint = "IC3"
pos = { x = 35000000, y = 15000000 }
pin_nets = { "1" = "N_OT", "2" = "N_TT", "3" = "N_OB" }
pin_types = { "1" = "tri", "2" = "tri", "3" = "bidir" }
"#;
    let conf_ir = BoardIr::from_spec(&toml::from_str(conflict_toml).unwrap()).unwrap();
    let conf_rep = check_board_erc(&conf_ir);
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
    assert!(conf_rep.violations.iter().any(|v| {
        v.kind == ErcViolationKind::OutputConflict
            && v.severity == Severity::Error
            && v.net == "N_OP"
            && v.pins == vec!["U1.3", "U2.3"]
    }));
    assert!(conf_rep.violations.iter().any(|v| {
        v.kind == ErcViolationKind::OutputTriStateConflict
            && v.severity == Severity::Warning
            && v.net == "N_OT"
    }));
    // Multiple tri-state drivers and out+bidir must NOT trigger conflicts.
    assert!(
        conf_rep
            .violations
            .iter()
            .all(|v| v.net != "N_TT" && v.net != "N_OB")
    );

    // 3. Undriven input (in + in), undriven power_in (power_in + passive cap only), unconnected non-nc, connected nc.
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
    let misc_ir = BoardIr::from_spec(&toml::from_str(misc_toml).unwrap()).unwrap();
    let misc_rep = check_board_erc(&misc_ir);
    assert!(!misc_rep.passed);
    assert!(
        misc_rep
            .violations
            .iter()
            .any(|v| { v.kind == ErcViolationKind::UndrivenInput && v.net == "DEAD_IN" })
    );
    assert!(
        misc_rep
            .violations
            .iter()
            .any(|v| { v.kind == ErcViolationKind::UndrivenPowerInput && v.net == "DEAD_PWR" })
    );
    assert!(
        misc_rep
            .violations
            .iter()
            .any(|v| { v.kind == ErcViolationKind::UnconnectedPin && v.pins == vec!["U1.3"] })
    );
    assert!(misc_rep.violations.iter().any(|v| {
        v.kind == ErcViolationKind::ConnectedNcPin && v.net == "NC_SHORT" && v.pins == vec!["U2.2"]
    }));
}

#[test]
fn erc_handles_placement_overrides_lib_overlays_multi_pad_pins_dnp_and_npth() {
    let dir = test_dir("overrides");
    let lib = dir.join("custom.pretty");
    fs::create_dir_all(&lib).unwrap();
    fs::write(
        lib.join("U_LIB.kicad_mod"),
        r#"(footprint "U_LIB"
  (version 20241229) (layer "F.Cu")
  (pad "1" smd rect (at -1 0) (size 0.5 0.5) (layers "F.Cu" "F.Mask" "F.Paste"))
  (pad "2" smd rect (at 1 0) (size 0.5 0.5) (layers "F.Cu" "F.Mask" "F.Paste"))
  (pad "" np_thru_hole circle (at 0 1.5) (size 1.0 1.0) (drill 1.0) (layers "*.Cu" "*.Mask"))
)"#,
    )
    .unwrap();

    // Library footprint with [footprints.U_LIB.pin_types] overlay (no `kind` field) + placement override.
    let toml_src = r#"
name = "lib_overlay"
size_nm = [30000000, 20000000]
footprint_libs = ["custom.pretty"]

[footprints.U_LIB.pin_types]
1 = "in"
2 = "out"

[[placements]]
refdes = "U1"
footprint = "U_LIB"
pos = { x = 8000000, y = 10000000 }
pin_nets = { "1" = "SIG", "2" = "SIG2" }

[[placements]]
refdes = "U2"
footprint = "U_LIB"
pos = { x = 18000000, y = 10000000 }
pin_nets = { "1" = "SIG2", "2" = "SIG" }

[[placements]]
refdes = "U_DNP"
footprint = "U_LIB"
pos = { x = 25000000, y = 10000000 }
dnp = true
pin_nets = { "2" = "SIG" }
"#;
    let spec_path = dir.join("board.toml");
    fs::write(&spec_path, toml_src).unwrap();
    let spec = load_spec_file(&spec_path).unwrap();
    let ir = BoardIr::from_spec_with_base(&spec, Some(&dir)).unwrap();
    // U_DNP has pin 1 unconnected and pin 2 = out on SIG, but dnp=true excludes it from ERC.
    let rep = check_board_erc(&ir);
    assert!(
        rep.passed,
        "expected clean ERC with DNP excluded, got {:?}",
        rep.violations
    );

    // Multi-pad logical pin ("1" appears on two physical pads of one footprint) must NOT self-conflict
    // and alone on a net must still be detected as a single-pin net.
    let multi_pad_fp = FootprintSpec::Custom {
        pads: vec![
            PadDef {
                name: "1".into(),
                kind: PadKind::Smd,
                shape: PadShape::Rect {
                    width: mm(0.5),
                    height: mm(0.5),
                },
                pos: Pt::new(mm(-0.5), 0),
                rot_deg: 0.0,
                drill_nm: None,
                layers: vec!["F.Cu".into(), "F.Mask".into()],
                mask_margin_nm: None,
                paste_margin_nm: None,
            },
            PadDef {
                name: "1".into(),
                kind: PadKind::Smd,
                shape: PadShape::Rect {
                    width: mm(0.5),
                    height: mm(0.5),
                },
                pos: Pt::new(mm(0.5), 0),
                rot_deg: 0.0,
                drill_nm: None,
                layers: vec!["F.Cu".into(), "F.Mask".into()],
                mask_margin_nm: None,
                paste_margin_nm: None,
            },
            PadDef {
                name: "2".into(),
                kind: PadKind::Smd,
                shape: PadShape::Rect {
                    width: mm(0.5),
                    height: mm(0.5),
                },
                pos: Pt::new(0, mm(1.0)),
                rot_deg: 0.0,
                drill_nm: None,
                layers: vec!["F.Cu".into(), "F.Mask".into()],
                mask_margin_nm: None,
                paste_margin_nm: None,
            },
        ],
        courtyard: vec![],
        graphics: vec![],
        keepouts: vec![],
        pin_types: BTreeMap::from([("1".into(), PinType::Out), ("2".into(), PinType::Nc)]),
    };
    let multi_spec = BoardSpec {
        name: "multi_pad".into(),
        size_nm: Some([mm(20.0), mm(20.0)]),
        footprints: BTreeMap::from([("DUAL".into(), multi_pad_fp)]),
        placements: vec![Placement {
            refdes: "U1".into(),
            footprint: "DUAL".into(),
            pos: Pt::new(mm(10.0), mm(10.0)),
            pin_nets: BTreeMap::from([
                ("1".into(), "LONE_OUT".into()),
                ("2".into(), "unconnected-(U1-NC-Pad2)".into()),
            ]),
            ..Default::default()
        }],
        ..Default::default()
    };
    let multi_rep = check_board_erc(&BoardIr::from_spec(&multi_spec).unwrap());
    assert!(
        multi_rep
            .violations
            .iter()
            .any(|v| v.kind == ErcViolationKind::SinglePinNet && v.net == "LONE_OUT")
    );
    assert!(
        !multi_rep
            .violations
            .iter()
            .any(|v| v.kind == ErcViolationKind::OutputConflict),
        "duplicate physical pad on one logical pin must not self-conflict: {:?}",
        multi_rep.violations
    );
    assert!(
        !multi_rep
            .violations
            .iter()
            .any(|v| v.kind == ErcViolationKind::ConnectedNcPin),
        "KiCad unconnected-(...) net must not trigger ConnectedNcPin"
    );

    // Unknown pin name in pin_types must error in from_spec_with_base.
    let bad_pin_toml = r#"
name = "bad_pin"
size_nm = [20000000, 20000000]

[footprints.U_LDO]
kind = "sot23"
pin_count = 5

[footprints.U_LDO.pin_types]
99 = "in"
"#;
    let bad_spec: BoardSpec = toml::from_str(bad_pin_toml).unwrap();
    assert!(BoardIr::from_spec(&bad_spec).is_err());

    let _ = fs::remove_dir_all(&dir);
}

#[test]
fn preflight_pipeline_and_cli_erc_report_and_gate_on_erc_errors() {
    let dir = test_dir("cli");
    let clean_path = dir.join("clean.toml");
    let bad_path = dir.join("bad.toml");
    fs::write(&clean_path, clean_ldo_board_toml()).unwrap();

    // Introduce an output-to-output short on EN by flipping U2.3 from bidir to out and U1.3 to out.
    let bad_toml = clean_ldo_board_toml().replace(
        "pin_nets = { \"1\" = \"V3V3\", \"2\" = \"GND\", \"3\" = \"EN\" }",
        "pin_nets = { \"1\" = \"V3V3\", \"2\" = \"GND\", \"3\" = \"EN\" }\npin_types = { \"3\" = \"out\" }",
    ).replace("3 = \"in\"", "3 = \"out\"");
    fs::write(&bad_path, &bad_toml).unwrap();

    // Preflight fails on ERC error and passes on clean circuit.
    let clean_ir = BoardIr::from_spec(&load_spec_file(&clean_path).unwrap()).unwrap();
    let clean_pf = run_preflight(&clean_ir, &HashSet::new()).unwrap();
    assert!(clean_pf.ok, "clean preflight failed: {clean_pf:?}");
    assert!(clean_pf.erc_violations.is_empty());

    let bad_ir = BoardIr::from_spec(&load_spec_file(&bad_path).unwrap()).unwrap();
    let bad_pf = run_preflight(&bad_ir, &HashSet::new()).unwrap();
    assert!(!bad_pf.ok);
    assert!(
        bad_pf
            .erc_violations
            .iter()
            .any(|v| v.kind == ErcViolationKind::OutputConflict && v.net == "EN")
    );

    // compile_board with fail_on_preflight = true surfaces erc_violations in JSON blockers.
    let cfg = BuildOptions {
        use_cache: false,
        fail_on_preflight: true,
        ..Default::default()
    };
    let err = compile_board(bad_ir, &cfg).unwrap_err();
    let json_start = err.find('{').expect("expected JSON blockers");
    let blockers: serde_json::Value = serde_json::from_str(&err[json_start..]).unwrap();
    assert!(!blockers["erc_violations"].as_array().unwrap().is_empty());

    // CLI `boardc erc` JSON & text + `boardc preflight --json`.
    let out_clean = Command::new(env!("CARGO_BIN_EXE_boardc"))
        .args(["erc", clean_path.to_str().unwrap(), "--json"])
        .output()
        .expect("boardc erc clean");
    assert!(out_clean.status.success());
    let rep_clean: ErcReport = serde_json::from_slice(&out_clean.stdout).unwrap();
    assert!(rep_clean.passed);

    let out_bad = Command::new(env!("CARGO_BIN_EXE_boardc"))
        .args(["erc", bad_path.to_str().unwrap(), "--json"])
        .output()
        .expect("boardc erc bad");
    assert!(out_bad.status.success());
    let rep_bad: ErcReport = serde_json::from_slice(&out_bad.stdout).unwrap();
    assert!(!rep_bad.passed);
    assert!(rep_bad.error_count >= 1);

    let out_bad_text = Command::new(env!("CARGO_BIN_EXE_boardc"))
        .args(["erc", bad_path.to_str().unwrap()])
        .output()
        .expect("boardc erc text");
    assert!(out_bad_text.status.success());
    let text = String::from_utf8(out_bad_text.stdout).unwrap();
    assert!(text.contains("ERC FAILED"), "unexpected output:\n{text}");

    let out_pf = Command::new(env!("CARGO_BIN_EXE_boardc"))
        .args(["preflight", bad_path.to_str().unwrap(), "--json"])
        .output()
        .expect("boardc preflight");
    assert!(out_pf.status.success());
    let pf_cli: PreflightSummary = serde_json::from_slice(&out_pf.stdout).unwrap();
    assert!(!pf_cli.ok);
    assert!(!pf_cli.erc_violations.is_empty());

    let _ = fs::remove_dir_all(&dir);
}
