use clipper2_rust::{FillRule, Paths64, difference_64};

use crate::fab_gerber::pad_world_shape_with_margin;
use crate::geom::{
    EPS_NM, SpatialGrid, ensure_ccw, local_to_board, pad_world_layers, pts_to_path64,
    remap_layer_for_side,
};
use crate::ir::{BBoxNm, BoardIr, Graphic, Nm, Placement, PourPoly, Pt, Side};
use crate::shapes::{ResolvedShape, circle_poly};
use crate::zone_scene::{paths_to_pour_polys, pour_poly_area, shape_inflated_poly};

const MIN_CLIPPED_SEG_NM: f64 = 50_000.0;
const MIN_FILLED_AREA_NM2: f64 = 50_000.0 * 50_000.0;

fn map_graphic_pt(p: Pt, pl: Option<&Placement>) -> Pt {
    match pl {
        Some(pl) => local_to_board(p, pl.pos.x, pl.pos.y, pl.rot_deg, pl.side),
        None => p,
    }
}

fn push_graphic_mask_shapes(g: &Graphic, pl: Option<&Placement>, out: &mut Vec<ResolvedShape>) {
    let mp = |p: Pt| map_graphic_pt(p, pl);
    match g {
        Graphic::Line { a, b, width, .. } => {
            out.push(ResolvedShape::Capsule {
                a: mp(*a),
                b: mp(*b),
                radius: (*width).max(10_000) / 2,
            });
        }
        Graphic::Rect {
            min,
            max,
            width,
            fill,
            ..
        } => {
            let c = [
                mp(*min),
                mp(Pt::new(max.x, min.y)),
                mp(*max),
                mp(Pt::new(min.x, max.y)),
            ];
            if *fill {
                out.push(ResolvedShape::polygon(c.to_vec()));
            } else {
                let r = (*width).max(10_000) / 2;
                for i in 0..4 {
                    out.push(ResolvedShape::Capsule {
                        a: c[i],
                        b: c[(i + 1) % 4],
                        radius: r,
                    });
                }
            }
        }
        Graphic::Circle {
            center,
            radius,
            width,
            fill,
            ..
        } if *radius > 0 => {
            let c = mp(*center);
            if *fill {
                out.push(ResolvedShape::Circle {
                    center: c,
                    radius: *radius,
                });
            } else {
                let r = (*width).max(10_000) / 2;
                let ring = circle_poly(c, *radius, 32);
                for i in 0..ring.len() {
                    out.push(ResolvedShape::Capsule {
                        a: ring[i],
                        b: ring[(i + 1) % ring.len()],
                        radius: r,
                    });
                }
            }
        }
        Graphic::Poly {
            verts, width, fill, ..
        } => {
            let pts: Vec<Pt> = verts.iter().map(|&v| mp(v)).collect();
            if *fill {
                if pts.len() >= 3 {
                    out.push(ResolvedShape::polygon(pts));
                }
            } else if pts.len() >= 2 {
                let r = (*width).max(10_000) / 2;
                for i in 0..pts.len() {
                    out.push(ResolvedShape::Capsule {
                        a: pts[i],
                        b: pts[(i + 1) % pts.len()],
                        radius: r,
                    });
                }
            }
        }
        _ => {}
    }
}

pub(crate) fn collect_mask_openings(
    board: &BoardIr,
    side: Side,
    cu_layers: &[String],
) -> Vec<ResolvedShape> {
    let mask_name = match side {
        Side::Top => "F.Mask",
        Side::Bottom => "B.Mask",
    };
    let cu_name = side.cu_layer();
    let mut openings = Vec::new();
    for pl in &board.placements {
        let Some(fp) = board.footprints.get(&pl.footprint) else {
            continue;
        };
        for pad in &fp.pads {
            if !pad_world_layers(pad, pl.side, cu_layers)
                .iter()
                .any(|l| l == mask_name)
            {
                continue;
            }
            let margin = pad.mask_margin_nm.unwrap_or(board.rules.mask_expansion_nm);
            if let Some(shape) = pad_world_shape_with_margin(pad, pl, margin) {
                openings.push(shape);
            }
        }
        for g in &fp.graphics {
            if remap_layer_for_side(g.layer(), pl.side) == mask_name {
                push_graphic_mask_shapes(g, Some(pl), &mut openings);
            }
        }
    }
    for g in &board.graphics {
        if g.layer() == mask_name {
            push_graphic_mask_shapes(g, None, &mut openings);
        }
    }
    for via in &board.vias {
        if !via.tented && via.touches_layer(cu_name, &board.layers) {
            let r = via.diameter / 2 + board.rules.mask_expansion_nm;
            if r > 0 {
                openings.push(ResolvedShape::Circle {
                    center: via.pos,
                    radius: r,
                });
            }
        }
    }
    openings
}

struct SegGeom {
    a: Pt,
    b: Pt,
    dx: f64,
    dy: f64,
    len_sq: f64,
}

impl SegGeom {
    fn new(a: Pt, b: Pt) -> Self {
        let dx = (b.x - a.x) as f64;
        let dy = (b.y - a.y) as f64;
        Self {
            a,
            b,
            dx,
            dy,
            len_sq: dx * dx + dy * dy,
        }
    }

    fn eval(&self, t: f64) -> Pt {
        if t <= 0.0 {
            self.a
        } else if t >= 1.0 {
            self.b
        } else {
            Pt::new(
                (self.a.x as f64 + t * self.dx).round() as Nm,
                (self.a.y as f64 + t * self.dy).round() as Nm,
            )
        }
    }
}

fn seg_disk_interval(seg: &SegGeom, c: Pt, rad: f64) -> Option<(f64, f64)> {
    if rad <= 0.0 {
        return None;
    }
    let fx = seg.a.x as f64 - c.x as f64;
    let fy = seg.a.y as f64 - c.y as f64;
    let b_half = fx * seg.dx + fy * seg.dy;
    let c_term = fx * fx + fy * fy - rad * rad;
    let disc = b_half * b_half - seg.len_sq * c_term;
    if disc <= 0.0 {
        return None;
    }
    let s = disc.sqrt();
    let lo = (-b_half - s) / seg.len_sq;
    let hi = (-b_half + s) / seg.len_sq;
    if hi <= 0.0 || lo >= 1.0 {
        return None;
    }
    Some((lo.max(0.0), hi.min(1.0)))
}

fn clip_slab(lo: &mut f64, hi: &mut f64, v0: f64, dv: f64, min_v: f64, max_v: f64) -> bool {
    if dv.abs() < 1e-12 {
        return v0 >= min_v && v0 <= max_v;
    }
    let ta = (min_v - v0) / dv;
    let tb = (max_v - v0) / dv;
    *lo = lo.max(ta.min(tb));
    *hi = hi.min(ta.max(tb));
    *lo < *hi
}

fn seg_edge_rect_interval(seg: &SegGeom, u: Pt, v: Pt, rad: f64) -> Option<(f64, f64)> {
    let ux = (v.x - u.x) as f64;
    let uy = (v.y - u.y) as f64;
    let ulen_sq = ux * ux + uy * uy;
    if ulen_sq == 0.0 || rad <= 0.0 {
        return None;
    }
    let ulen = ulen_sq.sqrt();
    let tx = ux / ulen;
    let ty = uy / ulen;
    let nx = -ty;
    let ny = tx;
    let rx = seg.a.x as f64 - u.x as f64;
    let ry = seg.a.y as f64 - u.y as f64;
    let mut lo = 0.0_f64;
    let mut hi = 1.0_f64;
    if clip_slab(
        &mut lo,
        &mut hi,
        rx * tx + ry * ty,
        seg.dx * tx + seg.dy * ty,
        0.0,
        ulen,
    ) && clip_slab(
        &mut lo,
        &mut hi,
        rx * nx + ry * ny,
        seg.dx * nx + seg.dy * ny,
        -rad,
        rad,
    ) {
        Some((lo, hi))
    } else {
        None
    }
}

fn push_seg_capsule_intervals(seg: &SegGeom, u: Pt, v: Pt, rad: f64, out: &mut Vec<(f64, f64)>) {
    if let Some(iv) = seg_disk_interval(seg, u, rad) {
        out.push(iv);
    }
    if u != v {
        if let Some(iv) = seg_disk_interval(seg, v, rad) {
            out.push(iv);
        }
        if let Some(iv) = seg_edge_rect_interval(seg, u, v, rad) {
            out.push(iv);
        }
    }
}

fn point_in_polygon_f64(px: f64, py: f64, verts: &[Pt]) -> bool {
    let n = verts.len();
    if n < 3 {
        return false;
    }
    let mut inside = false;
    let mut j = n - 1;
    for i in 0..n {
        let xi = verts[i].x as f64;
        let yi = verts[i].y as f64;
        let xj = verts[j].x as f64;
        let yj = verts[j].y as f64;
        if ((yi > py) != (yj > py)) && (px < (xj - xi) * (py - yi) / (yj - yi) + xi) {
            inside = !inside;
        }
        j = i;
    }
    inside
}

fn push_seg_poly_interior_intervals(seg: &SegGeom, verts: &[Pt], out: &mut Vec<(f64, f64)>) {
    let n = verts.len();
    if n < 3 {
        return;
    }
    let mut ts = Vec::with_capacity(n + 2);
    ts.push(0.0);
    ts.push(1.0);
    for i in 0..n {
        let u = verts[i];
        let v = verts[(i + 1) % n];
        let ex = (v.x - u.x) as f64;
        let ey = (v.y - u.y) as f64;
        let denom = seg.dx * ey - seg.dy * ex;
        if denom.abs() > 1e-12 {
            let rx = u.x as f64 - seg.a.x as f64;
            let ry = u.y as f64 - seg.a.y as f64;
            let t = (rx * ey - ry * ex) / denom;
            let s = (rx * seg.dy - ry * seg.dx) / denom;
            if t > 0.0 && t < 1.0 && (-1e-9..=1.0 + 1e-9).contains(&s) {
                ts.push(t);
            }
        } else {
            for p in [u, v] {
                let t = ((p.x - seg.a.x) as f64 * seg.dx + (p.y - seg.a.y) as f64 * seg.dy)
                    / seg.len_sq;
                if t > 0.0 && t < 1.0 {
                    ts.push(t);
                }
            }
        }
    }
    ts.sort_by(f64::total_cmp);
    for w in ts.windows(2) {
        if w[1] - w[0] <= 1e-12 {
            continue;
        }
        let tm = 0.5 * (w[0] + w[1]);
        let mx = seg.a.x as f64 + tm * seg.dx;
        let my = seg.a.y as f64 + tm * seg.dy;
        if point_in_polygon_f64(mx, my, verts) {
            out.push((w[0], w[1]));
        }
    }
}

pub(crate) struct MaskClipper {
    openings: Vec<ResolvedShape>,
    grid: SpatialGrid,
    candidates: Vec<usize>,
    clr: Nm,
}

impl MaskClipper {
    pub(crate) fn new(openings: Vec<ResolvedShape>, clr: Nm) -> Self {
        let bboxes: Vec<BBoxNm> = openings.iter().map(ResolvedShape::bbox).collect();
        let grid = SpatialGrid::new(&bboxes, 2_000_000);
        Self {
            openings,
            grid,
            candidates: Vec::new(),
            clr: clr.max(0),
        }
    }

    pub(crate) fn point_hits_mask(&mut self, p: Pt, radius: Nm) -> bool {
        if self.openings.is_empty() {
            return false;
        }
        let pad = radius + self.clr;
        let sb = BBoxNm::from_pt(p, pad);
        self.grid.query(sb, &mut self.candidates);
        self.candidates.iter().any(|&idx| {
            let op = &self.openings[idx];
            op.bbox().intersects(sb) && op.dist_to_circle(p, pad) < 0.0
        })
    }

    /// Clips stroke `(a, b)` of width `width` against all mask openings + silk clearance.
    pub(crate) fn clip_segment(&mut self, a: Pt, b: Pt, width: Nm, out: &mut Vec<(Pt, Pt)>) {
        out.clear();
        let r = width.max(10_000) / 2;
        if self.openings.is_empty() {
            out.push((a, b));
            return;
        }
        if a == b {
            if !self.point_hits_mask(a, r) {
                out.push((a, a));
            }
            return;
        }
        let pad = r + self.clr;
        // 2 nm safety pad keeps rounded integer endpoints outside the strict mask clearance boundary.
        let pad_f = pad as f64 + 2.0;
        let sb = BBoxNm::from_pts(&[a, b]).expand(pad + 2);
        self.grid.query(sb, &mut self.candidates);

        let seg = SegGeom::new(a, b);
        let mut forbidden: Vec<(f64, f64)> = Vec::new();
        for &idx in &self.candidates {
            let op = &self.openings[idx];
            if !op.bbox().intersects(sb) {
                continue;
            }
            match op {
                ResolvedShape::Circle { center, radius } => {
                    if let Some(iv) = seg_disk_interval(&seg, *center, *radius as f64 + pad_f) {
                        forbidden.push(iv);
                    }
                }
                ResolvedShape::Capsule {
                    a: ca,
                    b: cb,
                    radius,
                } => {
                    push_seg_capsule_intervals(
                        &seg,
                        *ca,
                        *cb,
                        *radius as f64 + pad_f,
                        &mut forbidden,
                    );
                }
                ResolvedShape::Poly { verts, .. } if verts.len() >= 3 => {
                    push_seg_poly_interior_intervals(&seg, verts, &mut forbidden);
                    for &v in verts {
                        if let Some(iv) = seg_disk_interval(&seg, v, pad_f) {
                            forbidden.push(iv);
                        }
                    }
                    for i in 0..verts.len() {
                        if let Some(iv) = seg_edge_rect_interval(
                            &seg,
                            verts[i],
                            verts[(i + 1) % verts.len()],
                            pad_f,
                        ) {
                            forbidden.push(iv);
                        }
                    }
                }
                _ => {}
            }
        }
        if forbidden.is_empty() {
            out.push((a, b));
            return;
        }
        forbidden.sort_by(|x, y| x.0.total_cmp(&y.0));
        let len = seg.len_sq.sqrt();
        let mut cur = 0.0_f64;
        for (lo, hi) in forbidden {
            if lo > cur && (lo - cur) * len >= MIN_CLIPPED_SEG_NM {
                out.push((seg.eval(cur), seg.eval(lo)));
            }
            cur = cur.max(hi);
        }
        if cur < 1.0 && (1.0 - cur) * len >= MIN_CLIPPED_SEG_NM {
            out.push((seg.eval(cur), b));
        }
    }

    /// Returns `None` if `shape` clears all mask openings, or `Some(polys)` with openings subtracted.
    pub(crate) fn clip_filled_shape(&mut self, shape: &ResolvedShape) -> Option<Vec<PourPoly>> {
        if self.openings.is_empty() {
            return None;
        }
        let sb = shape.bbox().expand(self.clr + 2);
        self.grid.query(sb, &mut self.candidates);
        let hitting: Vec<usize> = self
            .candidates
            .iter()
            .copied()
            .filter(|&idx| {
                let op = &self.openings[idx];
                op.bbox().intersects(sb) && op.dist_to_shape(shape) < self.clr as f64 - EPS_NM
            })
            .collect();
        if hitting.is_empty() {
            return None;
        }
        let mut subj_pts = shape.to_polygon(16);
        if subj_pts.len() < 3 {
            return Some(Vec::new());
        }
        ensure_ccw(&mut subj_pts);
        let subject = vec![pts_to_path64(&subj_pts)];
        let mut clip = Paths64::new();
        for idx in hitting {
            let mut poly = shape_inflated_poly(&self.openings[idx], self.clr);
            if poly.len() >= 3 {
                ensure_ccw(&mut poly);
                clip.push(pts_to_path64(&poly));
            }
        }
        let diff = difference_64(&subject, &clip, FillRule::NonZero);
        let mut polys = paths_to_pour_polys(&diff);
        polys.retain(|p| pour_poly_area(p) >= MIN_FILLED_AREA_NM2);
        Some(polys)
    }
}
