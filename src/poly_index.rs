use std::iter::once;

use crate::geom::{
    EPS_NM, on_segment_bbox, orient_i128, point_in_polygon, point_to_polygon_edge_dist,
    poly_extends_outside_board, polygon_area, polys_overlap_area, pour_poly_overlaps_poly,
    segment_to_segment_dist,
};
use crate::ir::{BBoxNm, Nm, PourPoly, Pt};
use crate::shapes::ResolvedShape;

/// Flat 2D edge-grid + horizontal Y-slab index for one closed polygon ring.
pub(crate) struct IndexedRing {
    pub(crate) pts: Vec<Pt>,
    pub(crate) bbox: BBoxNm,
    origin_x: Nm,
    origin_y: Nm,
    cell_nm: Nm,
    cols: usize,
    rows: usize,
    buckets: Vec<Vec<u32>>,
    y_origin: Nm,
    y_step: Nm,
    y_slabs: Vec<Vec<u32>>,
}

impl IndexedRing {
    pub(crate) fn new(pts: &[Pt]) -> Self {
        let n = pts.len();
        if n == 0 {
            return Self {
                pts: Vec::new(),
                bbox: BBoxNm::new(0, 0, 0, 0),
                origin_x: 0,
                origin_y: 0,
                cell_nm: 1,
                cols: 1,
                rows: 1,
                buckets: vec![Vec::new()],
                y_origin: 0,
                y_step: 1,
                y_slabs: vec![Vec::new()],
            };
        }
        let bbox = BBoxNm::from_pts(pts);
        let w = bbox.width().max(1);
        let h = bbox.height().max(1);
        let area = (w as f64) * (h as f64);
        let target_cells = (n as f64 * 0.5).clamp(16.0, 16_384.0);
        let mut cell_nm = ((area / target_cells).sqrt().round() as Nm).max(50_000);
        let max_dim = w.max(h);
        if max_dim / cell_nm > 192 {
            cell_nm = (max_dim / 192).max(1);
        }
        let cols = ((w / cell_nm) as usize + 1).max(1);
        let rows = ((h / cell_nm) as usize + 1).max(1);
        let mut buckets = vec![Vec::new(); cols * rows];

        let num_slabs = (n / 8).clamp(8, 256);
        let y_step = (h / num_slabs as Nm).max(1);
        let actual_slabs = ((h / y_step) as usize + 1).max(1);
        let mut y_slabs = vec![Vec::new(); actual_slabs];

        for i in 0..n {
            let a = pts[i];
            let b = pts[(i + 1) % n];
            let ex0 = ((a.x.min(b.x) - bbox.min_x) / cell_nm).clamp(0, cols as i64 - 1) as usize;
            let ex1 = ((a.x.max(b.x) - bbox.min_x) / cell_nm).clamp(0, cols as i64 - 1) as usize;
            let ey0 = ((a.y.min(b.y) - bbox.min_y) / cell_nm).clamp(0, rows as i64 - 1) as usize;
            let ey1 = ((a.y.max(b.y) - bbox.min_y) / cell_nm).clamp(0, rows as i64 - 1) as usize;
            let ei = i as u32;
            for gy in ey0..=ey1 {
                let row_off = gy * cols;
                for gx in ex0..=ex1 {
                    buckets[row_off + gx].push(ei);
                }
            }
            // Include all edges (horizontal included) so boundary points match `geom::point_in_polygon`.
            let s0 =
                ((a.y.min(b.y) - bbox.min_y) / y_step).clamp(0, actual_slabs as i64 - 1) as usize;
            let s1 =
                ((a.y.max(b.y) - bbox.min_y) / y_step).clamp(0, actual_slabs as i64 - 1) as usize;
            for slab in &mut y_slabs[s0..=s1] {
                slab.push(ei);
            }
        }

        Self {
            pts: pts.to_vec(),
            bbox,
            origin_x: bbox.min_x,
            origin_y: bbox.min_y,
            cell_nm,
            cols,
            rows,
            buckets,
            y_origin: bbox.min_y,
            y_step,
            y_slabs,
        }
    }

    #[inline]
    fn cell_range(&self, q: BBoxNm) -> (usize, usize, usize, usize) {
        let gx0 = ((q.min_x - self.origin_x).div_euclid(self.cell_nm))
            .clamp(0, self.cols as i64 - 1) as usize;
        let gx1 = ((q.max_x - self.origin_x).div_euclid(self.cell_nm))
            .clamp(0, self.cols as i64 - 1) as usize;
        let gy0 = ((q.min_y - self.origin_y).div_euclid(self.cell_nm))
            .clamp(0, self.rows as i64 - 1) as usize;
        let gy1 = ((q.max_y - self.origin_y).div_euclid(self.cell_nm))
            .clamp(0, self.rows as i64 - 1) as usize;
        (gx0, gx1, gy0, gy1)
    }

    /// Exact boundary-inclusive point-in-polygon matching `geom::point_in_polygon`.
    pub(crate) fn contains_pt(&self, pt: Pt) -> bool {
        let n = self.pts.len();
        if n < 3 || !self.bbox.contains_pt(pt) {
            return false;
        }
        let slab_idx =
            ((pt.y - self.y_origin) / self.y_step).clamp(0, self.y_slabs.len() as i64 - 1) as usize;
        let mut inside = false;
        let (px, py) = (pt.x as f64, pt.y as f64);
        for &ei in &self.y_slabs[slab_idx] {
            let a = self.pts[ei as usize];
            let b = self.pts[(ei as usize + 1) % n];
            if orient_i128(a, b, pt) == 0 && on_segment_bbox(a, pt, b) {
                return true;
            }
            let (ax, ay) = (a.x as f64, a.y as f64);
            let (bx, by) = (b.x as f64, b.y as f64);
            if (ay > py) != (by > py) {
                let x_cross = ax + (py - ay) * (bx - ax) / (by - ay);
                if px < x_cross {
                    inside = !inside;
                }
            }
        }
        inside
    }

    /// Exact minimum distance to ring edges within `max_dist`, or `f64::INFINITY` if none are within `max_dist`.
    pub(crate) fn min_dist_to_shape_bounded(&self, shape: &ResolvedShape, max_dist: Nm) -> f64 {
        let n = self.pts.len();
        if n < 2 {
            return f64::INFINITY;
        }
        let q = shape.bbox().expand(max_dist.max(0));
        if !q.intersects(self.bbox) {
            return f64::INFINITY;
        }
        let (gx0, gx1, gy0, gy1) = self.cell_range(q);
        let mut best = f64::INFINITY;
        for gy in gy0..=gy1 {
            let row_off = gy * self.cols;
            for gx in gx0..=gx1 {
                for &ei in &self.buckets[row_off + gx] {
                    let i = ei as usize;
                    let a = self.pts[i];
                    let b = self.pts[(i + 1) % n];
                    let eb = BBoxNm::new(a.x.min(b.x), a.y.min(b.y), a.x.max(b.x), a.y.max(b.y));
                    if !eb.intersects(q) {
                        continue;
                    }
                    let d = shape.dist_to_capsule(a, b, 0);
                    if d < best {
                        best = d;
                        if best < -EPS_NM
                            || (best <= 0.0 && matches!(shape, ResolvedShape::Poly { .. }))
                        {
                            return best;
                        }
                    }
                }
            }
        }
        best
    }

    /// Exact minimum distance between a polygon `poly` and this ring within `max_dist`.
    pub(crate) fn min_dist_to_poly_edges_bounded(
        &self,
        poly: &[Pt],
        poly_bbox: BBoxNm,
        max_dist: Nm,
    ) -> f64 {
        let n = self.pts.len();
        let m = poly.len();
        if n < 2 || m < 2 {
            return f64::INFINITY;
        }
        let pad = max_dist.max(0);
        if !poly_bbox.expand(pad).intersects(self.bbox) {
            return f64::INFINITY;
        }
        let self_exp = self.bbox.expand(pad);
        let mut best = f64::INFINITY;
        for i in 0..m {
            let a1 = poly[i];
            let a2 = poly[(i + 1) % m];
            let eb = BBoxNm::new(
                a1.x.min(a2.x),
                a1.y.min(a2.y),
                a1.x.max(a2.x),
                a1.y.max(a2.y),
            );
            if !eb.intersects(self_exp) {
                continue;
            }
            let q = eb.expand(pad);
            let (gx0, gx1, gy0, gy1) = self.cell_range(q);
            for gy in gy0..=gy1 {
                let row_off = gy * self.cols;
                for gx in gx0..=gx1 {
                    for &ej in &self.buckets[row_off + gx] {
                        let j = ej as usize;
                        let b1 = self.pts[j];
                        let b2 = self.pts[(j + 1) % n];
                        let bb = BBoxNm::new(
                            b1.x.min(b2.x),
                            b1.y.min(b2.y),
                            b1.x.max(b2.x),
                            b1.y.max(b2.y),
                        );
                        if !bb.intersects(q) {
                            continue;
                        }
                        let d = segment_to_segment_dist(a1, a2, b1, b2);
                        if d < best {
                            best = d;
                            if best <= 0.0 {
                                return 0.0;
                            }
                        }
                    }
                }
            }
        }
        best
    }

    /// Exact minimum distance between two rings within `max_dist`, else `f64::INFINITY`.
    pub(crate) fn min_dist_to_ring_bounded(&self, other: &IndexedRing, max_dist: Nm) -> f64 {
        let (small, big) = if self.pts.len() <= other.pts.len() {
            (self, other)
        } else {
            (other, self)
        };
        big.min_dist_to_poly_edges_bounded(&small.pts, small.bbox, max_dist)
    }

    /// True if any edge of `poly` touches or crosses any edge of this ring.
    pub(crate) fn intersects_poly_edges(&self, poly: &[Pt], poly_bbox: BBoxNm) -> bool {
        self.min_dist_to_poly_edges_bounded(poly, poly_bbox, 0) == 0.0
    }
}

/// Spatially indexed copper pour polygon (outer ring + indexed hole rings).
pub(crate) struct IndexedPour {
    pub(crate) outer: IndexedRing,
    pub(crate) holes: Vec<IndexedRing>,
}

impl IndexedPour {
    pub(crate) fn new(poly: &PourPoly) -> Self {
        Self {
            outer: IndexedRing::new(&poly.outer),
            holes: poly.holes.iter().map(|h| IndexedRing::new(h)).collect(),
        }
    }

    pub(crate) fn contains_pt(&self, pt: Pt) -> bool {
        if !self.outer.contains_pt(pt) {
            return false;
        }
        for h in &self.holes {
            if h.bbox.contains_pt(pt) && h.contains_pt(pt) {
                return false;
            }
        }
        true
    }

    /// Exact distance from a resolved shape to this pour if within `max_dist`, else `f64::INFINITY`.
    pub(crate) fn dist_to_shape_bounded(&self, shape: &ResolvedShape, max_dist: Nm) -> f64 {
        if self.outer.pts.len() < 3 {
            return f64::INFINITY;
        }
        let pad = max_dist.max(0);
        if !shape.bbox().expand(pad).intersects(self.outer.bbox) {
            return f64::INFINITY;
        }
        let d_outer = self.outer.min_dist_to_shape_bounded(shape, pad);
        if d_outer <= 0.0 || shape.dist_to_point(self.outer.pts[0]) == 0.0 {
            return 0.0;
        }
        let ref_pt = shape.ref_pt();
        if !self.outer.contains_pt(ref_pt) {
            return d_outer;
        }
        for h in &self.holes {
            if h.bbox.contains_pt(ref_pt) && h.contains_pt(ref_pt) {
                return h
                    .min_dist_to_shape_bounded(shape, pad)
                    .min(d_outer)
                    .max(0.0);
            }
        }
        0.0
    }

    /// True if `shape` strictly overlaps the interior of this pour (respecting holes).
    pub(crate) fn overlaps_shape(&self, poly: &PourPoly, shape: &ResolvedShape) -> bool {
        let sb = shape.bbox();
        if self.outer.pts.len() < 3 || !sb.intersects(self.outer.bbox) {
            return false;
        }
        match shape {
            ResolvedShape::Circle { radius, .. } | ResolvedShape::Capsule { radius, .. } => {
                if *radius <= 0 {
                    return false;
                }
                if self.outer.min_dist_to_shape_bounded(shape, 0) < -EPS_NM {
                    return true;
                }
                if self.holes.iter().any(|h| {
                    h.bbox.intersects(sb) && h.min_dist_to_shape_bounded(shape, 0) < -EPS_NM
                }) {
                    return true;
                }
                self.contains_pt(shape.ref_pt())
            }
            ResolvedShape::Poly { verts, bbox } => {
                self.intersects_poly(verts, *bbox) && pour_poly_overlaps_poly(poly, verts)
            }
        }
    }

    /// Exact intersection test between this pour (respecting holes) and a simple polygon `poly`.
    pub(crate) fn intersects_poly(&self, poly: &[Pt], poly_bbox: BBoxNm) -> bool {
        if self.outer.pts.len() < 3 || poly.len() < 3 || !poly_bbox.intersects(self.outer.bbox) {
            return false;
        }
        if poly.iter().any(|&v| self.contains_pt(v)) {
            return true;
        }
        if self.outer.intersects_poly_edges(poly, poly_bbox) {
            return true;
        }
        for h in &self.holes {
            if h.bbox.intersects(poly_bbox) && h.intersects_poly_edges(poly, poly_bbox) {
                return true;
            }
        }
        point_in_polygon(self.outer.pts[0], poly)
    }

    /// Exact distance between two pours if within `max_dist`, else `f64::INFINITY`.
    pub(crate) fn dist_to_pour_bounded(&self, other: &IndexedPour, max_dist: Nm) -> f64 {
        if self.outer.pts.len() < 3 || other.outer.pts.len() < 3 {
            return f64::INFINITY;
        }
        let pad = max_dist.max(0);
        if !self.outer.bbox.expand(pad).intersects(other.outer.bbox) {
            return f64::INFINITY;
        }
        let mut min_d = f64::INFINITY;
        for ra in once(&self.outer).chain(&self.holes) {
            for rb in once(&other.outer).chain(&other.holes) {
                if ra.bbox.expand(pad).intersects(rb.bbox) {
                    min_d = min_d.min(ra.min_dist_to_ring_bounded(rb, pad));
                    if min_d <= 0.0 {
                        return 0.0;
                    }
                }
            }
        }
        if self.contains_pt(other.outer.pts[0]) || other.contains_pt(self.outer.pts[0]) {
            return 0.0;
        }
        min_d.max(0.0)
    }

    /// Exact edge clearance of a pour (outer + holes) against board outline and cutouts.
    pub(crate) fn edge_clearance(
        &self,
        poly: &PourPoly,
        outline: &IndexedRing,
        cutouts: &[IndexedRing],
        req: Nm,
    ) -> (f64, bool) {
        let Some(&ref_pt) = self.outer.pts.first() else {
            return (0.0, false);
        };
        if !outline.contains_pt(ref_pt) {
            return (0.0, true);
        }
        for c in cutouts {
            if c.pts.first().is_some_and(|&p| self.contains_pt(p))
                && point_to_polygon_edge_dist(c.pts[0], &self.outer.pts) > EPS_NM
            {
                return (0.0, true);
            }
        }
        let mut min_d = f64::INFINITY;
        for r in once(&self.outer).chain(&self.holes) {
            min_d = min_d.min(r.min_dist_to_ring_bounded(outline, req));
            for c in cutouts {
                min_d = min_d.min(r.min_dist_to_ring_bounded(c, req));
            }
        }
        if min_d > 0.0 {
            let covers_cutout = cutouts
                .iter()
                .any(|c| c.pts.first().is_some_and(|&p| self.contains_pt(p)));
            return if covers_cutout {
                (0.0, true)
            } else {
                (min_d, false)
            };
        }
        let area = polygon_area(&self.outer.pts);
        let outside_outline = area - polys_overlap_area(&self.outer.pts, &outline.pts) > 1.0;
        let hits_cutout = cutouts
            .iter()
            .any(|c| pour_poly_overlaps_poly(poly, &c.pts));
        (0.0, outside_outline || hits_cutout)
    }
}

/// Exact edge clearance of any resolved shape against an indexed board outline and cutouts.
pub(crate) fn indexed_shape_edge_clearance(
    shape: &ResolvedShape,
    outline: &IndexedRing,
    cutouts: &[IndexedRing],
    req: Nm,
) -> (f64, bool) {
    let ref_pt = shape.ref_pt();
    let inside_outline = outline.contains_pt(ref_pt);
    let strictly_in_cutout = cutouts
        .iter()
        .any(|c| c.contains_pt(ref_pt) && point_to_polygon_edge_dist(ref_pt, &c.pts) > EPS_NM);
    if !inside_outline || strictly_in_cutout {
        return (0.0, true);
    }
    let mut min_d = outline.min_dist_to_shape_bounded(shape, req);
    for c in cutouts {
        min_d = min_d.min(c.min_dist_to_shape_bounded(shape, req));
    }
    if min_d > 0.0 {
        let encloses_cutout = cutouts.iter().any(|c| {
            c.pts
                .first()
                .is_some_and(|&p| shape.dist_to_point(p) == 0.0)
        });
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
                || !outline.contains_pt(*a)
                || !outline.contains_pt(*b)
                || cutouts.iter().any(|c| {
                    (c.contains_pt(*a) && point_to_polygon_edge_dist(*a, &c.pts) > EPS_NM)
                        || (c.contains_pt(*b) && point_to_polygon_edge_dist(*b, &c.pts) > EPS_NM)
                })
        }
        ResolvedShape::Poly { verts, .. } => {
            let cut_slices: Vec<Vec<Pt>> = cutouts.iter().map(|c| c.pts.clone()).collect();
            poly_extends_outside_board(verts, &outline.pts, &cut_slices)
        }
    };
    (0.0, off_board)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::geom::point_in_polygon;

    #[test]
    fn indexed_ring_contains_pt_matches_point_in_polygon_on_edges_and_corners() {
        let poly = vec![
            Pt::new(0, 0),
            Pt::new(10_000, 0),
            Pt::new(10_000, 10_000),
            Pt::new(5_000, 10_000),
            Pt::new(5_000, 5_000),
            Pt::new(0, 5_000),
        ];
        let ring = IndexedRing::new(&poly);
        for pt in [
            Pt::new(0, 0),
            Pt::new(5_000, 0),
            Pt::new(10_000, 0),
            Pt::new(10_000, 5_000),
            Pt::new(10_000, 10_000),
            Pt::new(7_500, 10_000),
            Pt::new(5_000, 5_000),
            Pt::new(2_500, 5_000),
            Pt::new(0, 2_500),
            Pt::new(2_500, 2_500),
            Pt::new(2_500, 7_500),
            Pt::new(-1, 0),
            Pt::new(10_001, 5_000),
        ] {
            assert_eq!(
                ring.contains_pt(pt),
                point_in_polygon(pt, &poly),
                "mismatch at {pt:?}"
            );
        }
    }
}
