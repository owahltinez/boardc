use std::f64::consts::{FRAC_PI_2, PI, TAU};

use clipper2_rust::{Path64, Paths64, Point64};

use crate::geom::{
    ensure_ccw, local_to_board, point_in_polygon, point_to_polygon_dist,
    point_to_polygon_edge_dist, point_to_segment_dist, point_to_segment_dist_sq,
    polygon_to_polygon_dist, rotate_pt, segment_to_polygon_dist, segment_to_polygon_edge_dist,
    segment_to_segment_dist,
};
use crate::ir::{BBoxNm, FootprintDef, Nm, PadDef, PadShape, Placement, Pt};

const DEFAULT_ARC_SEGS: usize = 16;

/// Exact world-space 2D shape in nanometers.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ResolvedShape {
    Circle {
        center: Pt,
        radius: Nm,
    },
    Capsule {
        a: Pt,
        b: Pt,
        radius: Nm,
    },
    RoundRect {
        corners: [Pt; 4],
        radius: Nm,
        bbox: BBoxNm,
    },
    Poly {
        verts: Vec<Pt>,
        bbox: BBoxNm,
    },
}

impl ResolvedShape {
    pub fn polygon(mut verts: Vec<Pt>) -> Self {
        if verts.len() > 1 && verts.first() == verts.last() {
            verts.pop();
        }
        ensure_ccw(&mut verts);
        let bbox = BBoxNm::from_pts(&verts);
        Self::Poly { verts, bbox }
    }

    pub fn bbox(&self) -> BBoxNm {
        match self {
            Self::Circle { center, radius } => BBoxNm::from_pt(*center, *radius),
            Self::Capsule { a, b, radius } => BBoxNm::from_pts(&[*a, *b]).expand(*radius),
            Self::RoundRect { bbox, .. } | Self::Poly { bbox, .. } => *bbox,
        }
    }

    pub fn ref_pt(&self) -> Pt {
        match self {
            Self::Circle { center, .. } => *center,
            Self::Capsule { a, .. } => *a,
            Self::RoundRect { corners, .. } => corners[0],
            Self::Poly { verts, bbox } => verts.first().copied().unwrap_or_else(|| bbox.center()),
        }
    }

    pub fn center(&self) -> Pt {
        match self {
            Self::Circle { center, .. } => *center,
            Self::Capsule { a, b, .. } => a.midpoint(*b),
            Self::RoundRect { bbox, .. } | Self::Poly { bbox, .. } => bbox.center(),
        }
    }

    pub fn contains_pt(&self, p: Pt) -> bool {
        match self {
            Self::Circle { center, radius } => {
                *radius >= 0 && center.dist_sq(p) <= (*radius as f64) * (*radius as f64)
            }
            Self::Capsule { a, b, radius } => {
                *radius >= 0
                    && point_to_segment_dist_sq(p, *a, *b) <= (*radius as f64) * (*radius as f64)
            }
            Self::RoundRect {
                corners, radius, ..
            } => *radius >= 0 && point_to_polygon_dist(p, corners) <= *radius as f64,
            Self::Poly { verts, .. } => point_in_polygon(p, verts),
        }
    }

    pub fn to_polygon(&self, arc_segs: usize) -> Vec<Pt> {
        let segs = arc_segs.max(4);
        match self {
            Self::Circle { center, radius } => circle_poly(*center, *radius, segs * 4),
            Self::Capsule { a, b, radius } => capsule_poly(*a, *b, *radius, segs),
            Self::RoundRect {
                corners, radius, ..
            } => roundrect_corners_poly(corners, *radius, segs),
            Self::Poly { verts, .. } => verts.clone(),
        }
    }

    pub fn dist_to_point(&self, p: Pt) -> f64 {
        match self {
            Self::Circle { center, radius } => (center.dist(p) - *radius as f64).max(0.0),
            Self::Capsule { a, b, radius } => {
                (point_to_segment_dist(p, *a, *b) - *radius as f64).max(0.0)
            }
            Self::RoundRect {
                corners, radius, ..
            } => (point_to_polygon_dist(p, corners) - *radius as f64).max(0.0),
            Self::Poly { verts, .. } => point_to_polygon_dist(p, verts),
        }
    }

    pub fn dist_to_circle(&self, center: Pt, radius: Nm) -> f64 {
        let r = radius as f64;
        match self {
            Self::Circle {
                center: c,
                radius: cr,
            } => c.dist(center) - *cr as f64 - r,
            Self::Capsule { a, b, radius: cr } => {
                point_to_segment_dist(center, *a, *b) - *cr as f64 - r
            }
            Self::RoundRect {
                corners,
                radius: sr,
                ..
            } => point_to_polygon_dist(center, corners) - *sr as f64 - r,
            Self::Poly { verts, .. } => point_to_polygon_dist(center, verts) - r,
        }
    }

    pub fn dist_to_capsule(&self, a: Pt, b: Pt, radius: Nm) -> f64 {
        let r = radius as f64;
        match self {
            Self::Circle { center, radius: cr } => {
                point_to_segment_dist(*center, a, b) - *cr as f64 - r
            }
            Self::Capsule {
                a: sa,
                b: sb,
                radius: sr,
            } => segment_to_segment_dist(*sa, *sb, a, b) - *sr as f64 - r,
            Self::RoundRect {
                corners,
                radius: sr,
                ..
            } => segment_to_polygon_dist(a, b, corners) - *sr as f64 - r,
            Self::Poly { verts, .. } => segment_to_polygon_dist(a, b, verts) - r,
        }
    }

    pub fn dist_to_shape(&self, other: &Self) -> f64 {
        match (self, other) {
            (_, Self::Circle { center, radius }) => self.dist_to_circle(*center, *radius),
            (Self::Circle { center, radius }, _) => other.dist_to_circle(*center, *radius),
            (_, Self::Capsule { a, b, radius }) => self.dist_to_capsule(*a, *b, *radius),
            (Self::Capsule { a, b, radius }, _) => other.dist_to_capsule(*a, *b, *radius),
            (
                Self::RoundRect {
                    corners: ac,
                    radius: ar,
                    ..
                },
                Self::RoundRect {
                    corners: oc,
                    radius: or,
                    ..
                },
            ) => polygon_to_polygon_dist(ac, oc) - *ar as f64 - *or as f64,
            (
                Self::RoundRect {
                    corners, radius, ..
                },
                Self::Poly { verts, .. },
            )
            | (
                Self::Poly { verts, .. },
                Self::RoundRect {
                    corners, radius, ..
                },
            ) => polygon_to_polygon_dist(corners, verts) - *radius as f64,
            (Self::Poly { verts: av, .. }, Self::Poly { verts: bv, .. }) => {
                polygon_to_polygon_dist(av, bv)
            }
        }
    }

    pub fn dist_to_polygon_edge(&self, verts: &[Pt]) -> f64 {
        match self {
            Self::Circle { center, radius } => {
                point_to_polygon_edge_dist(*center, verts) - *radius as f64
            }
            Self::Capsule { a, b, radius } => {
                segment_to_polygon_edge_dist(*a, *b, verts) - *radius as f64
            }
            Self::RoundRect {
                corners, radius, ..
            } => {
                let edge_d = (0..4)
                    .map(|i| segment_to_polygon_edge_dist(corners[i], corners[(i + 1) % 4], verts))
                    .fold(f64::INFINITY, f64::min);
                let inside_d = if verts.iter().any(|&p| self.contains_pt(p)) {
                    verts
                        .iter()
                        .map(|&p| point_to_polygon_dist(p, corners))
                        .fold(edge_d, f64::min)
                } else {
                    edge_d
                };
                inside_d - *radius as f64
            }
            Self::Poly { verts: pv, .. } => match pv.len() {
                0 => f64::INFINITY,
                1 => point_to_polygon_edge_dist(pv[0], verts),
                _ => (0..pv.len())
                    .map(|i| segment_to_polygon_edge_dist(pv[i], pv[(i + 1) % pv.len()], verts))
                    .fold(f64::INFINITY, f64::min),
            },
        }
    }
}

pub fn rect_poly(width: Nm, height: Nm) -> Vec<Pt> {
    if width <= 0 || height <= 0 {
        return Vec::new();
    }
    let hw = width / 2;
    let hh = height / 2;
    vec![
        Pt::new(-hw, -hh),
        Pt::new(hw, -hh),
        Pt::new(hw, hh),
        Pt::new(-hw, hh),
    ]
}

pub fn circle_poly(center: Pt, radius: Nm, segs: usize) -> Vec<Pt> {
    if radius <= 0 {
        return Vec::new();
    }
    let n = segs.max(8);
    let r = radius as f64;
    (0..n)
        .map(|i| {
            let theta = TAU * (i as f64) / (n as f64);
            Pt::new(
                center.x + (r * theta.cos()).round() as Nm,
                center.y + (r * theta.sin()).round() as Nm,
            )
        })
        .collect()
}

pub(crate) fn roundrect_corners_poly(corners: &[Pt; 4], radius: Nm, arc_segs: usize) -> Vec<Pt> {
    if radius <= 0 {
        return corners.to_vec();
    }
    let rf = radius as f64;
    let segs = arc_segs.max(2);
    let mut verts = Vec::with_capacity(4 * (segs + 1));
    for i in 0..4 {
        let prev = corners[(i + 3) % 4];
        let curr = corners[i];
        let next = corners[(i + 1) % 4];
        let dx_in = (curr.x - prev.x) as f64;
        let dy_in = (curr.y - prev.y) as f64;
        let dx_out = (next.x - curr.x) as f64;
        let dy_out = (next.y - curr.y) as f64;
        let a0 = (-dx_in).atan2(dy_in);
        let a1 = (-dx_out).atan2(dy_out);
        let mut sweep = (a1 - a0).rem_euclid(TAU);
        if sweep < 1e-6 {
            sweep = FRAC_PI_2;
        }
        for s in 0..=segs {
            let a = a0 + sweep * (s as f64) / (segs as f64);
            let p = Pt::new(
                curr.x + (rf * a.cos()).round() as Nm,
                curr.y + (rf * a.sin()).round() as Nm,
            );
            if verts.last() != Some(&p) {
                verts.push(p);
            }
        }
    }
    if verts.len() > 1 && verts.first() == verts.last() {
        verts.pop();
    }
    verts
}

pub fn roundrect_poly(width: Nm, height: Nm, corner_radius: Nm, arc_segs: usize) -> Vec<Pt> {
    if width <= 0 || height <= 0 {
        return Vec::new();
    }
    let r = corner_radius.clamp(0, width.min(height) / 2);
    if r == 0 {
        return rect_poly(width, height);
    }
    let hw = width as f64 / 2.0;
    let hh = height as f64 / 2.0;
    let rf = r as f64;
    let segs = arc_segs.max(2);
    let corners = [
        (hw - rf, -hh + rf, -FRAC_PI_2),
        (hw - rf, hh - rf, 0.0),
        (-hw + rf, hh - rf, FRAC_PI_2),
        (-hw + rf, -hh + rf, PI),
    ];
    let mut verts = Vec::with_capacity(4 * (segs + 1));
    for (cx, cy, a0) in corners {
        for i in 0..=segs {
            let a = a0 + FRAC_PI_2 * (i as f64) / (segs as f64);
            let p = Pt::new(
                (cx + rf * a.cos()).round() as Nm,
                (cy + rf * a.sin()).round() as Nm,
            );
            if verts.last() != Some(&p) {
                verts.push(p);
            }
        }
    }
    if verts.len() > 1 && verts.first() == verts.last() {
        verts.pop();
    }
    verts
}

pub fn oval_poly(width: Nm, height: Nm, arc_segs: usize) -> Vec<Pt> {
    if width == height {
        return circle_poly(Pt::new(0, 0), width / 2, arc_segs.max(4) * 4);
    }
    if width > height {
        let r = height / 2;
        let half = (width - height) / 2;
        capsule_poly(Pt::new(-half, 0), Pt::new(half, 0), r, arc_segs)
    } else {
        let r = width / 2;
        let half = (height - width) / 2;
        capsule_poly(Pt::new(0, -half), Pt::new(0, half), r, arc_segs)
    }
}

pub fn capsule_poly(a: Pt, b: Pt, radius: Nm, arc_segs: usize) -> Vec<Pt> {
    if a == b {
        return circle_poly(a, radius, arc_segs.max(4) * 4);
    }
    let segs = arc_segs.max(4);
    let dx = (b.x - a.x) as f64;
    let dy = (b.y - a.y) as f64;
    let base = dy.atan2(dx);
    let rf = radius as f64;
    let mut verts = Vec::with_capacity(2 * (segs + 1));
    for i in 0..=segs {
        let ang = (base - FRAC_PI_2) + PI * (i as f64) / (segs as f64);
        verts.push(Pt::new(
            b.x + (rf * ang.cos()).round() as Nm,
            b.y + (rf * ang.sin()).round() as Nm,
        ));
    }
    for i in 0..=segs {
        let ang = (base + FRAC_PI_2) + PI * (i as f64) / (segs as f64);
        verts.push(Pt::new(
            a.x + (rf * ang.cos()).round() as Nm,
            a.y + (rf * ang.sin()).round() as Nm,
        ));
    }
    verts
}

pub(crate) fn transform_pad_local_pt_raw(
    pt: Pt,
    pad_pos: Pt,
    pad_rot_deg: f64,
    pl: &Placement,
) -> Pt {
    let rotated = if pad_rot_deg.abs() > 1e-9 {
        rotate_pt(pt, pad_rot_deg)
    } else {
        pt
    };
    let in_fp = Pt::new(pad_pos.x + rotated.x, pad_pos.y + rotated.y);
    local_to_board(in_fp, pl.pos.x, pl.pos.y, pl.rot_deg, pl.side)
}

pub(crate) fn transform_pad_local_pt(pt: Pt, pad: &PadDef, pl: &Placement) -> Pt {
    transform_pad_local_pt_raw(pt, pad.pos, pad.rot_deg, pl)
}

/// Resolves a pad shape, local position, and local rotation into its exact world-space `ResolvedShape`.
pub(crate) fn shape_to_world_shape(
    shape: &PadShape,
    pad_pos: Pt,
    pad_rot_deg: f64,
    pl: &Placement,
) -> ResolvedShape {
    let tx = |pt: Pt| transform_pad_local_pt_raw(pt, pad_pos, pad_rot_deg, pl);
    match shape {
        PadShape::Circle { diameter } => ResolvedShape::Circle {
            center: local_to_board(pad_pos, pl.pos.x, pl.pos.y, pl.rot_deg, pl.side),
            radius: diameter / 2,
        },
        PadShape::Oval { width, height } if width == height => ResolvedShape::Circle {
            center: local_to_board(pad_pos, pl.pos.x, pl.pos.y, pl.rot_deg, pl.side),
            radius: width / 2,
        },
        PadShape::Oval { width, height } => {
            let (a_loc, b_loc, r) = if width > height {
                let half = (width - height) / 2;
                (Pt::new(-half, 0), Pt::new(half, 0), height / 2)
            } else {
                let half = (height - width) / 2;
                (Pt::new(0, -half), Pt::new(0, half), width / 2)
            };
            ResolvedShape::Capsule {
                a: tx(a_loc),
                b: tx(b_loc),
                radius: r,
            }
        }
        PadShape::Rect { width, height } => {
            let verts = rect_poly(*width, *height).into_iter().map(tx).collect();
            ResolvedShape::polygon(verts)
        }
        PadShape::RoundRect {
            width,
            height,
            corner_radius,
        } => {
            if *width <= 0 || *height <= 0 {
                return ResolvedShape::polygon(Vec::new());
            }
            let r = (*corner_radius).clamp(0, (*width).min(*height) / 2);
            if r == 0 {
                let verts = rect_poly(*width, *height).into_iter().map(tx).collect();
                ResolvedShape::polygon(verts)
            } else if *width == 2 * r && *height == 2 * r {
                ResolvedShape::Circle {
                    center: local_to_board(pad_pos, pl.pos.x, pl.pos.y, pl.rot_deg, pl.side),
                    radius: r,
                }
            } else if *width == 2 * r {
                let half = (*height - *width) / 2;
                ResolvedShape::Capsule {
                    a: tx(Pt::new(0, -half)),
                    b: tx(Pt::new(0, half)),
                    radius: r,
                }
            } else if *height == 2 * r {
                let half = (*width - *height) / 2;
                ResolvedShape::Capsule {
                    a: tx(Pt::new(-half, 0)),
                    b: tx(Pt::new(half, 0)),
                    radius: r,
                }
            } else {
                let hx = ((*width as f64 / 2.0) - r as f64).round() as Nm;
                let hy = ((*height as f64 / 2.0) - r as f64).round() as Nm;
                let mut corners = [
                    tx(Pt::new(hx, -hy)),
                    tx(Pt::new(hx, hy)),
                    tx(Pt::new(-hx, hy)),
                    tx(Pt::new(-hx, -hy)),
                ];
                ensure_ccw(&mut corners);
                let bbox = BBoxNm::from_pts(&corners).expand(r);
                ResolvedShape::RoundRect {
                    corners,
                    radius: r,
                    bbox,
                }
            }
        }
        PadShape::Poly { verts } => {
            let world = verts.iter().map(|&p| tx(p)).collect();
            ResolvedShape::polygon(world)
        }
    }
}

/// Resolves a footprint pad into its exact world-space `ResolvedShape`.
pub fn pad_to_world_shape(pad: &PadDef, pl: &Placement, _arc_segs: usize) -> ResolvedShape {
    shape_to_world_shape(&pad.shape, pad.pos, pad.rot_deg, pl)
}

/// Resolves a footprint pad directly to a world-space CCW polygon.
pub fn pad_world_polygon(pad: &PadDef, pl: &Placement, arc_segs: usize) -> Vec<Pt> {
    let segs = if arc_segs == 0 {
        DEFAULT_ARC_SEGS
    } else {
        arc_segs
    };
    pad_to_world_shape(pad, pl, segs).to_polygon(segs)
}

/// Resolves a footprint courtyard to world-space CCW polygon coordinates.
pub fn courtyard_world_polygon(fp: &FootprintDef, pl: &Placement) -> Vec<Pt> {
    let local = if !fp.courtyard.is_empty() {
        fp.courtyard.clone()
    } else {
        let mut pts = Vec::new();
        let identity = Placement::default();
        for pad in &fp.pads {
            let b = pad_to_world_shape(pad, &identity, 8).bbox();
            pts.push(Pt::new(b.min_x, b.min_y));
            pts.push(Pt::new(b.max_x, b.max_y));
        }
        let b = BBoxNm::from_pts(&pts).expand(250_000);
        vec![
            Pt::new(b.min_x, b.min_y),
            Pt::new(b.max_x, b.min_y),
            Pt::new(b.max_x, b.max_y),
            Pt::new(b.min_x, b.max_y),
        ]
    };
    let mut world: Vec<Pt> = local
        .into_iter()
        .map(|p| local_to_board(p, pl.pos.x, pl.pos.y, pl.rot_deg, pl.side))
        .collect();
    ensure_ccw(&mut world);
    world
}

pub fn pts_to_path64(pts: &[Pt]) -> Path64 {
    pts.iter().map(|p| Point64::new(p.x, p.y)).collect()
}

pub fn path64_to_pts(path: &Path64) -> Vec<Pt> {
    path.iter().map(|p| Pt::new(p.x, p.y)).collect()
}

pub fn polys_to_paths64(polys: &[Vec<Pt>]) -> Paths64 {
    polys.iter().map(|p| pts_to_path64(p)).collect()
}

pub fn paths64_to_polys(paths: &Paths64) -> Vec<Vec<Pt>> {
    paths.iter().map(path64_to_pts).collect()
}

pub fn shape_to_path64(shape: &ResolvedShape, arc_segs: usize) -> Path64 {
    pts_to_path64(&shape.to_polygon(arc_segs))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ir::{PadKind, Side};

    #[test]
    fn roundrect_analytic_distances_and_polygon_match_across_rotations_and_radii() {
        let test_cases = [
            (1_200_000, 800_000, 200_000, 0.0, Side::Top),
            (1_200_000, 800_000, 200_000, 37.0, Side::Top),
            (1_200_000, 800_000, 300_000, 125.0, Side::Bottom),
            (800_000, 800_000, 600_000, 45.0, Side::Top),
            (1_000_000, 600_000, 0, 25.0, Side::Top),
        ];
        for (w, h, cr, rot, side) in test_cases {
            let pad = PadDef {
                name: "1".into(),
                kind: PadKind::Smd,
                shape: PadShape::RoundRect {
                    width: w,
                    height: h,
                    corner_radius: cr,
                },
                pos: Pt::new(150_000, -100_000),
                rot_deg: 15.0,
                drill_nm: None,
                layers: vec!["F.Cu".into()],
                mask_margin_nm: None,
                paste_margin_nm: None,
            };
            let pl = Placement {
                pos: Pt::new(2_000_000, 3_000_000),
                rot_deg: rot,
                side,
                ..Default::default()
            };
            let analytic = pad_to_world_shape(&pad, &pl, 16);
            let fine_verts: Vec<Pt> = roundrect_poly(w, h, cr, 64)
                .into_iter()
                .map(|p| transform_pad_local_pt(p, &pad, &pl))
                .collect();
            let fine_poly = ResolvedShape::polygon(fine_verts);
            assert!(analytic.bbox().min_x <= fine_poly.bbox().min_x + 2);
            assert!(analytic.bbox().max_x >= fine_poly.bbox().max_x - 2);

            let c = analytic.center();
            assert!(analytic.contains_pt(c));
            assert_eq!(analytic.dist_to_point(c), 0.0);

            for off in [
                Pt::new(0, 0),
                Pt::new(700_000, 0),
                Pt::new(0, 500_000),
                Pt::new(800_000, 600_000),
                Pt::new(-900_000, -700_000),
            ] {
                let pt = Pt::new(c.x + off.x, c.y + off.y);
                assert!(
                    (analytic.dist_to_point(pt) - fine_poly.dist_to_point(pt)).abs() < 400.0,
                    "dist_to_point mismatch for ({w},{h},{cr},{rot})"
                );
                let dc_a = analytic.dist_to_circle(pt, 150_000);
                let dc_f = fine_poly.dist_to_circle(pt, 150_000);
                if analytic.contains_pt(pt) {
                    assert!(dc_a <= -150_000.0 && dc_f <= -150_000.0);
                } else {
                    assert!((dc_a - dc_f).abs() < 400.0);
                }
                let dk_a =
                    analytic.dist_to_capsule(pt, Pt::new(pt.x + 200_000, pt.y - 150_000), 100_000);
                let dk_f =
                    fine_poly.dist_to_capsule(pt, Pt::new(pt.x + 200_000, pt.y - 150_000), 100_000);
                if dk_f > -100_000.0 {
                    assert!((dk_a - dk_f).abs() < 400.0);
                } else {
                    assert!(dk_a <= -100_000.0);
                }
            }

            let other = ResolvedShape::polygon(vec![
                Pt::new(c.x + 750_000, c.y - 200_000),
                Pt::new(c.x + 1_150_000, c.y - 200_000),
                Pt::new(c.x + 1_150_000, c.y + 200_000),
                Pt::new(c.x + 750_000, c.y + 200_000),
            ]);
            assert!(
                (analytic.dist_to_shape(&other) - fine_poly.dist_to_shape(&other)).abs() < 400.0
            );
            assert!(
                (other.dist_to_shape(&analytic) - other.dist_to_shape(&fine_poly)).abs() < 400.0
            );
            let poly_out = analytic.to_polygon(16);
            assert!(poly_out.len() >= 4);
            let poly_out_shape = ResolvedShape::polygon(poly_out);
            assert!(
                (analytic.dist_to_shape(&other) - poly_out_shape.dist_to_shape(&other)).abs()
                    < 1_000.0
            );
        }
    }
}
