use std::collections::BTreeMap;
use std::fs;
use std::path::PathBuf;
use std::process::Command;

use boardc::ir::{
    BoardIr, BoardSpec, FootprintSpec, NetDef, Placement, Pt, SchematicMeta, Side, load_board_json,
    load_spec_file, mm,
};
use boardc::kicad_export::render_kicad_pcb;
use boardc::kicad_io::{parse_kicad_net, parse_kicad_pcb};

fn test_dir(name: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("boardc_issue5_{name}_{}", std::process::id()));
    let _ = fs::remove_dir_all(&dir);
    fs::create_dir_all(&dir).unwrap();
    dir
}

fn sample_kicad_mod(name: &str, pad_names: &[&str]) -> String {
    let mut s = format!(
        "(footprint \"{name}\"\n  (version 20241229)\n  (layer \"F.Cu\")\n  (fp_rect (start -1.0 -0.5) (end 1.0 0.5) (stroke (width 0.1) (type solid)) (fill no) (layer \"F.Fab\"))\n  (fp_rect (start -1.4 -0.9) (end 1.4 0.9) (stroke (width 0.05) (type solid)) (fill no) (layer \"F.CrtYd\"))\n"
    );
    for (idx, pin) in pad_names.iter().enumerate() {
        let x = -0.6 + idx as f64 * 1.2;
        s.push_str(&format!(
            "  (pad \"{pin}\" smd rect (at {x:.3} 0) (size 0.5 0.6) (layers \"F.Cu\" \"F.Mask\" \"F.Paste\"))\n"
        ));
    }
    s.push_str(")\n");
    s
}

#[test]
fn parse_kicad_net_covers_components_subsheets_multi_unit_fields_and_nets() {
    let src = r#"(export
  (version "E")
  (design
    (source "draco.kicad_sch")
    (sheet
      (number "1")
      (name "/")
      (tstamps "/")
      (title_block (source "draco.kicad_sch"))
    )
    (sheet
      (number "2")
      (name "/Power/")
      (tstamps "/11111111-2222-3333-4444-555555555555/")
      (title_block (source "power.kicad_sch"))
    )
  )
  (components
    (comp
      (ref "U1")
      (value "STM32H7")
      (footprint "draco:ST_VFBGA-169_6x6mm_P0.4mm")
      (datasheet "https://example.com/stm32.pdf")
      (fields
        (field (name "Footprint") "draco:ST_VFBGA-169_6x6mm_P0.4mm")
        (field (name "Datasheet") "https://example.com/stm32.pdf")
        (field (name "Description") "ARM Cortex-M7 MCU")
        (field (name "MPN") "STM32H743")
        (field (name "LCSC") "C123456")
      )
      (property (name "MPN") (value "STM32H743"))
      (property (name "LCSC") (value "C123456"))
      (property (name "Sheetname") (value ""))
      (property (name "Sheetfile") (value "draco.kicad_sch"))
      (property (name "ki_keywords") (value "mcu arm"))
      (sheetpath (names "/") (tstamps "/"))
      (tstamps "aaaaaaaa-bbbb-cccc-dddd-eeeeeeeeeeee")
    )
    (comp
      (ref "R1")
      (value "10k")
      (footprint "draco:R_0402")
      (fields
        (field (name "Footprint") "draco:R_0402")
        (field (name "Datasheet"))
      )
      (property (name "Sheetname") (value "Power"))
      (property (name "Sheetfile") (value "power.kicad_sch"))
      (property (name "dnp"))
      (property (name "exclude_from_bom"))
      (sheetpath (names "/Power/") (tstamps "/11111111-2222-3333-4444-555555555555/"))
      (tstamps "bbbbbbbb-1111-2222-3333-444444444444" "cccccccc-1111-2222-3333-444444444444")
    )
    (comp
      (ref "C1")
      (value "100nF")
      (footprint "Capacitor_SMD:C_0402")
      (sheetpath (names "/") (tstamps "/"))
      (tstamp "5A238DF8")
    )
  )
  (nets
    (net (code "1") (name "+3V3") (class "POWER")
      (node (ref "U1") (pin "A1") (pintype "power_in"))
      (node (ref "R1") (pin "1") (pintype "passive"))
      (node (ref "R1") (pin "1") (pintype "passive"))
    )
    (net (code "2") (name "unconnected-(U1-NC-PadA2)") (class "Default")
      (node (ref "U1") (pin "A2") (pintype "no_connect"))
    )
  )
)"#;

    let netlist = parse_kicad_net(src).expect("parse_kicad_net");
    assert_eq!(netlist.components.len(), 3);

    let u1 = &netlist.components["U1"];
    assert_eq!(u1.value, "STM32H7");
    assert_eq!(u1.footprint, "draco:ST_VFBGA-169_6x6mm_P0.4mm");
    assert_eq!(
        u1.path.as_deref(),
        Some("/aaaaaaaa-bbbb-cccc-dddd-eeeeeeeeeeee")
    );
    assert_eq!(u1.sheetname.as_deref(), Some("/"));
    assert_eq!(u1.sheetfile.as_deref(), Some("draco.kicad_sch"));
    assert_eq!(u1.mpn.as_deref(), Some("STM32H743"));
    assert_eq!(u1.lcsc.as_deref(), Some("C123456"));
    assert!(!u1.dnp);
    assert!(!u1.exclude_from_bom);
    assert_eq!(
        u1.fields.get("Datasheet").map(String::as_str),
        Some("https://example.com/stm32.pdf")
    );
    assert_eq!(
        u1.fields.get("Description").map(String::as_str),
        Some("ARM Cortex-M7 MCU")
    );
    assert!(!u1.fields.contains_key("ki_keywords"));

    let r1 = &netlist.components["R1"];
    assert_eq!(
        r1.path.as_deref(),
        Some("/11111111-2222-3333-4444-555555555555/bbbbbbbb-1111-2222-3333-444444444444")
    );
    assert_eq!(r1.sheetname.as_deref(), Some("/Power/"));
    assert_eq!(r1.sheetfile.as_deref(), Some("power.kicad_sch"));
    assert!(r1.dnp);
    assert!(r1.exclude_from_bom);

    let c1 = &netlist.components["C1"];
    assert_eq!(c1.path.as_deref(), Some("/5A238DF8"));
    assert_eq!(c1.sheetfile.as_deref(), Some("draco.kicad_sch"));

    assert_eq!(netlist.nets.len(), 2);
    assert_eq!(netlist.nets[0].name, "+3V3");
    assert_eq!(netlist.nets[0].class, "POWER");
    assert_eq!(
        netlist.nets[0].nodes,
        vec![("U1".into(), "A1".into()), ("R1".into(), "1".into())]
    );
    assert_eq!(netlist.nets[1].name, "unconnected-(U1-NC-PadA2)");

    // Reject non-export root and duplicate component refs.
    assert!(parse_kicad_net("(kicad_pcb (version 20241229))").is_err());
    let dup = r#"(export (version "E") (components (comp (ref "R1") (value "1k")) (comp (ref "R1") (value "2k"))))"#;
    assert!(parse_kicad_net(dup).is_err());
}

#[test]
fn board_spec_loads_kicad_mod_libs_and_netlist_preserving_schematic_parity() {
    let dir = test_dir("spec_merge");
    let lib_dir = dir.join("hardware/lib/draco.pretty");
    let root_lib = dir.join("hardware/kicad_libs");
    let cap_lib = root_lib.join("Capacitor_SMD.pretty");
    let out_dir = dir.join("hardware/outputs");
    fs::create_dir_all(&lib_dir).unwrap();
    fs::create_dir_all(&cap_lib).unwrap();
    fs::create_dir_all(&out_dir).unwrap();

    fs::write(
        lib_dir.join("ST_VFBGA-169_6x6mm_P0.4mm.kicad_mod"),
        sample_kicad_mod("ST_VFBGA-169_6x6mm_P0.4mm", &["A1", "A2"]),
    )
    .unwrap();
    fs::write(
        lib_dir.join("R_0402.kicad_mod"),
        sample_kicad_mod("R_0402", &["1", "2"]),
    )
    .unwrap();
    fs::write(
        cap_lib.join("C_0402_1005Metric.kicad_mod"),
        sample_kicad_mod("C_0402_1005Metric", &["1", "2"]),
    )
    .unwrap();
    let single_mod = dir.join("hardware/lib/Standalone_FP.kicad_mod");
    fs::write(&single_mod, sample_kicad_mod("Standalone_FP", &["1"])).unwrap();

    let net_path = out_dir.join("draco.net");
    fs::write(
        &net_path,
        r#"(export
  (version "E")
  (components
    (comp
      (ref "U1")
      (value "STM32H7")
      (footprint "draco:ST_VFBGA-169_6x6mm_P0.4mm")
      (datasheet "https://st.com/stm32.pdf")
      (fields
        (field (name "Footprint") "draco:ST_VFBGA-169_6x6mm_P0.4mm")
        (field (name "Datasheet") "https://st.com/stm32.pdf")
        (field (name "MPN") "STM32H7")
        (field (name "LCSC") "C11111")
      )
      (property (name "Sheetname") (value ""))
      (property (name "Sheetfile") (value "draco.kicad_sch"))
      (sheetpath (names "/") (tstamps "/"))
      (tstamps "00000000-0000-0000-0000-000000000001")
    )
    (comp
      (ref "R1")
      (value "10k")
      (footprint "draco:R_0402")
      (property (name "Sheetname") (value "Power"))
      (property (name "Sheetfile") (value "power.kicad_sch"))
      (property (name "dnp"))
      (property (name "exclude_from_bom"))
      (sheetpath (names "/Power/") (tstamps "/00000000-0000-0000-0000-0000000000aa/"))
      (tstamps "00000000-0000-0000-0000-000000000002")
    )
    (comp
      (ref "C1")
      (value "100nF")
      (footprint "Capacitor_SMD:C_0402_1005Metric")
      (property (name "Sheetname") (value ""))
      (property (name "Sheetfile") (value "draco.kicad_sch"))
      (sheetpath (names "/") (tstamps "/"))
      (tstamps "00000000-0000-0000-0000-000000000003")
    )
    (comp
      (ref "U_UNPLACED")
      (value "IGNORE_ME")
      (footprint "MissingLib:DoesNotExist")
      (sheetpath (names "/") (tstamps "/"))
      (tstamps "00000000-0000-0000-0000-000000000009")
    )
  )
  (nets
    (net (code "1") (name "VCC") (class "POWER")
      (node (ref "U1") (pin "A1"))
      (node (ref "R1") (pin "1"))
      (node (ref "C1") (pin "1"))
      (node (ref "U_UNPLACED") (pin "1"))
    )
    (net (code "2") (name "GND") (class "Default")
      (node (ref "R1") (pin "2"))
      (node (ref "C1") (pin "2"))
    )
    (net (code "3") (name "unconnected-(U1-NC-PadA2)") (class "Default")
      (node (ref "U1") (pin "A2"))
    )
  )
)"#,
    )
    .unwrap();

    let spec_toml = r#"
name = "draco"
size_nm = [40000000, 30000000]
footprint_libs = [
  "hardware/lib/draco.pretty",
  "hardware/kicad_libs",
  "hardware/lib/Standalone_FP.kicad_mod",
]
netlist = "hardware/outputs/draco.net"

[nets.GND]
name = "GND"
ignore_route = true

[[placements]]
refdes = "U1"
footprint = "ST_VFBGA-169_6x6mm_P0.4mm"
pos = { x = 17500000, y = 22500000 }

[[placements]]
refdes = "R1"
pos = { x = 8000000, y = 10000000 }

[[placements]]
refdes = "C1"
value = "220nF"
pos = { x = 12000000, y = 10000000 }
pin_nets = { "2" = "AGND" }

[[placements]]
refdes = "TP1"
footprint = "Standalone_FP"
pos = { x = 30000000, y = 10000000 }
"#;
    let spec_path = dir.join("board.toml");
    fs::write(&spec_path, spec_toml).unwrap();
    let spec = load_spec_file(&spec_path).unwrap();
    let board = BoardIr::from_spec_with_base(&spec, Some(&dir)).expect("from_spec_with_base");

    // 4 placed components only (U_UNPLACED is not auto-added and does not pollute nets).
    assert_eq!(board.placements.len(), 4);
    assert!(board.footprints.contains_key("ST_VFBGA-169_6x6mm_P0.4mm"));
    assert!(board.footprints.contains_key("draco:R_0402"));
    assert!(
        board
            .footprints
            .contains_key("Capacitor_SMD:C_0402_1005Metric")
    );
    assert!(board.footprints.contains_key("Standalone_FP"));

    let u1 = board.placements.iter().find(|p| p.refdes == "U1").unwrap();
    assert_eq!(u1.value, "STM32H7");
    assert_eq!(u1.mpn.as_deref(), Some("STM32H7"));
    assert_eq!(u1.lcsc.as_deref(), Some("C11111"));
    assert_eq!(u1.pin_nets.get("A1").map(String::as_str), Some("VCC"));
    assert_eq!(
        u1.pin_nets.get("A2").map(String::as_str),
        Some("unconnected-(U1-NC-PadA2)")
    );
    let u1_sch = u1.schematic.as_ref().expect("U1 schematic metadata");
    assert_eq!(
        u1_sch.footprint.as_deref(),
        Some("draco:ST_VFBGA-169_6x6mm_P0.4mm")
    );
    assert_eq!(
        u1_sch.path.as_deref(),
        Some("/00000000-0000-0000-0000-000000000001")
    );
    assert_eq!(u1_sch.sheetname.as_deref(), Some("/"));
    assert_eq!(u1_sch.sheetfile.as_deref(), Some("draco.kicad_sch"));

    let r1 = board.placements.iter().find(|p| p.refdes == "R1").unwrap();
    assert_eq!(r1.footprint, "draco:R_0402");
    assert_eq!(r1.value, "10k");
    assert!(r1.dnp);
    assert!(r1.exclude_from_bom);
    assert_eq!(
        r1.schematic.as_ref().and_then(|s| s.path.as_deref()),
        Some("/00000000-0000-0000-0000-0000000000aa/00000000-0000-0000-0000-000000000002")
    );
    assert_eq!(
        r1.schematic.as_ref().and_then(|s| s.sheetname.as_deref()),
        Some("/Power/")
    );
    assert_eq!(
        r1.schematic.as_ref().and_then(|s| s.sheetfile.as_deref()),
        Some("power.kicad_sch")
    );

    // Explicit BoardSpec overrides beat netlist values and pin_nets.
    let c1 = board.placements.iter().find(|p| p.refdes == "C1").unwrap();
    assert_eq!(c1.value, "220nF");
    assert_eq!(c1.pin_nets.get("1").map(String::as_str), Some("VCC"));
    assert_eq!(c1.pin_nets.get("2").map(String::as_str), Some("AGND"));
    assert!(!board.nets["GND"].pins.contains(&("C1".into(), "2".into())));
    assert!(board.nets["AGND"].pins.contains(&("C1".into(), "2".into())));
    assert_eq!(board.nets["VCC"].class, "POWER");
    assert!(
        !board.nets["VCC"]
            .pins
            .iter()
            .any(|(r, _)| r == "U_UNPLACED")
    );
    assert!(board.nets["GND"].ignore_route);

    // Export to .kicad_pcb and verify full KiCad schematic metadata + round-trip import.
    let rendered = render_kicad_pcb(&board);
    assert!(rendered.contains("(footprint \"draco:ST_VFBGA-169_6x6mm_P0.4mm\""));
    assert!(rendered.contains("(path \"/00000000-0000-0000-0000-000000000001\")"));
    assert!(rendered.contains("(sheetname \"/\")"));
    assert!(rendered.contains("(sheetfile \"draco.kicad_sch\")"));
    assert!(rendered.contains("(property \"Datasheet\" \"https://st.com/stm32.pdf\""));
    assert!(rendered.contains("(property \"LCSC\" \"C11111\""));
    assert!(rendered.contains("(property \"MPN\" \"STM32H7\""));
    assert!(rendered.contains(
        "(path \"/00000000-0000-0000-0000-0000000000aa/00000000-0000-0000-0000-000000000002\")"
    ));
    assert!(rendered.contains("(sheetname \"/Power/\")"));
    assert!(rendered.contains("(sheetfile \"power.kicad_sch\")"));
    assert!(rendered.contains("(attr smd exclude_from_bom dnp)"));

    let reimported = parse_kicad_pcb(&rendered, None).expect("reimport exported pcb");
    let u1_rt = reimported
        .placements
        .iter()
        .find(|p| p.refdes == "U1")
        .unwrap();
    assert_eq!(u1_rt.schematic, u1.schematic);
    assert_eq!(u1_rt.mpn, u1.mpn);
    assert_eq!(u1_rt.lcsc, u1.lcsc);
    let r1_rt = reimported
        .placements
        .iter()
        .find(|p| p.refdes == "R1")
        .unwrap();
    assert_eq!(r1_rt.schematic, r1.schematic);
    assert!(r1_rt.dnp);
    assert!(r1_rt.exclude_from_bom);

    let _ = fs::remove_dir_all(&dir);
}

#[test]
fn footprint_lib_errors_precedence_and_path_traversal_guards() {
    let dir = test_dir("guards");
    let lib_dir = dir.join("lib.pretty");
    fs::create_dir_all(&lib_dir).unwrap();
    fs::write(
        lib_dir.join("R_0402.kicad_mod"),
        sample_kicad_mod("R_0402", &["1", "2"]),
    )
    .unwrap();
    // Place an outside .kicad_mod that a path-traversal footprint name could try to reach.
    fs::write(
        dir.join("outside.kicad_mod"),
        sample_kicad_mod("outside", &["1"]),
    )
    .unwrap();

    // Missing footprint_libs entry errors cleanly.
    let bad_lib = BoardSpec {
        name: "bad_lib".into(),
        size_nm: Some([mm(20.0), mm(20.0)]),
        footprint_libs: vec!["does_not_exist.pretty".into()],
        ..Default::default()
    };
    assert!(BoardIr::from_spec_with_base(&bad_lib, Some(&dir)).is_err());

    // Missing netlist errors cleanly.
    let bad_net = BoardSpec {
        name: "bad_net".into(),
        size_nm: Some([mm(20.0), mm(20.0)]),
        netlist: Some("missing.net".into()),
        ..Default::default()
    };
    assert!(BoardIr::from_spec_with_base(&bad_net, Some(&dir)).is_err());

    // Path traversal in footprint name is rejected even though ../outside.kicad_mod exists.
    for bad_fp in ["../outside", "lib:../outside", "../lib:R_0402"] {
        let trav = BoardSpec {
            name: "trav".into(),
            size_nm: Some([mm(20.0), mm(20.0)]),
            footprint_libs: vec!["lib.pretty".into()],
            placements: vec![Placement {
                refdes: "U1".into(),
                footprint: bad_fp.into(),
                pos: Pt::new(mm(5.0), mm(5.0)),
                ..Default::default()
            }],
            ..Default::default()
        };
        assert!(
            BoardIr::from_spec_with_base(&trav, Some(&dir)).is_err(),
            "expected error for traversal footprint '{bad_fp}'"
        );
    }

    // Empty placement footprint without netlist errors cleanly.
    let empty_fp = BoardSpec {
        name: "empty_fp".into(),
        size_nm: Some([mm(20.0), mm(20.0)]),
        placements: vec![Placement {
            refdes: "U1".into(),
            footprint: String::new(),
            pos: Pt::new(mm(5.0), mm(5.0)),
            ..Default::default()
        }],
        ..Default::default()
    };
    assert!(BoardIr::from_spec_with_base(&empty_fp, Some(&dir)).is_err());

    // Explicit [footprints] entry takes precedence over footprint_libs.
    let override_spec = BoardSpec {
        name: "override".into(),
        size_nm: Some([mm(20.0), mm(20.0)]),
        footprint_libs: vec!["lib.pretty".into()],
        footprints: BTreeMap::from([("R_0402".into(), FootprintSpec::Sot23 { pin_count: 3 })]),
        placements: vec![Placement {
            refdes: "R1".into(),
            footprint: "R_0402".into(),
            pos: Pt::new(mm(10.0), mm(10.0)),
            ..Default::default()
        }],
        ..Default::default()
    };
    let ir = BoardIr::from_spec_with_base(&override_spec, Some(&dir)).unwrap();
    assert_eq!(ir.footprints["R_0402"].pads.len(), 3);

    // Explicit BoardSpec nets.pins override netlist pin bindings cleanly.
    let net_path = dir.join("small.net");
    fs::write(
        &net_path,
        r#"(export (version "E")
          (components (comp (ref "R1") (value "1k") (footprint "lib:R_0402") (sheetpath (names "/") (tstamps "/")) (tstamps "u1")))
          (nets (net (code "1") (name "NET_FROM_SCH") (class "Default") (node (ref "R1") (pin "1"))))
        )"#,
    )
    .unwrap();
    let net_override = BoardSpec {
        name: "net_ov".into(),
        size_nm: Some([mm(20.0), mm(20.0)]),
        footprint_libs: vec!["lib.pretty".into()],
        netlist: Some("small.net".into()),
        nets: BTreeMap::from([(
            "SPEC_NET".into(),
            NetDef {
                name: "SPEC_NET".into(),
                class: "Default".into(),
                pins: vec![("R1".into(), "1".into())],
                ignore_route: false,
            },
        )]),
        placements: vec![Placement {
            refdes: "R1".into(),
            pos: Pt::new(mm(10.0), mm(10.0)),
            ..Default::default()
        }],
        ..Default::default()
    };
    let ir2 = BoardIr::from_spec_with_base(&net_override, Some(&dir)).unwrap();
    assert_eq!(
        ir2.placements[0].pin_nets.get("1").map(String::as_str),
        Some("SPEC_NET")
    );
    assert!(!ir2.nets.contains_key("NET_FROM_SCH"));

    // SchematicMeta round-trip when constructed manually on Placement.
    let mut manual_pl = Placement {
        refdes: "U1".into(),
        footprint: "R_0402".into(),
        pos: Pt::new(mm(5.0), mm(5.0)),
        side: Side::Top,
        schematic: Some(SchematicMeta {
            path: Some("/manual-uuid".into()),
            sheetname: Some("/".into()),
            sheetfile: Some("top.kicad_sch".into()),
            footprint: Some("CustomLib:R_0402".into()),
            fields: BTreeMap::from([("Vendor".into(), "Acme".into())]),
        }),
        ..Default::default()
    };
    manual_pl.value = "4.7k".into();
    let manual_ir = BoardIr::from_spec_with_base(
        &BoardSpec {
            name: "manual".into(),
            size_nm: Some([mm(20.0), mm(20.0)]),
            footprint_libs: vec!["lib.pretty".into()],
            placements: vec![manual_pl],
            ..Default::default()
        },
        Some(&dir),
    )
    .unwrap();
    let kicad = render_kicad_pcb(&manual_ir);
    assert!(kicad.contains("(footprint \"CustomLib:R_0402\""));
    assert!(kicad.contains("(path \"/manual-uuid\")"));
    assert!(kicad.contains("(property \"Vendor\" \"Acme\""));

    let _ = fs::remove_dir_all(&dir);
}

#[test]
fn cli_resolves_spec_relative_libs_and_netlist_for_toml_and_json_specs() {
    let dir = test_dir("cli");
    let sub = dir.join("proj");
    let lib_dir = sub.join("lib/custom.pretty");
    fs::create_dir_all(&lib_dir).unwrap();
    fs::write(
        lib_dir.join("R_0402.kicad_mod"),
        sample_kicad_mod("R_0402", &["1", "2"]),
    )
    .unwrap();
    fs::write(
        sub.join("design.net"),
        r#"(export (version "E")
          (components
            (comp (ref "R1") (value "4k7") (footprint "custom:R_0402")
              (property (name "Sheetfile") (value "design.kicad_sch"))
              (sheetpath (names "/") (tstamps "/"))
              (tstamps "12345678-1234-1234-1234-123456789abc"))
          )
          (nets
            (net (code "1") (name "SIG") (class "Default") (node (ref "R1") (pin "1")))
          )
        )"#,
    )
    .unwrap();

    let toml_spec = sub.join("board.toml");
    fs::write(
        &toml_spec,
        r#"
name = "cli_board"
size_nm = [20000000, 15000000]
footprint_libs = ["lib/custom.pretty"]
netlist = "design.net"

[[placements]]
refdes = "R1"
footprint = "R_0402"
pos = { x = 10000000, y = 7500000 }
"#,
    )
    .unwrap();

    // JSON BoardSpec that also contains outline/layers/footprints/placements (must NOT be misparsed as BoardIr!).
    let json_spec = sub.join("board.spec.json");
    fs::write(
        &json_spec,
        r#"{
  "name": "cli_json_board",
  "outline": [{"x": 0, "y": 0}, {"x": 20000000, "y": 0}, {"x": 20000000, "y": 15000000}, {"x": 0, "y": 15000000}],
  "layers": [{"name": "F.Cu", "kind": "signal", "side": "top"}],
  "footprints": {},
  "footprint_libs": ["lib/custom.pretty"],
  "netlist": "design.net",
  "placements": [{"refdes": "R1", "footprint": "R_0402", "pos": {"x": 10000000, "y": 7500000}}]
}"#,
    )
    .unwrap();

    let dumped_ir = dir.join("dumped.json");
    let out_pcb_toml = dir.join("from_toml.kicad_pcb");
    let out_pcb_json = dir.join("from_json.kicad_pcb");

    // Run from a different CWD (std::env::temp_dir()) to verify spec-file-relative path resolution.
    let status1 = Command::new(env!("CARGO_BIN_EXE_boardc"))
        .current_dir(std::env::temp_dir())
        .args([
            "dump-spec",
            "-i",
            toml_spec.to_str().unwrap(),
            "-o",
            dumped_ir.to_str().unwrap(),
        ])
        .status()
        .expect("boardc dump-spec");
    assert!(status1.success());
    let ir = load_board_json(&dumped_ir).expect("load dumped BoardIr");
    assert_eq!(ir.placements[0].value, "4k7");
    assert_eq!(
        ir.placements[0].pin_nets.get("1").map(String::as_str),
        Some("SIG")
    );

    let status2 = Command::new(env!("CARGO_BIN_EXE_boardc"))
        .current_dir(std::env::temp_dir())
        .args([
            "export-kicad",
            toml_spec.to_str().unwrap(),
            "-o",
            out_pcb_toml.to_str().unwrap(),
            "--no-pro",
        ])
        .status()
        .expect("boardc export-kicad toml");
    assert!(status2.success());
    let pcb_toml = fs::read_to_string(&out_pcb_toml).unwrap();
    assert!(pcb_toml.contains("(footprint \"custom:R_0402\""));
    assert!(pcb_toml.contains("(path \"/12345678-1234-1234-1234-123456789abc\")"));
    assert!(pcb_toml.contains("(sheetfile \"design.kicad_sch\")"));

    let status3 = Command::new(env!("CARGO_BIN_EXE_boardc"))
        .current_dir(std::env::temp_dir())
        .args([
            "export-kicad",
            json_spec.to_str().unwrap(),
            "-o",
            out_pcb_json.to_str().unwrap(),
            "--no-pro",
        ])
        .status()
        .expect("boardc export-kicad json spec");
    assert!(status3.success());
    let pcb_json = fs::read_to_string(&out_pcb_json).unwrap();
    assert!(pcb_json.contains("(footprint \"custom:R_0402\""));
    assert!(pcb_json.contains("(path \"/12345678-1234-1234-1234-123456789abc\")"));

    let _ = fs::remove_dir_all(&dir);
}
