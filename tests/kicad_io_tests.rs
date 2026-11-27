use std::fs;
use std::process::Command;

use boardc::geom::{pad_to_world_shape, polygon_area};
use boardc::ir::{
    Graphic, KeepoutKind, LayerKind, PadKind, PadShape, Pt, Side, load_board_json, mm,
};
use boardc::kicad_io::{import_kicad_pcb, parse_kicad_mod, parse_kicad_pcb};

fn pad_center(shape: &boardc::geom::ResolvedShape) -> Pt {
    let b = shape.bbox();
    Pt::new((b.min_x + b.max_x) / 2, (b.min_y + b.max_y) / 2)
}

#[test]
fn parse_kicad_mod_covers_pads_drills_courtyard_and_silkscreen() {
    let src = r#"(footprint "TestPkg"
        (version 20241229)
        (layer "F.Cu")
        (solder_mask_margin 0.05)
        (fp_line (start -2.0 -1.0) (end 2.0 -1.0) (stroke (width 0.12) (type solid)) (layer "F.SilkS"))
        (fp_arc (start 2.0 -1.0) (mid 2.2 0.0) (end 2.0 1.0) (stroke (width 0.12) (type solid)) (layer "F.SilkS"))
        (fp_rect (start -1.0 -0.5) (end 1.0 0.5) (stroke (width 0.12) (type solid)) (fill no) (layer "F.SilkS"))
        (fp_line (start -2.5 -1.5) (end 2.5 -1.5) (stroke (width 0.05) (type solid)) (layer "F.CrtYd"))
        (fp_line (start 2.5 -1.5) (end 2.5 1.5) (stroke (width 0.05) (type solid)) (layer "F.CrtYd"))
        (fp_line (start 2.5 1.5) (end -2.5 1.5) (stroke (width 0.05) (type solid)) (layer "F.CrtYd"))
        (fp_line (start -2.5 1.5) (end -2.5 -1.5) (stroke (width 0.05) (type solid)) (layer "F.CrtYd"))
        (pad "1" smd circle (at -1.5 -0.5) (size 0.4 0.4) (layers "F.Cu" "F.Mask" "F.Paste") (solder_mask_margin 0.03) (solder_paste_margin -0.02))
        (pad "2" smd roundrect (at 1.5 0.5 90) (size 0.8 0.4) (layers "F.Cu" "F.Mask") (roundrect_rratio 0.25))
        (pad "3" thru_hole oval (at 0 -1.0) (size 1.4 2.5) (drill oval 0.7 1.8) (layers "*.Cu" "*.Mask"))
        (pad "" np_thru_hole circle (at 0 1.0) (size 1.0 1.0) (drill 1.0) (layers "*.Cu" "*.Mask"))
        (pad "4" smd custom (at 0.5 -0.5) (size 0.3 0.3) (layers "F.Cu" "F.Mask")
            (primitives (gr_poly (pts (xy -0.2 -0.2) (xy 0.2 -0.2) (xy 0 0.3)) (width 0) (fill yes))))
        (model "${KICAD10_3DMODEL_DIR}/Test.step" (offset (xyz 0.1 0.2 0.3)) (scale (xyz 1 1 1)) (rotate (xyz 0 0 90)))
    )"#;

    let fp = parse_kicad_mod(src).expect("parse_kicad_mod");
    assert_eq!(fp.name, "TestPkg");
    assert_eq!(fp.pads.len(), 5);

    assert_eq!(fp.pads[0].kind, PadKind::Smd);
    assert_eq!(fp.pads[0].pos, Pt::new(mm(-1.5), mm(0.5)));
    assert_eq!(fp.pads[0].shape, PadShape::Circle { diameter: mm(0.4) });
    assert_eq!(fp.pads[0].mask_margin_nm, Some(mm(0.03)));
    assert_eq!(fp.pads[0].paste_margin_nm, Some(mm(-0.02)));
    assert_eq!(fp.pads[1].mask_margin_nm, Some(mm(0.05)));

    assert_eq!(fp.pads[1].rot_deg, 90.0);
    assert_eq!(
        fp.pads[1].shape,
        PadShape::RoundRect {
            width: mm(0.8),
            height: mm(0.4),
            corner_radius: mm(0.1)
        }
    );

    assert_eq!(fp.pads[2].kind, PadKind::Tht);
    assert_eq!(fp.pads[2].drill_nm, Some(mm(0.7)));
    assert_eq!(
        fp.pads[2].shape,
        PadShape::Oval {
            width: mm(1.4),
            height: mm(2.5)
        }
    );

    assert_eq!(fp.pads[3].kind, PadKind::Npth);
    assert_eq!(fp.pads[3].pos, Pt::new(0, mm(-1.0)));
    assert_eq!(fp.pads[3].drill_nm, Some(mm(1.0)));

    assert!(matches!(fp.pads[4].shape, PadShape::Poly { ref verts } if verts.len() == 3));
    assert_eq!(fp.courtyard.len(), 4);
    assert!((polygon_area(&fp.courtyard) - 15e12).abs() < 1e6);
    assert!(
        fp.graphics
            .iter()
            .any(|g| matches!(g, Graphic::Line { layer, .. } if layer == "F.SilkS"))
    );
    assert!(
        fp.graphics
            .iter()
            .any(|g| matches!(g, Graphic::Rect { layer, .. } if layer == "F.SilkS"))
    );
    assert_eq!(fp.models.len(), 1);
    assert_eq!(fp.models[0].path, "${KICAD10_3DMODEL_DIR}/Test.step");
    assert_eq!(fp.models[0].offset_mm, [0.1, 0.2, 0.3]);
    assert_eq!(fp.models[0].rotate_deg, [0.0, 0.0, 90.0]);
}

#[test]
fn parse_kicad_pcb_handles_edge_cutout_bottom_flip_and_pro_rules() {
    let pcb = r#"(kicad_pcb (version 20241229)
        (layers (0 "F.Cu" signal) (2 "B.Cu" signal))
        (net 1 "VCC")
        (net 2 "GND")
        (gr_line (start 0 0) (end 19 0) (stroke (width 0.1)) (layer "Edge.Cuts"))
        (gr_arc (start 19 0) (mid 19.7071 0.2929) (end 20 1) (stroke (width 0.1)) (layer "Edge.Cuts"))
        (gr_line (start 20 1) (end 20 10) (stroke (width 0.1)) (layer "Edge.Cuts"))
        (gr_line (start 20 10) (end 0 10) (stroke (width 0.1)) (layer "Edge.Cuts"))
        (gr_line (start 0 10) (end 0 0) (stroke (width 0.1)) (layer "Edge.Cuts"))
        (gr_rect (start 4 4) (end 6 6) (stroke (width 0.1)) (layer "Edge.Cuts"))
        (footprint "Lib:SOT23" (layer "B.Cu") (at 10 5 90)
            (property "Reference" "U2")
            (property "Value" "DEV")
            (pad "1" smd rect (at -1 2 90) (size 1 0.5) (layers "B.Cu" "B.Mask") (net 1 "VCC"))
            (pad "2" smd rect (at 1 -2 90) (size 1 0.5) (layers "B.Cu" "B.Mask") (net 2 "GND"))
        )
        (segment (start 12 4) (end 15 4) (width 0.2) (layer "B.Cu") (net 1 "VCC"))
        (via (at 15 4) (size 0.5) (drill 0.25) (layers "F.Cu" "B.Cu") (net 1 "VCC"))
        (zone (net 2 "GND") (layer "B.Cu") (connect_pads (clearance 0.2)) (min_thickness 0.15)
            (fill yes (thermal_gap 0.25) (thermal_bridge_width 0.3) (island_removal_mode 0))
            (polygon (pts (xy 1 1) (xy 19 1) (xy 19 9) (xy 1 9))))
    )"#;
    let pro = r#"{
        "board": {"design_settings": {"rules": {"min_clearance": 0.12, "min_track_width": 0.14}}},
        "net_settings": {
            "classes": [{"name": "Default", "clearance": 0.12, "track_width": 0.14, "via_diameter": 0.45, "via_drill": 0.2}],
            "netclass_assignments": {"VCC": "Default"}
        }
    }"#;

    let ir = parse_kicad_pcb(pcb, Some(pro)).expect("parse_kicad_pcb");
    assert_eq!(ir.cutouts.len(), 1);
    assert_eq!(ir.rules.clearance_nm, mm(0.12));
    assert_eq!(ir.rules.track_width_nm, mm(0.14));
    assert_eq!(ir.rules.via_dia_nm, mm(0.45));
    assert_eq!(ir.rules.via_drill_nm, mm(0.2));
    assert_eq!(ir.tracks.len(), 1);
    assert_eq!(ir.vias.len(), 1);
    assert_eq!(ir.pours.len(), 1);

    // U2 on B.Cu at KiCad (10, 5) rot 90 -> pad 1 KiCad local (-1, +2) rotated 90 in Y-down is (+2, +1) -> world (12, -6) mm.
    let pl = &ir.placements[0];
    let fp = &ir.footprints[&pl.footprint];
    let p1 = pad_center(&pad_to_world_shape(&fp.pads[0], pl, 8));
    assert_eq!(p1, Pt::new(mm(12.0), mm(-6.0)));
    let p2 = pad_center(&pad_to_world_shape(&fp.pads[1], pl, 8));
    assert_eq!(p2, Pt::new(mm(8.0), mm(-4.0)));
}

#[test]
fn parse_kicad_pcb_handles_bottom_rotations_and_stackup_thickness() {
    let pcb = r#"(kicad_pcb (version 20241229)
        (layers
            (0 "F.Cu" signal)
            (1 "In1.Cu" power)
            (2 "B.Cu" signal)
        )
        (setup
            (stackup
                (layer "F.Cu" (type "copper") (thickness 0.035))
                (layer "In1.Cu" (type "copper") (thickness 0.0175))
                (layer "B.Cu" (type "copper") (thickness 0.035))
            )
        )
        (gr_rect (start 0 0) (end 30 20) (stroke (width 0.1)) (layer "Edge.Cuts"))
        (footprint "Lib:Test2" (layer "B.Cu") (at 10 10 0)
            (property "Reference" "B0")
            (pad "1" smd rect (at 2 1 0) (size 0.5 0.5) (layers "B.Cu"))
        )
        (footprint "Lib:Test2" (layer "B.Cu") (at 10 10 180)
            (property "Reference" "B180")
            (pad "1" smd rect (at 2 1 180) (size 0.5 0.5) (layers "B.Cu"))
        )
        (footprint "Lib:Test2" (layer "B.Cu") (at 10 10 270)
            (property "Reference" "B270")
            (pad "1" smd rect (at 2 1 270) (size 0.5 0.5) (layers "B.Cu"))
        )
    )"#;
    let ir = parse_kicad_pcb(pcb, None).expect("parse_kicad_pcb");
    assert_eq!(ir.layers.len(), 3);
    assert_eq!(ir.layers[0].thickness_nm, Some(35_000));
    assert_eq!(ir.layers[1].kind, LayerKind::Plane);

    let check = |refdes: &str, ex: f64, ey: f64| {
        let pl = ir.placements.iter().find(|p| p.refdes == refdes).unwrap();
        assert_eq!(pl.side, Side::Bottom);
        let fp = &ir.footprints[&pl.footprint];
        let c = pad_center(&pad_to_world_shape(&fp.pads[0], pl, 8));
        assert_eq!(c, Pt::new(mm(ex), mm(ey)), "{refdes}");
    };
    // KiCad B.Cu local (2, 1) mapped to Y-up at rot 0/180/270 around KiCad (10, 10).
    check("B0", 12.0, -11.0);
    check("B180", 8.0, -9.0);
    check("B270", 9.0, -12.0);
}

#[test]
fn cli_import_kicad_writes_valid_board_ir_json() {
    let tmp_dir = std::env::temp_dir().join("boardc_test_import_kicad");
    fs::create_dir_all(&tmp_dir).unwrap();
    let pcb_path = tmp_dir.join("mini.kicad_pcb");
    let pro_path = tmp_dir.join("mini.kicad_pro");
    let out_path = tmp_dir.join("mini.ir.json");

    fs::write(
        &pcb_path,
        r#"(kicad_pcb (version 20241229)
            (layers (0 "F.Cu" signal) (2 "B.Cu" signal))
            (gr_rect (start 0 0) (end 10 10) (stroke (width 0.1)) (layer "Edge.Cuts"))
            (footprint "R:0402" (layer "F.Cu") (at 5 5)
                (property "Reference" "R1")
                (pad "1" smd rect (at -0.5 0) (size 0.5 0.5) (layers "F.Cu") (net 1 "VCC")))
        )"#,
    )
    .unwrap();
    fs::write(
        &pro_path,
        r#"{"board":{"design_settings":{"rules":{"min_clearance":0.15}}}}"#,
    )
    .unwrap();

    let status = Command::new(env!("CARGO_BIN_EXE_boardc"))
        .args([
            "import-kicad",
            pcb_path.to_str().unwrap(),
            "-o",
            out_path.to_str().unwrap(),
            "--pro",
            pro_path.to_str().unwrap(),
        ])
        .status()
        .expect("run boardc import-kicad");
    assert!(status.success());

    let ir = load_board_json(&out_path).expect("load output json");
    let direct = import_kicad_pcb(&pcb_path, Some(&pro_path)).expect("direct import_kicad_pcb");
    assert_eq!(ir, direct);
    assert_eq!(ir.placements.len(), 1);
    assert_eq!(ir.rules.clearance_nm, mm(0.15));

    let _ = fs::remove_dir_all(&tmp_dir);
}

#[test]
fn parse_kicad_pcb_preserves_multi_flag_keepout_and_custom_pad_anchor() {
    let pcb = r#"(kicad_pcb (version 20241229)
        (layers (0 "F.Cu" signal) (2 "B.Cu" signal))
        (gr_rect (start 0 0) (end 20 20) (stroke (width 0.1)) (layer "Edge.Cuts"))
        (footprint "Lib:CustomAnchor" (layer "F.Cu") (at 10 10)
            (property "Reference" "J1")
            (pad "1" smd custom (at 0 0) (size 1.0 1.0) (layers "F.Cu" "F.Mask")
                (options (clearance outline) (anchor rect))
                (primitives (gr_poly (pts (xy 0.2 -0.2) (xy 1.5 -0.2) (xy 1.5 0.2) (xy 0.2 0.2)) (width 0) (fill yes))))
        )
        (zone (net 0) (net_name "") (layer "F.Cu")
            (keepout (tracks allowed) (vias not_allowed) (pads allowed) (copperpour not_allowed) (footprints allowed))
            (polygon (pts (xy 2 2) (xy 6 2) (xy 6 6) (xy 2 6)))
        )
    )"#;
    let ir = parse_kicad_pcb(pcb, None).expect("parse_kicad_pcb");
    assert_eq!(ir.keepouts.len(), 1);
    assert_eq!(
        ir.keepouts[0].kind,
        KeepoutKind::Custom {
            tracks: false,
            vias: true,
            copper: true,
            placement: false,
        }
    );
    assert!(!ir.keepouts[0].kind.blocks_tracks());
    assert!(ir.keepouts[0].kind.blocks_vias());
    assert!(ir.keepouts[0].kind.blocks_copper());

    let fp = &ir.footprints[&ir.placements[0].footprint];
    match &fp.pads[0].shape {
        PadShape::Poly { verts } => {
            let min_x = verts.iter().map(|p| p.x).min().unwrap();
            let max_x = verts.iter().map(|p| p.x).max().unwrap();
            assert!(
                min_x <= mm(-0.49) && max_x >= mm(1.49),
                "expected union of anchor rect [-0.5, 0.5] and arm [0.2, 1.5], got [{min_x}, {max_x}]"
            );
        }
        other => panic!("expected custom Poly pad, got {other:?}"),
    }
}

#[test]
fn parse_and_export_kicad_pcb_roundtrip_dnp_and_virtual_attrs() {
    let pcb = r#"(kicad_pcb (version 20241229)
        (layers (0 "F.Cu" signal) (2 "B.Cu" signal))
        (gr_rect (start 0 0) (end 30 20) (stroke (width 0.1)) (layer "Edge.Cuts"))
        (footprint "Lib:R_0402" (layer "F.Cu") (at 5 5)
            (property "Reference" "R1")
            (property "Value" "10k")
            (attr smd)
            (pad "1" smd rect (at 0 0) (size 0.5 0.5) (layers "F.Cu" "F.Mask" "F.Paste"))
        )
        (footprint "Lib:R_0402" (layer "F.Cu") (at 10 5)
            (property "Reference" "R_DNP")
            (property "Value" "0R")
            (attr smd dnp)
            (pad "1" smd rect (at 0 0) (size 0.5 0.5) (layers "F.Cu" "F.Mask" "F.Paste"))
        )
        (footprint "Lib:TestPad" (layer "F.Cu") (at 15 5)
            (property "Reference" "TP1")
            (property "Value" "TP")
            (attr smd exclude_from_pos_files exclude_from_bom)
            (pad "1" smd circle (at 0 0) (size 0.8 0.8) (layers "F.Cu" "F.Mask"))
        )
        (footprint "Lib:HDR" (layer "F.Cu") (at 20 5)
            (property "Reference" "J1")
            (property "Value" "HDR")
            (attr through_hole exclude_from_bom)
            (pad "1" thru_hole circle (at 0 0) (size 1.4 1.4) (drill 0.8) (layers "*.Cu" "*.Mask"))
        )
        (footprint "Lib:Hole" (layer "F.Cu") (at 25 5)
            (property "Reference" "H1")
            (property "Value" "M3")
            (attr board_only exclude_from_pos_files)
            (pad "1" np_thru_hole circle (at 0 0) (size 3.0 3.0) (drill 3.0) (layers "*.Cu" "*.Mask"))
        )
        (footprint "Lib:Legacy" (layer "F.Cu") (at 5 15)
            (property "Reference" "V1")
            (property "Value" "VIRT")
            (attr virtual)
            (pad "1" smd circle (at 0 0) (size 0.8 0.8) (layers "F.Cu" "F.Mask"))
        )
        (footprint "Lib:R_0402" (layer "F.Cu") (at 10 15)
            (property "Reference" "R_ALL")
            (property "Value" "0R")
            (attr smd dnp exclude_from_bom exclude_from_pos_files)
            (pad "1" smd rect (at 0 0) (size 0.5 0.5) (layers "F.Cu" "F.Mask" "F.Paste"))
        )
    )"#;

    let ir = parse_kicad_pcb(pcb, None).expect("parse_kicad_pcb");
    let flags = |board: &boardc::ir::BoardIr, refdes: &str| {
        let p = board
            .placements
            .iter()
            .find(|p| p.refdes == refdes)
            .unwrap();
        (p.dnp, p.exclude_from_bom, p.exclude_from_pos)
    };
    assert_eq!(flags(&ir, "R1"), (false, false, false));
    assert_eq!(flags(&ir, "R_DNP"), (true, false, false));
    assert_eq!(flags(&ir, "TP1"), (false, true, true));
    assert_eq!(flags(&ir, "J1"), (false, true, false));
    assert_eq!(flags(&ir, "H1"), (false, false, true));
    assert_eq!(flags(&ir, "V1"), (false, true, true));
    assert_eq!(flags(&ir, "R_ALL"), (true, true, true));

    let exported = boardc::kicad_export::render_kicad_pcb(&ir);
    assert!(exported.contains("(attr smd dnp)"));
    assert!(exported.contains("(attr smd exclude_from_pos_files exclude_from_bom)"));
    assert!(exported.contains("(attr through_hole exclude_from_bom)"));
    assert!(exported.contains("(attr board_only exclude_from_pos_files)"));
    assert!(exported.contains("(attr smd exclude_from_pos_files exclude_from_bom dnp)"));

    let reimported = parse_kicad_pcb(&exported, None).expect("reimport exported pcb");
    for refdes in ["R1", "R_DNP", "TP1", "J1", "H1", "V1", "R_ALL"] {
        assert_eq!(flags(&reimported, refdes), flags(&ir, refdes), "{refdes}");
    }
}

#[test]
fn export_and_import_kicad_pcb_roundtrip_stackup_thickness_finish_dielectric_and_pofv() {
    use boardc::ir::{BoardIr, BoardSpec, LayerDef, Stackup};
    use boardc::kicad_export::render_kicad_pcb;

    let spec = BoardSpec {
        name: "stackup_rt".into(),
        size_nm: Some([mm(40.0), mm(30.0)]),
        layers: vec![
            LayerDef {
                name: "F.Cu".into(),
                kind: LayerKind::Signal,
                side: Some(Side::Top),
                thickness_nm: Some(35_000),
            },
            LayerDef {
                name: "In1.Cu".into(),
                kind: LayerKind::Plane,
                side: None,
                thickness_nm: Some(17_500),
            },
            LayerDef {
                name: "In2.Cu".into(),
                kind: LayerKind::Signal,
                side: None,
                thickness_nm: Some(17_500),
            },
            LayerDef {
                name: "In3.Cu".into(),
                kind: LayerKind::Signal,
                side: None,
                thickness_nm: Some(17_500),
            },
            LayerDef {
                name: "In4.Cu".into(),
                kind: LayerKind::Plane,
                side: None,
                thickness_nm: Some(17_500),
            },
            LayerDef {
                name: "B.Cu".into(),
                kind: LayerKind::Signal,
                side: Some(Side::Bottom),
                thickness_nm: Some(35_000),
            },
        ],
        stackup: Stackup {
            thickness_nm: 1_000_000,
            finish: Some("ENIG".into()),
            pofv: true,
            dielectric_nm: vec![95_000, 240_000, 170_000, 240_000, 95_000],
        },
        ..Default::default()
    };
    let board = BoardIr::from_spec(&spec).unwrap();
    let rendered = render_kicad_pcb(&board);

    assert!(board.rules.via_at_smd, "stackup.pofv must sync via_at_smd");
    assert!(
        rendered.contains("(version 20260206)"),
        "POFV export must use KiCad 10 schema version"
    );
    assert!(
        rendered.contains("(thickness 1)"),
        "missing 1mm general thickness:\n{rendered}"
    );
    assert!(
        !rendered.contains("(thickness 1.6)"),
        "hardcoded 1.6mm leaked:\n{rendered}"
    );
    assert!(rendered.contains("(stackup\n"), "missing (stackup) block");
    assert!(rendered.contains("(copper_finish \"ENIG\")"));
    assert!(rendered.contains("(layer \"dielectric 1\" (type \"prepreg\") (thickness 0.095)"));
    assert!(rendered.contains("(layer \"dielectric 2\" (type \"core\") (thickness 0.24)"));
    assert!(rendered.contains("(layer \"dielectric 3\" (type \"prepreg\") (thickness 0.17)"));
    assert!(rendered.contains("(layer \"dielectric 4\" (type \"core\") (thickness 0.24)"));
    assert!(rendered.contains("(layer \"dielectric 5\" (type \"prepreg\") (thickness 0.095)"));
    assert!(rendered.contains("(plugging (front yes) (back yes))"));
    assert!(rendered.contains("(capping yes)"));
    assert!(rendered.contains("(filling yes)"));

    let reimported = parse_kicad_pcb(&rendered, None).expect("reimport rendered kicad_pcb");
    assert_eq!(reimported.stackup, board.stackup);
    assert_eq!(reimported.rules.via_at_smd, board.rules.via_at_smd);
    assert_eq!(reimported.layers[0].thickness_nm, Some(35_000));
    assert_eq!(reimported.layers[1].thickness_nm, Some(17_500));

    // Default 2-layer board uses KiCad 9 version, omits POFV and copper_finish, emits 1.6 mm / 1.51 mm core.
    let def_board = BoardIr::from_spec(&BoardSpec {
        name: "def_rt".into(),
        size_nm: Some([mm(20.0), mm(10.0)]),
        ..Default::default()
    })
    .unwrap();
    let def_rendered = render_kicad_pcb(&def_board);
    assert!(def_rendered.contains("(version 20241229)"));
    assert!(def_rendered.contains("(thickness 1.6)"));
    assert!(def_rendered.contains("(layer \"dielectric 1\" (type \"core\") (thickness 1.51)"));
    assert!(!def_rendered.contains("(copper_finish"));
    assert!(!def_rendered.contains("(plugging"));
    assert!(!def_rendered.contains("(capping"));
    assert!(!def_rendered.contains("(filling"));

    // Real KiCad bare `addsublayer` syntax and mixed-case Core/Dielectric names sum into one gap entry.
    let multi_sub = r#"(kicad_pcb (version 20260206)
        (general (thickness 1.2))
        (layers (0 "F.Cu" signal) (2 "B.Cu" signal))
        (setup
            (stackup
                (layer "F.Cu" (type "copper") (thickness 0.035))
                (layer "Dielectric 1" (type "Prepreg") (thickness 0.1) (material "FR4") addsublayer (thickness 0.2) (material "FR4"))
                (layer "Dielectric 1" (type "Core") (thickness 0.8))
                (layer "B.Cu" (type "copper") (thickness 0.035))
                (copper_finish "HAL lead-free")
            )
        )
        (gr_rect (start 0 0) (end 20 10) (stroke (width 0.1)) (layer "Edge.Cuts"))
    )"#;
    let parsed_multi = parse_kicad_pcb(multi_sub, None).expect("parse multi-sublayer stackup");
    assert_eq!(parsed_multi.stackup.thickness_nm, 1_200_000);
    assert_eq!(
        parsed_multi.stackup.finish.as_deref(),
        Some("HAL lead-free")
    );
    assert!(!parsed_multi.stackup.pofv);
    assert_eq!(parsed_multi.stackup.dielectric_nm, vec![1_100_000]);
}

#[test]
fn export_and_import_kicad_pcb_preserves_via_tenting() {
    use boardc::ir::{BoardIr, BoardSpec, LayerDef, Via};
    use boardc::kicad_export::render_kicad_pcb;

    let spec = BoardSpec {
        name: "via_tenting".into(),
        size_nm: Some([mm(30.0), mm(20.0)]),
        layers: vec![
            LayerDef {
                name: "F.Cu".into(),
                kind: LayerKind::Signal,
                side: Some(Side::Top),
                thickness_nm: None,
            },
            LayerDef {
                name: "In1.Cu".into(),
                kind: LayerKind::Plane,
                side: None,
                thickness_nm: None,
            },
            LayerDef {
                name: "In2.Cu".into(),
                kind: LayerKind::Signal,
                side: None,
                thickness_nm: None,
            },
            LayerDef {
                name: "B.Cu".into(),
                kind: LayerKind::Signal,
                side: Some(Side::Bottom),
                thickness_nm: None,
            },
        ],
        vias: vec![
            Via {
                net: "GND".into(),
                pos: Pt::new(mm(5.0), mm(10.0)),
                diameter: mm(0.6),
                drill: mm(0.3),
                layers: ("F.Cu".into(), "B.Cu".into()),
                locked: true,
                tented: true,
            },
            Via {
                net: "VCC".into(),
                pos: Pt::new(mm(12.0), mm(10.0)),
                diameter: mm(0.6),
                drill: mm(0.3),
                layers: ("F.Cu".into(), "B.Cu".into()),
                locked: true,
                tented: false,
            },
            Via {
                net: "SIG".into(),
                pos: Pt::new(mm(18.0), mm(10.0)),
                diameter: mm(0.5),
                drill: mm(0.2),
                layers: ("F.Cu".into(), "In1.Cu".into()),
                locked: true,
                tented: false,
            },
        ],
        ..Default::default()
    };
    let board = BoardIr::from_spec(&spec).unwrap();
    let rendered = render_kicad_pcb(&board);

    // Mixed tented + untented vias use KiCad 10 per-via tenting without fabricating secondary covering.
    assert!(rendered.contains("(version 20260206)"));
    assert!(rendered.contains("\t\t(tenting (front yes) (back yes))"));
    assert!(!rendered.contains("(covering "));

    let via_lines: Vec<&str> = rendered
        .lines()
        .filter(|l| l.trim_start().starts_with("(via "))
        .collect();
    assert_eq!(via_lines.len(), 3);
    assert!(
        via_lines[0].contains("(tenting (front yes) (back yes))"),
        "tented via missing tenting yes: {}",
        via_lines[0]
    );
    assert!(
        via_lines[1].contains("(tenting (front no) (back no))"),
        "untented via missing tenting no: {}",
        via_lines[1]
    );
    assert!(
        via_lines[2].trim_start().starts_with("(via blind ")
            && via_lines[2].contains("(tenting (front no) (back no))"),
        "blind untented via mismatch: {}",
        via_lines[2]
    );

    let reimported = parse_kicad_pcb(&rendered, None).expect("reimport via tenting");
    let tented_flags: Vec<bool> = reimported.vias.iter().map(|v| v.tented).collect();
    assert_eq!(tented_flags, vec![true, false, false]);

    // Uniform all-tented and all-untented boards stay KiCad 9 compatible and round-trip cleanly.
    let mut tented_spec = spec.clone();
    tented_spec.vias.retain(|v| v.tented);
    let tented_rendered = render_kicad_pcb(&BoardIr::from_spec(&tented_spec).unwrap());
    assert!(tented_rendered.contains("(version 20241229)"));
    assert!(tented_rendered.contains("\t\t(tenting front back)"));
    let reimported_tented = parse_kicad_pcb(&tented_rendered, None).unwrap();
    assert!(reimported_tented.vias.iter().all(|v| v.tented));

    let mut untented_spec = spec.clone();
    untented_spec.vias.retain(|v| !v.tented);
    let untented_rendered = render_kicad_pcb(&BoardIr::from_spec(&untented_spec).unwrap());
    assert!(untented_rendered.contains("(version 20241229)"));
    assert!(untented_rendered.contains("\t\t(tenting none)"));
    let reimported_untented = parse_kicad_pcb(&untented_rendered, None).unwrap();
    assert!(reimported_untented.vias.iter().all(|v| !v.tented));

    // KiCad 9 flat tenting, via-before-setup order, and per-via `(tenting none)` inherit semantics.
    let kicad9_pcb = r#"(kicad_pcb (version 20241229)
        (layers (0 "F.Cu" signal) (2 "B.Cu" signal))
        (gr_rect (start 0 0) (end 20 20) (stroke (width 0.1)) (layer "Edge.Cuts"))
        (via (at 4 4) (size 0.6) (drill 0.3) (layers "F.Cu" "B.Cu") (net 0))
        (via (at 8 4) (size 0.6) (drill 0.3) (layers "F.Cu" "B.Cu") (tenting front back) (net 0))
        (setup (tenting none))
    )"#;
    let parsed_k9 = parse_kicad_pcb(kicad9_pcb, None).expect("parse kicad 9 tenting");
    assert_eq!(
        parsed_k9.vias.iter().map(|v| v.tented).collect::<Vec<_>>(),
        vec![false, true]
    );

    let kicad9_inherit = r#"(kicad_pcb (version 20241229)
        (layers (0 "F.Cu" signal) (2 "B.Cu" signal))
        (setup (tenting front back))
        (gr_rect (start 0 0) (end 20 20) (stroke (width 0.1)) (layer "Edge.Cuts"))
        (via (at 4 4) (size 0.6) (drill 0.3) (layers "F.Cu" "B.Cu") (tenting none) (net 0))
    )"#;
    assert!(parse_kicad_pcb(kicad9_inherit, None).unwrap().vias[0].tented);

    // KiCad 10 `(tenting (front none) (back none))` and `(covering (front no) (back no))` inherit setup tenting.
    let kicad10_pcb = r#"(kicad_pcb (version 20260206)
        (layers (0 "F.Cu" signal) (4 "In1.Cu" signal) (2 "B.Cu" signal))
        (setup (tenting (front yes) (back yes)) (covering (front no) (back no)))
        (gr_rect (start 0 0) (end 20 20) (stroke (width 0.1)) (layer "Edge.Cuts"))
        (via (at 4 4) (size 0.6) (drill 0.3) (layers "F.Cu" "B.Cu") (tenting (front none) (back none)) (net 0))
        (via (at 8 4) (size 0.6) (drill 0.3) (layers "F.Cu" "B.Cu") (covering (front no) (back no)) (net 0))
        (via (at 12 4) (size 0.6) (drill 0.3) (layers "F.Cu" "B.Cu") (tenting (front no) (back none)) (net 0))
    )"#;
    let parsed_k10 = parse_kicad_pcb(kicad10_pcb, None).expect("parse kicad 10 tenting");
    assert_eq!(
        parsed_k10.vias.iter().map(|v| v.tented).collect::<Vec<_>>(),
        vec![true, true, false]
    );

    // Empty `(setup (tenting))` is untented; single-side override only tents blind via on touched side.
    let kicad10_empty_and_partial = r#"(kicad_pcb (version 20260206)
        (layers (0 "F.Cu" signal) (4 "In1.Cu" signal) (2 "B.Cu" signal))
        (setup (tenting))
        (gr_rect (start 0 0) (end 20 20) (stroke (width 0.1)) (layer "Edge.Cuts"))
        (via (at 4 4) (size 0.6) (drill 0.3) (layers "F.Cu" "B.Cu") (net 0))
        (via (at 8 4) (size 0.6) (drill 0.3) (layers "F.Cu" "B.Cu") (tenting (front yes) (back none)) (net 0))
        (via blind (at 12 4) (size 0.6) (drill 0.3) (layers "F.Cu" "In1.Cu") (tenting (front yes) (back none)) (net 0))
    )"#;
    let parsed_partial = parse_kicad_pcb(kicad10_empty_and_partial, None).unwrap();
    assert_eq!(
        parsed_partial
            .vias
            .iter()
            .map(|v| v.tented)
            .collect::<Vec<_>>(),
        vec![false, false, true]
    );

    // KiCad 7/8 `(pcbplotparams (viasonmask yes))` untents vias when `(tenting ...)` is absent.
    let kicad8_plot = r#"(kicad_pcb (version 20241229)
        (layers (0 "F.Cu" signal) (2 "B.Cu" signal))
        (setup (pcbplotparams (viasonmask yes)))
        (gr_rect (start 0 0) (end 20 20) (stroke (width 0.1)) (layer "Edge.Cuts"))
        (via (at 4 4) (size 0.6) (drill 0.3) (layers "F.Cu" "B.Cu") (net 0))
    )"#;
    assert!(!parse_kicad_pcb(kicad8_plot, None).unwrap().vias[0].tented);
}
