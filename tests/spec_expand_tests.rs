use std::collections::BTreeMap;
use std::fs;
use std::path::PathBuf;

use boardc::drc::{ViolationKind, check_board_drc};
use boardc::erc::{ErcViolationKind, check_board_erc};
use boardc::geom::local_to_board;
use boardc::ir::{
    BgaShadowRule, BoardIr, BoardSpec, Constraints, FootprintSpec, MatchGroupRule, NetDef, PinType,
    Placement, Pt, Side, SubcircuitInstance, SubcircuitSpec, Track, Via, load_board_json, mm,
    save_board_json,
};
use boardc::route::{RouteOptions, route_board_ir};

fn tmp_test_dir(tag: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("boardc_spec_expand_{tag}_{}", std::process::id()));
    let _ = fs::remove_dir_all(&dir);
    fs::create_dir_all(&dir).unwrap();
    dir
}

#[test]
fn issue_34_minimal_example_and_erc_drc_route() {
    let dir = tmp_test_dir("issue34");
    let lib = dir.join("libs");
    fs::create_dir_all(&lib).unwrap();
    fs::write(
        lib.join("VFBGA169.kicad_mod"),
        r#"(footprint "VFBGA169"
  (version 20241229) (layer "F.Cu")
  (pad "L11" smd circle (at -1.5 0) (size 0.35 0.35) (layers "F.Cu" "F.Mask" "F.Paste"))
  (pad "A1" smd circle (at 0 1.0) (size 0.35 0.35) (layers "F.Cu" "F.Mask" "F.Paste"))
  (pad "B2" smd circle (at 0 -1.0) (size 0.35 0.35) (layers "F.Cu" "F.Mask" "F.Paste"))
  (pad "G1" smd circle (at 1.5 0) (size 0.35 0.35) (layers "F.Cu" "F.Mask" "F.Paste"))
)"#,
    )
    .unwrap();

    let toml_spec = r#"
name = "issue34_minimal"
size_nm = [30000000, 20000000]
footprint_libs = ["libs"]

[footprints.VFBGA169.pin_names]
PB5 = "L11"
VDD_1V8 = ["A1", "B2"]
GND = "G1"

[footprints.VFBGA169.pin_types]
PB5 = "out"
VDD_1V8 = "power_in"
GND = "power_in"

[footprints.SWD_HDR]
kind = "pin_header"
pins = 3
pitch_nm = 2540000
pin_names = { SWO = "1", VREF = "2", GND = "3" }
pin_types = { SWO = "in", VREF = "power_out", GND = "power_out" }

[[placements]]
refdes = "U1"
footprint = "VFBGA169"
pos = { x = 10000000, y = 10000000 }

[[placements]]
refdes = "P1"
footprint = "SWD_HDR"
pos = { x = 22000000, y = 10000000 }

[nets]
SWO = ["U1.PB5", "P1.SWO"]
V1V8 = ["U1.VDD_1V8", "P1.VREF"]
GND = ["U1.GND", "P1.GND"]
"#;

    let spec: BoardSpec = toml::from_str(toml_spec).unwrap();
    let mut ir = BoardIr::from_spec_with_base(&spec, Some(&dir)).unwrap();

    assert_eq!(ir.footprints["VFBGA169"].pin_types["L11"], PinType::Out);
    assert_eq!(ir.footprints["VFBGA169"].pin_types["A1"], PinType::PowerIn);
    assert_eq!(ir.footprints["VFBGA169"].pin_types["B2"], PinType::PowerIn);
    assert_eq!(
        ir.nets["SWO"].pins,
        vec![("U1".into(), "L11".into()), ("P1".into(), "1".into())]
    );
    assert_eq!(
        ir.nets["V1V8"].pins,
        vec![
            ("U1".into(), "A1".into()),
            ("U1".into(), "B2".into()),
            ("P1".into(), "2".into()),
        ]
    );

    let erc = check_board_erc(&ir);
    assert!(erc.passed, "expected ERC pass, got: {:?}", erc.violations);

    let route_summary = route_board_ir(&mut ir, &RouteOptions::default()).unwrap();
    assert_eq!(route_summary.unconnected_pairs, 0);

    let drc = check_board_drc(&ir);
    assert!(drc.passed, "expected DRC pass, got: {:?}", drc.violations);
    let _ = fs::remove_dir_all(&dir);
}

#[test]
fn nets_shorthand_pin_ref_parsing_and_board_ir_roundtrip() {
    let toml_spec = r#"
name = "nets_syntax"
size_nm = [25000000, 20000000]

[footprints.R0402]
kind = "chip2_pin"
package = "0402"

[footprints.HDR]
kind = "custom"
[[footprints.HDR.pads]]
name = "1.A"
kind = "smd"
shape = { shape = "rect", width = 600000, height = 600000 }
pos = { x = 0, y = 0 }
layers = ["F.Cu", "F.Mask", "F.Paste"]

[[placements]]
refdes = "R1"
footprint = "R0402"
pos = { x = 8000000, y = 10000000 }

[[placements]]
refdes = "R2"
footprint = "R0402"
pos = { x = 14000000, y = 10000000 }

[[placements]]
refdes = "J1"
footprint = "HDR"
pos = { x = 19000000, y = 10000000 }

[net_classes.HS]
name = "HS"
track_width_nm = 200000
clearance_nm = 150000
via_dia_nm = 450000
via_drill_nm = 200000

[nets]
SIG_STR = ["R1.1", "R2.1"]
SIG_PAIR = [["R1", "2"], ["R2", "2"]]
SIG_MIXED = ["J1.1.A"]

[nets.SIG_TABLE]
class = "HS"
ignore_route = true
power_driven = true
pins = ["R2.1", ["J1", "1.A"]]
"#;
    let spec: BoardSpec = toml::from_str(toml_spec).unwrap();
    // Conflicting claim on R1.1 is caught by BoardIr::from_spec.
    let mut conflict_spec = spec.clone();
    conflict_spec
        .nets
        .get_mut("SIG_MIXED")
        .unwrap()
        .pins
        .push(("R1".into(), "1".into()));
    assert!(BoardIr::from_spec(&conflict_spec).is_err());

    // Clear SIG_TABLE pins so SIG_STR/SIG_MIXED don't conflict with SIG_TABLE on R2.1/J1.1.A, after verifying SIG_TABLE parsed its mixed pins.
    assert_eq!(
        spec.nets["SIG_TABLE"].pins,
        vec![("R2".into(), "1".into()), ("J1".into(), "1.A".into())]
    );
    let mut clean_spec = spec.clone();
    clean_spec.nets.get_mut("SIG_TABLE").unwrap().pins.clear();
    let ir = BoardIr::from_spec(&clean_spec).unwrap();
    assert_eq!(
        ir.nets["SIG_STR"].pins,
        vec![("R1".into(), "1".into()), ("R2".into(), "1".into())]
    );
    assert_eq!(
        ir.nets["SIG_PAIR"].pins,
        vec![("R1".into(), "2".into()), ("R2".into(), "2".into())]
    );
    assert_eq!(ir.nets["SIG_MIXED"].pins, vec![("J1".into(), "1.A".into())]);
    assert_eq!(ir.nets["SIG_TABLE"].class, "HS");
    assert!(ir.nets["SIG_TABLE"].ignore_route);
    assert!(ir.nets["SIG_TABLE"].power_driven);

    // BoardIr JSON roundtrip remains identical (`pins: Vec<(String, String)>`).
    let dir = tmp_test_dir("ir_rt");
    let ir_path = dir.join("board.json");
    save_board_json(&ir, &ir_path).unwrap();
    let json_val: serde_json::Value =
        serde_json::from_str(&fs::read_to_string(&ir_path).unwrap()).unwrap();
    assert_eq!(
        json_val["nets"]["SIG_STR"]["pins"],
        serde_json::json!([["R1", "1"], ["R2", "1"]])
    );
    let loaded = load_board_json(&ir_path).unwrap();
    assert_eq!(loaded, ir);
    let _ = fs::remove_dir_all(&dir);

    // Reject malformed pin references in shorthand and full NetDef tables.
    for bad_pin in [
        r#""R1""#,
        r#"".1""#,
        r#""R1.""#,
        r#"" .1""#,
        r#""R1. ""#,
        r#"["", "1"]"#,
        r#"["R1", " "]"#,
        r#"["R1"]"#,
        r#"["R1", "1", "2"]"#,
    ] {
        let bad_shorthand =
            format!("name = \"bad\"\nsize_nm = [10000000, 10000000]\n[nets]\nN = [{bad_pin}]\n");
        assert!(
            toml::from_str::<BoardSpec>(&bad_shorthand).is_err(),
            "expected shorthand error for {bad_pin}"
        );
        let bad_table = format!(
            "name = \"bad\"\nsize_nm = [10000000, 10000000]\n[nets.N]\npins = [{bad_pin}]\n"
        );
        assert!(
            toml::from_str::<BoardSpec>(&bad_table).is_err(),
            "expected table error for {bad_pin}"
        );
    }
}

#[test]
fn pin_names_all_variants_lowering_and_validation() {
    let dir = tmp_test_dir("pin_names");
    let lib = dir.join("custom.pretty");
    fs::create_dir_all(&lib).unwrap();
    fs::write(
        lib.join("LIB_IC.kicad_mod"),
        r#"(footprint "LIB_IC"
  (version 20241229) (layer "F.Cu")
  (pad "1" smd rect (at -1 0) (size 0.4 0.4) (layers "F.Cu" "F.Mask" "F.Paste"))
  (pad "2" smd rect (at 0 0) (size 0.4 0.4) (layers "F.Cu" "F.Mask" "F.Paste"))
  (pad "3" smd rect (at 1 0) (size 0.4 0.4) (layers "F.Cu" "F.Mask" "F.Paste"))
  (pad "MH1" np_thru_hole circle (at 0 1.5) (size 1.0 1.0) (drill 1.0) (layers "*.Cu" "*.Mask"))
)"#,
    )
    .unwrap();
    let net_path = dir.join("board.net");
    fs::write(
        &net_path,
        r#"(export (version "D")
  (components (comp (ref "UL1") (value "IC") (footprint "custom:LIB_IC")))
  (libparts
    (libpart (lib "my") (part "IC")
      (pins
        (pin (num "1") (name "IN") (type "input"))
        (pin (num "2") (name "OUT") (type "output"))
        (pin (num "3") (name "GND") (type "power_in"))))))"#,
    )
    .unwrap();

    // Test Lib (two placements sharing same lib footprint), Bga, Qfn, Chip2Pin, Sot23, PinHeader, Custom.
    let ok_toml = r#"
name = "all_fp_kinds"
size_nm = [50000000, 40000000]
footprint_libs = ["custom.pretty"]
netlist = "board.net"

[footprints.LIB_IC.pin_names]
IN_A = "1"
IN_ALIAS2 = "1"
PWR = ["2", "3"]

[footprints.U_BGA]
kind = "bga"
rows = 2
cols = 2
pitch_nm = 800000
ball_dia_nm = 400000
pin_names = { VDD = ["A1", "B2"], SIG = "A2", A1 = "A1" }
pin_types = { VDD = "power_in", SIG = "in" }

[footprints.U_QFN]
kind = "qfn"
pins_per_side = 4
pitch_nm = 500000
body_nm = 3000000
ep_nm = 1500000
pin_names = { OUT = "1", GND = "EP" }
pin_types = { OUT = "out", GND = "power_out" }

[footprints.R_0402]
kind = "chip2_pin"
package = "0402"
pin_names = { A = "1", B = "2" }

[footprints.U_SOT]
kind = "sot23"
pin_count = 3
pin_names = { GATE = "1", SRC = "2", DRN = "3" }

[footprints.J_HDR]
kind = "pin_header"
pins = 2
pitch_nm = 2540000
pin_names = { P1 = "1", P2 = "2" }

[footprints.U_CUSTOM]
kind = "custom"
pin_names = { ANODE = "1", CATHODE = "2" }
[[footprints.U_CUSTOM.pads]]
name = "1"
kind = "smd"
shape = { shape = "rect", width = 500000, height = 500000 }
pos = { x = -500000, y = 0 }
layers = ["F.Cu", "F.Mask", "F.Paste"]
[[footprints.U_CUSTOM.pads]]
name = "2"
kind = "smd"
shape = { shape = "rect", width = 500000, height = 500000 }
pos = { x = 500000, y = 0 }
layers = ["F.Cu", "F.Mask", "F.Paste"]

[[placements]]
refdes = "UL1"
footprint = "LIB_IC"
pos = { x = 6000000, y = 10000000 }
pin_types = { IN_A = "out" }
pin_nets = { IN_A = "SIG_NET", PWR = "VCC", "2" = "VCC" }

[[placements]]
refdes = "UL2"
footprint = "LIB_IC"
pos = { x = 14000000, y = 10000000 }
pin_nets = { IN_ALIAS2 = "SIG_NET" }

[[placements]]
refdes = "UB1"
footprint = "U_BGA"
pos = { x = 22000000, y = 10000000 }

[[placements]]
refdes = "UQ1"
footprint = "U_QFN"
pos = { x = 28000000, y = 10000000 }

[[placements]]
refdes = "R1"
footprint = "R_0402"
pos = { x = 34000000, y = 10000000 }

[[placements]]
refdes = "US1"
footprint = "U_SOT"
pos = { x = 40000000, y = 10000000 }

[[placements]]
refdes = "J1"
footprint = "J_HDR"
pos = { x = 45000000, y = 10000000 }

[[placements]]
refdes = "D1"
footprint = "U_CUSTOM"
pos = { x = 20000000, y = 25000000 }

[nets]
VDD_NET = ["UB1.VDD", "UB1.A1", "UQ1.GND"]
SIG2 = ["UQ1.OUT", "UB1.SIG", "R1.A", "US1.GATE", "J1.P1", "D1.ANODE"]
GND = ["R1.B", "US1.SRC", "J1.P2", "D1.CATHODE"]
"#;

    let spec: BoardSpec = toml::from_str(ok_toml).unwrap();
    let ir = BoardIr::from_spec_with_base(&spec, Some(&dir)).unwrap();
    // Placement alias overrides netlist (libparts) input -> output on UL1.1.
    assert_eq!(ir.placements[0].pin_types["1"], PinType::Out);
    assert_eq!(ir.placements[0].pin_nets["1"], "SIG_NET");
    assert_eq!(ir.placements[0].pin_nets["2"], "VCC");
    assert_eq!(ir.placements[0].pin_nets["3"], "VCC");
    // Second Lib placement also uses lowered alias.
    assert_eq!(ir.placements[1].pin_nets["1"], "SIG_NET");
    // Multi-pad expansion deduplicated UB1.A1 inside VDD_NET.
    assert_eq!(
        ir.nets["VDD_NET"].pins,
        vec![
            ("UB1".into(), "A1".into()),
            ("UB1".into(), "B2".into()),
            ("UQ1".into(), "EP".into()),
        ]
    );

    // Rejections on pin_names validation & conflicts.
    let bad_cases = [
        // Empty alias.
        ok_toml.replace("IN_A = \"1\"", "\"\" = \"1\""),
        // Untrimmed alias.
        ok_toml.replace("IN_A = \"1\"", "\" IN_A \" = \"1\""),
        // Empty target list.
        ok_toml.replace("IN_A = \"1\"", "IN_A = []"),
        // Unknown target pad.
        ok_toml.replace("IN_A = \"1\"", "IN_A = \"99\""),
        // NPTH target pad.
        ok_toml.replace("IN_A = \"1\"", "IN_A = \"MH1\""),
        // Duplicate target within one alias.
        ok_toml.replace("PWR = [\"2\", \"3\"]", "PWR = [\"2\", \"2\"]"),
        // Alias colliding with different physical pad.
        ok_toml.replace("IN_A = \"1\"", "\"1\" = \"2\""),
        // Conflicting footprint.pin_types via alias vs direct pad.
        ok_toml.replace(
            "pin_types = { VDD = \"power_in\", SIG = \"in\" }",
            "pin_types = { VDD = \"power_in\", A1 = \"out\", SIG = \"in\" }",
        ),
        // Conflicting placement.pin_types via two aliases to same pad.
        ok_toml.replace(
            "pin_types = { IN_A = \"out\" }",
            "pin_types = { IN_A = \"out\", IN_ALIAS2 = \"in\" }",
        ),
        // Conflicting placement.pin_nets via alias vs direct pad.
        ok_toml.replace(
            "pin_nets = { IN_A = \"SIG_NET\", PWR = \"VCC\", \"2\" = \"VCC\" }",
            "pin_nets = { IN_A = \"SIG_NET\", PWR = \"VCC\", \"2\" = \"GND\" }",
        ),
        // Conflicting placement.pin_nets via alias vs explicit disconnect.
        ok_toml.replace(
            "pin_nets = { IN_A = \"SIG_NET\", PWR = \"VCC\", \"2\" = \"VCC\" }",
            "pin_nets = { IN_A = \"SIG_NET\", PWR = \"VCC\", \"2\" = \"\" }",
        ),
        // Conflicting nets claiming same physical pad across two nets via alias vs direct.
        ok_toml.replace(
            "GND = [\"R1.B\", \"US1.SRC\", \"J1.P2\", \"D1.CATHODE\"]",
            "GND = [\"UB1.B2\", \"R1.B\", \"US1.SRC\", \"J1.P2\", \"D1.CATHODE\"]",
        ),
        // Empty Lib overlay.
        format!("{ok_toml}\n[footprints.EMPTY_LIB]\nkind = \"lib\"\n"),
    ];
    for (idx, bad) in bad_cases.iter().enumerate() {
        let s: BoardSpec = toml::from_str(bad).expect("valid TOML");
        assert!(
            BoardIr::from_spec_with_base(&s, Some(&dir)).is_err(),
            "expected error for case #{idx}"
        );
    }

    let _ = fs::remove_dir_all(&dir);
}

#[test]
fn subcircuits_expansion_ports_transforms_constraints_and_erc() {
    let toml_spec = r#"
name = "subcircuits_e2e"
size_nm = [60000000, 40000000]

[net_classes.PWR]
name = "PWR"
track_width_nm = 300000
clearance_nm = 200000
via_dia_nm = 500000
via_drill_nm = 250000

[footprints.R0402]
kind = "chip2_pin"
package = "0402"
pin_names = { A = "1", B = "2" }
pin_types = { A = "passive", B = "passive" }

[footprints.LDO]
kind = "sot23"
pin_count = 3
pin_names = { VIN = "1", GND = "2", VOUT = "3" }
pin_types = { VIN = "power_in", GND = "power_in", VOUT = "power_out" }

[footprints.HDR2]
kind = "pin_header"
pins = 2
pitch_nm = 2540000
pin_names = { P1 = "1", P2 = "2" }
pin_types = { P1 = "power_out", P2 = "power_out" }

[footprints.LOAD]
kind = "sot23"
pin_count = 3
pin_names = { VDD = "1", GND = "2", VDD_IO = "3" }
pin_types = { VDD = "power_in", GND = "power_in", VDD_IO = "power_in" }

# Inner reusable regulator stage with local coords, private FB net, and local constraints.
[subcircuits.LdoStage]
ports = ["VIN", "VOUT", "GND"]

[[subcircuits.LdoStage.placements]]
refdes = "U1"
footprint = "LDO"
pos = { x = 0, y = 0 }
rot_deg = 0.0
side = "top"
pin_nets = { VIN = "VIN", GND = "GND" }

[[subcircuits.LdoStage.placements]]
refdes = "R_FB"
footprint = "R0402"
pos = { x = 3000000, y = 1000000 }
rot_deg = 90.0
side = "bottom"

[[subcircuits.LdoStage.placements]]
refdes = "R_FB2"
footprint = "R0402"
pos = { x = 3000000, y = -2000000 }

[[subcircuits.LdoStage.placements]]
refdes = "C_OUT"
footprint = "R0402"
pos = { x = -3000000, y = 0 }

[subcircuits.LdoStage.nets]
VOUT = ["U1.VOUT", "R_FB.A", "C_OUT.A"]
GND = ["C_OUT.B", "R_FB2.B"]

[subcircuits.LdoStage.nets.FB_NODE]
class = "PWR"
power_driven = true
pins = ["R_FB.B", "R_FB2.A"]

[[subcircuits.LdoStage.constraints.cap_proximity]]
cap_refdes = "C_OUT"
ic_refdes = "U1"
net = "VOUT"
max_dist_nm = 4000000

[[subcircuits.LdoStage.constraints.max_lengths]]
nets = ["VOUT", "FB_NODE"]
max_nm = 60000000

[[subcircuits.LdoStage.constraints.match_groups]]
name = "REG_PAIR"
nets = ["VOUT", "FB_NODE"]
tolerance_nm = 500000

[[subcircuits.LdoStage.constraints.min_vias]]
net = "GND"
refdes = "U1"
min_vias = 1

[[subcircuits.LdoStage.constraints.bga_shadows]]
bga_refdes = "U1"
min_clearance_nm = 500000
allow_passives = true

# Outer wrapper channel instantiating LdoStage + an input decoupling cap on SUP/GND.
[subcircuits.Channel]
ports = ["SUP", "OUT", "GND"]

[[subcircuits.Channel.instances]]
subcircuit = "LdoStage"
prefix = "LDO_"
ports = { VIN = "SUP", VOUT = "OUT", GND = "GND" }
pos = { x = 2000000, y = 1000000 }
rot_deg = 90.0
side = "top"

[[subcircuits.Channel.placements]]
refdes = "C_IN"
footprint = "R0402"
pos = { x = -2000000, y = 0 }

[subcircuits.Channel.nets]
SUP = ["C_IN.A"]
GND = ["C_IN.B"]

# Top-level board: two instances (one Top, one Bottom).
[[placements]]
refdes = "J_PWR"
footprint = "HDR2"
pos = { x = 6000000, y = 20000000 }

[[placements]]
refdes = "U_LOAD"
footprint = "LOAD"
pos = { x = 50000000, y = 20000000 }

[[instances]]
subcircuit = "Channel"
prefix = "CH1"
ports = { SUP = "SYS_5V", OUT = "RAIL_3V3", GND = "GND" }
pos = { x = 15000000, y = 20000000 }
rot_deg = 0.0
side = "top"

[[instances]]
subcircuit = "Channel"
prefix = "CH2_"
ports = { SUP = "SYS_5V", OUT = "RAIL_1V8", GND = "GND" }
pos = { x = 35000000, y = 20000000 }
rot_deg = 90.0
side = "bottom"

[nets.SYS_5V]
class = "PWR"
pins = ["J_PWR.P1"]

[nets]
GND = ["J_PWR.P2", "U_LOAD.GND"]
RAIL_3V3 = ["U_LOAD.VDD"]
RAIL_1V8 = ["U_LOAD.VDD_IO"]
"#;

    let spec: BoardSpec = toml::from_str(toml_spec).unwrap();
    let mut ir = BoardIr::from_spec(&spec).unwrap();

    // Preserved top-level NetDef class on SYS_5V and merged pins from both nested instances.
    assert_eq!(ir.nets["SYS_5V"].class, "PWR");
    assert!(
        ir.nets["SYS_5V"]
            .pins
            .contains(&("CH1_LDO_U1".into(), "1".into()))
    );
    assert!(
        ir.nets["SYS_5V"]
            .pins
            .contains(&("CH2_LDO_U1".into(), "1".into()))
    );
    assert!(
        ir.nets["SYS_5V"]
            .pins
            .contains(&("CH1_C_IN".into(), "1".into()))
    );

    // Private internal nets scoped separately per instance with propagated metadata.
    assert!(ir.nets.contains_key("CH1_LDO_FB_NODE"));
    assert!(ir.nets.contains_key("CH2_LDO_FB_NODE"));
    assert_eq!(ir.nets["CH1_LDO_FB_NODE"].class, "PWR");
    assert!(ir.nets["CH1_LDO_FB_NODE"].power_driven);

    // Verify coordinate, rotation, and side composition for Top and Bottom instances.
    let by_ref: BTreeMap<&str, _> = ir
        .placements
        .iter()
        .map(|p| (p.refdes.as_str(), p))
        .collect();
    // CH1 (Top, rot 0) -> LDO (Top, rot 90 at (2, 1)) -> R_FB (Bottom, rot 90 at (3, 1)):
    // LDO origin = (15+2, 20+1) = (17, 21), rot = 90, side = Top.
    // R_FB world pos = local_to_board((3, 1), 17, 21, 90, Top) = (17 - 1, 21 + 3) = (16, 24).
    // R_FB rot = 90 + 90 = 180, side = Bottom.
    let ch1_rfb = by_ref["CH1_LDO_R_FB"];
    assert_eq!(ch1_rfb.pos, Pt::new(mm(16.0), mm(24.0)));
    assert!((ch1_rfb.rot_deg - 180.0).abs() < 1e-9);
    assert_eq!(ch1_rfb.side, Side::Bottom);

    // CH2 (Bottom, rot 90 at (35, 20)) -> LDO (Top, rot 90 at (2, 1)) -> R_FB (Bottom, rot 90 at (3, 1)):
    // LDO origin = local_to_board((2, 1), 35, 20, 90, Bottom) = (35 + 1, 20 + 2) = (36, 22).
    // LDO world rot = (90 - 90) = 0, world side = Bottom.
    // R_FB world pos = local_to_board((3, 1), 36, 22, 0, Bottom) = (36 + 3, 22 - 1) = (39, 21).
    // R_FB rot = (0 - 90).rem_euclid(360) = 270, side = Bottom.flip() = Top.
    let ch2_rfb = by_ref["CH2_LDO_R_FB"];
    assert_eq!(ch2_rfb.pos, Pt::new(mm(39.0), mm(21.0)));
    assert!((ch2_rfb.rot_deg - 270.0).abs() < 1e-9);
    assert_eq!(ch2_rfb.side, Side::Top);

    // Verify local_to_board of a pad matches step-by-step hierarchical composition.
    let pad_local = Pt::new(mm(0.5), mm(0.25));
    let step1 = local_to_board(pad_local, mm(3.0), mm(1.0), 90.0, Side::Bottom);
    let step2 = local_to_board(step1, mm(2.0), mm(1.0), 90.0, Side::Top);
    let step3 = local_to_board(step2, mm(35.0), mm(20.0), 90.0, Side::Bottom);
    let flat = local_to_board(
        pad_local,
        ch2_rfb.pos.x,
        ch2_rfb.pos.y,
        ch2_rfb.rot_deg,
        ch2_rfb.side,
    );
    assert_eq!(flat, step3);

    // Verify constraints rewriting.
    assert_eq!(ir.constraints.cap_proximity.len(), 2);
    assert_eq!(ir.constraints.cap_proximity[0].cap_refdes, "CH1_LDO_C_OUT");
    assert_eq!(ir.constraints.cap_proximity[0].ic_refdes, "CH1_LDO_U1");
    assert_eq!(ir.constraints.cap_proximity[0].net, "RAIL_3V3");
    assert_eq!(ir.constraints.match_groups[0].name, "CH1_LDO_REG_PAIR");
    assert_eq!(
        ir.constraints.match_groups[0].nets,
        vec!["RAIL_3V3".to_string(), "CH1_LDO_FB_NODE".to_string()]
    );
    assert_eq!(
        ir.constraints.min_vias[0].refdes.as_deref(),
        Some("CH1_LDO_U1")
    );
    assert_eq!(ir.constraints.bga_shadows[0].bga_refdes, "CH1_LDO_U1");

    // Clean design passes native ERC, routes cleanly, and passes DRC.
    let erc = check_board_erc(&ir);
    assert!(erc.passed, "expected ERC pass, got: {:?}", erc.violations);

    let route_summary = route_board_ir(&mut ir, &RouteOptions::default()).unwrap();
    assert_eq!(route_summary.unconnected_pairs, 0);

    // Clear synthetic constraints that are non-routable on a 2-layer SOT23 before DRC (cap_proximity and max_lengths stay active).
    ir.constraints.bga_shadows.clear();
    ir.constraints.min_vias.clear();
    ir.constraints.match_groups.clear();
    let drc = check_board_drc(&ir);
    assert!(drc.passed, "expected DRC pass, got: {:?}", drc.violations);

    // ERC catches errors inside an expanded subcircuit (e.g., two power_out shorted on RAIL_3V3).
    let mut bad_erc_spec = spec.clone();
    bad_erc_spec.instances[1]
        .ports
        .insert("OUT".into(), "RAIL_3V3".into());
    let bad_erc_ir = BoardIr::from_spec(&bad_erc_spec).unwrap();
    let bad_erc = check_board_erc(&bad_erc_ir);
    assert!(
        bad_erc
            .violations
            .iter()
            .any(|v| v.kind == ErcViolationKind::OutputConflict
                || v.kind == ErcViolationKind::PowerOutputConflict
                || v.kind == ErcViolationKind::UndrivenPowerInput),
        "expected ERC failure when modifying subcircuit output port, got: {:?}",
        bad_erc.violations
    );

    // Subcircuit validation rejections.
    let bad_specs = [
        // Empty prefix.
        toml_spec.replace("prefix = \"CH1\"", "prefix = \"   \""),
        // Underscore-only prefix.
        toml_spec.replace("prefix = \"CH1\"", "prefix = \"_\""),
        // Unknown subcircuit.
        toml_spec.replace("subcircuit = \"Channel\"", "subcircuit = \"NoSuchSub\""),
        // Missing port binding.
        toml_spec.replace(
            "ports = { SUP = \"SYS_5V\", OUT = \"RAIL_3V3\", GND = \"GND\" }",
            "ports = { SUP = \"SYS_5V\", GND = \"GND\" }",
        ),
        // Empty port binding.
        toml_spec.replace(
            "ports = { SUP = \"SYS_5V\", OUT = \"RAIL_3V3\", GND = \"GND\" }",
            "ports = { SUP = \"\", OUT = \"RAIL_3V3\", GND = \"GND\" }",
        ),
        // Unknown port binding.
        toml_spec.replace(
            "ports = { SUP = \"SYS_5V\", OUT = \"RAIL_3V3\", GND = \"GND\" }",
            "ports = { SUP = \"SYS_5V\", OUT = \"RAIL_3V3\", GND = \"GND\", EXTRA = \"X\" }",
        ),
        // Duplicate expanded refdes (CH1 and CH1_).
        toml_spec.replace("prefix = \"CH2_\"", "prefix = \"CH1_\""),
        // Duplicate port name in subcircuit definition.
        toml_spec.replace(
            "ports = [\"VIN\", \"VOUT\", \"GND\"]",
            "ports = [\"VIN\", \"VIN\", \"GND\"]",
        ),
        // Empty port name in subcircuit definition.
        toml_spec.replace(
            "ports = [\"VIN\", \"VOUT\", \"GND\"]",
            "ports = [\"\", \"VOUT\", \"GND\"]",
        ),
        // Duplicate local placement refdes.
        toml_spec.replace("refdes = \"C_OUT\"", "refdes = \"U1\""),
        // Self-recursion cycle (even uninstantiated).
        format!(
            "{toml_spec}\n[subcircuits.SelfLoop]\nports = []\n[[subcircuits.SelfLoop.instances]]\nsubcircuit = \"SelfLoop\"\nprefix = \"S1\"\n"
        ),
        // Mutual recursion cycle (even uninstantiated).
        format!(
            "{toml_spec}\n[subcircuits.A]\n[[subcircuits.A.instances]]\nsubcircuit = \"B\"\nprefix = \"P\"\n[subcircuits.B]\n[[subcircuits.B.instances]]\nsubcircuit = \"A\"\nprefix = \"Q\"\n"
        ),
    ];
    for (idx, bad) in bad_specs.iter().enumerate() {
        let s: BoardSpec = toml::from_str(bad).expect("valid TOML");
        assert!(
            BoardIr::from_spec(&s).is_err(),
            "expected error for subcircuit bad case #{idx}"
        );
    }
}

#[test]
fn board_ir_json_roundtrip_unnamed_and_untrimmed_pins() {
    let mut footprints = BTreeMap::new();
    footprints.insert(
        "R0402".into(),
        FootprintSpec::Chip2Pin {
            package: "0402".into(),
            pin_types: BTreeMap::new(),
            pin_names: BTreeMap::new(),
        }
        .resolve("R0402")
        .unwrap(),
    );
    let mut nets = BTreeMap::new();
    nets.insert(
        "N1".into(),
        NetDef {
            name: "N1".into(),
            pins: vec![("".into(), "1".into()), (" R1 ".into(), " 2 ".into())],
            ..Default::default()
        },
    );
    let ir = BoardIr {
        name: "ir_verbatim".into(),
        title: None,
        revision: None,
        outline: vec![
            Pt::new(0, 0),
            Pt::new(mm(10.0), 0),
            Pt::new(mm(10.0), mm(10.0)),
            Pt::new(0, mm(10.0)),
        ],
        cutouts: Vec::new(),
        layers: boardc::ir::default_two_layer_stackup(),
        footprints,
        placements: vec![Placement {
            refdes: "".into(),
            footprint: "R0402".into(),
            pos: Pt::new(mm(5.0), mm(5.0)),
            ..Default::default()
        }],
        keepouts: Vec::new(),
        pours: Vec::new(),
        filled_pours: Vec::new(),
        tracks: Vec::new(),
        vias: Vec::new(),
        net_classes: BTreeMap::new(),
        nets,
        rules: Default::default(),
        constraints: Default::default(),
        graphics: Vec::new(),
        stackup: Default::default(),
        fab: Default::default(),
    };
    let dir = tmp_test_dir("ir_verbatim");
    let path = dir.join("board.json");
    save_board_json(&ir, &path).unwrap();
    let loaded = load_board_json(&path).unwrap();
    assert_eq!(loaded, ir);
    let _ = fs::remove_dir_all(&dir);
}

#[test]
fn subcircuit_netdef_metadata_merge_order_and_conflict() {
    let make_spec = |first_a: bool, conflict_class: bool| -> BoardSpec {
        let base_toml = r#"
name = "netdef_merge"
size_nm = [30000000, 20000000]

[footprints.R0402]
kind = "chip2_pin"
package = "0402"

[net_classes.PWR]
name = "PWR"
track_width_nm = 300000
clearance_nm = 200000
via_dia_nm = 500000
via_drill_nm = 250000

[net_classes.HS]
name = "HS"
track_width_nm = 200000
clearance_nm = 150000
via_dia_nm = 450000
via_drill_nm = 200000

[[placements]]
refdes = "R_TOP"
footprint = "R0402"
pos = { x = 5000000, y = 10000000 }

[nets]
BUS = ["R_TOP.1"]

[subcircuits.SubA]
ports = ["P"]
[[subcircuits.SubA.placements]]
refdes = "R1"
footprint = "R0402"
pos = { x = 0, y = 0 }
[subcircuits.SubA.nets.P]
class = "Default"
ignore_route = true
pins = ["R1.1"]

[subcircuits.SubB]
ports = ["P"]
[[subcircuits.SubB.placements]]
refdes = "R1"
footprint = "R0402"
pos = { x = 0, y = 0 }
[subcircuits.SubB.nets.P]
class = "PWR"
power_driven = true
pins = ["R1.1"]
"#;
        let mut spec: BoardSpec = toml::from_str(base_toml).unwrap();
        let inst_a = SubcircuitInstance {
            subcircuit: "SubA".into(),
            prefix: "A".into(),
            ports: BTreeMap::from([("P".into(), "BUS".into())]),
            ..Default::default()
        };
        let inst_b = SubcircuitInstance {
            subcircuit: "SubB".into(),
            prefix: "B".into(),
            ports: BTreeMap::from([("P".into(), "BUS".into())]),
            ..Default::default()
        };
        spec.instances = if first_a {
            vec![inst_a, inst_b]
        } else {
            vec![inst_b, inst_a]
        };
        if conflict_class {
            spec.subcircuits
                .get_mut("SubA")
                .unwrap()
                .nets
                .get_mut("P")
                .unwrap()
                .class = "HS".into();
        }
        spec
    };

    let ir_ab = BoardIr::from_spec(&make_spec(true, false)).unwrap();
    let ir_ba = BoardIr::from_spec(&make_spec(false, false)).unwrap();
    for ir in [&ir_ab, &ir_ba] {
        assert_eq!(ir.nets["BUS"].class, "PWR");
        assert!(ir.nets["BUS"].ignore_route);
        assert!(ir.nets["BUS"].power_driven);
    }

    let err = BoardIr::from_spec(&make_spec(true, true)).unwrap_err();
    assert!(
        err.contains("BUS") && err.contains("HS") && err.contains("PWR"),
        "{err}"
    );
}

#[test]
fn subcircuit_prefix_private_net_and_match_group_collisions() {
    let base_toml = r#"
name = "collisions"
size_nm = [40000000, 30000000]

[footprints.R0402]
kind = "chip2_pin"
package = "0402"

[subcircuits.Empty]
ports = []

[subcircuits.Unit]
ports = []
[[subcircuits.Unit.placements]]
refdes = "R1"
footprint = "R0402"
pos = { x = 0, y = 0 }
[subcircuits.Unit.nets]
SW = ["R1.1"]
"#;
    let base: BoardSpec = toml::from_str(base_toml).unwrap();

    // Duplicate normalized prefixes at top-level even with 0 placements.
    let mut dup_top = base.clone();
    dup_top.instances = vec![
        SubcircuitInstance {
            subcircuit: "Empty".into(),
            prefix: "CH1".into(),
            ..Default::default()
        },
        SubcircuitInstance {
            subcircuit: "Empty".into(),
            prefix: "CH1_".into(),
            ..Default::default()
        },
    ];
    assert!(BoardIr::from_spec(&dup_top).is_err());

    // Duplicate normalized prefixes inside sub.instances.
    let mut dup_nested = base.clone();
    dup_nested.subcircuits.insert(
        "Parent".into(),
        SubcircuitSpec {
            instances: vec![
                SubcircuitInstance {
                    subcircuit: "Empty".into(),
                    prefix: "X".into(),
                    ..Default::default()
                },
                SubcircuitInstance {
                    subcircuit: "Empty".into(),
                    prefix: "X_".into(),
                    ..Default::default()
                },
            ],
            ..Default::default()
        },
    );
    assert!(BoardIr::from_spec(&dup_nested).is_err());

    // Private net colliding with top-level net (via spec.nets, placement.pin_nets, and instance ports).
    for kind in 0..3 {
        let mut s = base.clone();
        s.instances.push(SubcircuitInstance {
            subcircuit: "Unit".into(),
            prefix: "PSU".into(),
            ..Default::default()
        });
        match kind {
            0 => {
                s.nets.insert("PSU_SW".into(), NetDef::default());
            }
            1 => {
                s.placements.push(Placement {
                    refdes: "R_EXT".into(),
                    footprint: "R0402".into(),
                    pin_nets: BTreeMap::from([("1".into(), "PSU_SW".into())]),
                    ..Default::default()
                });
            }
            _ => {
                s.subcircuits.insert(
                    "Sink".into(),
                    SubcircuitSpec {
                        ports: vec!["IN".into()],
                        ..Default::default()
                    },
                );
                s.instances.push(SubcircuitInstance {
                    subcircuit: "Sink".into(),
                    prefix: "S".into(),
                    ports: BTreeMap::from([("IN".into(), "PSU_SW".into())]),
                    ..Default::default()
                });
            }
        }
        assert!(
            BoardIr::from_spec(&s).is_err(),
            "expected board collision #{kind}"
        );
    }

    // Cross-instance prefix-boundary private net collision: PSU + 1_FB vs PSU_1 + FB.
    let mut cross = base.clone();
    cross.subcircuits.insert(
        "SubA".into(),
        SubcircuitSpec {
            placements: vec![Placement {
                refdes: "R1".into(),
                footprint: "R0402".into(),
                pin_nets: BTreeMap::from([("1".into(), "1_FB".into())]),
                ..Default::default()
            }],
            ..Default::default()
        },
    );
    cross.subcircuits.insert(
        "SubB".into(),
        SubcircuitSpec {
            placements: vec![Placement {
                refdes: "R1".into(),
                footprint: "R0402".into(),
                pin_nets: BTreeMap::from([("1".into(), "FB".into())]),
                ..Default::default()
            }],
            ..Default::default()
        },
    );
    cross.instances = vec![
        SubcircuitInstance {
            subcircuit: "SubA".into(),
            prefix: "PSU".into(),
            ..Default::default()
        },
        SubcircuitInstance {
            subcircuit: "SubB".into(),
            prefix: "PSU_1".into(),
            ..Default::default()
        },
    ];
    assert!(BoardIr::from_spec(&cross).is_err());

    // Parent/child prefix-boundary private net collision: CH1 + LDO_FB vs CH1 -> LDO + FB.
    let mut nested_col = base.clone();
    nested_col.subcircuits.insert(
        "Child".into(),
        SubcircuitSpec {
            placements: vec![Placement {
                refdes: "R1".into(),
                footprint: "R0402".into(),
                pin_nets: BTreeMap::from([("1".into(), "FB".into())]),
                ..Default::default()
            }],
            ..Default::default()
        },
    );
    nested_col.subcircuits.insert(
        "Outer".into(),
        SubcircuitSpec {
            placements: vec![Placement {
                refdes: "R0".into(),
                footprint: "R0402".into(),
                pin_nets: BTreeMap::from([("1".into(), "LDO_FB".into())]),
                ..Default::default()
            }],
            instances: vec![SubcircuitInstance {
                subcircuit: "Child".into(),
                prefix: "LDO".into(),
                ..Default::default()
            }],
            ..Default::default()
        },
    );
    nested_col.instances = vec![SubcircuitInstance {
        subcircuit: "Outer".into(),
        prefix: "CH1".into(),
        ..Default::default()
    }];
    assert!(BoardIr::from_spec(&nested_col).is_err());

    // Colliding expanded match_groups (against top-level and across instances).
    let mut mg_top = base.clone();
    mg_top.constraints.match_groups.push(MatchGroupRule {
        name: "PSU_MG".into(),
        nets: vec!["A".into(), "B".into()],
        tolerance_nm: mm(1.0),
        target_nm: None,
    });
    mg_top.subcircuits.insert(
        "WithMg".into(),
        SubcircuitSpec {
            constraints: Constraints {
                match_groups: vec![MatchGroupRule {
                    name: "MG".into(),
                    nets: vec!["A".into(), "B".into()],
                    tolerance_nm: mm(1.0),
                    target_nm: None,
                }],
                ..Default::default()
            },
            ..Default::default()
        },
    );
    mg_top.instances.push(SubcircuitInstance {
        subcircuit: "WithMg".into(),
        prefix: "PSU".into(),
        ..Default::default()
    });
    assert!(BoardIr::from_spec(&mg_top).is_err());

    let mut mg_cross = base;
    mg_cross.subcircuits.insert(
        "MgA".into(),
        SubcircuitSpec {
            constraints: Constraints {
                match_groups: vec![MatchGroupRule {
                    name: "1_MG".into(),
                    nets: vec!["A".into(), "B".into()],
                    tolerance_nm: mm(1.0),
                    target_nm: None,
                }],
                ..Default::default()
            },
            ..Default::default()
        },
    );
    mg_cross.subcircuits.insert(
        "MgB".into(),
        SubcircuitSpec {
            constraints: Constraints {
                match_groups: vec![MatchGroupRule {
                    name: "MG".into(),
                    nets: vec!["A".into(), "B".into()],
                    tolerance_nm: mm(1.0),
                    target_nm: None,
                }],
                ..Default::default()
            },
            ..Default::default()
        },
    );
    mg_cross.instances = vec![
        SubcircuitInstance {
            subcircuit: "MgA".into(),
            prefix: "PSU".into(),
            ..Default::default()
        },
        SubcircuitInstance {
            subcircuit: "MgB".into(),
            prefix: "PSU_1".into(),
            ..Default::default()
        },
    ];
    assert!(BoardIr::from_spec(&mg_cross).is_err());
}

#[test]
fn bga_shadow_allow_passives_handles_subcircuit_prefixes() {
    let make_board = |bottom_refs: &[&str]| -> BoardIr {
        let mut footprints = BTreeMap::new();
        footprints.insert(
            "BGA4".into(),
            FootprintSpec::Bga {
                rows: 4,
                cols: 4,
                pitch_nm: mm(1.0),
                ball_dia_nm: mm(0.4),
                pin_types: BTreeMap::new(),
                pin_names: BTreeMap::new(),
            },
        );
        footprints.insert(
            "P0402".into(),
            FootprintSpec::Chip2Pin {
                package: "0402".into(),
                pin_types: BTreeMap::new(),
                pin_names: BTreeMap::new(),
            },
        );
        let mut placements = vec![Placement {
            refdes: "U1".into(),
            footprint: "BGA4".into(),
            pos: Pt::new(mm(15.0), mm(15.0)),
            side: Side::Top,
            ..Default::default()
        }];
        for (i, &r) in bottom_refs.iter().enumerate() {
            placements.push(Placement {
                refdes: r.into(),
                footprint: "P0402".into(),
                pos: Pt::new(mm(15.0), mm(15.0 + 0.01 * i as f64)),
                side: Side::Bottom,
                ..Default::default()
            });
        }
        let spec = BoardSpec {
            name: "bga_passive_prefix".into(),
            size_nm: Some([mm(30.0), mm(30.0)]),
            footprints,
            placements,
            constraints: Constraints {
                bga_shadows: vec![BgaShadowRule {
                    bga_refdes: "U1".into(),
                    min_clearance_nm: mm(0.5),
                    allow_passives: true,
                }],
                ..Default::default()
            },
            ..Default::default()
        };
        BoardIr::from_spec(&spec).unwrap()
    };

    let passives = [
        "MCU_C1",
        "PSU_R1",
        "MCU_C_OUT",
        "CH1_LDO_C_OUT",
        "CH1_LDO_R_FB2",
        "C_BYP1",
        "C1",
        "R_FB",
        "L1",
        "FB1",
    ];
    let ir_ok = make_board(&passives);
    let drc_ok = check_board_drc(&ir_ok);
    let shadow_ok: Vec<_> = drc_ok
        .violations
        .iter()
        .filter(|v| v.kind == ViolationKind::BgaShadow)
        .collect();
    assert!(
        shadow_ok.is_empty(),
        "passives failed BGA shadow: {shadow_ok:?}"
    );

    let non_passives = [
        "CH1_D1", "RF_J1", "C1_D1", "C_D1", "D1", "J1", "LED1", "CONN1", "LDO1",
    ];
    let ir_bad = make_board(&non_passives);
    let drc_bad = check_board_drc(&ir_bad);
    let flagged: Vec<&str> = drc_bad
        .violations
        .iter()
        .filter(|v| v.kind == ViolationKind::BgaShadow)
        .map(|v| v.item_a.as_str())
        .collect();
    for np in non_passives {
        assert!(
            flagged.contains(&np),
            "expected BgaShadow for non-passive {np}, got {flagged:?}"
        );
    }
}

#[test]
fn subcircuit_validation_edge_cases() {
    let mut base = BoardSpec {
        name: "edge".into(),
        size_nm: Some([mm(30.0), mm(20.0)]),
        footprints: BTreeMap::from([(
            "R0402".into(),
            FootprintSpec::Chip2Pin {
                package: "0402".into(),
                pin_types: BTreeMap::new(),
                pin_names: BTreeMap::new(),
            },
        )]),
        placements: vec![Placement {
            refdes: "R_OUTER".into(),
            footprint: "R0402".into(),
            pos: Pt::new(mm(5.0), mm(5.0)),
            ..Default::default()
        }],
        ..Default::default()
    };
    base.subcircuits.insert(
        "Leaf".into(),
        SubcircuitSpec {
            placements: vec![Placement {
                refdes: "R1".into(),
                footprint: "R0402".into(),
                pos: Pt::new(0, 0),
                ..Default::default()
            }],
            ..Default::default()
        },
    );

    // Unknown local refdes in sub.nets (typo or matching outer board placement).
    for bad_ref in ["NO_SUCH", "R_OUTER"] {
        let mut s = base.clone();
        s.subcircuits.get_mut("Leaf").unwrap().nets.insert(
            "N".into(),
            NetDef {
                pins: vec![(bad_ref.into(), "1".into())],
                ..Default::default()
            },
        );
        s.instances.push(SubcircuitInstance {
            subcircuit: "Leaf".into(),
            prefix: "U".into(),
            ..Default::default()
        });
        assert!(
            BoardIr::from_spec(&s).is_err(),
            "expected error for {bad_ref}"
        );
    }

    // Prefixes with internal whitespace.
    for bad_prefix in ["_ _", "A B"] {
        let mut s = base.clone();
        s.instances.push(SubcircuitInstance {
            subcircuit: "Leaf".into(),
            prefix: bad_prefix.into(),
            ..Default::default()
        });
        assert!(
            BoardIr::from_spec(&s).is_err(),
            "expected error for '{bad_prefix}'"
        );
    }

    // Alphabetically bottom-up deep chain (> 32) rejected even when uninstantiated.
    let mut chain = base.clone();
    chain.subcircuits.clear();
    chain
        .subcircuits
        .insert("S00".into(), SubcircuitSpec::default());
    for i in 1..=33usize {
        chain.subcircuits.insert(
            format!("S{i:02}"),
            SubcircuitSpec {
                instances: vec![SubcircuitInstance {
                    subcircuit: format!("S{:02}", i - 1),
                    prefix: "P".into(),
                    ..Default::default()
                }],
                ..Default::default()
            },
        );
    }
    assert!(BoardIr::from_spec(&chain).is_err());
    chain.subcircuits.remove("S33");
    assert!(BoardIr::from_spec(&chain).is_ok());

    // Exponential diamond DAG (depth 14 <= 32, 2^14 = 16384 instances > 4096 cap) fails fast.
    let mut diamond = base;
    diamond.subcircuits.clear();
    diamond
        .subcircuits
        .insert("D14".into(), SubcircuitSpec::default());
    for i in (0..14usize).rev() {
        let child = format!("D{:02}", i + 1);
        diamond.subcircuits.insert(
            format!("D{i:02}"),
            SubcircuitSpec {
                instances: vec![
                    SubcircuitInstance {
                        subcircuit: child.clone(),
                        prefix: "L".into(),
                        ..Default::default()
                    },
                    SubcircuitInstance {
                        subcircuit: child,
                        prefix: "R".into(),
                        ..Default::default()
                    },
                ],
                ..Default::default()
            },
        );
    }
    diamond.instances.push(SubcircuitInstance {
        subcircuit: "D00".into(),
        prefix: "TOP".into(),
        ..Default::default()
    });
    assert!(BoardIr::from_spec(&diamond).is_err());
}

#[test]
fn direct_footprint_spec_resolve_lowers_pin_names() {
    let fp_spec = FootprintSpec::Qfn {
        pins_per_side: 4,
        pitch_nm: 500_000,
        body_nm: 3_000_000,
        ep_nm: 1_500_000,
        pin_types: BTreeMap::from([
            ("OUT".into(), PinType::Out),
            ("GND".into(), PinType::PowerOut),
        ]),
        pin_names: BTreeMap::from([
            ("OUT".into(), vec!["1".into()]),
            ("GND".into(), vec!["EP".into()]),
        ]),
    };
    let fp = fp_spec.resolve("U_QFN").unwrap();
    assert_eq!(fp.pin_types.get("1"), Some(&PinType::Out));
    assert_eq!(fp.pin_types.get("EP"), Some(&PinType::PowerOut));

    let bad = FootprintSpec::Chip2Pin {
        package: "0402".into(),
        pin_types: BTreeMap::new(),
        pin_names: BTreeMap::from([("BAD".into(), vec!["99".into()])]),
    };
    assert!(bad.resolve("R_BAD").is_err());
}

#[test]
fn subcircuit_pin_nets_conflict_and_multipad_alias_split() {
    let base_toml = r#"
name = "pin_conflicts"
size_nm = [30000000, 20000000]

[footprints.BGA]
kind = "bga"
rows = 2
cols = 2
pitch_nm = 800000
ball_dia_nm = 400000
pin_names = { VDD = ["A1", "B2"], SIG = "A2" }
"#;
    let base: BoardSpec = toml::from_str(base_toml).unwrap();

    // Multi-pad alias partial split between [nets] and placement.pin_nets (override & disconnect).
    for override_val in ["GND", ""] {
        let mut s = base.clone();
        s.placements.push(Placement {
            refdes: "U1".into(),
            footprint: "BGA".into(),
            pos: Pt::new(mm(10.0), mm(10.0)),
            pin_nets: BTreeMap::from([("A1".into(), override_val.into())]),
            ..Default::default()
        });
        s.nets.insert(
            "PWR".into(),
            NetDef {
                pins: vec![("U1".into(), "VDD".into())],
                ..Default::default()
            },
        );
        assert!(
            BoardIr::from_spec(&s).is_err(),
            "expected multi-pad alias split error for '{override_val}'"
        );
    }

    // Subcircuit placement pin_nets vs sub.nets conflict (direct, via alias, and disconnect).
    for (pl_pin, pl_net) in [("A2", "OTHER"), ("SIG", "OTHER"), ("A2", "")] {
        let mut s = base.clone();
        s.subcircuits.insert(
            "Block".into(),
            SubcircuitSpec {
                placements: vec![Placement {
                    refdes: "U1".into(),
                    footprint: "BGA".into(),
                    pin_nets: BTreeMap::from([(pl_pin.into(), pl_net.into())]),
                    ..Default::default()
                }],
                nets: BTreeMap::from([(
                    "N_SUB".into(),
                    NetDef {
                        pins: vec![("U1".into(), "SIG".into())],
                        ..Default::default()
                    },
                )]),
                ..Default::default()
            },
        );
        s.instances.push(SubcircuitInstance {
            subcircuit: "Block".into(),
            prefix: "B1".into(),
            ..Default::default()
        });
        assert!(
            BoardIr::from_spec(&s).is_err(),
            "expected subcircuit pin_nets vs sub.nets conflict for ({pl_pin}, {pl_net})"
        );
    }
}

#[test]
fn spec_roundtrip_private_port_qualified_lib_and_subcircuit_drc() {
    let dir = tmp_test_dir("roundtrip_drc");
    let lib = dir.join("custom.pretty");
    fs::create_dir_all(&lib).unwrap();
    fs::write(
        lib.join("LIB_IC.kicad_mod"),
        r#"(footprint "LIB_IC"
  (version 20241229) (layer "F.Cu")
  (pad "1" smd rect (at -1 0) (size 0.5 0.5) (layers "F.Cu" "F.Mask" "F.Paste"))
  (pad "2" smd rect (at 1 0) (size 0.5 0.5) (layers "F.Cu" "F.Mask" "F.Paste"))
)"#,
    )
    .unwrap();

    let toml_spec = r#"
name = "roundtrip_drc"
size_nm = [40000000, 30000000]
footprint_libs = ["custom.pretty"]

[footprints.LIB_IC.pin_names]
IN = "1"
OUT = "2"

[footprints.BGA2]
kind = "bga"
rows = 2
cols = 2
pitch_nm = 1000000
ball_dia_nm = 400000
pin_names = { P1 = "A1", P2 = "B2" }

[footprints.R0402]
kind = "chip2_pin"
package = "0402"

[subcircuits.Inner]
ports = ["BUS_PORT"]
[[subcircuits.Inner.placements]]
refdes = "U_LIB"
footprint = "custom:LIB_IC"
pos = { x = 0, y = 0 }
[subcircuits.Inner.nets]
BUS_PORT = ["U_LIB.IN"]

[subcircuits.Outer]
ports = []
[[subcircuits.Outer.placements]]
refdes = "U_BGA"
footprint = "BGA2"
pos = { x = 0, y = 0 }
side = "top"
[[subcircuits.Outer.placements]]
refdes = "D1"
footprint = "R0402"
pos = { x = 0, y = 0 }
side = "bottom"
[[subcircuits.Outer.instances]]
subcircuit = "Inner"
prefix = "IN1"
ports = { BUS_PORT = "PRIV_BUS" }
pos = { x = 5000000, y = 0 }
[subcircuits.Outer.nets]
PRIV_BUS = ["U_BGA.P1"]
SIG_A = ["U_BGA.P2"]
SIG_B = ["D1.1"]

[[subcircuits.Outer.constraints.bga_shadows]]
bga_refdes = "U_BGA"
min_clearance_nm = 500000
allow_passives = true

[[subcircuits.Outer.constraints.min_vias]]
net = "PRIV_BUS"
min_vias = 1

[[subcircuits.Outer.constraints.match_groups]]
name = "PAIR"
nets = ["SIG_A", "SIG_B"]
tolerance_nm = 500000
target_nm = 2000000

[[instances]]
subcircuit = "Outer"
prefix = "CH1"
pos = { x = 20000000, y = 15000000 }
"#;

    let spec_toml: BoardSpec = toml::from_str(toml_spec).unwrap();
    let json = serde_json::to_string(&spec_toml).unwrap();
    let spec_json: BoardSpec = serde_json::from_str(&json).unwrap();
    assert_eq!(spec_json, spec_toml);

    let mut ir = BoardIr::from_spec_with_base(&spec_json, Some(&dir)).unwrap();
    // Child port bound to parent's private net PRIV_BUS resolves to CH1_PRIV_BUS.
    assert_eq!(
        ir.nets["CH1_PRIV_BUS"].pins,
        vec![
            ("CH1_IN1_U_LIB".into(), "1".into()),
            ("CH1_U_BGA".into(), "A1".into()),
        ]
    );
    assert!(ir.footprints.contains_key("custom:LIB_IC"));

    // Expanded bga_shadows, min_vias, and match_groups all trigger under check_board_drc.
    let drc = check_board_drc(&ir);
    let kinds: Vec<ViolationKind> = drc.violations.iter().map(|v| v.kind).collect();
    assert!(kinds.contains(&ViolationKind::BgaShadow), "{kinds:?}");
    assert!(kinds.contains(&ViolationKind::MinVias), "{kinds:?}");
    assert!(kinds.contains(&ViolationKind::MatchGroup), "{kinds:?}");

    // Satisfy expanded constraints (move D1 out of BGA shadow, add via on CH1_PRIV_BUS, add matched tracks) and re-check.
    ir.placements
        .iter_mut()
        .find(|p| p.refdes == "CH1_D1")
        .unwrap()
        .pos = Pt::new(mm(35.0), mm(15.0));
    ir.vias.push(Via {
        net: "CH1_PRIV_BUS".into(),
        pos: Pt::new(mm(22.0), mm(15.0)),
        diameter: 450_000,
        drill: 200_000,
        layers: ("F.Cu".into(), "B.Cu".into()),
        locked: false,
        tented: true,
    });
    ir.tracks.push(Track {
        net: "CH1_SIG_A".into(),
        layer: "F.Cu".into(),
        a: Pt::new(mm(10.0), mm(5.0)),
        b: Pt::new(mm(12.0), mm(5.0)),
        width: 150_000,
        locked: false,
    });
    ir.tracks.push(Track {
        net: "CH1_SIG_B".into(),
        layer: "B.Cu".into(),
        a: Pt::new(mm(10.0), mm(8.0)),
        b: Pt::new(mm(12.0), mm(8.0)),
        width: 150_000,
        locked: false,
    });
    let drc_after = check_board_drc(&ir);
    assert!(
        !drc_after.violations.iter().any(|v| matches!(
            v.kind,
            ViolationKind::BgaShadow | ViolationKind::MinVias | ViolationKind::MatchGroup
        )),
        "expected satisfied constraints, got {:?}",
        drc_after.violations
    );
    let _ = fs::remove_dir_all(&dir);
}
