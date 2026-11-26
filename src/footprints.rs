use crate::ir::{BBoxNm, FootprintDef, Graphic, MM, Nm, PadDef, PadKind, PadShape, Pt};

const BGA_ROW_LETTERS: &[u8] = b"ABCDEFGHJKLMNPRTUVWY";
const MAX_GENERATED_PADS: usize = 65_536;

fn smd_layers() -> Vec<String> {
    vec!["F.Cu".into(), "F.Mask".into(), "F.Paste".into()]
}

fn tht_layers() -> Vec<String> {
    vec!["*.Cu".into(), "*.Mask".into()]
}

fn smd_pad(name: impl Into<String>, shape: PadShape, pos: Pt) -> PadDef {
    PadDef {
        name: name.into(),
        kind: PadKind::Smd,
        shape,
        pos,
        rot_deg: 0.0,
        drill_nm: None,
        layers: smd_layers(),
        mask_margin_nm: None,
        paste_margin_nm: None,
    }
}

fn rect_courtyard(half_w: Nm, half_h: Nm) -> Vec<Pt> {
    vec![
        Pt::new(-half_w, -half_h),
        Pt::new(half_w, -half_h),
        Pt::new(half_w, half_h),
        Pt::new(-half_w, half_h),
    ]
}

fn bga_row_name(row: usize) -> String {
    let base = BGA_ROW_LETTERS.len();
    if row < base {
        return (BGA_ROW_LETTERS[row] as char).to_string();
    }
    let hi = (row / base) - 1;
    let lo = row % base;
    format!(
        "{}{}",
        BGA_ROW_LETTERS[hi % base] as char,
        BGA_ROW_LETTERS[lo] as char
    )
}

/// Parametric IPC-7351B two-terminal chip footprint (`0201`, `0402`, `0603`, `0805`).
pub fn chip_2pin(package: &str) -> Result<FootprintDef, String> {
    let (pad_w, pad_h, center_x, body_w, body_h, crtyd_margin) = match package {
        "0201" => (300_000, 320_000, 280_000, 600_000, 300_000, 150_000),
        "0402" => (500_000, 560_000, 480_000, 1_000_000, 500_000, 150_000),
        "0603" => (800_000, 900_000, 750_000, 1_600_000, 800_000, 250_000),
        "0805" => (1_000_000, 1_300_000, 950_000, 2_000_000, 1_250_000, 250_000),
        other => return Err(format!("unsupported 2-pin chip package '{other}'")),
    };
    let corner_r = (pad_w.min(pad_h) / 8).min(100_000);
    let shape = PadShape::RoundRect {
        width: pad_w,
        height: pad_h,
        corner_radius: corner_r,
    };
    let pads = vec![
        smd_pad("1", shape.clone(), Pt::new(-center_x, 0)),
        smd_pad("2", shape, Pt::new(center_x, 0)),
    ];
    let half_cx = (center_x + pad_w / 2).max(body_w / 2) + crtyd_margin;
    let half_cy = (pad_h / 2).max(body_h / 2) + crtyd_margin;
    let silk_y = pad_h / 2 + 180_000;
    let silk_x = (body_w / 2).min(center_x - pad_w / 2 + 50_000).max(100_000);
    let graphics = vec![
        Graphic::Rect {
            layer: "F.Fab".into(),
            min: Pt::new(-body_w / 2, -body_h / 2),
            max: Pt::new(body_w / 2, body_h / 2),
            width: 100_000,
            fill: false,
        },
        Graphic::Line {
            layer: "F.Silk".into(),
            a: Pt::new(-silk_x, silk_y),
            b: Pt::new(silk_x, silk_y),
            width: 120_000,
        },
        Graphic::Line {
            layer: "F.Silk".into(),
            a: Pt::new(-silk_x, -silk_y),
            b: Pt::new(silk_x, -silk_y),
            width: 120_000,
        },
    ];
    Ok(FootprintDef {
        name: format!("C_{package}"),
        pads,
        courtyard: rect_courtyard(half_cx, half_cy),
        graphics,
        keepouts: Vec::new(),
        models: Vec::new(),
    })
}

/// Parametric IPC-7351B NSMD BGA footprint; row A is top (+Y), col 1 is left (-X).
pub fn bga(
    rows: usize,
    cols: usize,
    pitch_nm: Nm,
    ball_dia_nm: Nm,
) -> Result<FootprintDef, String> {
    if rows == 0 || cols == 0 {
        return Err("BGA rows and cols must be >= 1".into());
    }
    let total_pads = rows
        .checked_mul(cols)
        .filter(|&n| n <= MAX_GENERATED_PADS)
        .ok_or_else(|| format!("BGA pad count {rows}x{cols} exceeds limit {MAX_GENERATED_PADS}"))?;
    if pitch_nm <= 0 || ball_dia_nm <= 0 {
        return Err("BGA pitch_nm and ball_dia_nm must be > 0".into());
    }
    // IPC-7351B NSMD ball pad is ~80% of nominal ball diameter, bounded strictly positive.
    let pad_dia = (ball_dia_nm * 4 / 5)
        .max(120_000)
        .min(pitch_nm - 100_000)
        .max(1);
    if pad_dia >= pitch_nm {
        return Err(format!("BGA pitch_nm ({pitch_nm}) too small for pad"));
    }
    let x0 = -((cols as i64 - 1) * pitch_nm) / 2;
    let y0 = ((rows as i64 - 1) * pitch_nm) / 2;
    let mut pads = Vec::with_capacity(total_pads);
    for r in 0..rows {
        let row_lbl = bga_row_name(r);
        let y = y0 - (r as i64) * pitch_nm;
        for c in 0..cols {
            let x = x0 + (c as i64) * pitch_nm;
            pads.push(smd_pad(
                format!("{row_lbl}{}", c + 1),
                PadShape::Circle { diameter: pad_dia },
                Pt::new(x, y),
            ));
        }
    }
    let body_half_w = (cols as i64 * pitch_nm) / 2 + pitch_nm / 2;
    let body_half_h = (rows as i64 * pitch_nm) / 2 + pitch_nm / 2;
    let crtyd_margin = 500_000;
    let graphics = vec![
        Graphic::Rect {
            layer: "F.Fab".into(),
            min: Pt::new(-body_half_w, -body_half_h),
            max: Pt::new(body_half_w, body_half_h),
            width: 100_000,
            fill: false,
        },
        Graphic::Circle {
            layer: "F.Silk".into(),
            center: Pt::new(-body_half_w - 250_000, body_half_h + 250_000),
            radius: 125_000,
            width: 120_000,
            fill: true,
        },
    ];
    Ok(FootprintDef {
        name: format!("BGA_{rows}x{cols}_P{:.2}", pitch_nm as f64 / MM as f64),
        pads,
        courtyard: rect_courtyard(body_half_w + crtyd_margin, body_half_h + crtyd_margin),
        graphics,
        keepouts: Vec::new(),
        models: Vec::new(),
    })
}

/// Parametric IPC-7351B square QFN footprint with optional centered exposed pad.
pub fn qfn(
    pins_per_side: usize,
    pitch_nm: Nm,
    body_nm: Nm,
    ep_nm: Nm,
) -> Result<FootprintDef, String> {
    if pins_per_side == 0 {
        return Err("QFN pins_per_side must be >= 1".into());
    }
    let total_pads = pins_per_side
        .checked_mul(4)
        .and_then(|n| n.checked_add(usize::from(ep_nm > 0)))
        .filter(|&n| n <= MAX_GENERATED_PADS)
        .ok_or_else(|| format!("QFN pin count exceeds limit {MAX_GENERATED_PADS}"))?;
    if pitch_nm <= 0 || body_nm <= 0 || ep_nm < 0 {
        return Err("QFN dimensions invalid".into());
    }
    let pad_w = (pitch_nm * 55 / 100)
        .min((pitch_nm - 120_000).max(1))
        .max(150_000.min(pitch_nm - 1).max(1));
    let pad_len = (pitch_nm * 16 / 10).clamp(550_000, 900_000);
    let corner_r = (pad_w.min(pad_len) / 8).min(80_000);
    let span = (pins_per_side as i64 - 1) * pitch_nm;
    let edge_offset = body_nm / 2;
    if ep_nm > 0 && ep_nm / 2 + 150_000 > edge_offset - pad_len / 2 {
        return Err("QFN exposed pad overlaps perimeter pads".into());
    }
    let mut pads = Vec::with_capacity(total_pads);
    let mut pin_num = 1usize;
    let horiz = PadShape::RoundRect {
        width: pad_len,
        height: pad_w,
        corner_radius: corner_r,
    };
    let vert = PadShape::RoundRect {
        width: pad_w,
        height: pad_len,
        corner_radius: corner_r,
    };
    // Sides in standard CCW pin-1 order: Left (top->bot), Bottom (L->R), Right (bot->top), Top (R->L).
    let sides = [
        (&horiz, Pt::new(-edge_offset, span / 2), (0, -pitch_nm)),
        (&vert, Pt::new(-span / 2, -edge_offset), (pitch_nm, 0)),
        (&horiz, Pt::new(edge_offset, -span / 2), (0, pitch_nm)),
        (&vert, Pt::new(span / 2, edge_offset), (-pitch_nm, 0)),
    ];
    for (shape, start, (dx, dy)) in sides {
        for i in 0..pins_per_side {
            let step = i as i64;
            pads.push(smd_pad(
                pin_num.to_string(),
                shape.clone(),
                Pt::new(start.x + step * dx, start.y + step * dy),
            ));
            pin_num += 1;
        }
    }
    if ep_nm > 0 {
        pads.push(smd_pad(
            "EP",
            PadShape::Rect {
                width: ep_nm,
                height: ep_nm,
            },
            Pt::new(0, 0),
        ));
    }
    let outer_half = (edge_offset + pad_len / 2).max(body_nm / 2) + 250_000;
    let graphics = vec![
        Graphic::Rect {
            layer: "F.Fab".into(),
            min: Pt::new(-body_nm / 2, -body_nm / 2),
            max: Pt::new(body_nm / 2, body_nm / 2),
            width: 100_000,
            fill: false,
        },
        Graphic::Circle {
            layer: "F.Silk".into(),
            center: Pt::new(-edge_offset - pad_len / 2 - 200_000, span / 2),
            radius: 120_000,
            width: 120_000,
            fill: true,
        },
    ];
    Ok(FootprintDef {
        name: format!(
            "QFN-{}_{:.2}mm",
            pins_per_side * 4,
            body_nm as f64 / MM as f64
        ),
        pads,
        courtyard: rect_courtyard(outer_half, outer_half),
        graphics,
        keepouts: Vec::new(),
        models: Vec::new(),
    })
}

/// Parametric IPC-7351B SOT-23 footprint for 3, 5, or 6 pins.
pub fn sot23(pin_count: usize) -> Result<FootprintDef, String> {
    let (pad_w, coords): (Nm, Vec<(&str, Nm, Nm)>) = match pin_count {
        3 => (
            800_000,
            vec![
                ("1", -950_000, -1_100_000),
                ("2", 950_000, -1_100_000),
                ("3", 0, 1_100_000),
            ],
        ),
        5 => (
            600_000,
            vec![
                ("1", -950_000, -1_100_000),
                ("2", 0, -1_100_000),
                ("3", 950_000, -1_100_000),
                ("4", 950_000, 1_100_000),
                ("5", -950_000, 1_100_000),
            ],
        ),
        6 => (
            600_000,
            vec![
                ("1", -950_000, -1_100_000),
                ("2", 0, -1_100_000),
                ("3", 950_000, -1_100_000),
                ("4", 950_000, 1_100_000),
                ("5", 0, 1_100_000),
                ("6", -950_000, 1_100_000),
            ],
        ),
        other => {
            return Err(format!(
                "unsupported SOT-23 pin count {other} (expected 3, 5, or 6)"
            ));
        }
    };
    let pad_h = 1_000_000;
    let shape = PadShape::RoundRect {
        width: pad_w,
        height: pad_h,
        corner_radius: 75_000,
    };
    let pads = coords
        .into_iter()
        .map(|(name, x, y)| smd_pad(name, shape.clone(), Pt::new(x, y)))
        .collect();
    let graphics = vec![
        Graphic::Rect {
            layer: "F.Fab".into(),
            min: Pt::new(-1_450_000, -800_000),
            max: Pt::new(1_450_000, 800_000),
            width: 100_000,
            fill: false,
        },
        Graphic::Circle {
            layer: "F.Silk".into(),
            center: Pt::new(-1_650_000, -1_100_000),
            radius: 120_000,
            width: 120_000,
            fill: true,
        },
    ];
    Ok(FootprintDef {
        name: format!("SOT-23-{pin_count}"),
        pads,
        courtyard: rect_courtyard(1_700_000, 1_850_000),
        graphics,
        keepouts: Vec::new(),
        models: Vec::new(),
    })
}

/// Parametric single-row THT pin header; pin 1 sits at origin, pins step along -Y.
pub fn pin_header(pins: usize, pitch_nm: Nm) -> Result<FootprintDef, String> {
    if pins == 0 || pins > MAX_GENERATED_PADS {
        return Err(format!(
            "pin_header pins must be in 1..={MAX_GENERATED_PADS}"
        ));
    }
    if pitch_nm <= 0 {
        return Err("pin_header pitch_nm must be > 0".into());
    }
    let drill_nm = (pitch_nm * 40 / 100).clamp(650_000, 1_100_000);
    let pad_dia = (drill_nm + 700_000)
        .min(pitch_nm - 250_000)
        .max(drill_nm + 300_000);
    let mut pads = Vec::with_capacity(pins);
    for i in 0..pins {
        let shape = if i == 0 {
            PadShape::RoundRect {
                width: pad_dia,
                height: pad_dia,
                corner_radius: pad_dia / 6,
            }
        } else {
            PadShape::Circle { diameter: pad_dia }
        };
        pads.push(PadDef {
            name: (i + 1).to_string(),
            kind: PadKind::Tht,
            shape,
            pos: Pt::new(0, -((i as i64) * pitch_nm)),
            rot_deg: 0.0,
            drill_nm: Some(drill_nm),
            layers: tht_layers(),
            mask_margin_nm: None,
            paste_margin_nm: None,
        });
    }
    let half_x = pitch_nm / 2 + 250_000;
    let top_y = pitch_nm / 2 + 250_000;
    let bot_y = -((pins as i64 - 1) * pitch_nm) - pitch_nm / 2 - 250_000;
    let crtyd = vec![
        Pt::new(-half_x, bot_y),
        Pt::new(half_x, bot_y),
        Pt::new(half_x, top_y),
        Pt::new(-half_x, top_y),
    ];
    let bbox = BBoxNm::from_pts(&crtyd);
    let graphics = vec![Graphic::Rect {
        layer: "F.Silk".into(),
        min: Pt::new(bbox.min_x + 150_000, bbox.min_y + 150_000),
        max: Pt::new(bbox.max_x - 150_000, bbox.max_y - 150_000),
        width: 120_000,
        fill: false,
    }];
    Ok(FootprintDef {
        name: format!(
            "PinHeader_1x{pins:02}_P{:.2}mm",
            pitch_nm as f64 / MM as f64
        ),
        pads,
        courtyard: crtyd,
        graphics,
        keepouts: Vec::new(),
        models: Vec::new(),
    })
}
