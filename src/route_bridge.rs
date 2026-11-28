use std::collections::{HashMap, HashSet};

use viahop::Board as ViahopBoard;
use viahop::board::{
    FixedPlane, FixedSegment, FixedVia, NetInfo, NetRules, Pad, ResolvedKeepout, SpatialGrid,
};
use viahop::dsn::KeepoutKind as ViahopKeepoutKind;
use viahop::geometry::{BBox, Point, Shape};

use crate::geom::{local_to_board, pad_copper_layers, pad_to_world_shape, remap_layer_for_side};
use crate::ir::{BoardIr, KeepoutKind, LayerKind, Nm, PadKind, Pt, nm_to_um, um};
use crate::route_mask::collect_mask_guards;
use crate::shapes::ResolvedShape;

#[inline]
pub(crate) fn pt_to_viahop(p: Pt) -> Point {
    Point::new(nm_to_um(p.x), nm_to_um(p.y))
}

#[inline]
pub(crate) fn pt_from_viahop(p: Point) -> Pt {
    Pt::new(um(p.x), um(p.y))
}

pub(crate) fn shape_to_viahop(shape: &ResolvedShape) -> Shape {
    match shape {
        ResolvedShape::Circle { center, radius } => Shape::Circle {
            center: pt_to_viahop(*center),
            radius: nm_to_um(*radius),
        },
        ResolvedShape::Capsule { a, b, radius } => Shape::Capsule {
            a: pt_to_viahop(*a),
            b: pt_to_viahop(*b),
            radius: nm_to_um(*radius),
        },
        ResolvedShape::RoundRect { .. } => {
            Shape::polygon(shape.to_polygon(16).into_iter().map(pt_to_viahop).collect())
        }
        ResolvedShape::Poly { verts, .. } => {
            Shape::polygon(verts.iter().map(|&p| pt_to_viahop(p)).collect())
        }
    }
}

fn map_keepout_kind(kind: KeepoutKind) -> Option<ViahopKeepoutKind> {
    match (kind.blocks_tracks(), kind.blocks_vias()) {
        (true, true) => Some(ViahopKeepoutKind::All),
        (true, false) => Some(ViahopKeepoutKind::Wire),
        (false, true) => Some(ViahopKeepoutKind::Via),
        (false, false) => None,
    }
}

fn map_keepout_layers(
    layers: &[String],
    layer_name_to_idx: &HashMap<String, usize>,
) -> Option<Vec<usize>> {
    if layers.is_empty() || layers.iter().any(|l| l == "*.Cu") {
        return Some(Vec::new());
    }
    let mapped: Vec<usize> = layers
        .iter()
        .filter_map(|l| layer_name_to_idx.get(l).copied())
        .collect();
    (!mapped.is_empty()).then_some(mapped)
}

pub(crate) fn via_spanned_indices(
    layers: &(String, String),
    layer_name_to_idx: &HashMap<String, usize>,
    num_layers: usize,
) -> Vec<usize> {
    if let (Some(&a), Some(&b)) = (
        layer_name_to_idx.get(&layers.0),
        layer_name_to_idx.get(&layers.1),
    ) {
        return (a.min(b)..=a.max(b)).collect();
    }
    if layers.0 == "*.Cu"
        || layers.1 == "*.Cu"
        || (layers.0 == "F.Cu" && layers.1 == "B.Cu")
        || (layers.0 == "B.Cu" && layers.1 == "F.Cu")
    {
        return (0..num_layers).collect();
    }
    let mut out = Vec::new();
    if let Some(&a) = layer_name_to_idx.get(&layers.0) {
        out.push(a);
    }
    if let Some(&b) = layer_name_to_idx.get(&layers.1)
        && !out.contains(&b)
    {
        out.push(b);
    }
    if out.is_empty() {
        out.extend(0..num_layers);
    }
    out
}

fn build_net_rules(board: &BoardIr, class_name: &str, num_layers: usize) -> NetRules {
    let nc = board.net_classes.get(class_name);
    let width = nm_to_um(
        nc.map_or(board.rules.track_width_nm, |c| c.track_width_nm)
            .max(1),
    );
    let clearance = nm_to_um(
        nc.map_or(board.rules.clearance_nm, |c| c.clearance_nm)
            .max(0),
    );
    let clr_or = |v: Option<Nm>| match v {
        Some(ov) if class_name == "Default" => nm_to_um(ov.max(0)),
        Some(ov) => nm_to_um(ov.max(0)).max(clearance),
        None => clearance,
    };
    let via_dia = nm_to_um(nc.map_or(board.rules.via_dia_nm, |c| c.via_dia_nm).max(1));
    let via_drill = nm_to_um(
        nc.map_or(board.rules.via_drill_nm, |c| c.via_drill_nm)
            .max(1),
    );
    let via_padstack = format!(
        "Via[0-{}]_{:.0}:{:.0}_um",
        num_layers.saturating_sub(1),
        via_dia,
        via_drill
    );
    NetRules {
        width,
        clearance,
        smd_smd: clr_or(board.rules.smd_to_smd_nm),
        via_wire: clr_or(board.rules.via_to_track_nm),
        via_via: clr_or(board.rules.via_to_via_nm),
        via_smd: clr_or(board.rules.via_to_smd_nm),
        via_padstack,
        via_radius: via_dia * 0.5,
        via_drill,
        via_layers: (0..num_layers).collect(),
    }
}

fn push_keepout(
    out: &mut Vec<ResolvedKeepout>,
    kind: KeepoutKind,
    layers: &[String],
    pts: Vec<Point>,
    is_structure: bool,
    layer_name_to_idx: &HashMap<String, usize>,
) {
    if pts.len() < 3 {
        return;
    }
    let Some(vk) = map_keepout_kind(kind) else {
        return;
    };
    let Some(klayers) = map_keepout_layers(layers, layer_name_to_idx) else {
        return;
    };
    let shape = Shape::polygon(pts);
    out.push(ResolvedKeepout {
        kind: vk,
        is_structure,
        layers: klayers,
        bbox: shape.bbox(),
        shape,
    });
}

fn build_grid<T>(bounds: BBox, items: &[T], bbox_fn: impl Fn(&T) -> Option<BBox>) -> SpatialGrid {
    let mut grid = SpatialGrid::new(bounds, 1000.0);
    for (i, item) in items.iter().enumerate() {
        if let Some(bb) = bbox_fn(item) {
            grid.insert(i, bb);
        }
    }
    grid
}

/// Converts a `BoardIr` directly into an in-memory `viahop::Board` without DSN/SES serialization.
pub fn to_viahop_board(
    board: &BoardIr,
    ignored_nets: &HashSet<String>,
    via_at_smd: Option<bool>,
) -> Result<ViahopBoard, String> {
    let layers = board.effective_layers();
    let num_layers = layers.len();
    let layer_names: Vec<String> = layers.iter().map(|l| l.name.clone()).collect();
    let mut layer_name_to_idx = HashMap::with_capacity(num_layers);
    let mut signal_layers = Vec::new();
    for (i, l) in layers.iter().enumerate() {
        layer_name_to_idx.insert(l.name.clone(), i);
        if l.kind != LayerKind::Plane {
            signal_layers.push(i);
        }
    }
    if signal_layers.is_empty() {
        signal_layers.extend(0..num_layers);
    }

    let net_names = board.all_net_names();
    let mut net_name_to_id = HashMap::with_capacity(net_names.len());
    for (id, name) in net_names.iter().enumerate() {
        net_name_to_id.insert(name.clone(), id);
    }

    let is_net_ignored = |name: &str| board.is_net_ignored(name, ignored_nets);

    let mut pin_to_net: HashMap<(String, String), usize> = HashMap::new();
    for (net_name, net_def) in &board.nets {
        if let Some(&nid) = net_name_to_id.get(net_name) {
            for (refdes, pin) in &net_def.pins {
                pin_to_net.insert((refdes.clone(), pin.clone()), nid);
            }
        }
    }
    for pl in &board.placements {
        for (pin, net_name) in &pl.pin_nets {
            if let Some(&nid) = net_name_to_id.get(net_name) {
                pin_to_net.insert((pl.refdes.clone(), pin.clone()), nid);
            }
        }
    }

    let mut pads = Vec::new();
    let mut net_pad_ids = vec![Vec::new(); net_names.len()];
    let mut keepouts = Vec::new();

    for cutout in &board.cutouts {
        push_keepout(
            &mut keepouts,
            KeepoutKind::All,
            &[],
            cutout.iter().map(|&p| pt_to_viahop(p)).collect(),
            true,
            &layer_name_to_idx,
        );
    }
    for ko in &board.keepouts {
        push_keepout(
            &mut keepouts,
            ko.kind,
            &ko.layers,
            ko.poly.iter().map(|&p| pt_to_viahop(p)).collect(),
            false,
            &layer_name_to_idx,
        );
    }

    for pl in &board.placements {
        let Some(fp) = board.footprints.get(&pl.footprint) else {
            continue;
        };
        for ko in &fp.keepouts {
            let remapped: Vec<String> = ko
                .layers
                .iter()
                .map(|l| remap_layer_for_side(l, pl.side))
                .collect();
            let pts: Vec<Point> = ko
                .poly
                .iter()
                .map(|&p| pt_to_viahop(local_to_board(p, pl.pos.x, pl.pos.y, pl.rot_deg, pl.side)))
                .collect();
            push_keepout(
                &mut keepouts,
                ko.kind,
                &remapped,
                pts,
                false,
                &layer_name_to_idx,
            );
        }

        for pad in &fp.pads {
            let center = pt_to_viahop(local_to_board(
                pad.pos, pl.pos.x, pl.pos.y, pl.rot_deg, pl.side,
            ));
            let world_shape = pad_to_world_shape(pad, pl, 16);
            let viahop_shape = shape_to_viahop(&world_shape);
            let mapped_layers: Vec<usize> = pad_copper_layers(pad, pl.side, &layer_names)
                .into_iter()
                .filter_map(|l| layer_name_to_idx.get(&l).copied())
                .collect();

            let mut layer_shapes = Vec::new();
            let mut bbox: Option<BBox> = None;
            if mapped_layers.is_empty() && pad.kind == PadKind::Npth {
                if let Some(drill) = pad.drill_nm
                    && drill > 0
                {
                    let hole = Shape::Circle {
                        center,
                        radius: nm_to_um(drill) * 0.5,
                    };
                    bbox = Some(hole.bbox());
                    for l in 0..num_layers {
                        layer_shapes.push((l, hole.clone()));
                    }
                }
            } else {
                let sb = viahop_shape.bbox();
                bbox = Some(sb);
                for l in mapped_layers {
                    layer_shapes.push((l, viahop_shape.clone()));
                }
            }
            if layer_shapes.is_empty() {
                continue;
            }
            let unique_layers: HashSet<usize> = layer_shapes.iter().map(|(l, _)| *l).collect();
            let is_smd = pad.kind == PadKind::Smd && unique_layers.len() == 1;
            let is_through_hole =
                pad.kind != PadKind::Smd || unique_layers.len() >= num_layers.min(2);
            let bbox = bbox.unwrap();
            let max_radius = ((bbox.max_x - bbox.min_x).hypot(bbox.max_y - bbox.min_y)) * 0.5;
            let net_id = if pad.kind == PadKind::Npth {
                None
            } else {
                pin_to_net
                    .get(&(pl.refdes.clone(), pad.name.clone()))
                    .copied()
            };
            let id = pads.len();
            if let Some(nid) = net_id {
                net_pad_ids[nid].push(id);
            }
            pads.push(Pad {
                id,
                refdes: pl.refdes.clone(),
                pin_name: pad.name.clone(),
                center,
                net_id,
                layer_shapes,
                bbox,
                is_smd,
                is_through_hole,
                max_radius,
            });
        }
    }

    // Treat locked, no-net, and ignored-net copper as fixed obstacles so routed nets never short them.
    let mut fixed_segments = Vec::new();
    for (idx, t) in board.tracks.iter().enumerate() {
        if !t.locked && !t.net.is_empty() && !is_net_ignored(&t.net) {
            continue;
        }
        let Some(&layer) = layer_name_to_idx.get(&t.layer) else {
            continue;
        };
        let a = pt_to_viahop(t.a);
        let b = pt_to_viahop(t.b);
        let half_w = nm_to_um(t.width) * 0.5;
        fixed_segments.push(FixedSegment {
            layer,
            a,
            b,
            half_w,
            net_id: net_name_to_id.get(&t.net).copied(),
            bbox: BBox::from_points(&[a, b], half_w),
            is_synthetic: false,
            wire_group: Some(idx),
        });
    }

    let mut fixed_vias = Vec::new();
    for v in &board.vias {
        if !v.locked && !v.net.is_empty() && !is_net_ignored(&v.net) {
            continue;
        }
        let pos = pt_to_viahop(v.pos);
        let radius = nm_to_um(v.diameter) * 0.5;
        let drill = nm_to_um(v.drill);
        let vlayers = via_spanned_indices(&v.layers, &layer_name_to_idx, num_layers);
        fixed_vias.push(FixedVia {
            pos,
            radius,
            drill,
            layers: vlayers,
            net_id: net_name_to_id.get(&v.net).copied(),
            bbox: BBox::from_point(pos, radius),
        });
    }

    let mut nets = Vec::with_capacity(net_names.len());
    for (id, name) in net_names.into_iter().enumerate() {
        let net_def = board.nets.get(&name);
        let class_name = net_def
            .map(|d| d.class.clone())
            .unwrap_or_else(|| "Default".into());
        let rules = build_net_rules(board, &class_name, num_layers);
        let ignored = is_net_ignored(&name);
        nets.push(NetInfo {
            id,
            name,
            class_name,
            pad_ids: std::mem::take(&mut net_pad_ids[id]),
            rules,
            ignored,
        });
    }

    // Only dedicated plane layers map to FixedPlane so signal-layer dynamic pours don't block routing.
    let mut planes = Vec::new();
    let make_plane = |layer_name: &str, net: &str, pts: &[Pt]| -> Option<FixedPlane> {
        let &layer = layer_name_to_idx.get(layer_name)?;
        if pts.len() < 3 || layers[layer].kind != LayerKind::Plane {
            return None;
        }
        let shape = Shape::polygon(pts.iter().map(|&p| pt_to_viahop(p)).collect());
        Some(FixedPlane {
            layer,
            net_id: net_name_to_id.get(net).copied(),
            bbox: shape.bbox(),
            shape,
        })
    };
    for pour in &board.pours {
        if let Some(fp) = make_plane(&pour.layer, &pour.net, &pour.outline) {
            planes.push(fp);
        }
    }
    if planes.is_empty() {
        for fp in &board.filled_pours {
            for poly in &fp.polys {
                if let Some(p) = make_plane(&fp.layer, &fp.net, &poly.outer) {
                    planes.push(p);
                }
            }
        }
    }
    let (mask_planes, mask_keepouts) = collect_mask_guards(
        board,
        &layer_names,
        &signal_layers,
        &net_name_to_id,
        &pin_to_net,
        &nets,
    );
    planes.extend(mask_planes);
    keepouts.extend(mask_keepouts);

    let boundary: Vec<Point> = board.outline.iter().map(|&p| pt_to_viahop(p)).collect();
    let mut bounds = if boundary.len() >= 3 {
        BBox::from_points(&boundary, 0.0)
    } else if !pads.is_empty() {
        let mut b = pads[0].bbox;
        for p in &pads[1..] {
            b = b.union(p.bbox);
        }
        b.expand(2000.0)
    } else {
        BBox::new(0.0, 0.0, 10_000.0, 10_000.0)
    };
    for p in &pads {
        bounds = bounds.union(p.bbox);
    }
    for pl in &planes {
        if signal_layers.contains(&pl.layer) {
            bounds = bounds.union(pl.bbox);
        }
    }
    for ko in &keepouts {
        bounds = bounds.union(ko.bbox);
    }

    let pad_index = build_grid(bounds, &pads, |p| Some(p.bbox));
    let keepout_index = build_grid(bounds, &keepouts, |k| Some(k.bbox));
    let fixed_seg_index = build_grid(bounds, &fixed_segments, |s| Some(s.bbox));
    let fixed_via_index = build_grid(bounds, &fixed_vias, |v| Some(v.bbox));
    let fixed_plane_index = build_grid(bounds, &planes, |pl| {
        signal_layers.contains(&pl.layer).then_some(pl.bbox)
    });

    let hole_to_hole_um = nm_to_um(board.rules.hole_to_hole_nm.max(0));
    let edge_clearance_um = Some(nm_to_um(board.rules.edge_clearance_nm.max(0)));
    let max_net_clearance = nets.iter().fold(100.0_f64, |acc, n| {
        let r = &n.rules;
        acc.max(r.clearance)
            .max(r.via_wire)
            .max(r.via_via)
            .max(r.via_smd)
    });

    Ok(ViahopBoard {
        layer_names,
        signal_layers,
        layer_name_to_idx,
        boundary,
        bounds,
        pads,
        keepouts,
        fixed_segments,
        fixed_vias,
        planes,
        nets,
        net_name_to_id,
        via_at_smd: via_at_smd.unwrap_or(board.rules.via_at_smd),
        hole_to_hole_um,
        edge_clearance_um,
        max_net_clearance,
        pad_index,
        keepout_index,
        fixed_seg_index,
        fixed_via_index,
        fixed_plane_index,
    })
}
