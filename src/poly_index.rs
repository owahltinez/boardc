use std::cell::RefCell;

use crate::geom::{
    EPS_NM, GridQueryScratch, SpatialGrid, on_segment_bbox, orient_i128, point_in_polygon,
    point_to_polygon_edge_dist, poly_extends_outside_board, polygon_area, polys_overlap_area,
    pour_poly_overlaps_poly, segment_to_segment_dist,
};
use crate::ir::{BBoxNm, Nm, PourPoly, Pt};
use crate::shapes::ResolvedShape;

const SMALL_RING_MAX_VERTS: usize = 32;

thread_local! {
    static HOLE_QUERY_SCRATCH: RefCell<(GridQueryScratch, Vec<usize>)> =
        RefCell::new((GridQueryScratch::default(), Vec::new()));
}

enum RingIndex {
    Small,
    Grid {
        cell_nm: Nm,
        cols: usize,
        rows: usize,
        buckets: Vec<Vec<u32>>,
        y_step: Nm,
        y_slabs: Vec<Vec<u32>>,
    },
}

/// Flat 2D edge-grid + horizontal Y-slab index for one closed polygon ring.
pub(crate) struct IndexedRing {
    pub(crate) pts: Vec<Pt>,
    pub(crate) bbox: BBoxNm,
    index: RingIndex,
}

impl IndexedRing {
    pub(crate) fn new(pts: &[Pt]) -> Self {
        let n = pts.len();
        if n == 0 {
            return Self {
                pts: Vec::new(),
                bbox: BBoxNm::new(0, 0, 0, 0),
                index: RingIndex::Small,
            };
        }
        let bbox = BBoxNm::from_pts(pts);
        if n <= SMALL_RING_MAX_VERTS {
            return Self {
                pts: pts.to_vec(),
                bbox,
                index: RingIndex::Small,
            };
        }
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
            index: RingIndex::Grid {
                cell_nm,
                cols,
                rows,
                buckets,
                y_step,
                y_slabs,
            },
        }
    }

    #[inline]
    fn cell_range(
        origin_x: Nm,
        origin_y: Nm,
        cell_nm: Nm,
        cols: usize,
        rows: usize,
        q: BBoxNm,
    ) -> (usize, usize, usize, usize) {
        let gx0 = ((q.min_x - origin_x).div_euclid(cell_nm)).clamp(0, cols as i64 - 1) as usize;
        let gx1 = ((q.max_x - origin_x).div_euclid(cell_nm)).clamp(0, cols as i64 - 1) as usize;
        let gy0 = ((q.min_y - origin_y).div_euclid(cell_nm)).clamp(0, rows as i64 - 1) as usize;
        let gy1 = ((q.max_y - origin_y).div_euclid(cell_nm)).clamp(0, rows as i64 - 1) as usize;
        (gx0, gx1, gy0, gy1)
    }

    fn for_each_edge_in_bbox(&self, q: BBoxNm, mut f: impl FnMut(Pt, Pt) -> bool) {
        let n = self.pts.len();
        match &self.index {
            RingIndex::Small => {
                for i in 0..n {
                    let a = self.pts[i];
                    let b = self.pts[(i + 1) % n];
                    let eb = BBoxNm::new(a.x.min(b.x), a.y.min(b.y), a.x.max(b.x), a.y.max(b.y));
                    if eb.intersects(q) && !f(a, b) {
                        return;
                    }
                }
            }
            RingIndex::Grid {
                cell_nm,
                cols,
                rows,
                buckets,
                ..
            } => {
                let (gx0, gx1, gy0, gy1) =
                    Self::cell_range(self.bbox.min_x, self.bbox.min_y, *cell_nm, *cols, *rows, q);
                for gy in gy0..=gy1 {
                    let row_off = gy * *cols;
                    for gx in gx0..=gx1 {
                        for &ei in &buckets[row_off + gx] {
                            let i = ei as usize;
                            let a = self.pts[i];
                            let b = self.pts[(i + 1) % n];
                            let eb =
                                BBoxNm::new(a.x.min(b.x), a.y.min(b.y), a.x.max(b.x), a.y.max(b.y));
                            if eb.intersects(q) && !f(a, b) {
                                return;
                            }
                        }
                    }
                }
            }
        }
    }

    /// Exact boundary-inclusive point-in-polygon matching `geom::point_in_polygon`.
    pub(crate) fn contains_pt(&self, pt: Pt) -> bool {
        let n = self.pts.len();
        if n < 3 || !self.bbox.contains_pt(pt) {
            return false;
        }
        match &self.index {
            RingIndex::Small => point_in_polygon(pt, &self.pts),
            RingIndex::Grid {
                y_step, y_slabs, ..
            } => {
                let slab_idx = ((pt.y - self.bbox.min_y) / *y_step)
                    .clamp(0, y_slabs.len() as i64 - 1) as usize;
                let mut inside = false;
                let (px, py) = (pt.x as f64, pt.y as f64);
                for &ei in &y_slabs[slab_idx] {
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
        }
    }

    /// Exact minimum distance to ring edges within `max_dist`, or `f64::INFINITY` if none are within `max_dist`.
    pub(crate) fn min_dist_to_shape_bounded(&self, shape: &ResolvedShape, max_dist: Nm) -> f64 {
        if self.pts.len() < 2 {
            return f64::INFINITY;
        }
        let q = shape.bbox().expand(max_dist.max(0));
        if !q.intersects(self.bbox) {
            return f64::INFINITY;
        }
        let stop_early =
            |d: f64| d < -EPS_NM || (d <= 0.0 && matches!(shape, ResolvedShape::Poly { .. }));
        let mut best = f64::INFINITY;
        self.for_each_edge_in_bbox(q, |a, b| {
            let d = shape.dist_to_capsule(a, b, 0);
            if d < best {
                best = d;
                if stop_early(best) {
                    return false;
                }
            }
            true
        });
        best
    }

    /// Exact minimum distance between a polygon `poly` and this ring within `max_dist`.
    pub(crate) fn min_dist_to_poly_edges_bounded(
        &self,
        poly: &[Pt],
        poly_bbox: BBoxNm,
        max_dist: Nm,
    ) -> f64 {
        let m = poly.len();
        if self.pts.len() < 2 || m < 2 {
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
            self.for_each_edge_in_bbox(eb.expand(pad), |b1, b2| {
                let d = segment_to_segment_dist(a1, a2, b1, b2);
                if d < best {
                    best = d;
                    if best <= 0.0 {
                        return false;
                    }
                }
                true
            });
            if best <= 0.0 {
                return 0.0;
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
    hole_grid: Option<SpatialGrid>,
}

impl IndexedPour {
    pub(crate) fn new(poly: &PourPoly) -> Self {
        let outer = IndexedRing::new(&poly.outer);
        let holes: Vec<IndexedRing> = poly.holes.iter().map(|h| IndexedRing::new(h)).collect();
        let hole_grid = (holes.len() > 8).then(|| {
            let max_dim = outer.bbox.width().max(outer.bbox.height());
            let cell = (max_dim / 128).clamp(1_000_000, 4_000_000);
            SpatialGrid::new(holes.iter().map(|h| h.bbox), cell)
        });
        Self {
            outer,
            holes,
            hole_grid,
        }
    }

    #[inline]
    fn any_hole_in_bbox(&self, q: BBoxNm, mut pred: impl FnMut(&IndexedRing) -> bool) -> bool {
        if let Some(grid) = &self.hole_grid {
            HOLE_QUERY_SCRATCH.with(|cell| {
                let mut borrow = cell.borrow_mut();
                let (scratch, cands) = &mut *borrow;
                grid.query_with(q, scratch, cands);
                cands.iter().any(|&hi| pred(&self.holes[hi]))
            })
        } else {
            self.holes.iter().any(pred)
        }
    }

    fn find_hole_containing_pt(&self, pt: Pt) -> Option<&IndexedRing> {
        if let Some(grid) = &self.hole_grid {
            let mut found = None;
            grid.query_point_any(pt, |hi| {
                let h = &self.holes[hi];
                if h.bbox.contains_pt(pt) && h.contains_pt(pt) {
                    found = Some(h);
                    true
                } else {
                    false
                }
            });
            found
        } else {
            self.holes
                .iter()
                .find(|h| h.bbox.contains_pt(pt) && h.contains_pt(pt))
        }
    }

    pub(crate) fn contains_pt(&self, pt: Pt) -> bool {
        self.outer.contains_pt(pt) && self.find_hole_containing_pt(pt).is_none()
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
        if let Some(h) = self.find_hole_containing_pt(ref_pt) {
            return h
                .min_dist_to_shape_bounded(shape, pad)
                .min(d_outer)
                .max(0.0);
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
            ResolvedShape::Circle { radius, .. }
            | ResolvedShape::Capsule { radius, .. }
            | ResolvedShape::RoundRect { radius, .. } => {
                if *radius <= 0 {
                    return false;
                }
                if self.outer.min_dist_to_shape_bounded(shape, 0) < -EPS_NM {
                    return true;
                }
                if self.any_hole_in_bbox(sb, |h| {
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
        if self.any_hole_in_bbox(poly_bbox, |h| {
            h.bbox.intersects(poly_bbox) && h.intersects_poly_edges(poly, poly_bbox)
        }) {
            return true;
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
        let check_pair = |ra: &IndexedRing, rb: &IndexedRing, min_d: &mut f64| -> bool {
            if ra.bbox.expand(pad).intersects(rb.bbox) {
                *min_d = (*min_d).min(ra.min_dist_to_ring_bounded(rb, pad));
                if *min_d <= 0.0 {
                    return true;
                }
            }
            false
        };
        if check_pair(&self.outer, &other.outer, &mut min_d)
            || other.any_hole_in_bbox(self.outer.bbox.expand(pad), |rb| {
                check_pair(&self.outer, rb, &mut min_d)
            })
            || self.any_hole_in_bbox(other.outer.bbox.expand(pad), |ra| {
                check_pair(ra, &other.outer, &mut min_d)
            })
        {
            return 0.0;
        }
        for ra in &self.holes {
            let ra_exp = ra.bbox.expand(pad);
            if ra_exp.intersects(other.outer.bbox)
                && other.any_hole_in_bbox(ra_exp, |rb| check_pair(ra, rb, &mut min_d))
            {
                return 0.0;
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
        let mut min_d = self.outer.min_dist_to_ring_bounded(outline, req);
        for c in cutouts {
            min_d = min_d.min(self.outer.min_dist_to_ring_bounded(c, req));
        }
        // `self.outer` encloses all holes, so holes only need checking against cutouts.
        if !cutouts.is_empty() && !self.holes.is_empty() {
            for c in cutouts {
                self.any_hole_in_bbox(c.bbox.expand(req.max(0)), |h| {
                    min_d = min_d.min(h.min_dist_to_ring_bounded(c, req));
                    false
                });
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
    let pt_off_board = |p: Pt| {
        !outline.contains_pt(p)
            || cutouts
                .iter()
                .any(|c| c.contains_pt(p) && point_to_polygon_edge_dist(p, &c.pts) > EPS_NM)
    };
    if pt_off_board(shape.ref_pt()) {
        return (0.0, true);
    }
    let encloses_cutout = cutouts
        .iter()
        .any(|c| !c.pts.is_empty() && c.pts.iter().all(|&p| shape.contains_pt(p)));
    if encloses_cutout {
        return (0.0, true);
    }
    let mut min_d = outline.min_dist_to_shape_bounded(shape, req);
    for c in cutouts {
        min_d = min_d.min(c.min_dist_to_shape_bounded(shape, req));
    }
    if min_d > 0.0 {
        return (min_d, false);
    }
    let off_board = match shape {
        ResolvedShape::Circle { .. } => min_d < -EPS_NM,
        ResolvedShape::Capsule { a, b, .. } => {
            min_d < -EPS_NM || pt_off_board(*a) || pt_off_board(*b)
        }
        ResolvedShape::RoundRect { corners, .. } => {
            min_d < -EPS_NM || corners.iter().any(|&c| pt_off_board(c))
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

    #[test]
    fn indexed_pour_hole_grid_and_small_ring_match_bruteforce() {
        let mut holes = Vec::new();
        for ix in 0..5 {
            for iy in 0..4 {
                let cx = 2_000_000 + ix * 3_000_000;
                let cy = 2_000_000 + iy * 3_000_000;
                // 8-vertex hole exercises small-ring path (<= 32 verts) inside > 8 hole grid.
                holes.push(vec![
                    Pt::new(cx - 400_000, cy - 200_000),
                    Pt::new(cx - 200_000, cy - 400_000),
                    Pt::new(cx + 200_000, cy - 400_000),
                    Pt::new(cx + 400_000, cy - 200_000),
                    Pt::new(cx + 400_000, cy + 200_000),
                    Pt::new(cx + 200_000, cy + 400_000),
                    Pt::new(cx - 200_000, cy + 400_000),
                    Pt::new(cx - 400_000, cy + 200_000),
                ]);
            }
        }
        let poly = PourPoly {
            outer: vec![
                Pt::new(0, 0),
                Pt::new(18_000_000, 0),
                Pt::new(18_000_000, 14_000_000),
                Pt::new(0, 14_000_000),
            ],
            holes,
        };
        let indexed = IndexedPour::new(&poly);
        assert!(indexed.hole_grid.is_some());
        assert!(matches!(indexed.holes[0].index, RingIndex::Small));

        let brute_contains = |pt: Pt| -> bool {
            point_in_polygon(pt, &poly.outer) && !poly.holes.iter().any(|h| point_in_polygon(pt, h))
        };
        for pt in [
            Pt::new(2_000_000, 2_000_000),
            Pt::new(3_500_000, 3_500_000),
            Pt::new(8_000_000, 5_000_000),
            Pt::new(8_400_000, 5_000_000),
            Pt::new(-100_000, 5_000_000),
        ] {
            assert_eq!(indexed.contains_pt(pt), brute_contains(pt), "at {pt:?}");
        }

        // Shape inside a hole, shape overlapping hole edge, and shape outside pour.
        let in_hole = ResolvedShape::Circle {
            center: Pt::new(8_000_000, 5_000_000),
            radius: 100_000,
        };
        assert!(!indexed.overlaps_shape(&poly, &in_hole));
        assert!((indexed.dist_to_shape_bounded(&in_hole, 500_000) - 300_000.0).abs() < 1.0);

        let crosses_hole = ResolvedShape::Circle {
            center: Pt::new(8_000_000, 5_000_000),
            radius: 450_000,
        };
        assert!(indexed.overlaps_shape(&poly, &crosses_hole));
        assert_eq!(indexed.dist_to_shape_bounded(&crosses_hole, 500_000), 0.0);

        let rect_in_hole = vec![
            Pt::new(7_850_000, 4_850_000),
            Pt::new(8_150_000, 4_850_000),
            Pt::new(8_150_000, 5_150_000),
            Pt::new(7_850_000, 5_150_000),
        ];
        assert!(!indexed.intersects_poly(&rect_in_hole, BBoxNm::from_pts(&rect_in_hole)));

        let rect_hit = vec![
            Pt::new(8_300_000, 4_900_000),
            Pt::new(8_600_000, 4_900_000),
            Pt::new(8_600_000, 5_100_000),
            Pt::new(8_300_000, 5_100_000),
        ];
        assert!(indexed.intersects_poly(&rect_hit, BBoxNm::from_pts(&rect_hit)));

        let other_pour = IndexedPour::new(&PourPoly {
            outer: vec![
                Pt::new(7_850_000, 4_850_000),
                Pt::new(8_150_000, 4_850_000),
                Pt::new(8_150_000, 5_150_000),
                Pt::new(7_850_000, 5_150_000),
            ],
            holes: Vec::new(),
        });
        let expected_chamfer_dist = 300_000.0 / 2.0_f64.sqrt();
        assert!(
            (indexed.dist_to_pour_bounded(&other_pour, 500_000) - expected_chamfer_dist).abs()
                < 1.0
        );
    }

    #[test]
    fn roundrect_enclosing_cutout_agrees_between_geom_and_poly_index() {
        let outline = vec![
            Pt::new(0, 0),
            Pt::new(20_000_000, 0),
            Pt::new(20_000_000, 20_000_000),
            Pt::new(0, 20_000_000),
        ];
        let outline_ring = IndexedRing::new(&outline);

        // Strictly interior cutout inside RoundRect.
        let rr_mid = ResolvedShape::RoundRect {
            corners: [
                Pt::new(2_200_000, 2_200_000),
                Pt::new(5_800_000, 2_200_000),
                Pt::new(5_800_000, 5_800_000),
                Pt::new(2_200_000, 5_800_000),
            ],
            radius: 200_000,
            bbox: BBoxNm::new(2_000_000, 2_000_000, 6_000_000, 6_000_000),
        };
        let cutout_strict = vec![
            Pt::new(3_500_000, 3_500_000),
            Pt::new(4_500_000, 3_500_000),
            Pt::new(4_500_000, 4_500_000),
            Pt::new(3_500_000, 4_500_000),
        ];
        let cuts_strict = vec![cutout_strict.clone()];
        let idx_strict = vec![IndexedRing::new(&cutout_strict)];
        assert_eq!(
            crate::geom::shape_edge_clearance(&rr_mid, &outline, &cuts_strict),
            (0.0, true)
        );
        assert_eq!(
            indexed_shape_edge_clearance(&rr_mid, &outline_ring, &idx_strict, 200_000),
            (0.0, true)
        );

        // RoundRect touching board outline (min_d == 0) while enclosing a cutout.
        let rr_touch = ResolvedShape::RoundRect {
            corners: [
                Pt::new(200_000, 2_200_000),
                Pt::new(3_800_000, 2_200_000),
                Pt::new(3_800_000, 5_800_000),
                Pt::new(200_000, 5_800_000),
            ],
            radius: 200_000,
            bbox: BBoxNm::new(0, 2_000_000, 4_000_000, 6_000_000),
        };
        let cutout_touch = vec![
            Pt::new(0, 3_500_000),
            Pt::new(1_000_000, 3_500_000),
            Pt::new(1_000_000, 4_500_000),
            Pt::new(0, 4_500_000),
        ];
        for cuts in [vec![cutout_strict.clone()], vec![cutout_touch]] {
            let idx_cuts: Vec<IndexedRing> = cuts.iter().map(|c| IndexedRing::new(c)).collect();
            let g = crate::geom::shape_edge_clearance(&rr_touch, &outline, &cuts);
            let ix = indexed_shape_edge_clearance(&rr_touch, &outline_ring, &idx_cuts, 200_000);
            assert_eq!(g, (0.0, true));
            assert_eq!(ix, (0.0, true));
        }

        // External kissing cutout should not trigger enclosure false positive.
        let rr_ext_cut = vec![
            Pt::new(6_000_000, 3_500_000),
            Pt::new(7_000_000, 3_500_000),
            Pt::new(7_000_000, 4_500_000),
            Pt::new(6_000_000, 4_500_000),
        ];
        let idx_ext = vec![IndexedRing::new(&rr_ext_cut)];
        assert_eq!(
            crate::geom::shape_edge_clearance(&rr_mid, &outline, &[rr_ext_cut]),
            (0.0, false)
        );
        assert_eq!(
            indexed_shape_edge_clearance(&rr_mid, &outline_ring, &idx_ext, 200_000),
            (0.0, false)
        );
    }
}
