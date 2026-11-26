use std::collections::{BTreeMap, HashMap};

use crate::drc::{Severity, Violation, ViolationKind};
use crate::drc_scene::{PadItem, PourItem, TrackItem, ViaItem};
use crate::geom::{SpatialGrid, UnionFind};
use crate::ir::{BBoxNm, BoardIr, Nm, PadKind, Pt};
use crate::poly_index::IndexedPour;
use crate::shapes::ResolvedShape;

const TOUCH_TOL_NM: f64 = 1.0;

struct ConnNode<'a> {
    layers: Vec<String>,
    bbox: BBoxNm,
    kind: ConnKind<'a>,
}

enum ConnKind<'a> {
    Pad {
        label: &'a str,
        center: Pt,
        shape: &'a ResolvedShape,
    },
    Shape(&'a ResolvedShape),
    Barrel(ResolvedShape),
    Pour(&'a IndexedPour),
}

fn node_shape<'a>(n: &'a ConnNode<'a>) -> &'a ResolvedShape {
    match &n.kind {
        ConnKind::Pad { shape, .. } | ConnKind::Shape(shape) => shape,
        ConnKind::Barrel(shape) => shape,
        ConnKind::Pour(_) => unreachable!(),
    }
}

fn nodes_touch(a: &ConnNode<'_>, b: &ConnNode<'_>) -> bool {
    if !a.bbox.expand(2).intersects(b.bbox) || !a.layers.iter().any(|l| b.layers.contains(l)) {
        return false;
    }
    match (&a.kind, &b.kind) {
        (ConnKind::Pour(pa), ConnKind::Pour(pb)) => pa.dist_to_pour_bounded(pb, 2) <= TOUCH_TOL_NM,
        (ConnKind::Pour(indexed), _) => {
            indexed.dist_to_shape_bounded(node_shape(b), 2) <= TOUCH_TOL_NM
        }
        (_, ConnKind::Pour(indexed)) => {
            indexed.dist_to_shape_bounded(node_shape(a), 2) <= TOUCH_TOL_NM
        }
        _ => node_shape(a).dist_to_shape(node_shape(b)) <= TOUCH_TOL_NM,
    }
}

/// Checks electrical connectivity per net over pads, tracks, vias, and filled pours.
pub(crate) fn check_connectivity(
    board: &BoardIr,
    cu_layers: &[String],
    tracks: &[TrackItem<'_>],
    pads: &[PadItem],
    vias: &[ViaItem],
    pours: &[PourItem<'_>],
) -> Vec<Violation> {
    let ignored =
        |net: &str| net.is_empty() || board.nets.get(net).is_some_and(|nd| nd.ignore_route);
    let mut nodes_by_net: BTreeMap<&str, Vec<ConnNode<'_>>> = BTreeMap::new();

    for pad in pads {
        if pad.kind == PadKind::Npth || pad.layers.is_empty() || ignored(&pad.net) {
            continue;
        }
        let entry = nodes_by_net.entry(pad.net.as_str()).or_default();
        entry.push(ConnNode {
            layers: pad.layers.clone(),
            bbox: pad.bbox,
            kind: ConnKind::Pad {
                label: &pad.label,
                center: pad.center,
                shape: &pad.shape,
            },
        });
        // Plated THT barrels bridge all copper layers even when pad copper is only on F.Cu/B.Cu.
        if pad.kind == PadKind::Tht
            && let Some(drill) = pad.drill_nm
            && drill > 0
            && pad.layers.len() < cu_layers.len()
        {
            let barrel = ResolvedShape::Circle {
                center: pad.center,
                radius: drill / 2,
            };
            entry.push(ConnNode {
                layers: cu_layers.to_vec(),
                bbox: barrel.bbox(),
                kind: ConnKind::Barrel(barrel),
            });
        }
    }

    for tr in tracks {
        if ignored(tr.net) {
            continue;
        }
        nodes_by_net.entry(tr.net).or_default().push(ConnNode {
            layers: vec![tr.layer.to_string()],
            bbox: tr.bbox,
            kind: ConnKind::Shape(&tr.shape),
        });
    }

    for (vi, via) in vias.iter().enumerate() {
        if ignored(&via.net) {
            continue;
        }
        nodes_by_net
            .entry(via.net.as_str())
            .or_default()
            .push(ConnNode {
                layers: board.vias[vi].spanned_layers(cu_layers, &board.layers),
                bbox: via.bbox,
                kind: ConnKind::Shape(&via.shape),
            });
    }

    for pour in pours {
        if ignored(pour.net) {
            continue;
        }
        nodes_by_net.entry(pour.net).or_default().push(ConnNode {
            layers: vec![pour.layer.to_string()],
            bbox: pour.bbox,
            kind: ConnKind::Pour(&pour.indexed),
        });
    }

    let mut out = Vec::new();
    for (net_name, nodes) in nodes_by_net {
        let pad_indices: Vec<usize> = nodes
            .iter()
            .enumerate()
            .filter_map(|(i, n)| matches!(n.kind, ConnKind::Pad { .. }).then_some(i))
            .collect();
        if pad_indices.len() <= 1 {
            continue;
        }
        let mut uf = UnionFind::new(nodes.len());
        let bboxes: Vec<BBoxNm> = nodes.iter().map(|n| n.bbox).collect();
        let mut grid = SpatialGrid::new(&bboxes, 2_000_000);
        let mut candidates = Vec::new();
        for i in 0..nodes.len() {
            grid.query(bboxes[i].expand(10), &mut candidates);
            for &j in &candidates {
                if j <= i || uf.find(i) == uf.find(j) {
                    continue;
                }
                if nodes_touch(&nodes[i], &nodes[j]) {
                    uf.union(i, j);
                }
            }
        }

        let mut comp_map: HashMap<usize, Vec<(&str, Pt, &str)>> = HashMap::new();
        for &pi in &pad_indices {
            let root = uf.find(pi);
            let ConnKind::Pad { label, center, .. } = &nodes[pi].kind else {
                continue;
            };
            let layer = nodes[pi].layers.first().map(String::as_str).unwrap_or("");
            comp_map
                .entry(root)
                .or_default()
                .push((label, *center, layer));
        }
        if comp_map.len() <= 1 {
            continue;
        }

        let mut comps: Vec<Vec<(&str, Pt, &str)>> = comp_map.into_values().collect();
        for c in &mut comps {
            c.sort_by_key(|a| a.0);
        }
        comps.sort_by_key(|a| a[0].0);

        // Connect disconnected pad components via Prim's MST to emit exact open-net pad pairs.
        let k = comps.len();
        let mut in_tree = vec![false; k];
        in_tree[0] = true;
        for _ in 1..k {
            let mut best = (
                0usize,
                0usize,
                f64::INFINITY,
                "",
                Pt::default(),
                "",
                "",
                Pt::default(),
            );
            for (i, ci) in comps.iter().enumerate() {
                if !in_tree[i] {
                    continue;
                }
                for (j, cj) in comps.iter().enumerate() {
                    if in_tree[j] {
                        continue;
                    }
                    for &(la, pa, layer_a) in ci {
                        for &(lb, pb, _) in cj {
                            let d = pa.dist(pb);
                            if d < best.2 {
                                best = (i, j, d, la, pa, layer_a, lb, pb);
                            }
                        }
                    }
                }
            }
            in_tree[best.1] = true;
            out.push(Violation {
                kind: ViolationKind::OpenNet,
                severity: Severity::Error,
                layer: best.5.to_string(),
                pos_nm: best.4.midpoint(best.7),
                measured_nm: best.2.round() as Nm,
                required_nm: 0,
                item_a: best.3.to_string(),
                item_b: Some(best.6.to_string()),
                net_a: net_name.to_string(),
                net_b: net_name.to_string(),
                message: format!("open net {net_name} between {} and {}", best.3, best.6),
            });
        }
    }
    out
}
