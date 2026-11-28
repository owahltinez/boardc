use std::collections::HashMap;
use std::f64::consts::{FRAC_1_SQRT_2, PI, SQRT_2};

use clipper2_rust::{EndType, JoinType, inflate_paths_64};
use viahop::board::{FixedPlane, NetInfo, ResolvedKeepout};
use viahop::dsn::KeepoutKind as ViahopKeepoutKind;
use viahop::geometry::Shape;

use crate::fab_gerber::pad_world_shape_with_margin;
use crate::geom::{ensure_ccw, pad_world_layers, pts_to_path64};
use crate::ir::{BoardIr, Nm, PadDef, PadKind, PadShape, Placement, Pt, nm_to_um, um};
use crate::route_bridge::{pt_to_viahop, shape_to_viahop};
use crate::shapes::{ResolvedShape, roundrect_poly, transform_pad_local_pt};
use crate::zone_scene::{ARC_TOLERANCE, inflate_round_outer};

#[derive(Clone, Copy)]
struct Clearances {
    clr: Nm,
    via_wire: Nm,
    via_smd: Nm,
    via_via: Nm,
}

fn clearances_from_net(n: &NetInfo) -> Clearances {
    Clearances {
        clr: um(n.rules.clearance),
        via_wire: um(n.rules.via_wire),
        via_smd: um(n.rules.via_smd),
        via_via: um(n.rules.via_via),
    }
}

fn default_clearances(board: &BoardIr, nets: &[NetInfo]) -> Clearances {
    let mut iter = nets.iter().filter(|n| !n.ignored).map(clearances_from_net);
    if let Some(first) = iter.next() {
        return iter.fold(first, |acc, c| Clearances {
            clr: acc.clr.min(c.clr),
            via_wire: acc.via_wire.min(c.via_wire),
            via_smd: acc.via_smd.min(c.via_smd),
            via_via: acc.via_via.min(c.via_via),
        });
    }
    let clr = board.rules.clearance_nm.max(0);
    Clearances {
        clr,
        via_wire: board.rules.via_to_track_nm.map_or(clr, |v| v.max(0)),
        via_smd: board.rules.via_to_smd_nm.map_or(clr, |v| v.max(0)),
        via_via: board.rules.via_to_via_nm.map_or(clr, |v| v.max(0)),
    }
}

fn offset_mask_shapes(
    pad: &PadDef,
    pl: &Placement,
    mask_shape: &ResolvedShape,
    margin_nm: Nm,
    offset_nm: Nm,
) -> Vec<Shape> {
    if offset_nm == 0 {
        return vec![shape_to_viahop(mask_shape)];
    }
    match mask_shape {
        ResolvedShape::Circle { center, radius } => vec![Shape::Circle {
            center: pt_to_viahop(*center),
            radius: nm_to_um((*radius + offset_nm).max(0)),
        }],
        ResolvedShape::Capsule { a, b, radius } => vec![Shape::Capsule {
            a: pt_to_viahop(*a),
            b: pt_to_viahop(*b),
            radius: nm_to_um((*radius + offset_nm).max(0)),
        }],
        ResolvedShape::RoundRect { .. } | ResolvedShape::Poly { .. } => match pad.shape {
            PadShape::Rect { width, height } | PadShape::RoundRect { width, height, .. } => {
                let wm = width + 2 * margin_nm;
                let hm = height + 2 * margin_nm;
                let rm = match pad.shape {
                    PadShape::RoundRect { corner_radius, .. } => {
                        (corner_radius + margin_nm).clamp(0, wm.min(hm) / 2)
                    }
                    _ => 0,
                };
                let (w, h, r) = if offset_nm > 0 {
                    // Circumscribe corner arcs by 1/cos(pi/(4*16)) so chord midpoints never dip inside m2c.
                    let r_exact = rm + offset_nm;
                    let r_circ = (r_exact as f64 / (PI / 64.0).cos()).ceil() as Nm + 1;
                    let pad_out = r_circ - r_exact;
                    (
                        wm + 2 * (offset_nm + pad_out),
                        hm + 2 * (offset_nm + pad_out),
                        r_circ,
                    )
                } else {
                    let shrink_nm = -offset_nm;
                    if rm >= shrink_nm {
                        (
                            (wm - 2 * shrink_nm).max(1),
                            (hm - 2 * shrink_nm).max(1),
                            rm - shrink_nm,
                        )
                    } else {
                        // Use inset/sqrt(2) so 45-degree corners on sharp rects stay protected.
                        let inset_nm = (shrink_nm as f64 * FRAC_1_SQRT_2).floor() as Nm;
                        (
                            (wm - 2 * inset_nm).max(1),
                            (hm - 2 * inset_nm).max(1),
                            (rm - inset_nm).max(0),
                        )
                    }
                };
                let mut world: Vec<Pt> = roundrect_poly(w, h, r, 16)
                    .into_iter()
                    .map(|p| transform_pad_local_pt(p, pad, pl))
                    .collect();
                ensure_ccw(&mut world);
                vec![Shape::polygon(
                    world.into_iter().map(pt_to_viahop).collect(),
                )]
            }
            _ => {
                let poly = mask_shape.to_polygon(16);
                let paths = vec![pts_to_path64(&poly)];
                let out = if offset_nm > 0 {
                    inflate_round_outer(&paths, offset_nm)
                } else if margin_nm > 0 {
                    let d = -((-offset_nm).min(margin_nm) as f64);
                    inflate_paths_64(
                        &paths,
                        d,
                        JoinType::Round,
                        EndType::Polygon,
                        2.0,
                        ARC_TOLERANCE,
                    )
                } else {
                    return vec![shape_to_viahop(mask_shape)];
                };
                let shapes: Vec<Shape> = out
                    .iter()
                    .filter(|p| p.len() >= 3)
                    .map(|p| {
                        Shape::polygon(
                            p.iter()
                                .map(|pt| pt_to_viahop(Pt::new(pt.x, pt.y)))
                                .collect(),
                        )
                    })
                    .collect();
                if shapes.is_empty() && offset_nm < 0 {
                    vec![shape_to_viahop(mask_shape)]
                } else {
                    shapes
                }
            }
        },
    }
}

/// Collects outer-layer mask-to-copper guard `FixedPlane`s and `ResolvedKeepout`s for pads and untented fixed vias.
pub(crate) fn collect_mask_guards(
    board: &BoardIr,
    layer_names: &[String],
    signal_layers: &[usize],
    net_name_to_id: &HashMap<String, usize>,
    pin_to_net: &HashMap<(String, String), usize>,
    nets: &[NetInfo],
) -> (Vec<FixedPlane>, Vec<ResolvedKeepout>) {
    let mut planes = Vec::new();
    let mut keepouts = Vec::new();
    let Some(m2c_nm) = board.rules.mask_to_copper_nm.filter(|&v| v >= 0) else {
        return (planes, keepouts);
    };
    let outer_sides: Vec<(usize, &str, &str)> = [("F.Cu", "F.Mask"), ("B.Cu", "B.Mask")]
        .into_iter()
        .filter_map(|(cu, mask)| {
            let idx = layer_names.iter().position(|n| n == cu)?;
            signal_layers.contains(&idx).then_some((idx, cu, mask))
        })
        .collect();
    if outer_sides.is_empty() {
        return (planes, keepouts);
    }

    let default_clr = default_clearances(board, nets);

    for pl in &board.placements {
        let Some(fp) = board.footprints.get(&pl.footprint) else {
            continue;
        };
        for pad in &fp.pads {
            let world_layers = pad_world_layers(pad, pl.side, layer_names);
            if !outer_sides
                .iter()
                .any(|&(_, _, mask)| world_layers.iter().any(|l| l == mask))
            {
                continue;
            }
            let margin_nm = pad.mask_margin_nm.unwrap_or(board.rules.mask_expansion_nm);
            let Some(mask_shape) = pad_world_shape_with_margin(pad, pl, margin_nm) else {
                continue;
            };
            let net_id = if pad.kind == PadKind::Npth {
                None
            } else {
                pin_to_net
                    .get(&(pl.refdes.clone(), pad.name.clone()))
                    .copied()
            };
            let clr = net_id.map_or(default_clr, |nid| clearances_from_net(&nets[nid]));
            let copper_layer_count = if pad.kind == PadKind::Npth {
                0
            } else {
                world_layers
                    .iter()
                    .filter(|l| layer_names.contains(l))
                    .count()
            };
            let is_smd = pad.kind == PadKind::Smd && copper_layer_count == 1;
            let via_copper_clr_nm = if is_smd { clr.via_smd } else { clr.via_via };
            let copper_min_clr = clr.clr.min(via_copper_clr_nm);
            let guard_clr_nm = clr.clr.min(clr.via_wire);
            let delta_nm = m2c_nm - guard_clr_nm;

            for &(cu_idx, cu_name, mask_name) in &outer_sides {
                if !world_layers.iter().any(|l| l == mask_name) {
                    continue;
                }
                let has_copper =
                    pad.kind != PadKind::Npth && world_layers.iter().any(|l| l == cu_name);
                if has_copper {
                    let eff_margin = if matches!(pad.shape, PadShape::Rect { .. }) && margin_nm > 0
                    {
                        (margin_nm as f64 * SQRT_2).ceil() as Nm
                    } else {
                        margin_nm
                    };
                    if eff_margin + m2c_nm <= copper_min_clr {
                        continue;
                    }
                }
                // No-net mask-only/NPTH pads with delta_nm < 0 use Keepout (5um viahop margin) to avoid phantom plane bloat.
                if net_id.is_none() && !has_copper && delta_nm < 0 {
                    let ko_offset = (m2c_nm - 5_000).max(0);
                    for shape in offset_mask_shapes(pad, pl, &mask_shape, margin_nm, ko_offset) {
                        keepouts.push(ResolvedKeepout {
                            kind: ViahopKeepoutKind::All,
                            is_structure: false,
                            layers: vec![cu_idx],
                            bbox: shape.bbox(),
                            shape,
                        });
                    }
                } else {
                    for shape in offset_mask_shapes(pad, pl, &mask_shape, margin_nm, delta_nm) {
                        planes.push(FixedPlane {
                            layer: cu_idx,
                            net_id,
                            bbox: shape.bbox(),
                            shape,
                        });
                    }
                }
            }
        }
    }

    for v in &board.vias {
        let net_id = net_name_to_id.get(&v.net).copied();
        if v.tented || (!v.locked && net_id.is_some_and(|nid| !nets[nid].ignored)) {
            continue;
        }
        let mask_dia = v.diameter + 2 * board.rules.mask_expansion_nm;
        if mask_dia <= 0 {
            continue;
        }
        let clr = net_id.map_or(default_clr, |nid| clearances_from_net(&nets[nid]));
        if board.rules.mask_expansion_nm + m2c_nm <= clr.via_wire.min(clr.via_via) {
            continue;
        }
        let delta_nm = m2c_nm - clr.clr.min(clr.via_wire);
        let guard_r_um = (nm_to_um(mask_dia) * 0.5 + nm_to_um(delta_nm)).max(0.0);
        let shape = Shape::Circle {
            center: pt_to_viahop(v.pos),
            radius: guard_r_um,
        };
        let bbox = shape.bbox();
        for &(cu_idx, cu_name, _) in &outer_sides {
            if v.touches_layer(cu_name, &board.layers) {
                planes.push(FixedPlane {
                    layer: cu_idx,
                    net_id,
                    bbox,
                    shape: shape.clone(),
                });
            }
        }
    }

    (planes, keepouts)
}
