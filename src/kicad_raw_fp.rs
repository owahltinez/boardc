use std::collections::{BTreeMap, HashMap};
use std::fmt::Write as _;

use viahop::sexp::SExp;

use crate::geom::sin_cos_deg;
use crate::ir::{
    AutoRefdes, BoardIr, FootprintDef, PadKind, Placement, SchematicMeta, Side, nm_to_mm,
};
use crate::kicad_export_items::{UuidGen, esc, remap_kicad_layer, write_footprint_body};
use crate::kicad_geom::{atom_at, f64_at, is_exportable_kicad_field, parse_kicad_sexp};
use crate::kicad_io::parse_footprint_node;

struct RawCtx<'a> {
    bx_mm: f64,
    by_mm: f64,
    fp_rot: f64,
    needs_flip: bool,
    remap_side: Side,
    zone_xf: Option<(f64, f64)>,
    uuids: &'a mut UuidGen,
    uuid_map: HashMap<String, String>,
}

impl RawCtx<'_> {
    fn replace_uuid(&mut self, node: &mut SExp) {
        let new_u = self.uuids.next();
        let SExp::List(items) = node else { return };
        if let Some(SExp::Atom(old)) = items.get_mut(1) {
            if !old.is_empty() {
                self.uuid_map.insert(old.clone(), new_u.clone());
            }
            *old = new_u;
        } else {
            *node = SExp::List(vec![SExp::Atom("uuid".into()), SExp::Atom(new_u)]);
        }
    }

    fn ensure_uuid(&mut self, node: &mut SExp) {
        if let SExp::List(items) = node
            && !items.iter().any(|it| it.tag() == Some("uuid"))
        {
            items.push(SExp::List(vec![
                SExp::Atom("uuid".into()),
                SExp::Atom(self.uuids.next()),
            ]));
        }
    }
}

fn format_rot(deg: f64) -> String {
    let mut norm = deg.rem_euclid(360.0);
    if (norm - 360.0).abs() < 1e-6 || norm.abs() < 1e-6 {
        norm = 0.0;
    }
    let s = format!("{norm:.4}");
    s.trim_end_matches('0').trim_end_matches('.').to_string()
}

fn format_mm(v: f64) -> String {
    let s = format!("{:.6}", if v.abs() < 5e-7 { 0.0 } else { v });
    let trimmed = s.trim_end_matches('0').trim_end_matches('.');
    if trimmed == "-0" || trimmed.is_empty() {
        "0".into()
    } else {
        trimmed.to_string()
    }
}

fn negate_num_atom(raw: &str) -> String {
    let Ok(v) = raw.parse::<f64>() else {
        return raw.to_string();
    };
    if v == 0.0 {
        return "0".into();
    }
    let trimmed = raw.strip_prefix('+').unwrap_or(raw);
    match trimmed.strip_prefix('-') {
        Some(rest) => rest.to_string(),
        None => format!("-{trimmed}"),
    }
}

fn transform_at(node: &mut SExp, fp_rot: f64, needs_flip: bool) {
    let SExp::List(items) = node else { return };
    if items.len() < 3 {
        return;
    }
    if needs_flip && let SExp::Atom(y) = &mut items[2] {
        *y = negate_num_atom(y);
    }
    let lrot = items.get(3).and_then(SExp::as_f64).unwrap_or(0.0);
    let has_unlocked = items[3..].iter().any(|x| x.as_atom() == Some("unlocked"));
    let abs_rot = if needs_flip {
        fp_rot - lrot
    } else {
        fp_rot + lrot
    };
    items.truncate(3);
    items.push(SExp::Atom(format_rot(abs_rot)));
    if has_unlocked {
        items.push(SExp::Atom("unlocked".into()));
    }
}

fn mirror_effects(node: &mut SExp) {
    let SExp::List(items) = node else { return };
    let (mut saw_justify, mut drop_empty) = (false, false);
    for item in items
        .iter_mut()
        .skip(1)
        .filter(|i| i.tag() == Some("justify"))
    {
        saw_justify = true;
        if let SExp::List(j) = item {
            let orig_len = j.len();
            j.retain(|t| t.as_atom() != Some("mirror"));
            if j.len() == orig_len {
                j.push(SExp::Atom("mirror".into()));
            } else if j.len() <= 1 {
                drop_empty = true;
            }
        }
    }
    if drop_empty {
        items.retain(|item| item.tag() != Some("justify"));
    } else if !saw_justify {
        items.push(SExp::List(vec![
            SExp::Atom("justify".into()),
            SExp::Atom("mirror".into()),
        ]));
    }
}

pub(crate) fn fp_text_val_idx(items: &[SExp]) -> usize {
    if items.first().and_then(SExp::as_atom) == Some("fp_text")
        && items.get(2).and_then(SExp::as_atom) == Some("locked")
        && items.len() > 3
    {
        3
    } else {
        2
    }
}

fn is_hide(node: &SExp) -> bool {
    node.as_atom() == Some("hide") || node.tag() == Some("hide")
}

fn hide_text_item(node: &mut SExp) {
    let SExp::List(items) = node else { return };
    let prefix = (fp_text_val_idx(items) + 1).min(items.len());
    let mut idx = 0usize;
    items.retain_mut(|item| {
        let i = idx;
        idx += 1;
        if i < prefix {
            return true;
        }
        if is_hide(item) {
            return false;
        }
        if item.tag() == Some("effects")
            && let SExp::List(eff) = item
        {
            eff.retain(|it| !is_hide(it));
        }
        true
    });
    let pos = items[prefix..]
        .iter()
        .position(|it| matches!(it.tag(), Some("uuid") | Some("effects")))
        .map_or(items.len(), |p| prefix + p);
    items.insert(
        pos,
        SExp::List(vec![SExp::Atom("hide".into()), SExp::Atom("yes".into())]),
    );
}

fn transform_generic(node: &mut SExp, ctx: &mut RawCtx<'_>) {
    let SExp::List(items) = node else { return };
    let Some(tag) = items.first().and_then(SExp::as_atom) else {
        return;
    };
    match tag {
        "uuid" => return ctx.replace_uuid(node),
        "gr_text" => return transform_text_item(node, None, ctx),
        "layer" | "layers" => {
            let single = tag == "layer";
            let end = if single {
                2.min(items.len())
            } else {
                items.len()
            };
            for item in &mut items[1..end] {
                if let SExp::Atom(l) = item {
                    *l = remap_kicad_layer(l, ctx.remap_side);
                }
            }
            if single {
                for item in items.iter_mut().skip(2) {
                    transform_generic(item, ctx);
                }
            }
            return;
        }
        "chamfer" if ctx.needs_flip => {
            for item in items.iter_mut().skip(1) {
                if let SExp::Atom(s) = item {
                    *s = match s.as_str() {
                        "top_left" => "bottom_left".into(),
                        "bottom_left" => "top_left".into(),
                        "top_right" => "bottom_right".into(),
                        "bottom_right" => "top_right".into(),
                        _ => continue,
                    };
                }
            }
            return;
        }
        "height" if ctx.needs_flip => {
            if let Some(SExp::Atom(h)) = items.get_mut(1) {
                *h = negate_num_atom(h);
            }
            return;
        }
        "xy" | "start" | "mid" | "end" if ctx.zone_xf.is_some() => {
            if let (Some(lx), Some(raw_ly), Some((sr, cr))) = (
                items.get(1).and_then(SExp::as_f64),
                items.get(2).and_then(SExp::as_f64),
                ctx.zone_xf,
            ) {
                let ly = if ctx.needs_flip { -raw_ly } else { raw_ly };
                items[1] = SExp::Atom(format_mm(ctx.bx_mm + lx * cr + ly * sr));
                items[2] = SExp::Atom(format_mm(ctx.by_mm - lx * sr + ly * cr));
                return;
            }
        }
        _ => {}
    }

    let flip_y = ctx.needs_flip
        && (matches!(
            tag,
            "start" | "end" | "mid" | "center" | "xy" | "at" | "rect_delta"
        ) || (tag == "offset" && items.get(1).and_then(SExp::as_atom).is_some()));
    if flip_y && let Some(SExp::Atom(y)) = items.get_mut(2) {
        *y = negate_num_atom(y);
    }
    for item in items.iter_mut().skip(1) {
        transform_generic(item, ctx);
    }
}

fn transform_text_item(node: &mut SExp, val_override: Option<&str>, ctx: &mut RawCtx<'_>) {
    let SExp::List(items) = node else { return };
    let val_idx = fp_text_val_idx(items);
    if let Some(v) = val_override
        && let Some(slot) = items.get_mut(val_idx)
    {
        *slot = SExp::Atom(v.to_string());
    }
    let mut has_effects = false;
    for item in items.iter_mut().skip(1) {
        match item.tag() {
            Some("at") => transform_at(item, ctx.fp_rot, ctx.needs_flip),
            Some("angle") => {
                if let Some(lrot) = f64_at(item, 1)
                    && let SExp::List(a) = item
                {
                    let abs_rot = if ctx.needs_flip {
                        ctx.fp_rot - lrot
                    } else {
                        ctx.fp_rot + lrot
                    };
                    a[1] = SExp::Atom(format_rot(abs_rot));
                }
            }
            Some("effects") => {
                has_effects = true;
                if ctx.needs_flip {
                    mirror_effects(item);
                }
            }
            Some("uuid") => ctx.replace_uuid(item),
            _ => transform_generic(item, ctx),
        }
    }
    if !has_effects && ctx.needs_flip {
        items.push(SExp::List(vec![
            SExp::Atom("effects".into()),
            SExp::List(vec![
                SExp::Atom("justify".into()),
                SExp::Atom("mirror".into()),
            ]),
        ]));
    }
    ctx.ensure_uuid(node);
}

fn transform_pad(
    node: &mut SExp,
    pl: &Placement,
    net_ids: &BTreeMap<String, usize>,
    ctx: &mut RawCtx<'_>,
) {
    let pad_name = atom_at(node, 1).unwrap_or("").to_string();
    let pad_kind = atom_at(node, 2).unwrap_or("").to_string();
    let SExp::List(items) = node else { return };
    items.retain(|it| it.tag() != Some("net"));
    for item in items.iter_mut().skip(1) {
        if item.tag() == Some("at") {
            transform_at(item, ctx.fp_rot, ctx.needs_flip);
        } else {
            transform_generic(item, ctx);
        }
    }
    if pad_kind != "np_thru_hole"
        && let Some(net_name) = pl.pin_nets.get(&pad_name)
        && let Some(&net_id) = net_ids.get(net_name)
    {
        items.push(SExp::List(vec![
            SExp::Atom("net".into()),
            SExp::Atom(net_id.to_string()),
            SExp::Atom(net_name.clone()),
        ]));
    }
    ctx.ensure_uuid(node);
}

fn is_quoted_arg(parent: &[SExp], parent_tag: &str, idx: usize) -> bool {
    match parent_tag {
        // Footprint `(property "Key" "Val" ...)` has >= 3 items; pad `(property pad_prop_*)` has 2 unquoted items.
        "property" => parent.len() >= 3 && (idx == 1 || idx == 2),
        "fp_text" => idx == fp_text_val_idx(parent),
        "net" => idx == 2,
        "layers" | "members" | "private_layers" | "net_tie_pad_groups" => idx >= 1,
        "gr_text" | "fp_text_box" | "override_value" | "face" | "layer" | "uuid" | "descr"
        | "tags" | "model" | "pad" | "name" | "net_name" | "group" | "pinfunction" | "pintype"
        | "prefix" | "suffix" => idx == 1,
        _ => false,
    }
}

fn write_atom(out: &mut String, s: &str, force_quote: bool) {
    let safe_bare = !s.is_empty()
        && s.bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'_' | b'.' | b'+' | b'-'));
    if !force_quote && safe_bare {
        out.push_str(s);
    } else {
        out.push('"');
        out.push_str(&esc(s));
        out.push('"');
    }
}

fn write_sexp_inline(out: &mut String, node: &SExp, force_quote: bool) {
    match node {
        SExp::Atom(s) => write_atom(out, s, force_quote),
        SExp::List(items) => {
            out.push('(');
            let tag = node.tag().unwrap_or("");
            for (i, item) in items.iter().enumerate() {
                if i > 0 {
                    out.push(' ');
                }
                write_sexp_inline(out, item, i > 0 && is_quoted_arg(items, tag, i));
            }
            out.push(')');
        }
    }
}

fn write_top_child(out: &mut String, node: &SExp) {
    let Some(items) = node.as_list() else { return };
    let tag = node.tag().unwrap_or("");
    if !matches!(tag, "pad" | "zone" | "model" | "fp_poly") {
        out.push_str("\t\t");
        write_sexp_inline(out, node, false);
        out.push('\n');
        return;
    }
    let split = items
        .iter()
        .position(|it| matches!(it, SExp::List(_)))
        .unwrap_or(items.len());
    out.push_str("\t\t(");
    for (i, item) in items[..split].iter().enumerate() {
        if i > 0 {
            out.push(' ');
        }
        write_sexp_inline(out, item, i > 0 && is_quoted_arg(items, tag, i));
    }
    out.push('\n');
    for (i, item) in items[split..].iter().enumerate() {
        out.push_str("\t\t\t");
        let idx = split + i;
        write_sexp_inline(out, item, idx > 0 && is_quoted_arg(items, tag, idx));
        out.push('\n');
    }
    out.push_str("\t\t)\n");
}

fn write_fp_header(out: &mut String, fp_id: &str, pl: &Placement, rot: f64, uuids: &mut UuidGen) {
    let _ = writeln!(out, "\t(footprint \"{}\"", esc(fp_id));
    let _ = writeln!(out, "\t\t(layer \"{}\")", pl.side.cu_layer());
    let _ = writeln!(out, "\t\t(uuid \"{}\")", uuids.next());
    let _ = writeln!(
        out,
        "\t\t(at {:.6} {:.6} {})",
        nm_to_mm(pl.pos.x),
        nm_to_mm(-pl.pos.y),
        format_rot(rot)
    );
    if pl.locked {
        out.push_str("\t\t(locked yes)\n");
    }
}

fn write_fp_property(out: &mut String, key: &str, val: &str, mirror: bool, uuids: &mut UuidGen) {
    let fab_layer = if mirror { "B.Fab" } else { "F.Fab" };
    let just = if mirror { " (justify mirror)" } else { "" };
    let _ = writeln!(
        out,
        "\t\t(property \"{}\" \"{}\" (at 0 0 0) (layer \"{fab_layer}\") (hide yes) (uuid \"{}\") (effects (font (size 1 1) (thickness 0.15)){just}))",
        esc(key),
        esc(val),
        uuids.next()
    );
}

fn write_schematic_paths(out: &mut String, sch: &SchematicMeta) {
    for (tag, val) in [
        ("path", sch.path.as_deref()),
        ("sheetname", sch.sheetname.as_deref()),
        ("sheetfile", sch.sheetfile.as_deref()),
    ] {
        if let Some(v) = val.map(str::trim).filter(|s| !s.is_empty()) {
            let _ = writeln!(out, "\t\t({tag} \"{}\")", esc(v));
        }
    }
}

fn write_fp_attr(
    out: &mut String,
    raw_attr: Option<Vec<String>>,
    fp: &FootprintDef,
    pl: &Placement,
) {
    let has_sch_path = pl
        .schematic
        .as_ref()
        .and_then(|s| s.path.as_deref())
        .is_some_and(|p| !p.trim().is_empty());
    let mut toks: Vec<String> = match raw_attr {
        Some(mut raw) => {
            if has_sch_path {
                raw.retain(|t| t != "board_only");
            }
            raw
        }
        None => {
            // Schematic-linked padless/NPTH footprints (e.g. MountingHole) must omit `board_only` for parity.
            if fp.pads.iter().any(|p| p.kind == PadKind::Tht) {
                vec!["through_hole".into()]
            } else if fp.pads.iter().any(|p| p.kind == PadKind::Smd) {
                vec!["smd".into()]
            } else if !has_sch_path {
                vec!["board_only".into()]
            } else {
                Vec::new()
            }
        }
    };
    for (flag, name) in [
        (pl.exclude_from_pos, "exclude_from_pos_files"),
        (pl.exclude_from_bom, "exclude_from_bom"),
        (pl.dnp, "dnp"),
    ] {
        if flag && !toks.iter().any(|t| t == name) {
            toks.push(name.to_string());
        }
    }
    if !toks.is_empty() {
        let _ = writeln!(out, "\t\t(attr {})", toks.join(" "));
    }
}

fn raw_matches_fp(root: &SExp, fp: &FootprintDef) -> bool {
    let Ok((parsed, _)) = parse_footprint_node(root, false, &HashMap::new()) else {
        return false;
    };
    parsed.pads == fp.pads
        && parsed.courtyard == fp.courtyard
        && parsed.graphics == fp.graphics
        && parsed.keepouts == fp.keepouts
        && parsed.models == fp.models
}

fn prepare_raw_footprint(fp: &FootprintDef) -> Option<(Vec<SExp>, bool)> {
    let raw = fp.kicad_raw.as_deref()?;
    let root = parse_kicad_sexp(raw).ok()?;
    if root.tag() != Some("footprint") || !raw_matches_fp(&root, fp) {
        return None;
    }
    let raw_is_bottom = root.find_child("layer").and_then(|n| atom_at(n, 1)) == Some("B.Cu");
    let SExp::List(mut items) = root else {
        return None;
    };
    let start_idx = usize::from(items.get(1).and_then(SExp::as_atom).is_some()) + 1;
    items.drain(..start_idx);
    Some((items, raw_is_bottom))
}

fn placement_fp_id<'a>(pl: &'a Placement, fp: &'a FootprintDef) -> &'a str {
    pl.schematic
        .as_ref()
        .and_then(|s| s.footprint.as_deref())
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .unwrap_or(&fp.name)
}

#[allow(clippy::too_many_arguments)]
fn write_raw_footprint(
    out: &mut String,
    pl: &Placement,
    fp: &FootprintDef,
    raw_children: &[SExp],
    raw_is_bottom: bool,
    hide_refdes: bool,
    net_ids: &BTreeMap<String, usize>,
    uuids: &mut UuidGen,
) {
    let fp_id = placement_fp_id(pl, fp);
    let items = raw_children.to_vec();

    let bottom = pl.side == Side::Bottom;
    let needs_flip = bottom != raw_is_bottom;
    let mut ctx = RawCtx {
        bx_mm: nm_to_mm(pl.pos.x),
        by_mm: nm_to_mm(-pl.pos.y),
        fp_rot: pl.rot_deg.rem_euclid(360.0),
        needs_flip,
        remap_side: if needs_flip { Side::Bottom } else { Side::Top },
        zone_xf: None,
        uuids,
        uuid_map: HashMap::new(),
    };
    let mut sch_fields: BTreeMap<&str, &str> = pl
        .schematic
        .as_ref()
        .map(|s| {
            s.fields
                .iter()
                .filter(|(k, v)| is_exportable_kicad_field(k, v))
                .map(|(k, v)| (k.as_str(), v.as_str()))
                .collect()
        })
        .unwrap_or_default();

    let (mut head_meta, mut props, mut body, mut group_indices) =
        (Vec::new(), Vec::new(), Vec::new(), Vec::new());
    let mut raw_attr: Option<Vec<String>> = None;
    let (mut saw_ref, mut saw_val) = (false, false);

    for mut child in items {
        let Some(tag) = child.tag().map(str::to_string) else {
            continue;
        };
        match tag.as_str() {
            "version" | "generator" | "generator_version" | "tedit" | "layer" | "uuid" | "at"
            | "locked" | "path" | "sheetname" | "sheetfile" => {}
            "descr" | "tags" => head_meta.push(child),
            "property" => {
                let key = atom_at(&child, 1).unwrap_or("").to_string();
                let val_ov: Option<&str> = match key.as_str() {
                    "Reference" => {
                        saw_ref = true;
                        Some(pl.refdes.as_str())
                    }
                    "Value" => {
                        saw_val = true;
                        Some(pl.value.as_str())
                    }
                    "Footprint" => Some(fp_id),
                    _ => sch_fields.remove(key.as_str()),
                };
                transform_text_item(&mut child, val_ov, &mut ctx);
                if hide_refdes && key == "Reference" {
                    hide_text_item(&mut child);
                }
                props.push(child);
            }
            "fp_text" => {
                let kind = atom_at(&child, 1).unwrap_or("").to_string();
                let val_ov = match kind.as_str() {
                    "reference" => {
                        saw_ref = true;
                        Some(pl.refdes.as_str())
                    }
                    "value" => {
                        saw_val = true;
                        Some(pl.value.as_str())
                    }
                    _ => None,
                };
                transform_text_item(&mut child, val_ov, &mut ctx);
                if hide_refdes && kind == "reference" {
                    hide_text_item(&mut child);
                }
                body.push(child);
            }
            "fp_text_box" => {
                transform_text_item(&mut child, None, &mut ctx);
                body.push(child);
            }
            "attr" => {
                let mut toks = Vec::new();
                for t in child
                    .as_list()
                    .into_iter()
                    .flatten()
                    .skip(1)
                    .filter_map(SExp::as_atom)
                {
                    if !toks.iter().any(|x: &String| x == t) {
                        toks.push(t.to_string());
                    }
                }
                raw_attr = Some(toks);
            }
            "pad" => {
                transform_pad(&mut child, pl, net_ids, &mut ctx);
                body.push(child);
            }
            "zone" => {
                ctx.zone_xf = Some(sin_cos_deg(ctx.fp_rot));
                transform_generic(&mut child, &mut ctx);
                ctx.zone_xf = None;
                ctx.ensure_uuid(&mut child);
                body.push(child);
            }
            "group" => {
                if let SExp::List(g_items) = &mut child {
                    for g_ch in g_items
                        .iter_mut()
                        .skip(1)
                        .filter(|c| c.tag() == Some("uuid"))
                    {
                        ctx.replace_uuid(g_ch);
                    }
                }
                ctx.ensure_uuid(&mut child);
                group_indices.push(body.len());
                body.push(child);
            }
            "model" => body.push(child),
            _ => {
                transform_generic(&mut child, &mut ctx);
                if matches!(
                    tag.as_str(),
                    "fp_line"
                        | "fp_rect"
                        | "fp_circle"
                        | "fp_arc"
                        | "fp_poly"
                        | "fp_curve"
                        | "dimension"
                ) {
                    ctx.ensure_uuid(&mut child);
                }
                body.push(child);
            }
        }
    }

    for idx in group_indices {
        if let Some(SExp::List(g_items)) = body.get_mut(idx) {
            for g_ch in g_items
                .iter_mut()
                .skip(1)
                .filter(|c| c.tag() == Some("members"))
            {
                if let SExp::List(m_items) = g_ch {
                    for m in m_items.iter_mut().skip(1) {
                        if let SExp::Atom(u) = m
                            && let Some(new_u) = ctx.uuid_map.get(u.as_str())
                        {
                            *u = new_u.clone();
                        }
                    }
                }
            }
        }
    }

    write_fp_header(out, fp_id, pl, ctx.fp_rot, ctx.uuids);
    for node in &head_meta {
        write_top_child(out, node);
    }
    for node in &props {
        write_top_child(out, node);
    }
    if !saw_ref {
        write_fp_property(out, "Reference", &pl.refdes, bottom, ctx.uuids);
    }
    if !saw_val {
        write_fp_property(out, "Value", &pl.value, bottom, ctx.uuids);
    }
    for (k, v) in sch_fields {
        write_fp_property(out, k, v, bottom, ctx.uuids);
    }
    if let Some(sch) = &pl.schematic {
        write_schematic_paths(out, sch);
    }
    write_fp_attr(out, Some(raw_attr.unwrap_or_default()), fp, pl);
    for node in &body {
        write_top_child(out, node);
    }
    out.push_str("\t)\n");
}

pub(crate) fn write_footprints(
    out: &mut String,
    board: &BoardIr,
    net_ids: &BTreeMap<String, usize>,
    uuids: &mut UuidGen,
) {
    let hide_refdes = board.fab.auto_refdes == AutoRefdes::Off;
    let mut raw_fp_cache: HashMap<&str, Option<(Vec<SExp>, bool)>> =
        HashMap::with_capacity(board.footprints.len());
    for pl in &board.placements {
        let Some(fp) = board.footprints.get(&pl.footprint) else {
            continue;
        };
        let cached = raw_fp_cache
            .entry(pl.footprint.as_str())
            .or_insert_with(|| prepare_raw_footprint(fp));
        if let Some((raw_items, raw_is_bottom)) = cached.as_ref() {
            write_raw_footprint(
                out,
                pl,
                fp,
                raw_items,
                *raw_is_bottom,
                hide_refdes,
                net_ids,
                uuids,
            );
            continue;
        }
        let fp_id = placement_fp_id(pl, fp);
        let bottom = pl.side == Side::Bottom;
        write_fp_header(out, fp_id, pl, pl.rot_deg, uuids);
        write_fp_property(out, "Reference", &pl.refdes, bottom, uuids);
        write_fp_property(out, "Value", &pl.value, bottom, uuids);
        if let Some(sch) = &pl.schematic {
            for (k, v) in &sch.fields {
                if is_exportable_kicad_field(k, v) {
                    write_fp_property(out, k, v, bottom, uuids);
                }
            }
            write_schematic_paths(out, sch);
        }
        write_fp_attr(out, None, fp, pl);
        write_footprint_body(out, pl, fp, net_ids, uuids);
        out.push_str("\t)\n");
    }
}
