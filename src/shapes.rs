use std::f64::consts::{FRAC_PI_2, PI, TAU};

use clipper2_rust::{Path64, Paths64, Point64};

use crate::geom::{
    ensure_ccw, local_to_board, point_to_polygon_dist, point_to_polygon_edge_dist,
    point_to_segment_dist, polygon_to_polygon_dist, rotate_pt, segment_to_polygon_dist,
    segment_to_polygon_edge_dist, segment_to_segment_dist,
};
use crate::ir::{BBoxNm, FootprintDef, Nm, PadDef, PadShape, Placement, Pt};

const DEFAULT_ARC_SEGS: usize = 16;

/// Exact world-space 2D shape in nanometers.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ResolvedShape {
    Circle { center: Pt, radius: Nm },
    Capsule { a: Pt, b: Pt, radius: Nm },
    Poly { verts: Vec<Pt>, bbox: BBoxNm },
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
            Self::Poly { bbox, .. } => *bbox,
        }
    }

    pub fn ref_pt(&self) -> Pt {
        match self {
            Self::Circle { center, .. } => *center,
            Self::Capsule { a, .. } => *a,
            Self::Poly { verts, bbox } => verts.first().copied().unwrap_or_else(|| bbox.center()),
        }
    }

    pub fn center(&self) -> Pt {
        match self {
            Self::Circle { center, .. } => *center,
            Self::Capsule { a, b, .. } => a.midpoint(*b),
            Self::Poly { bbox, .. } => bbox.center(),
        }
    }

    pub fn to_polygon(&self, arc_segs: usize) -> Vec<Pt> {
        let segs = arc_segs.max(4);
        match self {
            Self::Circle { center, radius } => circle_poly(*center, *radius, segs * 4),
            Self::Capsule { a, b, radius } => capsule_poly(*a, *b, *radius, segs),
            Self::Poly { verts, .. } => verts.clone(),
        }
    }

    pub fn dist_to_point(&self, p: Pt) -> f64 {
        match self {
            Self::Circle { center, radius } => (center.dist(p) - *radius as f64).max(0.0),
            Self::Capsule { a, b, radius } => {
                (point_to_segment_dist(p, *a, *b) - *radius as f64).max(0.0)
            }
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
            Self::Poly { verts, .. } => segment_to_polygon_dist(a, b, verts) - r,
        }
    }

    pub fn dist_to_shape(&self, other: &Self) -> f64 {
        match other {
            Self::Circle { center, radius } => self.dist_to_circle(*center, *radius),
            Self::Capsule { a, b, radius } => self.dist_to_capsule(*a, *b, *radius),
            Self::Poly { verts, .. } => match self {
                Self::Circle { center, radius } => {
                    point_to_polygon_dist(*center, verts) - *radius as f64
                }
                Self::Capsule { a, b, radius } => {
                    segment_to_polygon_dist(*a, *b, verts) - *radius as f64
                }
                Self::Poly { verts: av, .. } => polygon_to_polygon_dist(av, verts),
            },
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

pub(crate) fn transform_pad_local_pt(pt: Pt, pad: &PadDef, pl: &Placement) -> Pt {
    let rotated = if pad.rot_deg.abs() > 1e-9 {
        rotate_pt(pt, pad.rot_deg)
    } else {
        pt
    };
    let in_fp = Pt::new(pad.pos.x + rotated.x, pad.pos.y + rotated.y);
    local_to_board(in_fp, pl.pos.x, pl.pos.y, pl.rot_deg, pl.side)
}

/// Resolves a footprint pad into its exact world-space `ResolvedShape`.
pub fn pad_to_world_shape(pad: &PadDef, pl: &Placement, arc_segs: usize) -> ResolvedShape {
    let segs = if arc_segs == 0 {
        DEFAULT_ARC_SEGS
    } else {
        arc_segs
    };
    match &pad.shape {
        PadShape::Circle { diameter } => ResolvedShape::Circle {
            center: local_to_board(pad.pos, pl.pos.x, pl.pos.y, pl.rot_deg, pl.side),
            radius: diameter / 2,
        },
        PadShape::Oval { width, height } if width == height => ResolvedShape::Circle {
            center: local_to_board(pad.pos, pl.pos.x, pl.pos.y, pl.rot_deg, pl.side),
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
                a: transform_pad_local_pt(a_loc, pad, pl),
                b: transform_pad_local_pt(b_loc, pad, pl),
                radius: r,
            }
        }
        PadShape::Rect { width, height } => {
            let verts = rect_poly(*width, *height)
                .into_iter()
                .map(|p| transform_pad_local_pt(p, pad, pl))
                .collect();
            ResolvedShape::polygon(verts)
        }
        PadShape::RoundRect {
            width,
            height,
            corner_radius,
        } => {
            let verts = roundrect_poly(*width, *height, *corner_radius, segs)
                .into_iter()
                .map(|p| transform_pad_local_pt(p, pad, pl))
                .collect();
            ResolvedShape::polygon(verts)
        }
        PadShape::Poly { verts } => {
            let world = verts
                .iter()
                .map(|&p| transform_pad_local_pt(p, pad, pl))
                .collect();
            ResolvedShape::polygon(world)
        }
    }
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
