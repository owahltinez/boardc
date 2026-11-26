use std::collections::BTreeMap;
use std::fs::File;
use std::io::BufWriter;
use std::path::Path;

use clipper2_rust::{EndType, JoinType, inflate_paths_64};
use gerber_types::{
    Aperture, ApertureDefinition, ApertureMacro, Circle, Command, CommentContent, CoordinateFormat,
    CoordinateMode, CoordinateNumber, Coordinates, DCode, FileAttribute, FileFunction,
    FilePolarity, GCode, GenerationSoftware, GerberCode, InterpolationMode, MCode, MacroBoolean,
    MacroDecimal, Operation, OutlinePrimitive, Part, Polarity, QuadrantMode, Rectangular, Unit,
    ZeroOmission,
};

use crate::geom::{ensure_ccw, local_to_board, polygon_area};
use crate::ir::{BBoxNm, Graphic, Nm, PadDef, PadShape, Placement, Pt, nm_to_mm, safe_slug};
use crate::shapes::{
    ResolvedShape, circle_poly, pad_to_world_shape, paths64_to_polys, pts_to_path64,
};

pub(crate) fn gerber_coord_format() -> CoordinateFormat {
    CoordinateFormat::new(ZeroOmission::Leading, CoordinateMode::Absolute, 4, 6)
}

pub(crate) fn copper_gerber_filename(layer_name: &str, idx: usize, cu_layers: &[String]) -> String {
    if layer_name == "F.Cu" {
        return "F_Cu.gtl".into();
    }
    if layer_name == "B.Cu" {
        return "B_Cu.gbl".into();
    }
    if let Some(rest) = layer_name
        .strip_prefix("In")
        .and_then(|s| s.strip_suffix(".Cu"))
    {
        return format!("In{rest}_Cu.g{rest}");
    }
    if idx == 0 && !cu_layers.iter().any(|l| l == "F.Cu") {
        return "F_Cu.gtl".into();
    }
    if idx + 1 == cu_layers.len() && !cu_layers.iter().any(|l| l == "B.Cu") {
        return "B_Cu.gbl".into();
    }
    format!("{}.g{idx}", safe_slug(layer_name))
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
enum ApertureKey {
    Circle(Nm),
    Rect(Nm, Nm),
    Obround(Nm, Nm),
    PolyMacro(Vec<(Nm, Nm)>),
}

pub(crate) struct GerberBuilder {
    cf: CoordinateFormat,
    function: FileFunction,
    next_code: i32,
    apertures: BTreeMap<ApertureKey, (i32, Aperture, Option<ApertureMacro>)>,
    draw_cmds: Vec<Command>,
    polarity: Polarity,
    current_ap: Option<i32>,
}

impl GerberBuilder {
    pub(crate) fn new(function: FileFunction) -> Self {
        Self {
            cf: gerber_coord_format(),
            function,
            next_code: 10,
            apertures: BTreeMap::new(),
            draw_cmds: Vec::new(),
            polarity: Polarity::Dark,
            current_ap: None,
        }
    }

    fn ensure_polarity(&mut self, pol: Polarity) {
        if self.polarity != pol {
            self.draw_cmds.push(pol.into());
            self.polarity = pol;
        }
    }

    fn select_aperture(&mut self, code: i32) {
        if self.current_ap != Some(code) {
            self.draw_cmds.push(DCode::SelectAperture(code).into());
            self.current_ap = Some(code);
        }
    }

    fn register_aperture(
        &mut self,
        key: ApertureKey,
        build: impl FnOnce(i32) -> (Aperture, Option<ApertureMacro>),
    ) -> i32 {
        if let Some((code, _, _)) = self.apertures.get(&key) {
            return *code;
        }
        let code = self.next_code;
        self.next_code += 1;
        let (ap, am) = build(code);
        self.apertures.insert(key, (code, ap, am));
        code
    }

    fn register_circle(&mut self, diameter: Nm) -> i32 {
        let dia = diameter.max(1_000);
        self.register_aperture(ApertureKey::Circle(dia), |_| {
            (Aperture::Circle(Circle::new(nm_to_mm(dia))), None)
        })
    }

    fn register_rect(&mut self, width: Nm, height: Nm) -> i32 {
        let (w, h) = (width.max(1_000), height.max(1_000));
        self.register_aperture(ApertureKey::Rect(w, h), |_| {
            (
                Aperture::Rectangle(Rectangular::new(nm_to_mm(w), nm_to_mm(h))),
                None,
            )
        })
    }

    fn register_obround(&mut self, width: Nm, height: Nm) -> i32 {
        let (w, h) = (width.max(1_000), height.max(1_000));
        self.register_aperture(ApertureKey::Obround(w, h), |_| {
            (
                Aperture::Obround(Rectangular::new(nm_to_mm(w), nm_to_mm(h))),
                None,
            )
        })
    }

    fn register_poly_macro(&mut self, rel_verts: Vec<(Nm, Nm)>) -> i32 {
        let key = ApertureKey::PolyMacro(rel_verts.clone());
        self.register_aperture(key, |code| {
            let name = format!("M_PAD_{code}");
            let mut pts: Vec<(MacroDecimal, MacroDecimal)> = rel_verts
                .iter()
                .map(|&(x, y)| {
                    (
                        MacroDecimal::Value(nm_to_mm(x)),
                        MacroDecimal::Value(nm_to_mm(y)),
                    )
                })
                .collect();
            if let Some(first) = pts.first().cloned()
                && pts.last() != Some(&first)
            {
                pts.push(first);
            }
            let outline = OutlinePrimitive {
                exposure: MacroBoolean::Value(true),
                points: pts,
                angle: MacroDecimal::Value(0.0),
            };
            let am = ApertureMacro::new(name.clone()).add_content(outline);
            (Aperture::Macro(name, None), Some(am))
        })
    }

    fn coords(&self, pt: Pt) -> Coordinates {
        Coordinates::new(
            CoordinateNumber::new(pt.x),
            CoordinateNumber::new(pt.y),
            self.cf,
        )
    }

    fn flash_code_at(&mut self, code: i32, pt: Pt) {
        self.select_aperture(code);
        self.draw_cmds
            .push(DCode::Operation(Operation::Flash(Some(self.coords(pt)))).into());
    }

    fn emit_closed_contour(&mut self, verts: &[Pt]) {
        self.draw_cmds
            .push(DCode::Operation(Operation::Move(Some(self.coords(verts[0])))).into());
        for &v in &verts[1..] {
            self.draw_cmds
                .push(DCode::Operation(Operation::Interpolate(Some(self.coords(v)), None)).into());
        }
        self.draw_cmds.push(
            DCode::Operation(Operation::Interpolate(Some(self.coords(verts[0])), None)).into(),
        );
    }

    pub(crate) fn flash_circle(&mut self, center: Pt, diameter: Nm) {
        if diameter <= 0 {
            return;
        }
        self.ensure_polarity(Polarity::Dark);
        let code = self.register_circle(diameter);
        self.flash_code_at(code, center);
    }

    pub(crate) fn flash_shape(&mut self, shape: &ResolvedShape, anchor: Pt) {
        self.ensure_polarity(Polarity::Dark);
        match shape {
            ResolvedShape::Circle { center, radius } if *radius > 0 => {
                let code = self.register_circle(*radius * 2);
                self.flash_code_at(code, *center);
            }
            ResolvedShape::Capsule { a, b, radius } if *radius > 0 => {
                let mid = a.midpoint(*b);
                if a.y == b.y {
                    let code = self.register_obround((b.x - a.x).abs() + *radius * 2, *radius * 2);
                    self.flash_code_at(code, mid);
                } else if a.x == b.x {
                    let code = self.register_obround(*radius * 2, (b.y - a.y).abs() + *radius * 2);
                    self.flash_code_at(code, mid);
                } else {
                    self.flash_poly_verts(&shape.to_polygon(16), mid);
                }
            }
            ResolvedShape::Poly { verts, bbox } => {
                if let Some((w, h, center)) = axis_aligned_rect_dims(verts, *bbox) {
                    let code = self.register_rect(w, h);
                    self.flash_code_at(code, center);
                } else {
                    self.flash_poly_verts(verts, anchor);
                }
            }
            _ => {}
        }
    }

    fn flash_poly_verts(&mut self, verts: &[Pt], anchor: Pt) {
        let mut clean = Vec::with_capacity(verts.len());
        for &v in verts {
            let rel = (v.x - anchor.x, v.y - anchor.y);
            if clean.last() != Some(&rel) {
                clean.push(rel);
            }
        }
        if clean.len() > 1 && clean.first() == clean.last() {
            clean.pop();
        }
        if clean.len() < 3 {
            return;
        }
        let code = self.register_poly_macro(clean);
        self.flash_code_at(code, anchor);
    }

    pub(crate) fn stroke_segment(&mut self, a: Pt, b: Pt, width: Nm) {
        let w = width.max(10_000);
        self.ensure_polarity(Polarity::Dark);
        let code = self.register_circle(w);
        self.select_aperture(code);
        if a == b {
            self.draw_cmds
                .push(DCode::Operation(Operation::Flash(Some(self.coords(a)))).into());
        } else {
            self.draw_cmds
                .push(DCode::Operation(Operation::Move(Some(self.coords(a)))).into());
            self.draw_cmds
                .push(DCode::Operation(Operation::Interpolate(Some(self.coords(b)), None)).into());
        }
    }

    pub(crate) fn stroke_closed_poly(&mut self, verts: &[Pt], width: Nm) {
        if verts.len() < 2 {
            return;
        }
        let w = width.max(10_000);
        self.ensure_polarity(Polarity::Dark);
        let code = self.register_circle(w);
        self.select_aperture(code);
        self.emit_closed_contour(verts);
    }

    pub(crate) fn fill_region(&mut self, verts: &[Pt], polarity: Polarity) {
        if verts.len() < 3 {
            return;
        }
        self.ensure_polarity(polarity);
        self.draw_cmds.push(GCode::RegionMode(true).into());
        self.emit_closed_contour(verts);
        self.draw_cmds.push(GCode::RegionMode(false).into());
    }

    pub(crate) fn emit_graphic(&mut self, g: &Graphic, pl: Option<&Placement>) {
        let map_pt = |p: Pt| match pl {
            Some(pl) => local_to_board(p, pl.pos.x, pl.pos.y, pl.rot_deg, pl.side),
            None => p,
        };
        match g {
            Graphic::Line { a, b, width, .. } => {
                self.stroke_segment(map_pt(*a), map_pt(*b), *width);
            }
            Graphic::Rect {
                min,
                max,
                width,
                fill,
                ..
            } => {
                let corners = [
                    map_pt(*min),
                    map_pt(Pt::new(max.x, min.y)),
                    map_pt(*max),
                    map_pt(Pt::new(min.x, max.y)),
                ];
                if *fill {
                    self.fill_region(&corners, Polarity::Dark);
                } else {
                    self.stroke_closed_poly(&corners, *width);
                }
            }
            Graphic::Circle {
                center,
                radius,
                width,
                fill,
                ..
            } => {
                let c = map_pt(*center);
                if *fill {
                    self.flash_circle(c, *radius * 2);
                } else {
                    let ring = circle_poly(c, *radius, 32);
                    self.stroke_closed_poly(&ring, *width);
                }
            }
            Graphic::Poly {
                verts, width, fill, ..
            } => {
                let pts: Vec<Pt> = verts.iter().map(|&v| map_pt(v)).collect();
                if *fill {
                    self.fill_region(&pts, Polarity::Dark);
                } else {
                    self.stroke_closed_poly(&pts, *width);
                }
            }
        }
    }

    pub(crate) fn write_file(self, path: &Path) -> Result<(), String> {
        let file_polarity = if matches!(self.function, FileFunction::SolderMask { .. }) {
            FilePolarity::Negative
        } else {
            FilePolarity::Positive
        };
        let mut cmds: Vec<Command> = vec![
            GCode::Comment(CommentContent::String("Generated by boardc".into())).into(),
            FileAttribute::GenerationSoftware(GenerationSoftware::new(
                "boardc",
                "boardc",
                Some("0.1.0"),
            ))
            .into(),
            FileAttribute::Part(Part::Single).into(),
            FileAttribute::FilePolarity(file_polarity).into(),
            FileAttribute::FileFunction(self.function).into(),
            FileAttribute::SameCoordinates(None).into(),
            self.cf.into(),
            Unit::Millimeters.into(),
            GCode::InterpolationMode(InterpolationMode::Linear).into(),
            GCode::QuadrantMode(QuadrantMode::Multi).into(),
        ];

        // Emit macros first, then aperture definitions sorted by D-code.
        let mut defs: Vec<(i32, Aperture, Option<ApertureMacro>)> =
            self.apertures.into_values().collect();
        defs.sort_by_key(|(code, _, _)| *code);
        for (_, _, macro_opt) in &defs {
            if let Some(am) = macro_opt {
                cmds.push(am.clone().into());
            }
        }
        for (code, ap, _) in defs {
            cmds.push(ApertureDefinition::new(code, ap).into());
        }

        cmds.push(Polarity::Dark.into());
        cmds.extend(self.draw_cmds);
        cmds.push(MCode::EndOfFile.into());

        let file = File::create(path)
            .map_err(|e| format!("failed to create Gerber '{}': {e}", path.display()))?;
        let mut writer = BufWriter::new(file);
        cmds.serialize(&mut writer)
            .map_err(|e| format!("failed to serialize Gerber '{}': {e}", path.display()))?;
        Ok(())
    }
}

fn axis_aligned_rect_dims(verts: &[Pt], bbox: BBoxNm) -> Option<(Nm, Nm, Pt)> {
    if verts.len() != 4 {
        return None;
    }
    let ok = verts.iter().all(|p| {
        (p.x == bbox.min_x || p.x == bbox.max_x) && (p.y == bbox.min_y || p.y == bbox.max_y)
    });
    if !ok || bbox.width() <= 0 || bbox.height() <= 0 {
        return None;
    }
    Some((bbox.width(), bbox.height(), bbox.center()))
}

pub(crate) fn pad_world_shape_with_margin(
    pad: &PadDef,
    pl: &Placement,
    margin_nm: Nm,
) -> Option<ResolvedShape> {
    if margin_nm == 0 {
        return Some(pad_to_world_shape(pad, pl, 16));
    }
    let delta = margin_nm.saturating_mul(2);
    let expanded_shape = match &pad.shape {
        PadShape::Circle { diameter } => {
            let d = diameter + delta;
            (d > 0).then_some(PadShape::Circle { diameter: d })?
        }
        PadShape::Rect { width, height } => {
            let (w, h) = (width + delta, height + delta);
            (w > 0 && h > 0).then_some(PadShape::Rect {
                width: w,
                height: h,
            })?
        }
        PadShape::RoundRect {
            width,
            height,
            corner_radius,
        } => {
            let (w, h) = (width + delta, height + delta);
            if w <= 0 || h <= 0 {
                return None;
            }
            let r = (corner_radius + margin_nm).clamp(0, w.min(h) / 2);
            PadShape::RoundRect {
                width: w,
                height: h,
                corner_radius: r,
            }
        }
        PadShape::Oval { width, height } => {
            let (w, h) = (width + delta, height + delta);
            (w > 0 && h > 0).then_some(PadShape::Oval {
                width: w,
                height: h,
            })?
        }
        PadShape::Poly { verts } => {
            let mut ccw = verts.clone();
            ensure_ccw(&mut ccw);
            let p64 = vec![pts_to_path64(&ccw)];
            let out = inflate_paths_64(
                &p64,
                margin_nm as f64,
                JoinType::Round,
                EndType::Polygon,
                2.0,
                1_000.0,
            );
            let poly = paths64_to_polys(&out)
                .into_iter()
                .max_by(|a, b| polygon_area(a).total_cmp(&polygon_area(b)))?;
            PadShape::Poly { verts: poly }
        }
    };
    let tmp = PadDef {
        shape: expanded_shape,
        ..pad.clone()
    };
    Some(pad_to_world_shape(&tmp, pl, 16))
}

pub(crate) fn pad_anchor_world(pad: &PadDef, pl: &Placement) -> Pt {
    local_to_board(pad.pos, pl.pos.x, pl.pos.y, pl.rot_deg, pl.side)
}
