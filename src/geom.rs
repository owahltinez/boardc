use std::collections::HashMap;

use clipper2_rust::{FillRule, Paths64, intersect_64};

pub use crate::shapes::{
    ResolvedShape, capsule_poly, circle_poly, courtyard_world_polygon, oval_poly,
    pad_to_world_shape, pad_world_polygon, path64_to_pts, paths64_to_polys, polys_to_paths64,
    pts_to_path64, rect_poly, roundrect_poly, shape_to_path64,
};

use crate::ir::{BBoxNm, Nm, PadDef, PourPoly, Pt, Side};

pub(crate) const EPS_NM: f64 = 0.5;

pub(crate) fn sin_cos_deg(rot_deg: f64) -> (f64, f64) {
    let rem = rot_deg.rem_euclid(360.0);
    let q = (rem / 90.0).round();
    if (rem - q * 90.0).abs() < 1e-9 {
        return match (q as i64).rem_euclid(4) {
            0 => (0.0, 1.0),
            1 => (1.0, 0.0),
            2 => (0.0, -1.0),
            _ => (-1.0, 0.0),
        };
    }
    rot_deg.to_radians().sin_cos()
}

/// Rotates `pt` CCW around the origin by `rot_deg`.
pub fn rotate_pt(pt: Pt, rot_deg: f64) -> Pt {
    let (s, c) = sin_cos_deg(rot_deg);
    let x = (pt.x as f64 * c - pt.y as f64 * s).round() as Nm;
    let y = (pt.x as f64 * s + pt.y as f64 * c).round() as Nm;
    Pt::new(x, y)
}

/// Maps footprint-local coordinates (viewed from Top, Y-up) to canonical board coordinates.
pub fn local_to_board(pt: Pt, cx: Nm, cy: Nm, rot_deg: f64, side: Side) -> Pt {
    let (s, c) = sin_cos_deg(rot_deg);
    let lx = pt.x as f64;
    let ly = match side {
        Side::Top => pt.y as f64,
        Side::Bottom => -(pt.y as f64),
    };
    let x = cx + (lx * c - ly * s).round() as Nm;
    let y = cy + (lx * s + ly * c).round() as Nm;
    Pt::new(x, y)
}

/// Maps canonical board coordinates back into footprint-local Y-up coordinates (inverse of `local_to_board`).
pub fn board_to_local(pt: Pt, cx: Nm, cy: Nm, rot_deg: f64, side: Side) -> Pt {
    let (s, c) = sin_cos_deg(rot_deg);
    let dx = (pt.x - cx) as f64;
    let dy = (pt.y - cy) as f64;
    let lx = (dx * c + dy * s).round() as Nm;
    let ly_rot = (-dx * s + dy * c).round() as Nm;
    let ly = match side {
        Side::Top => ly_rot,
        Side::Bottom => -ly_rot,
    };
    Pt::new(lx, ly)
}

/// Remaps Top <-> Bottom layer names for `Side::Bottom` placements.
pub fn remap_layer_for_side(layer: &str, side: Side) -> String {
    if side == Side::Top {
        return layer.to_string();
    }
    if let Some(rest) = layer.strip_prefix("F.") {
        format!("B.{rest}")
    } else if let Some(rest) = layer.strip_prefix("B.") {
        format!("F.{rest}")
    } else {
        layer.to_string()
    }
}

/// Expands pad layer specs (`*.Cu`, `*.Mask`, `*.Paste`, `F.*`, `B.*`) into concrete board layer names.
pub fn pad_world_layers(pad: &PadDef, side: Side, copper_layers: &[String]) -> Vec<String> {
    let mut out = Vec::new();
    let mut push_unique = |name: String| {
        if !out.contains(&name) {
            out.push(name);
        }
    };
    for raw in &pad.layers {
        match raw.as_str() {
            "*.Cu" => {
                for cl in copper_layers {
                    push_unique(cl.clone());
                }
            }
            "*.Mask" => {
                push_unique("F.Mask".into());
                push_unique("B.Mask".into());
            }
            "*.Paste" => {
                push_unique("F.Paste".into());
                push_unique("B.Paste".into());
            }
            _ => push_unique(remap_layer_for_side(raw, side)),
        }
    }
    out
}

/// Expands pad copper layer specs (`*.Cu`, `F.Cu`, etc.) filtered strictly to `copper_layers`.
pub fn pad_copper_layers(pad: &PadDef, side: Side, copper_layers: &[String]) -> Vec<String> {
    let mut out = Vec::new();
    for raw in &pad.layers {
        if raw == "*.Cu" {
            for cl in copper_layers {
                if !out.contains(cl) {
                    out.push(cl.clone());
                }
            }
        } else {
            let mapped = remap_layer_for_side(raw, side);
            if copper_layers.contains(&mapped) && !out.contains(&mapped) {
                out.push(mapped);
            }
        }
    }
    out
}

pub fn point_to_segment_dist_sq(p: Pt, a: Pt, b: Pt) -> f64 {
    let abx = (b.x - a.x) as f64;
    let aby = (b.y - a.y) as f64;
    let len_sq = abx * abx + aby * aby;
    if len_sq == 0.0 {
        return p.dist_sq(a);
    }
    let t = ((((p.x - a.x) as f64) * abx + ((p.y - a.y) as f64) * aby) / len_sq).clamp(0.0, 1.0);
    let px = a.x as f64 + t * abx;
    let py = a.y as f64 + t * aby;
    let dx = p.x as f64 - px;
    let dy = p.y as f64 - py;
    dx * dx + dy * dy
}

pub fn point_to_segment_dist(p: Pt, a: Pt, b: Pt) -> f64 {
    point_to_segment_dist_sq(p, a, b).sqrt()
}

pub(crate) fn orient_i128(p: Pt, q: Pt, r: Pt) -> i128 {
    (q.x - p.x) as i128 * (r.y - p.y) as i128 - (q.y - p.y) as i128 * (r.x - p.x) as i128
}

pub(crate) fn on_segment_bbox(p: Pt, q: Pt, r: Pt) -> bool {
    q.x <= p.x.max(r.x) && q.x >= p.x.min(r.x) && q.y <= p.y.max(r.y) && q.y >= p.y.min(r.y)
}

pub fn segments_intersect(a1: Pt, a2: Pt, b1: Pt, b2: Pt) -> bool {
    let o1 = orient_i128(a1, a2, b1);
    let o2 = orient_i128(a1, a2, b2);
    let o3 = orient_i128(b1, b2, a1);
    let o4 = orient_i128(b1, b2, a2);
    if (o1 > 0) != (o2 > 0) && o1 != 0 && o2 != 0 && (o3 > 0) != (o4 > 0) && o3 != 0 && o4 != 0 {
        return true;
    }
    (o1 == 0 && on_segment_bbox(a1, b1, a2))
        || (o2 == 0 && on_segment_bbox(a1, b2, a2))
        || (o3 == 0 && on_segment_bbox(b1, a1, b2))
        || (o4 == 0 && on_segment_bbox(b1, a2, b2))
}

pub fn segment_to_segment_dist(a1: Pt, a2: Pt, b1: Pt, b2: Pt) -> f64 {
    if segments_intersect(a1, a2, b1, b2) {
        return 0.0;
    }
    let d1 = point_to_segment_dist_sq(a1, b1, b2);
    let d2 = point_to_segment_dist_sq(a2, b1, b2);
    let d3 = point_to_segment_dist_sq(b1, a1, a2);
    let d4 = point_to_segment_dist_sq(b2, a1, a2);
    d1.min(d2).min(d3).min(d4).sqrt()
}

/// Twice the signed shoelace area (positive = CCW, negative = CW).
pub fn polygon_signed_area_2(verts: &[Pt]) -> i128 {
    let n = verts.len();
    if n < 3 {
        return 0;
    }
    let mut sum: i128 = 0;
    for i in 0..n {
        let a = verts[i];
        let b = verts[(i + 1) % n];
        sum += (a.x as i128) * (b.y as i128) - (b.x as i128) * (a.y as i128);
    }
    sum
}

pub fn polygon_area(verts: &[Pt]) -> f64 {
    polygon_signed_area_2(verts).abs() as f64 * 0.5
}

pub fn ensure_ccw(verts: &mut [Pt]) {
    if polygon_signed_area_2(verts) < 0 {
        verts.reverse();
    }
}

pub fn point_in_polygon(p: Pt, verts: &[Pt]) -> bool {
    let n = verts.len();
    if n < 3 {
        return false;
    }
    let mut inside = false;
    let mut j = n - 1;
    for i in 0..n {
        let vi = verts[i];
        let vj = verts[j];
        if orient_i128(vj, vi, p) == 0 && on_segment_bbox(vj, p, vi) {
            return true;
        }
        let intersects = ((vi.y > p.y) != (vj.y > p.y))
            && ((p.x as f64)
                < ((vj.x - vi.x) as f64) * ((p.y - vi.y) as f64) / ((vj.y - vi.y) as f64)
                    + (vi.x as f64));
        if intersects {
            inside = !inside;
        }
        j = i;
    }
    inside
}

pub fn point_to_polygon_edge_dist(p: Pt, verts: &[Pt]) -> f64 {
    let n = verts.len();
    if n < 2 {
        return if n == 1 {
            p.dist(verts[0])
        } else {
            f64::INFINITY
        };
    }
    let mut min_sq = f64::INFINITY;
    for i in 0..n {
        min_sq = min_sq.min(point_to_segment_dist_sq(p, verts[i], verts[(i + 1) % n]));
    }
    min_sq.sqrt()
}

pub fn point_to_polygon_dist(p: Pt, verts: &[Pt]) -> f64 {
    if verts.is_empty() {
        return f64::INFINITY;
    }
    if point_in_polygon(p, verts) {
        return 0.0;
    }
    point_to_polygon_edge_dist(p, verts)
}

pub fn segment_to_polygon_edge_dist(a: Pt, b: Pt, verts: &[Pt]) -> f64 {
    let n = verts.len();
    if n < 2 {
        return if n == 1 {
            point_to_segment_dist(verts[0], a, b)
        } else {
            f64::INFINITY
        };
    }
    let mut min_d = f64::INFINITY;
    for i in 0..n {
        let d = segment_to_segment_dist(a, b, verts[i], verts[(i + 1) % n]);
        if d == 0.0 {
            return 0.0;
        }
        min_d = min_d.min(d);
    }
    min_d
}

pub fn segment_to_polygon_dist(a: Pt, b: Pt, verts: &[Pt]) -> f64 {
    match verts.len() {
        0 => f64::INFINITY,
        1 => point_to_segment_dist(verts[0], a, b),
        _ if point_in_polygon(a, verts) || point_in_polygon(b, verts) => 0.0,
        _ => segment_to_polygon_edge_dist(a, b, verts),
    }
}

pub fn polygon_to_polygon_dist(a: &[Pt], b: &[Pt]) -> f64 {
    if a.is_empty() || b.is_empty() {
        return f64::INFINITY;
    }
    if a.iter().any(|&p| point_in_polygon(p, b)) || b.iter().any(|&p| point_in_polygon(p, a)) {
        return 0.0;
    }
    let mut min_d = f64::INFINITY;
    for i in 0..a.len() {
        let d = segment_to_polygon_edge_dist(a[i], a[(i + 1) % a.len()], b);
        if d == 0.0 {
            return 0.0;
        }
        min_d = min_d.min(d);
    }
    min_d
}

pub(crate) struct SpatialGrid {
    cell_nm: Nm,
    cells: HashMap<(i64, i64), Vec<usize>>,
    globals: Vec<usize>,
    seen: Vec<u32>,
    stamp: u32,
}

impl SpatialGrid {
    pub(crate) fn from_bboxes(bboxes: impl IntoIterator<Item = BBoxNm>, cell_nm: Nm) -> Self {
        let mut grid = Self {
            cell_nm: cell_nm.max(250_000),
            cells: HashMap::new(),
            globals: Vec::new(),
            seen: Vec::new(),
            stamp: 0,
        };
        for b in bboxes {
            grid.push(b);
        }
        grid
    }

    pub(crate) fn new(bboxes: &[BBoxNm], cell_nm: Nm) -> Self {
        Self::from_bboxes(bboxes.iter().copied(), cell_nm)
    }

    pub(crate) fn push(&mut self, b: BBoxNm) {
        let idx = self.seen.len();
        self.seen.push(0);
        let x0 = b.min_x.div_euclid(self.cell_nm);
        let x1 = b.max_x.div_euclid(self.cell_nm);
        let y0 = b.min_y.div_euclid(self.cell_nm);
        let y1 = b.max_y.div_euclid(self.cell_nm);
        let dx = x1.saturating_sub(x0).saturating_add(1);
        let dy = y1.saturating_sub(y0).saturating_add(1);
        if dx <= 0 || dy <= 0 || dx.saturating_mul(dy) > 256 {
            self.globals.push(idx);
        } else {
            for gx in x0..=x1 {
                for gy in y0..=y1 {
                    self.cells.entry((gx, gy)).or_default().push(idx);
                }
            }
        }
    }

    pub(crate) fn query(&mut self, bbox: BBoxNm, out: &mut Vec<usize>) {
        out.clear();
        let x0 = bbox.min_x.div_euclid(self.cell_nm);
        let x1 = bbox.max_x.div_euclid(self.cell_nm);
        let y0 = bbox.min_y.div_euclid(self.cell_nm);
        let y1 = bbox.max_y.div_euclid(self.cell_nm);
        let dx = x1.saturating_sub(x0).saturating_add(1);
        let dy = y1.saturating_sub(y0).saturating_add(1);
        if dx <= 0 || dy <= 0 || dx.saturating_mul(dy) > 256 {
            out.extend(0..self.seen.len());
            return;
        }
        self.stamp = self.stamp.wrapping_add(1);
        if self.stamp == 0 {
            self.seen.fill(0);
            self.stamp = 1;
        }
        let s = self.stamp;
        for &idx in &self.globals {
            self.seen[idx] = s;
            out.push(idx);
        }
        for gx in x0..=x1 {
            for gy in y0..=y1 {
                if let Some(list) = self.cells.get(&(gx, gy)) {
                    for &idx in list {
                        if self.seen[idx] != s {
                            self.seen[idx] = s;
                            out.push(idx);
                        }
                    }
                }
            }
        }
    }
}

pub(crate) struct UnionFind {
    parent: Vec<usize>,
    rank: Vec<u8>,
}

impl UnionFind {
    pub(crate) fn new(n: usize) -> Self {
        Self {
            parent: (0..n).collect(),
            rank: vec![0; n],
        }
    }

    pub(crate) fn find(&mut self, mut x: usize) -> usize {
        while self.parent[x] != x {
            self.parent[x] = self.parent[self.parent[x]];
            x = self.parent[x];
        }
        x
    }

    pub(crate) fn union(&mut self, a: usize, b: usize) {
        let mut ra = self.find(a);
        let mut rb = self.find(b);
        if ra == rb {
            return;
        }
        if self.rank[ra] < self.rank[rb] {
            std::mem::swap(&mut ra, &mut rb);
        }
        self.parent[rb] = ra;
        if self.rank[ra] == self.rank[rb] {
            self.rank[ra] += 1;
        }
    }
}

pub(crate) fn closest_pt_on_segment(p: Pt, a: Pt, b: Pt) -> Pt {
    let abx = (b.x - a.x) as f64;
    let aby = (b.y - a.y) as f64;
    let len_sq = abx * abx + aby * aby;
    if len_sq == 0.0 {
        return a;
    }
    let t = ((((p.x - a.x) as f64) * abx + ((p.y - a.y) as f64) * aby) / len_sq).clamp(0.0, 1.0);
    Pt::new(a.x + (t * abx).round() as Nm, a.y + (t * aby).round() as Nm)
}

pub(crate) fn segment_pair_midpoint(a1: Pt, a2: Pt, b1: Pt, b2: Pt) -> Pt {
    if segments_intersect(a1, a2, b1, b2) {
        let dx_a = (a2.x - a1.x) as f64;
        let dy_a = (a2.y - a1.y) as f64;
        let dx_b = (b2.x - b1.x) as f64;
        let dy_b = (b2.y - b1.y) as f64;
        let denom = dx_a * dy_b - dy_a * dx_b;
        if denom.abs() > 1e-6 {
            let t = (((b1.x - a1.x) as f64 * dy_b - (b1.y - a1.y) as f64 * dx_b) / denom)
                .clamp(0.0, 1.0);
            return Pt::new(
                a1.x + (t * dx_a).round() as Nm,
                a1.y + (t * dy_a).round() as Nm,
            );
        }
    }
    let candidates = [
        (a1, closest_pt_on_segment(a1, b1, b2)),
        (a2, closest_pt_on_segment(a2, b1, b2)),
        (b1, closest_pt_on_segment(b1, a1, a2)),
        (b2, closest_pt_on_segment(b2, a1, a2)),
    ];
    let (p, q) = candidates
        .into_iter()
        .min_by(|(p1, q1), (p2, q2)| p1.dist_sq(*q1).total_cmp(&p2.dist_sq(*q2)))
        .unwrap();
    p.midpoint(q)
}

pub(crate) fn polys_overlap_area(a: &[Pt], b: &[Pt]) -> f64 {
    if a.len() < 3 || b.len() < 3 {
        return 0.0;
    }
    let mut ca = a.to_vec();
    let mut cb = b.to_vec();
    ensure_ccw(&mut ca);
    ensure_ccw(&mut cb);
    let res = intersect_64(
        &vec![pts_to_path64(&ca)],
        &vec![pts_to_path64(&cb)],
        FillRule::NonZero,
    );
    res.iter().map(|p| polygon_area(&path64_to_pts(p))).sum()
}

pub(crate) fn polygon_violates_clearance(a: &[Pt], b: &[Pt], min_clr: Nm) -> bool {
    let dist = polygon_to_polygon_dist(a, b);
    if min_clr > 0 {
        dist < min_clr as f64 - EPS_NM
    } else {
        dist == 0.0 && polys_overlap_area(a, b) > 0.0
    }
}

pub(crate) fn pour_poly_overlaps_poly(pour: &PourPoly, poly: &[Pt]) -> bool {
    if pour.outer.len() < 3 || poly.len() < 3 {
        return false;
    }
    let mut subject = Paths64::new();
    let mut outer = pour.outer.clone();
    ensure_ccw(&mut outer);
    subject.push(pts_to_path64(&outer));
    for h in &pour.holes {
        let mut hr = h.clone();
        if polygon_signed_area_2(&hr) > 0 {
            hr.reverse();
        }
        subject.push(pts_to_path64(&hr));
    }
    let mut clip = poly.to_vec();
    ensure_ccw(&mut clip);
    let res = intersect_64(&subject, &vec![pts_to_path64(&clip)], FillRule::NonZero);
    res.iter()
        .map(|p| polygon_area(&path64_to_pts(p)))
        .sum::<f64>()
        > 0.0
}

pub(crate) fn poly_extends_outside_board(
    verts: &[Pt],
    outline: &[Pt],
    cutouts: &[Vec<Pt>],
) -> bool {
    let area = polygon_area(verts);
    if area <= 0.0 {
        return verts.iter().any(|&p| !point_in_polygon(p, outline));
    }
    if area - polys_overlap_area(verts, outline) > 1.0 {
        return true;
    }
    cutouts.iter().any(|c| polys_overlap_area(verts, c) > 0.0)
}

pub(crate) fn shape_edge_clearance(
    shape: &ResolvedShape,
    outline: &[Pt],
    cutouts: &[Vec<Pt>],
) -> (f64, bool) {
    let ref_pt = shape.ref_pt();
    let inside_outline = point_in_polygon(ref_pt, outline);
    let strictly_in_cutout = cutouts
        .iter()
        .any(|c| point_in_polygon(ref_pt, c) && point_to_polygon_edge_dist(ref_pt, c) > EPS_NM);
    if !inside_outline || strictly_in_cutout {
        return (0.0, true);
    }
    let mut min_d = shape.dist_to_polygon_edge(outline);
    for c in cutouts {
        min_d = min_d.min(shape.dist_to_polygon_edge(c));
    }
    if min_d > 0.0 {
        let encloses_cutout = cutouts
            .iter()
            .any(|c| c.first().is_some_and(|&p| shape.dist_to_point(p) == 0.0));
        return if encloses_cutout {
            (0.0, true)
        } else {
            (min_d, false)
        };
    }
    let off_board = match shape {
        ResolvedShape::Circle { .. } => min_d < -EPS_NM,
        ResolvedShape::Capsule { a, b, .. } => {
            min_d < -EPS_NM
                || !point_in_polygon(*a, outline)
                || !point_in_polygon(*b, outline)
                || cutouts.iter().any(|c| {
                    (point_in_polygon(*a, c) && point_to_polygon_edge_dist(*a, c) > EPS_NM)
                        || (point_in_polygon(*b, c) && point_to_polygon_edge_dist(*b, c) > EPS_NM)
                })
        }
        ResolvedShape::Poly { verts, .. } => poly_extends_outside_board(verts, outline, cutouts),
    };
    (0.0, off_board)
}
