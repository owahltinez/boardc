use std::collections::BTreeMap;
use std::fs;
use std::path::PathBuf;
use std::process::Command;

use boardc::ir::{
    AutoRefdes, BoardIr, BoardSpec, FabConfig, FootprintSpec, NetDef, Placement, Pt, SchematicMeta,
    Side, load_board_json, load_spec_file, mm,
};
use boardc::kicad_export::{render_kicad_pcb, render_kicad_pro};
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
    assert!(rendered.contains("(attr exclude_from_bom dnp)"));

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
        footprints: BTreeMap::from([(
            "R_0402".into(),
            FootprintSpec::Sot23 {
                pin_count: 3,
                pin_types: Default::default(),
            },
        )]),
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
                pins: vec![("R1".into(), "1".into())],
                ..Default::default()
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

    let run_boardc = |args: &[&str]| {
        Command::new(env!("CARGO_BIN_EXE_boardc"))
            .current_dir(std::env::temp_dir())
            .args(args)
            .status()
            .expect("run boardc")
    };

    // Run from a different CWD (std::env::temp_dir()) to verify spec-file-relative path resolution.
    assert!(
        run_boardc(&[
            "dump-spec",
            "-i",
            toml_spec.to_str().unwrap(),
            "-o",
            dumped_ir.to_str().unwrap(),
        ])
        .success()
    );
    let ir = load_board_json(&dumped_ir).expect("load dumped BoardIr");
    assert_eq!(ir.placements[0].value, "4k7");
    assert_eq!(
        ir.placements[0].pin_nets.get("1").map(String::as_str),
        Some("SIG")
    );

    assert!(
        run_boardc(&[
            "export-kicad",
            toml_spec.to_str().unwrap(),
            "-o",
            out_pcb_toml.to_str().unwrap(),
            "--no-pro",
        ])
        .success()
    );
    let pcb_toml = fs::read_to_string(&out_pcb_toml).unwrap();
    assert!(pcb_toml.contains("(footprint \"custom:R_0402\""));
    assert!(pcb_toml.contains("(path \"/12345678-1234-1234-1234-123456789abc\")"));
    assert!(pcb_toml.contains("(sheetfile \"design.kicad_sch\")"));

    assert!(
        run_boardc(&[
            "export-kicad",
            json_spec.to_str().unwrap(),
            "-o",
            out_pcb_json.to_str().unwrap(),
            "--no-pro",
        ])
        .success()
    );
    let pcb_json = fs::read_to_string(&out_pcb_json).unwrap();
    assert!(pcb_json.contains("(footprint \"custom:R_0402\""));
    assert!(pcb_json.contains("(path \"/12345678-1234-1234-1234-123456789abc\")"));

    // JSON BoardSpec without size_nm/footprint_libs/netlist must still go through spec validation.
    let plain_json_spec = sub.join("plain.spec.json");
    fs::write(
        &plain_json_spec,
        r#"{
  "name": "plain_json",
  "outline": [{"x": 0, "y": 0}, {"x": 20000000, "y": 0}, {"x": 20000000, "y": 15000000}, {"x": 0, "y": 15000000}],
  "layers": [],
  "footprints": {},
  "placements": []
}"#,
    )
    .unwrap();
    let plain_out = dir.join("plain.kicad_pcb");
    assert!(
        run_boardc(&[
            "export-kicad",
            plain_json_spec.to_str().unwrap(),
            "-o",
            plain_out.to_str().unwrap(),
            "--no-pro",
        ])
        .success()
    );
    let plain_pcb = fs::read_to_string(&plain_out).unwrap();
    assert!(plain_pcb.contains("(0 \"F.Cu\" signal)"));
    assert!(plain_pcb.contains("(2 \"B.Cu\" signal)"));

    // Direct BoardIr JSON with an empty placement footprint must be rejected.
    let bad_ir = dir.join("bad_ir.json");
    fs::write(
        &bad_ir,
        r#"{
  "name": "bad",
  "outline": [{"x": 0, "y": 0}, {"x": 10000000, "y": 0}, {"x": 10000000, "y": 10000000}, {"x": 0, "y": 10000000}],
  "layers": [{"name": "F.Cu", "kind": "signal", "side": "top"}],
  "footprints": {},
  "filled_pours": [],
  "placements": [{"refdes": "U1", "pos": {"x": 5000000, "y": 5000000}}]
}"#,
    )
    .unwrap();
    assert!(load_board_json(&bad_ir).is_err());
    assert!(
        !run_boardc(&[
            "export-kicad",
            bad_ir.to_str().unwrap(),
            "-o",
            dir.join("bad.kicad_pcb").to_str().unwrap(),
            "--no-pro",
        ])
        .success()
    );

    let _ = fs::remove_dir_all(&dir);
}

#[test]
fn roast_regressions_sexp_escapes_lib_isolation_net_sync_and_parity() {
    // 1. KiCad vD unquoted #PWR01 atoms, trailing backslash, quotes, newlines, multi-class, and deep sheets.
    let vd_net = r#"(export
  (version D)
  (design
    (sheet (number 1) (name /A/B/C/) (tstamps /s1/s2/s3) (title_block (source deep.kicad_sch)))
  )
  (components
    (comp (ref #PWR01) (value GND) (footprint Power:GND) (sheetpath (names /) (tstamps //)) (tstamps "" "u_pwr1 u_pwr2"))
    (comp (ref " U1 ") (value "10k\\")
      (footprint "ACME_v1.2:R_0402")
      (fields
        (field (name "Dir") "C:\\proj\\")
        (field (name "Note") "line1\nline2\t\"q\"")
        (field (name "MPN") "OLD_MPN")
        (field (name "LCSC") "C11111")
      )
      (property (name "dnp") (value "no"))
      (property (name "exclude_from_bom") (value "false"))
      (property (name "exclude_from_board") (value "0"))
      (property (name "Sheetname") (value "SubSheet"))
      (sheetpath (names "") (tstamps "/s1/s2/s3/"))
      (tstamps "" "u_deep1")
    )
    (comp (ref "X1") (value "EXCL") (footprint "ACME_v1.2:R_0402")
      (property (name "exclude_from_board"))
      (sheetpath (names "/") (tstamps "/"))
      (tstamps "u_excl")
    )
  )
  (nets
    (net (code 1) (name " +5V ") (class "Default, POWER")
      (node (ref #PWR01) (pin 1))
      (node (ref " U1 ") (pin " 1 "))
    )
    (net (code 2) (name "SIG") (class ",,,")
      (node (ref " U1 ") (pin "2"))
    )
    (net (code 3) (name "   ") (class "Default")
      (node (ref " U1 ") (pin "3"))
    )
  )
)"#;
    let parsed = parse_kicad_net(vd_net).expect("parse vD + escapes");
    assert!(parsed.components.contains_key("#PWR01"));
    assert_eq!(parsed.components["#PWR01"].path.as_deref(), Some("/u_pwr1"));
    let u1 = &parsed.components["U1"];
    assert_eq!(u1.value, "10k\\");
    assert_eq!(u1.fields.get("Dir").map(String::as_str), Some("C:\\proj\\"));
    assert_eq!(
        u1.fields.get("Note").map(String::as_str),
        Some("line1\nline2\t\"q\"")
    );
    assert!(!u1.dnp && !u1.exclude_from_bom && !u1.exclude_from_board);
    assert_eq!(u1.path.as_deref(), Some("/s1/s2/s3/u_deep1"));
    assert_eq!(u1.sheetname.as_deref(), Some("/A/B/C/"));
    assert_eq!(u1.sheetfile.as_deref(), Some("deep.kicad_sch"));
    assert!(parsed.components["X1"].exclude_from_board);
    assert_eq!(parsed.nets.len(), 2);
    assert_eq!(parsed.nets[0].name, "+5V");
    assert_eq!(parsed.nets[0].class, "POWER");
    assert_eq!(parsed.nets[1].class, "Default");

    // 2. Library isolation, dotted flat dir, uppercase .KICAD_MOD, qualified fallback for bare name, drive prefix rejection.
    let dir = test_dir("roast");
    let lib_a = dir.join("LibA.pretty");
    let lib_b = dir.join("LibB.pretty");
    let dotted = dir.join("ACME_v1.2");
    let kicad_root = dir.join("kicad_root");
    let cap_pretty = kicad_root.join("Capacitor_SMD.pretty");
    fs::create_dir_all(&lib_a).unwrap();
    fs::create_dir_all(&lib_b).unwrap();
    fs::create_dir_all(&dotted).unwrap();
    fs::create_dir_all(&cap_pretty).unwrap();
    fs::write(
        lib_b.join("R_0402.kicad_mod"),
        sample_kicad_mod("R_0402", &["1", "2"]),
    )
    .unwrap();
    fs::write(
        dotted.join("R_0402.KICAD_MOD"),
        sample_kicad_mod("R_0402", &["1", "2"]),
    )
    .unwrap();
    fs::write(
        cap_pretty.join("C_0402.kicad_mod"),
        sample_kicad_mod("C_0402", &["1", "2"]),
    )
    .unwrap();
    let hole_mod = r#"(footprint "MountingHole_3mm"
  (version 20241229) (layer "F.Cu")
  (pad "" np_thru_hole circle (at 0 0) (size 3.0 3.0) (drill 3.0) (layers "*.Cu" "*.Mask"))
)"#;
    fs::write(lib_a.join("MountingHole_3mm.kicad_mod"), hole_mod).unwrap();
    fs::write(dir.join("not_a_mod.txt"), "hello").unwrap();

    // LibA.pretty exists in footprint_libs without R_0402 -> must NOT steal LibB.pretty/R_0402.
    let cross_lib = BoardSpec {
        name: "cross".into(),
        size_nm: Some([mm(20.0), mm(20.0)]),
        footprint_libs: vec!["LibB.pretty".into(), "LibA.pretty".into()],
        placements: vec![Placement {
            refdes: "R1".into(),
            footprint: "LibA:R_0402".into(),
            pos: Pt::new(mm(5.0), mm(5.0)),
            ..Default::default()
        }],
        ..Default::default()
    };
    assert!(BoardIr::from_spec_with_base(&cross_lib, Some(&dir)).is_err());

    // Parent root dir named "footprints" containing only *.pretty subdirs must NOT shadow aliased libs (e.g. footprints:R_0402 in LibB.pretty).
    let parent_fp = dir.join("footprints");
    fs::create_dir_all(parent_fp.join("Other.pretty")).unwrap();
    let aliased = BoardSpec {
        name: "aliased".into(),
        size_nm: Some([mm(20.0), mm(20.0)]),
        footprint_libs: vec!["LibB.pretty".into(), "footprints".into()],
        placements: vec![Placement {
            refdes: "R1".into(),
            footprint: "footprints:R_0402".into(),
            pos: Pt::new(mm(5.0), mm(5.0)),
            ..Default::default()
        }],
        ..Default::default()
    };
    assert!(BoardIr::from_spec_with_base(&aliased, Some(&dir)).is_ok());

    // Drive-prefix / multi-colon names, empty paths, non-.kicad_mod files, duplicate refdes all rejected.
    for bad_fp in ["lib:C:evil", "a:b:c", "C:evil"] {
        let bad = BoardSpec {
            name: "bad_fp".into(),
            size_nm: Some([mm(20.0), mm(20.0)]),
            footprint_libs: vec!["LibB.pretty".into()],
            placements: vec![Placement {
                refdes: "R1".into(),
                footprint: bad_fp.into(),
                pos: Pt::new(mm(5.0), mm(5.0)),
                ..Default::default()
            }],
            ..Default::default()
        };
        assert!(BoardIr::from_spec_with_base(&bad, Some(&dir)).is_err());
    }
    assert!(
        BoardIr::from_spec_with_base(
            &BoardSpec {
                name: "empty_lib".into(),
                size_nm: Some([mm(20.0), mm(20.0)]),
                footprint_libs: vec!["".into()],
                ..Default::default()
            },
            Some(&dir),
        )
        .is_err()
    );
    assert!(
        BoardIr::from_spec_with_base(
            &BoardSpec {
                name: "bad_file_lib".into(),
                size_nm: Some([mm(20.0), mm(20.0)]),
                footprint_libs: vec!["not_a_mod.txt".into()],
                ..Default::default()
            },
            Some(&dir),
        )
        .is_err()
    );
    assert!(
        BoardIr::from_spec_with_base(
            &BoardSpec {
                name: "dup_ref".into(),
                size_nm: Some([mm(20.0), mm(20.0)]),
                footprint_libs: vec!["LibB.pretty".into()],
                placements: vec![
                    Placement {
                        refdes: "R1".into(),
                        footprint: "R_0402".into(),
                        pos: Pt::new(mm(5.0), mm(5.0)),
                        ..Default::default()
                    },
                    Placement {
                        refdes: "R1".into(),
                        footprint: "R_0402".into(),
                        pos: Pt::new(mm(10.0), mm(5.0)),
                        ..Default::default()
                    },
                ],
                ..Default::default()
            },
            Some(&dir),
        )
        .is_err()
    );

    // 3. Full merge: dotted dir + uppercase .KICAD_MOD + bare name into parent *.pretty root +
    //    MPN/LCSC override sync + empty pin disconnect + stale spec.nets prune + schematic MountingHole +
    //    undefined netclass in .kicad_pro + escaped string round-trip.
    let net_path = dir.join("roast.net");
    fs::write(
        &net_path,
        format!(
            "{}\n",
            vd_net.replace(
                "(comp (ref \"X1\")",
                r#"(comp (ref "C1") (value "100nF") (footprint "Capacitor_SMD:C_0402") (sheetpath (names "/") (tstamps "/")) (tstamps "u_c1"))
    (comp (ref "H1") (value "M3") (footprint "LibA:MountingHole_3mm") (property (name "exclude_from_bom")) (sheetpath (names "/") (tstamps "/")) (tstamps "u_h1"))
    (comp (ref "X1")"#
            )
        ),
    )
    .unwrap();

    // Placing exclude_from_board component X1 must error.
    let excl_spec = BoardSpec {
        name: "excl".into(),
        size_nm: Some([mm(30.0), mm(20.0)]),
        footprint_libs: vec!["ACME_v1.2".into()],
        netlist: Some("roast.net".into()),
        placements: vec![Placement {
            refdes: "X1".into(),
            pos: Pt::new(mm(5.0), mm(5.0)),
            ..Default::default()
        }],
        ..Default::default()
    };
    assert!(BoardIr::from_spec_with_base(&excl_spec, Some(&dir)).is_err());

    let merged_spec = BoardSpec {
        name: "roast_ok".into(),
        size_nm: Some([mm(40.0), mm(30.0)]),
        footprint_libs: vec![
            "ACME_v1.2".into(),
            "kicad_root".into(),
            "LibA.pretty".into(),
        ],
        netlist: Some("roast.net".into()),
        nets: BTreeMap::from([(
            "OLD_SPEC_NET".into(),
            NetDef {
                name: "OLD_SPEC_NET".into(),
                pins: vec![("U1".into(), "1".into())],
                ..Default::default()
            },
        )]),
        placements: vec![
            Placement {
                refdes: "U1".into(),
                pos: Pt::new(mm(10.0), mm(10.0)),
                mpn: Some("NEW_MPN".into()),
                lcsc: Some("C99999".into()),
                // Pin 1 overrides OLD_SPEC_NET; Pin 2 explicitly disconnects from netlist SIG.
                pin_nets: BTreeMap::from([("1".into(), "+5V".into()), ("2".into(), "".into())]),
                schematic: Some(SchematicMeta {
                    fields: BTreeMap::from([
                        ("".into(), "bad".into()),
                        ("ki_keywords".into(), "dropme".into()),
                        ("EmptyVal".into(), "".into()),
                    ]),
                    ..Default::default()
                }),
                ..Default::default()
            },
            Placement {
                // Bare name matched via netlist's Capacitor_SMD:C_0402 inside kicad_root/Capacitor_SMD.pretty.
                refdes: "C1".into(),
                footprint: "C_0402".into(),
                pos: Pt::new(mm(20.0), mm(10.0)),
                ..Default::default()
            },
            Placement {
                refdes: "H1".into(),
                pos: Pt::new(mm(30.0), mm(10.0)),
                ..Default::default()
            },
        ],
        ..Default::default()
    };
    let board = BoardIr::from_spec_with_base(&merged_spec, Some(&dir)).expect("merged_spec");
    // No empty net leaked, pin 2 disconnected, stale OLD_SPEC_NET pin pruned.
    assert!(!board.nets.contains_key(""));
    assert!(!board.nets.contains_key("SIG"));
    assert!(board.nets["OLD_SPEC_NET"].pins.is_empty());
    assert_eq!(board.nets["+5V"].class, "POWER");
    assert_eq!(board.footprints["C_0402"].pads.len(), 2);

    // .kicad_pro must define fallback POWER netclass so netclass_assignments has no dangling ref.
    let pro_json: serde_json::Value = serde_json::from_str(&render_kicad_pro(&board)).unwrap();
    let class_names: Vec<&str> = pro_json["net_settings"]["classes"]
        .as_array()
        .unwrap()
        .iter()
        .filter_map(|c| c["name"].as_str())
        .collect();
    assert!(class_names.contains(&"POWER"));

    // Export + re-import round-trips escaped backslash/newline/quotes, MPN/LCSC overrides, and drops invalid fields.
    let pcb = render_kicad_pcb(&board);
    assert!(!pcb.contains("(property \"\""));
    assert!(!pcb.contains("ki_keywords"));
    assert!(!pcb.contains("EmptyVal"));
    // Schematic-linked NPTH MountingHole H1 must NOT emit `board_only`.
    let h1_block = pcb
        .split("(footprint \"LibA:MountingHole_3mm\"")
        .nth(1)
        .unwrap()
        .split("\n\t)\n")
        .next()
        .unwrap();
    assert!(!h1_block.contains("board_only"), "H1 block:\n{h1_block}");
    assert!(h1_block.contains("(attr exclude_from_bom)"));

    let rt = parse_kicad_pcb(&pcb, None).expect("round-trip escaped pcb");
    let u1_rt = rt.placements.iter().find(|p| p.refdes == "U1").unwrap();
    assert_eq!(u1_rt.value, "10k\\");
    assert_eq!(u1_rt.mpn.as_deref(), Some("NEW_MPN"));
    assert_eq!(u1_rt.lcsc.as_deref(), Some("C99999"));
    let rt_fields = &u1_rt.schematic.as_ref().unwrap().fields;
    assert_eq!(rt_fields.get("Dir").map(String::as_str), Some("C:\\proj\\"));
    assert_eq!(
        rt_fields.get("Note").map(String::as_str),
        Some("line1\nline2\t\"q\"")
    );

    let _ = fs::remove_dir_all(&dir);
}

#[test]
fn export_kicad_preserves_kicad_mod_fidelity_across_sides_rotations_and_roundtrip() {
    use boardc::geom::pad_to_world_shape;

    let dir = test_dir("raw_fidelity");
    let lib_dir = dir.join("RichLib.pretty");
    fs::create_dir_all(&lib_dir).unwrap();

    let rich_mod = r#"(footprint "RichPart"
  (version 20260206)
  (generator "kicad-footprint-generator")
  (generator_version "10.0")
  (layer "F.Cu")
  (descr "Rich SMD test part")
  (tags "rich smd arc")
  (solder_mask_margin 0.05)
  (property "Reference" "REF**" (at 0 -1.5 90) (layer "F.SilkS") (uuid "11111111-1111-1111-1111-111111111111") (effects (font (size 1 1) (thickness 0.15)) (justify left)))
  (property "Value" "RichPart" (at 0 1.5 0) (layer "F.Fab") (hide yes) (uuid "22222222-2222-2222-2222-222222222222") (effects (font (size 0.8 0.8) (thickness 0.12))))
  (property "Datasheet" "" (at 0 0 0) (layer "F.Fab") (hide yes) (uuid "33333333-3333-3333-3333-333333333333") (effects (font (size 1.27 1.27) (thickness 0.15))))
  (attr smd allow_missing_courtyard)
  (duplicate_pad_numbers_are_jumpers no)
  (fp_arc (start -1.0 -0.8) (mid 0.0 -1.2) (end 1.0 -0.8) (stroke (width 0.12) (type solid)) (layer "F.SilkS"))
  (fp_rect (start -1.6 -1.4) (end 1.6 1.4) (stroke (width 0.05) (type solid)) (fill no) (layer "F.CrtYd"))
  (fp_text user "${REFERENCE}" (at 0 0 0) (layer "F.Fab") (effects (font (size 0.5 0.5) (thickness 0.1))))
  (zone (layer "F.Cu") (keepout (tracks not_allowed) (vias not_allowed) (pads allowed) (copperpour not_allowed) (footprints allowed))
    (polygon (pts (xy -0.4 -0.4) (xy 0.4 -0.4) (xy 0.4 0.4) (xy -0.4 0.4))))
  (pad "1" smd roundrect (at -0.8 -0.4 30) (size 0.6 0.4) (layers "F.Cu" "F.Mask" "F.Paste") (roundrect_rratio 0.25))
  (pad "2" smd custom (at 0.8 0.4 15) (size 0.4 0.4) (property pad_prop_heatsink) (layers "F.Cu" "F.Mask")
    (options (clearance outline) (anchor rect))
    (primitives (gr_poly (pts (xy -0.2 -0.2) (xy 0.2 -0.2) (xy 0 0.3)) (width 0) (fill yes))))
  (pad "" np_thru_hole circle (at 0 -0.9) (size 0.5 0.5) (drill 0.5) (layers "*.Cu" "*.Mask"))
  (embedded_fonts no)
  (model "${KICAD10_3DMODEL_DIR}/RichLib.3dshapes/RichPart.step" (offset (xyz 0 0 0.1)) (scale (xyz 1 1 1)) (rotate (xyz 0 0 90)))
)"#;
    fs::write(lib_dir.join("RichPart.kicad_mod"), rich_mod).unwrap();

    let legacy_mod = r#"(module "LegacyMod" (layer "F.Cu")
  (fp_text reference "REF**" (at 0 -1.0) (layer "F.SilkS") (effects (font (size 1 1) (thickness 0.15))))
  (fp_text value "LegacyMod" (at 0 1.0) (layer "F.Fab") (effects (font (size 1 1) (thickness 0.15))))
  (fp_arc (start 0 -0.5) (end 0.5 0) (angle 90) (width 0.12) (layer "F.SilkS"))
  (fp_rect (start -1.0 -0.8) (end 1.0 0.8) (stroke (width 0.05) (type solid)) (fill no) (layer "F.CrtYd"))
  (pad "1" smd rect (at -0.5 0) (size 0.4 0.5) (layers "F.Cu" "F.Mask" "F.Paste"))
)"#;
    fs::write(lib_dir.join("LegacyMod.kicad_mod"), legacy_mod).unwrap();

    let fptext_mod = r#"(footprint "FpTextPart"
  (version 20211014)
  (layer "F.Cu")
  (fp_text reference "REF**" (at 0 -1.0) (layer "F.SilkS") (effects (font (size 1 1) (thickness 0.15))))
  (fp_text value "FpTextPart" (at 0 1.0) (layer "F.Fab") (effects (font (size 1 1) (thickness 0.15))))
  (fp_rect (start -1.0 -0.8) (end 1.0 0.8) (stroke (width 0.05) (type solid)) (fill no) (layer "F.CrtYd"))
  (pad "1" smd rect (at -0.5 0) (size 0.4 0.5) (layers "F.Cu" "F.Mask" "F.Paste"))
)"#;
    fs::write(lib_dir.join("FpTextPart.kicad_mod"), fptext_mod).unwrap();

    let mut placements = Vec::new();
    let mut idx = 0usize;
    for side in [Side::Top, Side::Bottom] {
        for rot in [0.0, 90.0, 180.0, 270.0] {
            idx += 1;
            placements.push(Placement {
                refdes: format!("U{idx}"),
                footprint: "RichLib:RichPart".into(),
                value: format!("V{idx}"),
                pos: Pt::new(mm(10.0 + idx as f64 * 8.0), mm(15.0)),
                rot_deg: rot,
                side,
                locked: idx == 1,
                dnp: idx == 2,
                exclude_from_bom: idx == 2,
                exclude_from_pos: idx == 2,
                pin_nets: BTreeMap::from([("1".into(), "VCC".into()), ("2".into(), "GND".into())]),
                schematic: Some(SchematicMeta {
                    path: Some(format!("/path-{idx}")),
                    sheetname: Some("/".into()),
                    sheetfile: Some("top.kicad_sch".into()),
                    footprint: Some("RichLib:RichPart".into()),
                    fields: BTreeMap::from([
                        ("Datasheet".into(), "https://example.com/rich.pdf".into()),
                        ("MPN".into(), "RICH-01".into()),
                    ]),
                }),
                ..Default::default()
            });
        }
    }
    placements.push(Placement {
        refdes: "U_LEG".into(),
        footprint: "RichLib:LegacyMod".into(),
        value: "1k".into(),
        pos: Pt::new(mm(20.0), mm(30.0)),
        rot_deg: 90.0,
        side: Side::Bottom,
        ..Default::default()
    });
    placements.push(Placement {
        refdes: "U_FTX".into(),
        footprint: "RichLib:FpTextPart".into(),
        value: "2k".into(),
        pos: Pt::new(mm(25.0), mm(30.0)),
        rot_deg: 90.0,
        side: Side::Bottom,
        ..Default::default()
    });
    placements.push(Placement {
        refdes: "R_PARAM".into(),
        footprint: "C_0402".into(),
        value: "100nF".into(),
        pos: Pt::new(mm(30.0), mm(30.0)),
        ..Default::default()
    });

    let spec = BoardSpec {
        name: "raw_fidelity".into(),
        size_nm: Some([mm(120.0), mm(50.0)]),
        footprint_libs: vec!["RichLib.pretty".into()],
        footprints: BTreeMap::from([(
            "C_0402".into(),
            FootprintSpec::Chip2Pin {
                package: "0402".into(),
                pin_types: BTreeMap::new(),
            },
        )]),
        placements,
        ..Default::default()
    };

    let board = BoardIr::from_spec_with_base(&spec, Some(&dir)).expect("from_spec_with_base");
    assert!(board.footprints["RichLib:RichPart"].kicad_raw.is_some());
    assert!(board.footprints["RichLib:FpTextPart"].kicad_raw.is_some());
    assert!(board.footprints["RichLib:LegacyMod"].kicad_raw.is_none());
    assert!(board.footprints["C_0402"].kicad_raw.is_none());

    // Non-library footprints must omit `kicad_raw` in JSON so pipeline cache keys stay unchanged.
    let ir_json = serde_json::to_value(&board).unwrap();
    assert!(ir_json["footprints"]["C_0402"].get("kicad_raw").is_none());
    assert!(
        ir_json["footprints"]["RichLib:RichPart"]
            .get("kicad_raw")
            .is_some()
    );

    let pcb = render_kicad_pcb(&board);
    assert!(!pcb.contains("(version 20260206)\n\t(generator \"kicad-footprint-generator\")"));
    assert!(pcb.contains("(descr \"Rich SMD test part\")"));
    assert!(pcb.contains("(tags \"rich smd arc\")"));
    assert!(pcb.contains("(solder_mask_margin 0.05)"));
    assert!(pcb.contains("(duplicate_pad_numbers_are_jumpers no)"));
    assert!(pcb.contains("(embedded_fonts no)"));
    assert!(pcb.contains("(property pad_prop_heatsink)"));
    assert!(!pcb.contains("(property \"pad_prop_heatsink\")"));
    assert!(pcb.contains("(fp_text user \"${REFERENCE}\""));
    assert!(pcb.contains(
        "(attr smd allow_missing_courtyard exclude_from_pos_files exclude_from_bom dnp)"
    ));

    // Parametric C_0402 courtyard now emits fp_rect instead of 4 fp_lines.
    let c0402_block = pcb.split("(footprint \"C_0402\"").nth(1).unwrap();
    assert!(c0402_block.contains("(fp_rect "));
    assert!(c0402_block.contains("(layer \"F.CrtYd\")"));

    // Top 90 deg (U2): Reference at (0 -1.5 (90+90)=180), Value hidden at (0 1.5 90), Datasheet updated in-place (once).
    let u2_block = pcb
        .split("(property \"Reference\" \"U2\"")
        .nth(1)
        .unwrap()
        .split("(footprint ")
        .next()
        .unwrap();
    assert!(u2_block.contains("(at 0 -1.5 180)"));
    assert!(u2_block.contains("(layer \"F.SilkS\")"));
    assert!(
        u2_block.contains("(property \"Value\" \"V2\" (at 0 1.5 90) (layer \"F.Fab\") (hide yes)")
    );
    assert!(u2_block.contains("(property \"Datasheet\" \"https://example.com/rich.pdf\""));
    assert_eq!(u2_block.matches("(property \"Datasheet\"").count(), 1);
    assert!(u2_block.contains("(property \"MPN\" \"RICH-01\""));
    assert!(u2_block.contains("(path \"/path-2\")"));
    assert!(u2_block.contains("(fp_rect (start -1.6 -1.4) (end 1.6 1.4)"));
    assert!(u2_block.contains("(fp_arc (start -1.0 -0.8) (mid 0.0 -1.2) (end 1.0 -0.8)"));

    // Bottom 90 deg (U6): Y flipped, layers remapped F.*->B.*, Reference rot=(90-90)=0, justify left mirror.
    let u6_block = pcb
        .split("(property \"Reference\" \"U6\"")
        .nth(1)
        .unwrap()
        .split("(footprint ")
        .next()
        .unwrap();
    assert!(u6_block.contains("(at 0 1.5 0)"));
    assert!(u6_block.contains("(layer \"B.SilkS\")"));
    assert!(u6_block.contains("(justify left mirror)"));
    assert!(
        u6_block.contains("(property \"Value\" \"V6\" (at 0 -1.5 90) (layer \"B.Fab\") (hide yes)")
    );
    assert!(u6_block.contains("(fp_rect (start -1.6 1.4) (end 1.6 -1.4)"));
    assert!(u6_block.contains("(layer \"B.CrtYd\")"));
    assert!(u6_block.contains("(fp_arc (start -1.0 0.8) (mid 0.0 1.2) (end 1.0 0.8)"));
    assert!(u6_block.contains("(pad \"1\" smd roundrect\n\t\t\t(at -0.8 0.4 60)"));
    assert!(u6_block.contains("(pad \"2\" smd custom\n\t\t\t(at 0.8 -0.4 75)"));

    // Legacy KiCad 5 `(module ...)` falls back to structured emission (no 2-point fp_arc passthrough).
    let leg_block = pcb
        .split("(footprint \"RichLib:LegacyMod\"")
        .nth(1)
        .unwrap()
        .split("(footprint ")
        .next()
        .unwrap();
    assert!(leg_block.contains("(property \"Reference\" \"U_LEG\""));
    assert!(leg_block.contains("(property \"Value\" \"1k\""));
    assert!(!leg_block.contains("(angle 90)"));

    // KiCad 6+ fp_text footprint updates reference/value without adding duplicate property nodes.
    let ftx_block = pcb
        .split("(footprint \"RichLib:FpTextPart\"")
        .nth(1)
        .unwrap()
        .split("(footprint ")
        .next()
        .unwrap();
    assert!(ftx_block.contains("(fp_text reference \"U_FTX\" (at 0 1.0 90) (layer \"B.SilkS\")"));
    assert!(ftx_block.contains("(fp_text value \"2k\" (at 0 -1.0 90) (layer \"B.Fab\")"));
    assert!(!ftx_block.contains("(property \"Reference\""));
    assert!(!ftx_block.contains("(property \"Value\""));

    // Re-importing exported .kicad_pcb preserves exact world pad geometry and deduplicates U1..U8 into 1 FootprintDef.
    let rt = parse_kicad_pcb(&pcb, None).expect("reimport exported pcb");
    assert_eq!(rt.placements.len(), 11);
    assert!(rt.footprints.contains_key("RichLib:RichPart"));
    assert!(
        !rt.footprints
            .keys()
            .any(|k| k.starts_with("RichLib:RichPart#")),
        "all 8 RichPart placements across Top/Bottom and 0/90/180/270 must deduplicate: {:?}",
        rt.footprints.keys().collect::<Vec<_>>()
    );
    for idx in 1..=8 {
        let refdes = format!("U{idx}");
        let orig_pl = board
            .placements
            .iter()
            .find(|p| p.refdes == refdes)
            .unwrap();
        let orig_fp = &board.footprints[&orig_pl.footprint];
        let rt_pl = rt.placements.iter().find(|p| p.refdes == refdes).unwrap();
        let rt_fp = &rt.footprints[&rt_pl.footprint];
        assert_eq!(rt_pl.locked, orig_pl.locked);
        assert_eq!(rt_pl.dnp, orig_pl.dnp);
        assert_eq!(rt_pl.exclude_from_bom, orig_pl.exclude_from_bom);
        assert_eq!(rt_pl.exclude_from_pos, orig_pl.exclude_from_pos);
        assert_eq!(rt_pl.pin_nets, orig_pl.pin_nets);
        assert_eq!(rt_fp.courtyard, orig_fp.courtyard);
        assert_eq!(rt_fp.keepouts, orig_fp.keepouts);
        for (op, rp) in orig_fp.pads.iter().zip(&rt_fp.pads) {
            let os = pad_to_world_shape(op, orig_pl, 16);
            let rs = pad_to_world_shape(rp, rt_pl, 16);
            assert_eq!(os.bbox(), rs.bbox(), "{refdes} pad {}", op.name);
        }
    }

    let _ = fs::remove_dir_all(&dir);
}

#[test]
fn opportunistic_kicad_cli_zero_lib_footprint_mismatch() {
    let sys_libs = std::path::Path::new("/usr/share/kicad/footprints");
    if !sys_libs
        .join("Resistor_SMD.pretty/R_0402_1005Metric.kicad_mod")
        .is_file()
        || Command::new("kicad-cli").arg("--version").output().is_err()
    {
        return;
    }
    let dir = test_dir("kicad_cli_fidelity");
    fs::write(
        dir.join("fp-lib-table"),
        "(fp_lib_table (version 7)\n  (lib (name \"Resistor_SMD\")(type \"KiCad\")(uri \"/usr/share/kicad/footprints/Resistor_SMD.pretty\")(options \"\")(descr \"\"))\n  (lib (name \"Package_DFN_QFN\")(type \"KiCad\")(uri \"/usr/share/kicad/footprints/Package_DFN_QFN.pretty\")(options \"\")(descr \"\"))\n  (lib (name \"RF_Module\")(type \"KiCad\")(uri \"/usr/share/kicad/footprints/RF_Module.pretty\")(options \"\")(descr \"\"))\n  (lib (name \"Connector_FFC-FPC\")(type \"KiCad\")(uri \"/usr/share/kicad/footprints/Connector_FFC-FPC.pretty\")(options \"\")(descr \"\"))\n  (lib (name \"Package_LGA\")(type \"KiCad\")(uri \"/usr/share/kicad/footprints/Package_LGA.pretty\")(options \"\")(descr \"\"))\n  (lib (name \"Package_DIP\")(type \"KiCad\")(uri \"/usr/share/kicad/footprints/Package_DIP.pretty\")(options \"\")(descr \"\"))\n  (lib (name \"Symbol\")(type \"KiCad\")(uri \"/usr/share/kicad/footprints/Symbol.pretty\")(options \"\")(descr \"\"))\n)\n",
    )
    .unwrap();
    let mut placements = Vec::new();
    let mut idx = 0usize;
    for fp in [
        "Resistor_SMD:R_0402_1005Metric",
        "Package_DFN_QFN:QFN-24-1EP_4x4mm_P0.5mm_EP2.6x2.6mm",
        "RF_Module:Raytac_MDBT42Q",
        "Connector_FFC-FPC:Molex_502244-3330_1x33-1MP_P0.5mm_Horizontal",
        "Package_LGA:AMS_LGA-10-1EP_2.7x4mm_P0.6mm",
        "Package_DIP:DIP-24_18.0mmx34.29mm_W15.24mm",
        "Symbol:Smolhaj_Scale_0.1",
    ] {
        for side in [Side::Top, Side::Bottom] {
            for rot in [0.0, 90.0, 180.0, 270.0] {
                idx += 1;
                placements.push(Placement {
                    refdes: format!("U{idx}"),
                    footprint: fp.into(),
                    value: "VAL".into(),
                    pos: Pt::new(
                        mm(30.0 + (idx % 7) as f64 * 70.0),
                        mm(30.0 + (idx / 7) as f64 * 70.0),
                    ),
                    rot_deg: rot,
                    side,
                    ..Default::default()
                });
            }
        }
    }
    let spec = BoardSpec {
        name: "cli_fidelity".into(),
        size_nm: Some([mm(550.0), mm(650.0)]),
        footprint_libs: vec![sys_libs.to_string_lossy().into_owned()],
        placements,
        ..Default::default()
    };
    let board = BoardIr::from_spec(&spec).unwrap();
    let pcb_path = dir.join("board.kicad_pcb");
    let drc_path = dir.join("drc.json");
    fs::write(&pcb_path, render_kicad_pcb(&board)).unwrap();
    let status = Command::new("kicad-cli")
        .args([
            "pcb",
            "drc",
            "--severity-all",
            "--format",
            "json",
            "-o",
            drc_path.to_str().unwrap(),
            pcb_path.to_str().unwrap(),
        ])
        .status()
        .unwrap();
    assert!(status.success(), "kicad-cli pcb drc failed");
    let rep: serde_json::Value =
        serde_json::from_str(&fs::read_to_string(&drc_path).unwrap()).unwrap();
    let mismatches: Vec<_> = rep["violations"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|v| v["type"].as_str() == Some("lib_footprint_mismatch"))
        .collect();
    assert!(
        mismatches.is_empty(),
        "unexpected lib_footprint_mismatch: {mismatches:#?}"
    );
    let _ = fs::remove_dir_all(&dir);
}

#[test]
fn roast_raw_fp_fixes_zone_rect_delta_padstack_groups_bcu_textbox_dimension_and_stale_guard() {
    use boardc::ir::{Keepout, KeepoutKind};
    use boardc::kicad_io::parse_kicad_mod;

    let legacy_fp = parse_kicad_mod(r#"(module "Leg" (layer "F.Cu") (pad "1" smd rect (at 0 0) (size 0.5 0.5) (layers "F.Cu")))"#).unwrap();
    assert!(legacy_fp.kicad_raw.is_none());

    let dir = test_dir("roast_raw_fp");
    let lib_dir = dir.join("Edge.pretty");
    fs::create_dir_all(&lib_dir).unwrap();

    // Covers: zone + arc, rect_delta, padstack(layer ...), group(members ...), omitted attr,
    // fp_text_box (angle, no uuid), dimension (gr_text, height, override_value), private_layers,
    // net_tie_pad_groups, font face, and `(at ... unlocked)`.
    let edge_mod = r#"(footprint "EdgeCase"
  (version 20260206)
  (layer "F.Cu")
  (private_layers "User.2")
  (net_tie_pad_groups "1,2")
  (property "Reference" "REF**" (at 0 -2 30 unlocked) (layer "F.SilkS") (uuid "ref-old") (effects (font (face "Arial") (size 1 1) (thickness 0.15))))
  (property "Value" "EdgeCase" (at 0 2 unlocked) (layer "F.Fab") (uuid "val-old") (effects (font (size 1 1) (thickness 0.15))))
  (fp_line (start -1 -1) (end 1 -1) (stroke (width 0.12) (type solid)) (layer "F.SilkS") (uuid "line-old"))
  (fp_text_box "Box\nText" (start -1 -1) (end 1 1) (angle 30) (layer "F.SilkS") (effects (font (size 0.5 0.5) (thickness 0.1))))
  (dimension (type aligned) (layer "Dwgs.User") (uuid "dim-old")
    (pts (xy 0 0) (xy 4 0))
    (height 2.5)
    (format (prefix "") (suffix "") (units 3) (units_format 1) (precision 4) (override_value "Span"))
    (gr_text "Span" (at 2 -2.5 0) (layer "Dwgs.User") (uuid "dim-old") (effects (font (size 1 1) (thickness 0.15))))
  )
  (zone (layer "F.Cu") (uuid "zone-old")
    (keepout (tracks not_allowed) (vias not_allowed) (pads allowed) (copperpour not_allowed) (footprints allowed))
    (polygon (pts (xy 1 0) (arc (start 1 0) (mid 0 1) (end -1 0)) (xy 0 -1))))
  (pad "1" smd trapezoid (at -1 0.5 0) (size 1 0.5) (rect_delta 0.2 0.3) (layers "F.Cu" "F.Mask")
    (padstack (layer "F.Cu" (offset 0.1 0.2) (chamfer top_left) (rect_delta 0 0.3)))
    (uuid "pad1-old"))
  (group "" (uuid "grp-old") (members "line-old" "pad1-old"))
)"#;
    fs::write(lib_dir.join("EdgeCase.kicad_mod"), edge_mod).unwrap();

    // Board-only raw attr + B.Cu-authored footprint.
    let bo_mod = r#"(footprint "BoardOnly"
  (version 20260206) (layer "F.Cu")
  (attr board_only)
  (pad "1" np_thru_hole circle (at 0 0) (size 1 1) (drill 1) (layers "*.Cu" "*.Mask"))
)"#;
    fs::write(lib_dir.join("BoardOnly.kicad_mod"), bo_mod).unwrap();

    let bcu_mod = r#"(footprint "BCuPart"
  (version 20260206) (layer "B.Cu")
  (property "Reference" "REF**" (at 0 -2 30) (layer "B.SilkS") (uuid "bref") (effects (font (size 1 1) (thickness 0.15)) (justify mirror)))
  (property "Value" "BCuPart" (at 0 2 0) (layer "B.Fab") (uuid "bval") (effects (font (size 1 1) (thickness 0.15)) (justify mirror)))
  (attr smd)
  (pad "1" smd rect (at -1 0.5 30) (size 0.5 0.5) (layers "B.Cu" "B.Mask"))
)"#;
    fs::write(lib_dir.join("BCuPart.kicad_mod"), bcu_mod).unwrap();

    let spec = BoardSpec {
        name: "roast_raw".into(),
        size_nm: Some([mm(100.0), mm(80.0)]),
        footprint_libs: vec!["Edge.pretty".into()],
        placements: vec![
            Placement {
                refdes: "U_TOP".into(),
                footprint: "Edge:EdgeCase".into(),
                value: "V1".into(),
                pos: Pt::new(mm(20.0), mm(10.0)),
                rot_deg: 90.0,
                side: Side::Top,
                ..Default::default()
            },
            Placement {
                refdes: "U_BOT".into(),
                footprint: "Edge:EdgeCase".into(),
                value: "V2".into(),
                pos: Pt::new(mm(40.0), mm(10.0)),
                rot_deg: 90.0,
                side: Side::Bottom,
                dnp: true,
                ..Default::default()
            },
            Placement {
                refdes: "H_SCH".into(),
                footprint: "Edge:BoardOnly".into(),
                pos: Pt::new(mm(60.0), mm(10.0)),
                schematic: Some(SchematicMeta {
                    path: Some("/sch-h".into()),
                    ..Default::default()
                }),
                ..Default::default()
            },
            Placement {
                refdes: "B_BOT".into(),
                footprint: "Edge:BCuPart".into(),
                pos: Pt::new(mm(70.0), mm(10.0)),
                rot_deg: 90.0,
                side: Side::Bottom,
                ..Default::default()
            },
            Placement {
                refdes: "B_TOP".into(),
                footprint: "Edge:BCuPart".into(),
                pos: Pt::new(mm(80.0), mm(10.0)),
                rot_deg: 90.0,
                side: Side::Top,
                ..Default::default()
            },
        ],
        ..Default::default()
    };

    let mut board = BoardIr::from_spec_with_base(&spec, Some(&dir)).unwrap();
    let pcb = render_kicad_pcb(&board);

    let block_of = |refdes: &str| -> String {
        let key = format!("(property \"Reference\" \"{refdes}\"");
        let pos = pcb.find(&key).unwrap();
        let start = pcb[..pos].rfind("\t(footprint ").unwrap();
        let end = pcb[pos..].find("\n\t)\n").map(|o| pos + o + 4).unwrap();
        pcb[start..end].to_string()
    };

    let u_top = block_of("U_TOP");
    let u_bot = block_of("U_BOT");
    // Omitted (attr ...) on EdgeCase: U_TOP omits attr completely; U_BOT emits only `(attr dnp)`.
    assert!(!u_top.contains("(attr "));
    assert!(u_bot.contains("(attr dnp)"));
    // Quoted tags & unlocked preserved in `(at ...)`.
    assert!(u_top.contains("(private_layers \"User.2\")"));
    assert!(u_top.contains("(net_tie_pad_groups \"1,2\")"));
    assert!(u_top.contains("(face \"Arial\")"));
    assert!(u_top.contains("(override_value \"Span\")"));
    assert!(u_top.contains("(at 0 -2 120 unlocked)"));
    assert!(u_top.contains("(at 0 2 90 unlocked)"));
    // fp_text_box quoting + angle + uuid synthesis on Top/Bottom.
    assert!(u_top.contains("(fp_text_box \"Box\\nText\""));
    assert!(u_top.contains("(angle 120)"));
    assert!(u_bot.contains("(angle 60)"));
    assert!(u_bot.contains("(fp_text_box \"Box\\nText\" (start -1 1) (end 1 -1)"));
    assert!(u_bot.contains("(justify mirror)"));
    // dimension gr_text idx==1 quoted, height negated on Bottom, gr_text mirrored on Bottom.
    assert!(u_top.contains("(gr_text \"Span\" (at 2 -2.5 90)"));
    assert!(u_bot.contains("(height -2.5)"));
    assert!(u_bot.contains("(gr_text \"Span\" (at 2 2.5 90)"));
    // Zone transformed to board coords: U_TOP at board (20, 10) -> KiCad (20, -10), rot 90 CCW:
    // local (1, 0) -> (20, -11), (0, 1) -> (21, -10), (-1, 0) -> (20, -9).
    assert!(u_top.contains("(xy 20 -11)"));
    assert!(u_top.contains("(mid 21 -10)"));
    assert!(u_top.contains("(end 20 -9)"));
    // U_BOT at KiCad (40, -10), rot 90 CCW, Y-flipped:
    // local (0, 1) -> flip (0, -1) -> rot90 (-1, 0) -> (39, -10).
    assert!(u_bot.contains("(mid 39 -10)"));
    // rect_delta & padstack(layer ...) recursion on Bottom flip.
    assert!(u_bot.contains("(rect_delta 0.2 -0.3)"));
    assert!(u_bot.contains(
        "(padstack (layer \"B.Cu\" (offset 0.1 -0.2) (chamfer bottom_left) (rect_delta 0 -0.3))"
    ));
    // Group member UUIDs remapped to actual emitted line & pad UUIDs (no `line-old`/`pad1-old`).
    assert!(!u_bot.contains("line-old"));
    assert!(!u_bot.contains("pad1-old"));
    let line_uuid = u_bot
        .split("(fp_line ")
        .nth(1)
        .unwrap()
        .split("(uuid \"")
        .nth(1)
        .unwrap()
        .split('"')
        .next()
        .unwrap();
    let pad_uuid = u_bot
        .split("(pad \"1\"")
        .nth(1)
        .unwrap()
        .split("(uuid \"")
        .nth(1)
        .unwrap()
        .split('"')
        .next()
        .unwrap();
    let grp_line = u_bot.lines().find(|l| l.contains("(group ")).unwrap();
    assert!(grp_line.contains(line_uuid) && grp_line.contains(pad_uuid));

    // Schematic-linked MountingHole strips `board_only`.
    let h_sch = block_of("H_SCH");
    assert!(!h_sch.contains("board_only"));
    assert!(!h_sch.contains("(attr "));

    // B.Cu-authored footprint placed on Bottom (unflipped) and Top (flipped).
    let b_bot = block_of("B_BOT");
    assert!(b_bot.contains("(layer \"B.Cu\")"));
    assert!(b_bot.contains("(at 0 -2 120)"));
    assert!(b_bot.contains("(justify mirror)"));
    assert!(b_bot.contains("(pad \"1\" smd rect\n\t\t\t(at -1 0.5 120)"));
    let b_top = block_of("B_TOP");
    assert!(b_top.contains("(layer \"F.Cu\")"));
    assert!(b_top.contains("(at 0 2 60)"));
    assert!(!b_top.contains("(justify mirror)"));
    assert!(b_top.contains("(pad \"1\" smd rect\n\t\t\t(at -1 -0.5 60)"));

    // Mutating structural IR invalidates stale `kicad_raw` -> structured fallback runs,
    // and degenerate 4-vertex L-spike courtyard falls back to `fp_line` instead of `fp_rect`.
    let edge_fp = board.footprints.get_mut("Edge:EdgeCase").unwrap();
    edge_fp.courtyard = vec![
        Pt::new(0, 0),
        Pt::new(mm(2.0), 0),
        Pt::new(mm(2.0), mm(2.0)),
        Pt::new(mm(2.0), 0),
    ];
    edge_fp.keepouts = vec![Keepout {
        kind: KeepoutKind::All,
        layers: vec!["F.Cu".into()],
        poly: vec![Pt::new(0, 0), Pt::new(mm(1.0), 0), Pt::new(0, mm(1.0))],
    }];
    let mut mutated_board = board;
    mutated_board.placements.truncate(1);
    let mutated_pcb = render_kicad_pcb(&mutated_board);
    assert!(!mutated_pcb.contains("(fp_rect "));
    assert!(mutated_pcb.contains("(fp_line (start 0.000000 0.000000)"));
    // Fallback keepout emitted in board coords around U_TOP (20, 10) rot 90 -> (20, -10), (19, -10), (20, -11).
    assert!(mutated_pcb.contains("(xy 20.000000 -10.000000)"));
    assert!(mutated_pcb.contains("(xy 19.000000 -10.000000)"));

    let _ = fs::remove_dir_all(&dir);
}

#[test]
fn lib_courtyard_override_replaces_kicad_mod_courtyard_and_validates() {
    use boardc::drc::check_board_drc;
    use boardc::ir::PinType;

    let dir = test_dir("lib_courtyard_override");
    let lib_dir = dir.join("conn.pretty");
    fs::create_dir_all(&lib_dir).unwrap();
    // Library footprint has a huge overhanging courtyard (-10..+10, -10..+10) around pads at (-0.6, 0) and (0.6, 0).
    let mod_src = "(footprint \"CONN_2P\"\n  (version 20241229)\n  (layer \"F.Cu\")\n  (fp_rect (start -10.0 -10.0) (end 10.0 10.0) (stroke (width 0.05) (type solid)) (fill no) (layer \"F.CrtYd\"))\n  (pad \"1\" smd rect (at -0.6 0) (size 0.5 0.6) (layers \"F.Cu\" \"F.Mask\" \"F.Paste\"))\n  (pad \"2\" smd rect (at 0.6 0) (size 0.5 0.6) (layers \"F.Cu\" \"F.Mask\" \"F.Paste\"))\n)\n";
    fs::write(lib_dir.join("CONN_2P.kicad_mod"), mod_src).unwrap();

    // 1. Inferred lib overlay (no `kind = "lib"`, courtyard only) shrinks courtyard to fit inside board and passes DRC.
    let inferred_cy_toml = r#"
name = "lib_cy_inferred"
size_nm = [10000000, 10000000]
footprint_libs = ["conn.pretty"]

[footprints.CONN_2P]
courtyard = [
  { x = -1200000, y = -600000 },
  { x = 1200000, y = -600000 },
  { x = 1200000, y = 600000 },
  { x = -1200000, y = 600000 },
]

[[placements]]
refdes = "J1"
footprint = "CONN_2P"
pos = { x = 5000000, y = 5000000 }
pin_nets = { "1" = "SIG1", "2" = "SIG2" }
"#;
    let spec: BoardSpec = toml::from_str(inferred_cy_toml).unwrap();
    let ir = BoardIr::from_spec_with_base(&spec, Some(&dir)).unwrap();
    assert_eq!(ir.footprints["CONN_2P"].courtyard.len(), 4);
    assert_eq!(
        ir.footprints["CONN_2P"].courtyard[0],
        Pt::new(mm(-1.2), mm(-0.6))
    );
    let drc = check_board_drc(&ir);
    assert!(
        drc.passed,
        "expected DRC pass with tightened lib courtyard: {:?}",
        drc.violations
    );

    // 2. Explicit `kind = "lib"` with both `courtyard` and `pin_types`.
    let explicit_toml = r#"
name = "lib_cy_explicit"
size_nm = [10000000, 10000000]
footprint_libs = ["conn.pretty"]

[footprints."conn:CONN_2P"]
kind = "lib"
pin_types = { "1" = "passive", "2" = "passive" }
courtyard = [
  { x = -1100000, y = -500000 },
  { x = 1100000, y = -500000 },
  { x = 1100000, y = 500000 },
  { x = -1100000, y = 500000 },
]

[[placements]]
refdes = "J1"
footprint = "conn:CONN_2P"
pos = { x = 5000000, y = 5000000 }
"#;
    let exp_spec: BoardSpec = toml::from_str(explicit_toml).unwrap();
    let exp_ir = BoardIr::from_spec_with_base(&exp_spec, Some(&dir)).unwrap();
    assert_eq!(
        exp_ir.footprints["conn:CONN_2P"].pin_types.get("1"),
        Some(&PinType::Passive)
    );
    assert_eq!(
        exp_ir.footprints["conn:CONN_2P"].courtyard[0],
        Pt::new(mm(-1.1), mm(-0.5))
    );
    let exp_pcb = render_kicad_pcb(&exp_ir);
    let crtyd_lines: Vec<&str> = exp_pcb
        .lines()
        .filter(|l| l.contains("(layer \"F.CrtYd\")"))
        .collect();
    assert_eq!(
        crtyd_lines.len(),
        1,
        "expected 1 F.CrtYd rect: {crtyd_lines:?}"
    );
    assert!(
        crtyd_lines[0].contains("(fp_rect ")
            && crtyd_lines[0].contains("1.100000")
            && crtyd_lines[0].contains("0.500000"),
        "expected overridden courtyard rect in F.CrtYd: {crtyd_lines:?}"
    );
    assert!(
        crtyd_lines.iter().all(|l| !l.contains("10.000000")),
        "old library courtyard must not appear in F.CrtYd: {crtyd_lines:?}"
    );

    // 3. Reject < 3 vertex, empty `courtyard = []`, collinear, empty `kind = "lib"`, unmatched overlay, and unknown fields.
    let bad_degenerate = inferred_cy_toml.replace(
        "  { x = 1200000, y = 600000 },\n  { x = -1200000, y = 600000 },\n",
        "",
    );
    let deg_spec: BoardSpec = toml::from_str(&bad_degenerate).unwrap();
    assert!(BoardIr::from_spec_with_base(&deg_spec, Some(&dir)).is_err());

    let empty_cy = inferred_cy_toml.replace(
        "[\n  { x = -1200000, y = -600000 },\n  { x = 1200000, y = -600000 },\n  { x = 1200000, y = 600000 },\n  { x = -1200000, y = 600000 },\n]",
        "[]",
    );
    assert!(BoardIr::from_spec_with_base(&toml::from_str(&empty_cy).unwrap(), Some(&dir)).is_err());

    let collinear_cy = inferred_cy_toml.replace(
        "  { x = 1200000, y = 600000 },\n  { x = -1200000, y = 600000 },\n",
        "  { x = 0, y = -600000 },\n",
    );
    assert!(
        BoardIr::from_spec_with_base(&toml::from_str(&collinear_cy).unwrap(), Some(&dir)).is_err()
    );

    let empty_lib = r#"
name = "empty_lib"
size_nm = [10000000, 10000000]
footprint_libs = ["conn.pretty"]
[footprints.CONN_2P]
kind = "lib"
[[placements]]
refdes = "J1"
footprint = "CONN_2P"
pos = { x = 5000000, y = 5000000 }
"#;
    assert!(BoardIr::from_spec_with_base(&toml::from_str(empty_lib).unwrap(), Some(&dir)).is_err());

    let unmatched = format!(
        "{inferred_cy_toml}\n[footprints.NO_SUCH_CONN]\ncourtyard = [{{ x = 0, y = 0 }}, {{ x = 1000000, y = 0 }}, {{ x = 0, y = 1000000 }}]\n"
    );
    let unm_spec: BoardSpec = toml::from_str(&unmatched).unwrap();
    assert!(BoardIr::from_spec_with_base(&unm_spec, Some(&dir)).is_err());

    let bad_extra = r#"
name = "bad_extra"
size_nm = [10000000, 10000000]
[footprints.CONN_2P]
kind = "lib"
courtyard = [{ x = 0, y = 0 }, { x = 1000000, y = 0 }, { x = 0, y = 1000000 }]
pads = []
"#;
    assert!(toml::from_str::<BoardSpec>(bad_extra).is_err());

    let _ = fs::remove_dir_all(&dir);
}

#[test]
fn export_kicad_auto_refdes_off_hides_raw_reference_and_preserves_other_modes() {
    let dir = test_dir("auto_refdes_off_raw");
    let lib_dir = dir.join("RefLib.pretty");
    fs::create_dir_all(&lib_dir).unwrap();

    let mk_mod = |name: &str, legacy: bool, ref_mid: &str, ref_extra: &str, eff_extra: &str| {
        let (ver, ref_node, val_node, user_node) = if legacy {
            (
                20211014,
                format!(
                    "(fp_text reference{ref_mid} \"REF**\" (at 0 -1.5) (layer \"F.SilkS\"){ref_extra} (effects (font (size 1 1) (thickness 0.15)){eff_extra}))"
                ),
                format!(
                    "(fp_text value \"{name}\" (at 0 1.5) (layer \"F.Fab\") (effects (font (size 1 1) (thickness 0.15))))"
                ),
                "",
            )
        } else {
            (
                20241229,
                format!(
                    "(property \"Reference\" \"REF**\" (at 0 -1.5 0) (layer \"F.SilkS\"){ref_extra} (effects (font (size 1 1) (thickness 0.15)){eff_extra}))"
                ),
                format!(
                    "(property \"Value\" \"{name}\" (at 0 1.5 0) (layer \"F.Fab\") (effects (font (size 1 1) (thickness 0.15))))"
                ),
                "\n  (fp_text user \"${REFERENCE}\" (at 0 0 0) (layer \"F.Fab\") (effects (font (size 0.5 0.5) (thickness 0.1))))",
            )
        };
        format!(
            "(footprint \"{name}\"\n  (version {ver})\n  (layer \"F.Cu\")\n  {ref_node}\n  {val_node}{user_node}\n  (fp_rect (start -1.0 -0.8) (end 1.0 0.8) (stroke (width 0.05) (type solid)) (fill no) (layer \"F.CrtYd\"))\n  (pad \"1\" smd rect (at -0.5 0) (size 0.4 0.5) (layers \"F.Cu\" \"F.Mask\" \"F.Paste\"))\n)"
        )
    };

    for (name, legacy, ref_mid, ref_extra, eff_extra) in [
        ("PropVis", false, "", "", ""),
        ("PropHideNo", false, "", " (hide no)", ""),
        ("PropHideYes", false, "", " (hide yes)", ""),
        ("PropEffHide", false, "", "", " (hide)"),
        ("PropEffBareHide", false, "", "", " hide"),
        ("FpTextVis", true, "", "", ""),
        ("FpTextBareHide", true, "", " hide", ""),
        ("FpTextEffHideNo", true, "", "", " (hide no)"),
        ("FpTextLocked", true, " locked", "", ""),
    ] {
        fs::write(
            lib_dir.join(format!("{name}.kicad_mod")),
            mk_mod(name, legacy, ref_mid, ref_extra, eff_extra),
        )
        .unwrap();
    }

    struct RefCase<'a> {
        refdes: &'a str,
        fp: &'a str,
        side: Side,
        has: &'a [&'a str],
        lacks: &'a [&'a str],
    }
    let cases = [
        RefCase {
            refdes: "U_PVIS",
            fp: "RefLib:PropVis",
            side: Side::Top,
            has: &["\"U_PVIS\""],
            lacks: &["hide"],
        },
        RefCase {
            refdes: "hide",
            fp: "RefLib:PropVis",
            side: Side::Bottom,
            has: &["\"hide\"", "(layer \"B.SilkS\")", "(justify mirror)"],
            lacks: &["(hide", " hide ", " hide)"],
        },
        RefCase {
            refdes: "U_PHN",
            fp: "RefLib:PropHideNo",
            side: Side::Top,
            has: &["(hide no)"],
            lacks: &[],
        },
        RefCase {
            refdes: "U_PHY",
            fp: "RefLib:PropHideYes",
            side: Side::Top,
            has: &["(hide yes)"],
            lacks: &[],
        },
        RefCase {
            refdes: "U_PEH",
            fp: "RefLib:PropEffHide",
            side: Side::Top,
            has: &["(hide)"],
            lacks: &[],
        },
        RefCase {
            refdes: "U_PEBH",
            fp: "RefLib:PropEffBareHide",
            side: Side::Top,
            has: &[" hide)"],
            lacks: &[],
        },
        RefCase {
            refdes: "U_FTV",
            fp: "RefLib:FpTextVis",
            side: Side::Top,
            has: &["(fp_text reference \"U_FTV\""],
            lacks: &["hide"],
        },
        RefCase {
            refdes: "U_FTB",
            fp: "RefLib:FpTextBareHide",
            side: Side::Top,
            has: &[" hide "],
            lacks: &[],
        },
        RefCase {
            refdes: "U_FTEN",
            fp: "RefLib:FpTextEffHideNo",
            side: Side::Top,
            has: &["(hide no)"],
            lacks: &[],
        },
        RefCase {
            refdes: "U_FTL",
            fp: "RefLib:FpTextLocked",
            side: Side::Top,
            has: &["(fp_text reference locked \"U_FTL\""],
            lacks: &["hide", "\"locked\""],
        },
        RefCase {
            refdes: "R_FALLBACK",
            fp: "C_0402",
            side: Side::Top,
            has: &["(hide yes)"],
            lacks: &[],
        },
    ];

    let placements = cases
        .iter()
        .enumerate()
        .map(|(i, c)| Placement {
            refdes: c.refdes.into(),
            footprint: c.fp.into(),
            pos: Pt::new(mm(10.0 + i as f64 * 5.0), mm(10.0)),
            side: c.side,
            ..Default::default()
        })
        .collect();

    let spec = BoardSpec {
        name: "auto_refdes_raw".into(),
        size_nm: Some([mm(70.0), mm(20.0)]),
        footprint_libs: vec!["RefLib.pretty".into()],
        footprints: BTreeMap::from([(
            "C_0402".into(),
            FootprintSpec::Chip2Pin {
                package: "0402".into(),
                pin_types: BTreeMap::new(),
            },
        )]),
        placements,
        fab: FabConfig {
            auto_refdes: AutoRefdes::Off,
            ..Default::default()
        },
        ..Default::default()
    };
    let mut board = BoardIr::from_spec_with_base(&spec, Some(&dir)).unwrap();

    fn fp_block<'a>(pcb: &'a str, refdes: &str) -> &'a str {
        let needle = format!("\"{refdes}\"");
        pcb.split("\t(footprint ")
            .skip(1)
            .find(|chunk| chunk.contains(&needle))
            .unwrap_or_else(|| panic!("missing footprint block for {refdes} in:\n{pcb}"))
    }
    fn ref_line(block: &str) -> &str {
        block
            .lines()
            .find(|l| l.contains("(property \"Reference\"") || l.contains("(fp_text reference "))
            .unwrap_or_else(|| panic!("missing Reference line in:\n{block}"))
    }

    let off_pcb = render_kicad_pcb(&board);
    for c in &cases {
        let rline = ref_line(fp_block(&off_pcb, c.refdes));
        assert_eq!(
            rline.matches("(hide yes)").count(),
            1,
            "{}: {rline}",
            c.refdes
        );
        for bad in ["(hide no)", "(hide)", " hide ", " hide)"] {
            assert!(
                !rline.contains(bad),
                "{} still has {bad}: {rline}",
                c.refdes
            );
        }
    }
    let bot_rline = ref_line(fp_block(&off_pcb, "hide"));
    assert!(
        bot_rline.contains("(property \"Reference\" \"hide\"")
            && bot_rline.contains("(layer \"B.SilkS\")")
            && bot_rline.contains("(justify mirror)"),
        "bottom placement lost layer/mirror: {bot_rline}"
    );
    let ftl_rline = ref_line(fp_block(&off_pcb, "U_FTL"));
    assert!(
        ftl_rline.contains("(fp_text reference locked \"U_FTL\"")
            && !ftl_rline.contains("\"locked\""),
        "legacy locked fp_text failed in off mode: {ftl_rline}"
    );
    let user_ref_line = fp_block(&off_pcb, "U_PVIS")
        .lines()
        .find(|l| l.contains("(fp_text user \"${REFERENCE}\""))
        .unwrap();
    assert!(
        !user_ref_line.contains("hide"),
        "F.Fab ${{REFERENCE}} must stay visible: {user_ref_line}"
    );
    let parsed_refs: Vec<String> = parse_kicad_pcb(&off_pcb, None)
        .unwrap()
        .placements
        .into_iter()
        .map(|p| p.refdes)
        .collect();
    let expected_refs: Vec<&str> = cases.iter().map(|c| c.refdes).collect();
    assert_eq!(parsed_refs, expected_refs);

    for mode in [AutoRefdes::Rotate, AutoRefdes::Upright] {
        board.fab.auto_refdes = mode;
        let pcb = render_kicad_pcb(&board);
        for c in &cases {
            let rline = ref_line(fp_block(&pcb, c.refdes));
            for needle in c.has {
                assert!(
                    rline.contains(needle),
                    "{mode:?} {} missing {needle}: {rline}",
                    c.refdes
                );
            }
            for bad in c.lacks {
                assert!(
                    !rline.contains(bad),
                    "{mode:?} {} unexpected {bad}: {rline}",
                    c.refdes
                );
            }
            if c.refdes == "U_PHY" {
                assert_eq!(rline.matches("(hide yes)").count(), 1, "{mode:?}: {rline}");
            }
        }
    }

    let _ = fs::remove_dir_all(&dir);
}

#[test]
fn opportunistic_kicad_cli_auto_refdes_off_zero_mismatch_and_zero_silk_drc() {
    let sys_libs = std::path::Path::new("/usr/share/kicad/footprints");
    if !sys_libs
        .join("Resistor_SMD.pretty/R_0402_1005Metric.kicad_mod")
        .is_file()
        || Command::new("kicad-cli").arg("--version").output().is_err()
    {
        return;
    }
    let dir = test_dir("kicad_cli_auto_refdes_off");
    let local_pretty = dir.join("Local6.pretty");
    fs::create_dir_all(&local_pretty).unwrap();
    fs::write(
        local_pretty.join("LegChip.kicad_mod"),
        r#"(footprint "LegChip"
  (version 20211014)
  (layer "F.Cu")
  (fp_text reference "REF**" (at 0 -1.17) (layer "F.SilkS") (effects (font (size 1 1) (thickness 0.15))))
  (fp_text value "LegChip" (at 0 1.17) (layer "F.Fab") (effects (font (size 1 1) (thickness 0.15))))
  (fp_rect (start -0.93 -0.47) (end 0.93 0.47) (stroke (width 0.05) (type solid)) (fill no) (layer "F.CrtYd"))
  (pad "1" smd rect (at -0.5 0) (size 0.5 0.6) (layers "F.Cu" "F.Mask" "F.Paste"))
  (pad "2" smd rect (at 0.5 0) (size 0.5 0.6) (layers "F.Cu" "F.Mask" "F.Paste"))
)"#,
    )
    .unwrap();
    fs::write(
        dir.join("fp-lib-table"),
        format!(
            "(fp_lib_table (version 7)\n  (lib (name \"Resistor_SMD\")(type \"KiCad\")(uri \"/usr/share/kicad/footprints/Resistor_SMD.pretty\")(options \"\")(descr \"\"))\n  (lib (name \"Local6\")(type \"KiCad\")(uri \"{}\")(options \"\")(descr \"\"))\n)\n",
            local_pretty.display()
        ),
    )
    .unwrap();

    // Place footprints near board edges (Top at y=18.8, Bottom at y=1.2 on 20 mm board) so visible refdes clips Edge.Cuts.
    let spec = BoardSpec {
        name: "cli_refdes".into(),
        size_nm: Some([mm(32.0), mm(20.0)]),
        footprint_libs: vec![
            sys_libs.to_string_lossy().into_owned(),
            local_pretty.to_string_lossy().into_owned(),
        ],
        placements: vec![
            Placement {
                refdes: "R1".into(),
                footprint: "Resistor_SMD:R_0402_1005Metric".into(),
                value: "10k".into(),
                pos: Pt::new(mm(8.0), mm(18.8)),
                ..Default::default()
            },
            Placement {
                refdes: "R2".into(),
                footprint: "Local6:LegChip".into(),
                value: "10k".into(),
                pos: Pt::new(mm(18.0), mm(18.8)),
                ..Default::default()
            },
            Placement {
                refdes: "R3".into(),
                footprint: "Resistor_SMD:R_0402_1005Metric".into(),
                value: "10k".into(),
                pos: Pt::new(mm(24.0), mm(1.2)),
                side: Side::Bottom,
                ..Default::default()
            },
        ],
        fab: FabConfig {
            auto_refdes: AutoRefdes::Rotate,
            ..Default::default()
        },
        ..Default::default()
    };
    let mut board = BoardIr::from_spec(&spec).unwrap();

    let run_cli = |board: &BoardIr, tag: &str| -> Vec<serde_json::Value> {
        let pcb_path = dir.join(format!("{tag}.kicad_pcb"));
        let drc_path = dir.join(format!("{tag}.json"));
        fs::write(&pcb_path, render_kicad_pcb(board)).unwrap();
        let status = Command::new("kicad-cli")
            .args([
                "pcb",
                "drc",
                "--severity-all",
                "--format",
                "json",
                "-o",
                drc_path.to_str().unwrap(),
                pcb_path.to_str().unwrap(),
            ])
            .status()
            .unwrap();
        assert!(status.success(), "kicad-cli pcb drc failed");
        let rep: serde_json::Value =
            serde_json::from_str(&fs::read_to_string(&drc_path).unwrap()).unwrap();
        rep["violations"].as_array().unwrap().clone()
    };

    let rot_violations = run_cli(&board, "rotate");
    assert!(
        !rot_violations.iter().any(|v| matches!(
            v["type"].as_str(),
            Some("lib_footprint_mismatch" | "lib_footprint_issues")
        )),
        "unexpected lib footprint violation in rotate: {rot_violations:#?}"
    );
    let silk_items: Vec<String> = rot_violations
        .iter()
        .filter(|v| v["type"].as_str() == Some("silk_edge_clearance"))
        .flat_map(|v| v["items"].as_array().into_iter().flatten())
        .filter_map(|it| it["description"].as_str().map(str::to_string))
        .collect();
    for expected in [
        "Reference field of R1",
        "Reference field of R2",
        "Reference field of R3",
    ] {
        assert!(
            silk_items.iter().any(|d| d.contains(expected)),
            "expected silk_edge_clearance for {expected} in rotate: {rot_violations:#?}"
        );
    }

    board.fab.auto_refdes = AutoRefdes::Off;
    let off_violations = run_cli(&board, "off");
    assert!(
        !off_violations.iter().any(|v| matches!(
            v["type"].as_str(),
            Some("lib_footprint_mismatch" | "lib_footprint_issues")
        )),
        "unexpected lib footprint violation in off: {off_violations:#?}"
    );
    assert!(
        !off_violations.iter().any(|v| matches!(
            v["type"].as_str(),
            Some("silk_edge_clearance" | "silk_over_copper" | "silk_overlap")
        )),
        "hidden Reference must not trigger silkscreen DRC in off: {off_violations:#?}"
    );

    let _ = fs::remove_dir_all(&dir);
}
