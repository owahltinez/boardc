use std::collections::{BTreeMap, HashMap};

use crate::drc::{Severity, Violation, ViolationKind};
use crate::drc_scene::{PadItem, PourItem, TrackItem, ViaItem, all_cu_mask};
use crate::geom::{GridQueryScratch, SpatialGrid, UnionFind};
use crate::ir::{BBoxNm, BoardIr, Nm, PadKind, Pt};
use crate::poly_index::IndexedPour;
use crate::shapes::ResolvedShape;

const TOUCH_TOL_NM: f64 = 1.0;

struct ConnNode<'a> {
    layers: Vec<String>,
    layer_mask: u64,
    bbox: BBoxNm,
    kind: ConnKind<'a>,
}

enum ConnKind<'a> {
    Pad {
        label: &'a str,
        layer: &'a str,
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

fn nodes_touch(a: &ConnNode<'_>, b: &ConnNode<'_>, use_mask: bool) -> bool {
    if !a.bbox.expand(2).intersects(b.bbox) {
        return false;
    }
    if use_mask {
        if (a.layer_mask & b.layer_mask) == 0 {
            return false;
        }
    } else if !a.layers.iter().any(|l| b.layers.contains(l)) {
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
    let use_mask = cu_layers.len() <= 64;
    let full_cu_mask = all_cu_mask(cu_layers);
    let ignored =
        |net: &str| net.is_empty() || board.nets.get(net).is_some_and(|nd| nd.ignore_route);
    let mut nodes_by_net: BTreeMap<&str, Vec<ConnNode<'_>>> = BTreeMap::new();

    for pad in pads {
        if pad.kind == PadKind::Npth || pad.layers.is_empty() || ignored(&pad.net) {
            continue;
        }
        let entry = nodes_by_net.entry(pad.net.as_str()).or_default();
        entry.push(ConnNode {
            layers: if use_mask {
                Vec::new()
            } else {
                pad.layers.clone()
            },
            layer_mask: pad.layer_mask,
            bbox: pad.bbox,
            kind: ConnKind::Pad {
                label: &pad.label,
                layer: pad.layers.first().map_or("", String::as_str),
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
                layers: if use_mask {
                    Vec::new()
                } else {
                    cu_layers.to_vec()
                },
                layer_mask: full_cu_mask,
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
            layers: if use_mask {
                Vec::new()
            } else {
                vec![tr.layer.to_string()]
            },
            layer_mask: tr.layer_mask,
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
                layers: if use_mask {
                    Vec::new()
                } else {
                    board.vias[vi].spanned_layers(cu_layers, &board.layers)
                },
                layer_mask: via.layer_mask,
                bbox: via.bbox,
                kind: ConnKind::Shape(&via.shape),
            });
    }

    for pour in pours {
        if ignored(pour.net) {
            continue;
        }
        nodes_by_net.entry(pour.net).or_default().push(ConnNode {
            layers: if use_mask {
                Vec::new()
            } else {
                vec![pour.layer.to_string()]
            },
            layer_mask: pour.layer_mask,
            bbox: pour.bbox,
            kind: ConnKind::Pour(&pour.indexed),
        });
    }

    let mut out = Vec::new();
    let mut scratch = GridQueryScratch::default();
    let mut candidates = Vec::new();
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
        let grid = SpatialGrid::new(nodes.iter().map(|n| n.bbox), 2_000_000);
        for i in 0..nodes.len() {
            grid.query_with(nodes[i].bbox.expand(10), &mut scratch, &mut candidates);
            for &j in &candidates {
                if j <= i || uf.find(i) == uf.find(j) {
                    continue;
                }
                if nodes_touch(&nodes[i], &nodes[j], use_mask) {
                    uf.union(i, j);
                }
            }
        }

        let mut comp_map: HashMap<usize, Vec<(&str, Pt, &str)>> = HashMap::new();
        for &pi in &pad_indices {
            let root = uf.find(pi);
            let ConnKind::Pad {
                label,
                layer,
                center,
                ..
            } = &nodes[pi].kind
            else {
                continue;
            };
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

        // Connect disconnected pad components via O(K^2) Prim's MST with exact tie-breaking.
        let k = comps.len();
        fn best_pair<'a>(
            ci: &[(&'a str, Pt, &'a str)],
            cj: &[(&'a str, Pt, &'a str)],
        ) -> (f64, &'a str, Pt, &'a str, &'a str, Pt) {
            let mut best = (f64::INFINITY, "", Pt::default(), "", "", Pt::default());
            for &(la, pa, layer_a) in ci {
                for &(lb, pb, _) in cj {
                    let d = pa.dist(pb);
                    if d < best.0 {
                        best = (d, la, pa, layer_a, lb, pb);
                    }
                }
            }
            best
        }
        let mut in_tree = vec![false; k];
        in_tree[0] = true;
        let mut best_to_tree: Vec<(f64, usize, &str, Pt, &str, &str, Pt)> = (0..k)
            .map(|j| {
                if j == 0 {
                    (f64::INFINITY, 0, "", Pt::default(), "", "", Pt::default())
                } else {
                    let b = best_pair(&comps[0], &comps[j]);
                    (b.0, 0, b.1, b.2, b.3, b.4, b.5)
                }
            })
            .collect();
        for _ in 1..k {
            let mut pick = 0usize;
            for j in 1..k {
                if in_tree[j] {
                    continue;
                }
                if pick == 0
                    || best_to_tree[j].0 < best_to_tree[pick].0
                    || (best_to_tree[j].0 == best_to_tree[pick].0
                        && best_to_tree[j].1 < best_to_tree[pick].1)
                {
                    pick = j;
                }
            }
            in_tree[pick] = true;
            let best = best_to_tree[pick];
            out.push(Violation {
                kind: ViolationKind::OpenNet,
                severity: Severity::Error,
                layer: best.4.to_string(),
                pos_nm: best.3.midpoint(best.6),
                measured_nm: best.0.round() as Nm,
                required_nm: 0,
                item_a: best.2.to_string(),
                item_b: Some(best.5.to_string()),
                net_a: net_name.to_string(),
                net_b: net_name.to_string(),
                message: format!("open net {net_name} between {} and {}", best.2, best.5),
            });
            for m in 1..k {
                if in_tree[m] {
                    continue;
                }
                let cand = best_pair(&comps[pick], &comps[m]);
                if cand.0 < best_to_tree[m].0
                    || (cand.0 == best_to_tree[m].0 && pick < best_to_tree[m].1)
                {
                    best_to_tree[m] = (cand.0, pick, cand.1, cand.2, cand.3, cand.4, cand.5);
                }
            }
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::drc_scene::collect_pads;
    use crate::ir::{FootprintDef, PadDef, PadShape, Placement, Side};

    #[test]
    fn connectivity_mst_tie_breaking_is_deterministic() {
        let mut board: BoardIr = serde_json::from_str(
            r#"{"name":"t","outline":[],"layers":[],"footprints":{},"placements":[]}"#,
        )
        .unwrap();
        board.footprints.insert(
            "P".into(),
            FootprintDef {
                name: "P".into(),
                pads: vec![PadDef {
                    name: "1".into(),
                    kind: PadKind::Smd,
                    shape: PadShape::Circle { diameter: 400_000 },
                    pos: Pt::new(0, 0),
                    rot_deg: 0.0,
                    drill_nm: None,
                    layers: vec!["F.Cu".into()],
                    mask_margin_nm: None,
                    paste_margin_nm: None,
                }],
                courtyard: Vec::new(),
                graphics: Vec::new(),
                keepouts: Vec::new(),
                models: Vec::new(),
                pin_types: Default::default(),
                kicad_raw: None,
            },
        );
        let placements = [
            ("U1", Pt::new(0, 0)),
            ("U2", Pt::new(10_000_000, 0)),
            ("U3", Pt::new(0, 10_000_000)),
            ("U4", Pt::new(10_000_000, 10_000_000)),
        ];
        for (rd, pos) in placements {
            board.placements.push(Placement {
                refdes: rd.into(),
                footprint: "P".into(),
                pos,
                side: Side::Top,
                pin_nets: [("1".into(), "SIG".into())].into_iter().collect(),
                ..Default::default()
            });
        }
        let cu = board.copper_layer_names();
        let pads = collect_pads(&board, &cu);
        let v1 = check_connectivity(&board, &cu, &[], &pads, &[], &[]);
        assert_eq!(v1.len(), 3);
        let edges: Vec<(&str, Option<&str>)> = v1
            .iter()
            .map(|v| (v.item_a.as_str(), v.item_b.as_deref()))
            .collect();
        assert_eq!(
            edges,
            vec![
                ("U1.1", Some("U2.1")),
                ("U1.1", Some("U3.1")),
                ("U2.1", Some("U4.1")),
            ]
        );
    }
}
