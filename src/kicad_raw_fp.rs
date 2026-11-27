use std::collections::{BTreeMap, HashSet};
use std::fmt::Write as _;

use viahop::sexp::SExp;

use crate::ir::{FootprintDef, Placement, Side, nm_to_mm};
use crate::kicad_export_items::{
    UuidGen, collect_attr_tokens, esc, is_exportable_kicad_field, remap_kicad_layer,
    write_fp_property, write_schematic_paths,
};
use crate::kicad_geom::parse_kicad_sexp;

fn list_tag(node: &SExp) -> Option<&str> {
    node.as_list()?.first()?.as_atom()
}

fn atom(node: &SExp, idx: usize) -> Option<&str> {
    node.as_list()?.get(idx)?.as_atom()
}

fn uuid_node(uuids: &mut UuidGen) -> SExp {
    SExp::List(vec![SExp::Atom("uuid".into()), SExp::Atom(uuids.next())])
}

fn format_rot(deg: f64) -> String {
    let mut norm = deg.rem_euclid(360.0);
    if (norm - 360.0).abs() < 1e-6 || norm.abs() < 1e-6 {
        norm = 0.0;
    }
    let s = format!("{norm:.4}");
    s.trim_end_matches('0').trim_end_matches('.').to_string()
}

fn negate_num_atom(raw: &str) -> String {
    if let Ok(v) = raw.parse::<f64>() {
        if v == 0.0 {
            return "0".into();
        }
        if !raw.contains(['e', 'E']) {
            let trimmed = raw.strip_prefix('+').unwrap_or(raw);
            return match trimmed.strip_prefix('-') {
                Some(rest) => rest.to_string(),
                None => format!("-{trimmed}"),
            };
        }
        let s = format!("{:.6}", -v);
        return s.trim_end_matches('0').trim_end_matches('.').to_string();
    }
    raw.to_string()
}

fn transform_at(node: &SExp, fp_rot: f64, bottom: bool) -> SExp {
    let items = node.as_list().unwrap_or(&[]);
    let x = items.get(1).and_then(SExp::as_atom).unwrap_or("0");
    let y_raw = items.get(2).and_then(SExp::as_atom).unwrap_or("0");
    let y = if bottom {
        negate_num_atom(y_raw)
    } else {
        y_raw.to_string()
    };
    let lrot = items.get(3).and_then(SExp::as_f64).unwrap_or(0.0);
    let abs_rot = if bottom { fp_rot - lrot } else { fp_rot + lrot };
    SExp::List(vec![
        SExp::Atom("at".into()),
        SExp::Atom(x.to_string()),
        SExp::Atom(y),
        SExp::Atom(format_rot(abs_rot)),
    ])
}

fn transform_effects(node: &SExp, bottom: bool) -> SExp {
    let mut out = Vec::new();
    let mut saw_justify = false;
    for item in node.as_list().unwrap_or(&[]) {
        if list_tag(item) != Some("justify") {
            out.push(item.clone());
            continue;
        }
        saw_justify = true;
        let mut j = vec![SExp::Atom("justify".into())];
        let mut had_mirror = false;
        for tok in item.as_list().unwrap_or(&[]).iter().skip(1) {
            if tok.as_atom() == Some("mirror") {
                had_mirror = true;
                if !bottom {
                    j.push(tok.clone());
                }
            } else {
                j.push(tok.clone());
            }
        }
        if bottom && !had_mirror {
            j.push(SExp::Atom("mirror".into()));
        }
        if j.len() > 1 {
            out.push(SExp::List(j));
        }
    }
    if bottom && !saw_justify {
        out.push(SExp::List(vec![
            SExp::Atom("justify".into()),
            SExp::Atom("mirror".into()),
        ]));
    }
    SExp::List(out)
}

fn transform_generic(node: &SExp, side: Side, uuids: &mut UuidGen, top_item: bool) -> SExp {
    let SExp::List(items) = node else {
        return node.clone();
    };
    let tag = items.first().and_then(SExp::as_atom).unwrap_or("");
    let bottom = side == Side::Bottom;
    if tag == "uuid" {
        return uuid_node(uuids);
    }
    if tag == "layer" && items.len() >= 2 {
        let mut out = items.clone();
        if let Some(l) = items[1].as_atom() {
            out[1] = SExp::Atom(remap_kicad_layer(l, side));
        }
        return SExp::List(out);
    }
    if tag == "layers" {
        let mut out = vec![SExp::Atom("layers".into())];
        for item in items.iter().skip(1) {
            out.push(match item.as_atom() {
                Some(l) => SExp::Atom(remap_kicad_layer(l, side)),
                None => item.clone(),
            });
        }
        return SExp::List(out);
    }
    if bottom && tag == "chamfer" {
        let mut out = vec![SExp::Atom("chamfer".into())];
        for item in items.iter().skip(1) {
            out.push(match item.as_atom() {
                Some("top_left") => SExp::Atom("bottom_left".into()),
                Some("bottom_left") => SExp::Atom("top_left".into()),
                Some("top_right") => SExp::Atom("bottom_right".into()),
                Some("bottom_right") => SExp::Atom("top_right".into()),
                Some(other) => SExp::Atom(other.to_string()),
                None => item.clone(),
            });
        }
        return SExp::List(out);
    }
    let flip_y_here = bottom
        && (matches!(tag, "start" | "end" | "mid" | "center" | "xy" | "at")
            || (tag == "offset" && items.get(1).and_then(SExp::as_atom).is_some()));

    let mut out = Vec::with_capacity(items.len() + 1);
    let mut has_uuid = false;
    for (idx, item) in items.iter().enumerate() {
        if flip_y_here
            && idx == 2
            && let Some(s) = item.as_atom()
        {
            out.push(SExp::Atom(negate_num_atom(s)));
            continue;
        }
        if list_tag(item) == Some("uuid") {
            has_uuid = true;
        }
        out.push(transform_generic(item, side, uuids, false));
    }
    if top_item
        && !has_uuid
        && matches!(
            tag,
            "fp_line" | "fp_rect" | "fp_circle" | "fp_arc" | "fp_poly" | "fp_curve" | "zone"
        )
    {
        out.push(uuid_node(uuids));
    }
    SExp::List(out)
}

fn transform_text_item(
    items: &[SExp],
    val_override: Option<&str>,
    fp_rot: f64,
    side: Side,
    uuids: &mut UuidGen,
) -> SExp {
    let bottom = side == Side::Bottom;
    let mut out = Vec::with_capacity(items.len() + 2);
    let mut has_effects = false;
    let mut has_uuid = false;
    for (idx, item) in items.iter().enumerate() {
        if idx == 2
            && let Some(v) = val_override
        {
            out.push(SExp::Atom(v.to_string()));
            continue;
        }
        match list_tag(item) {
            Some("at") => out.push(transform_at(item, fp_rot, bottom)),
            Some("effects") => {
                has_effects = true;
                out.push(transform_effects(item, bottom));
            }
            Some("uuid") => {
                has_uuid = true;
                out.push(uuid_node(uuids));
            }
            _ => out.push(transform_generic(item, side, uuids, false)),
        }
    }
    if !has_effects && bottom {
        out.push(SExp::List(vec![
            SExp::Atom("effects".into()),
            SExp::List(vec![
                SExp::Atom("justify".into()),
                SExp::Atom("mirror".into()),
            ]),
        ]));
    }
    if !has_uuid {
        out.push(uuid_node(uuids));
    }
    SExp::List(out)
}

fn transform_pad(
    items: &[SExp],
    pl: &Placement,
    net_ids: &BTreeMap<String, usize>,
    uuids: &mut UuidGen,
) -> SExp {
    let bottom = pl.side == Side::Bottom;
    let fp_rot = pl.rot_deg.rem_euclid(360.0);
    let pad_name = items.get(1).and_then(SExp::as_atom).unwrap_or("");
    let pad_kind = items.get(2).and_then(SExp::as_atom).unwrap_or("");
    let mut out = Vec::with_capacity(items.len() + 2);
    for item in items {
        match list_tag(item) {
            Some("at") => out.push(transform_at(item, fp_rot, bottom)),
            Some("net") | Some("uuid") => {}
            _ => out.push(transform_generic(item, pl.side, uuids, false)),
        }
    }
    if pad_kind != "np_thru_hole"
        && let Some(net_name) = pl.pin_nets.get(pad_name)
        && let Some(&net_id) = net_ids.get(net_name)
    {
        out.push(SExp::List(vec![
            SExp::Atom("net".into()),
            SExp::Atom(net_id.to_string()),
            SExp::Atom(net_name.clone()),
        ]));
    }
    out.push(uuid_node(uuids));
    SExp::List(out)
}

fn is_quoted_arg(parent_tag: &str, parent_len: usize, idx: usize) -> bool {
    match parent_tag {
        // Footprint `(property "Key" "Val" ...)` has >= 3 items; pad `(property pad_prop_*)` has 2 unquoted items.
        "property" => parent_len >= 3 && (idx == 1 || idx == 2),
        "fp_text" | "gr_text" | "net" => idx == 2,
        "layers" | "members" => idx >= 1,
        "footprint" | "layer" | "uuid" | "descr" | "tags" | "model" | "pad" | "path"
        | "sheetname" | "sheetfile" | "name" | "net_name" | "group" | "pinfunction" | "pintype"
        | "prefix" | "suffix" | "tstamp" | "generator" | "generator_version" | "paper"
        | "title" | "copper_finish" | "material" => idx == 1,
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

fn write_sexp_inline(
    out: &mut String,
    node: &SExp,
    parent_tag: &str,
    parent_len: usize,
    idx: usize,
) {
    match node {
        SExp::Atom(s) => write_atom(
            out,
            s,
            idx > 0 && is_quoted_arg(parent_tag, parent_len, idx),
        ),
        SExp::List(items) => {
            out.push('(');
            let tag = items.first().and_then(SExp::as_atom).unwrap_or("");
            let len = items.len();
            for (i, item) in items.iter().enumerate() {
                if i > 0 {
                    out.push(' ');
                }
                write_sexp_inline(out, item, tag, len, i);
            }
            out.push(')');
        }
    }
}

fn write_top_child(out: &mut String, node: &SExp) {
    let Some(items) = node.as_list() else {
        return;
    };
    let tag = items.first().and_then(SExp::as_atom).unwrap_or("");
    let len = items.len();
    if !matches!(tag, "pad" | "zone" | "model" | "fp_poly") {
        out.push_str("\t\t");
        write_sexp_inline(out, node, "", 0, 0);
        out.push('\n');
        return;
    }
    out.push_str("\t\t(");
    let mut header_done = false;
    for (i, item) in items.iter().enumerate() {
        match item {
            SExp::Atom(s) if !header_done => {
                if i > 0 {
                    out.push(' ');
                }
                write_atom(out, s, i > 0 && is_quoted_arg(tag, len, i));
            }
            _ => {
                if !header_done {
                    out.push('\n');
                    header_done = true;
                }
                out.push_str("\t\t\t");
                write_sexp_inline(out, item, tag, len, i);
                out.push('\n');
            }
        }
    }
    if !header_done {
        out.push('\n');
    }
    out.push_str("\t\t)\n");
}

pub(crate) fn try_write_raw_footprint(
    out: &mut String,
    raw: &str,
    fp_id: &str,
    pl: &Placement,
    fp: &FootprintDef,
    net_ids: &BTreeMap<String, usize>,
    uuids: &mut UuidGen,
) -> bool {
    let Ok(root) = parse_kicad_sexp(raw) else {
        return false;
    };
    let Some(items) = root.as_list() else {
        return false;
    };
    if !matches!(
        items.first().and_then(SExp::as_atom),
        Some("footprint" | "module")
    ) {
        return false;
    }

    let bottom = pl.side == Side::Bottom;
    let fp_rot = pl.rot_deg.rem_euclid(360.0);
    let fab_layer = if bottom { "B.Fab" } else { "F.Fab" };
    let sch_fields: BTreeMap<&str, &str> = pl
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

    let start_idx = if items.get(1).and_then(SExp::as_atom).is_some() {
        2
    } else {
        1
    };
    let mut head_meta = Vec::new();
    let mut props = Vec::new();
    let mut raw_attr: Option<Vec<String>> = None;
    let mut body = Vec::new();
    let mut saw_reference = false;
    let mut saw_value = false;
    let mut emitted_fields: HashSet<&str> = HashSet::new();

    for child in &items[start_idx..] {
        let Some(ch) = child.as_list() else {
            continue;
        };
        let tag = ch.first().and_then(SExp::as_atom).unwrap_or("");
        match tag {
            "version" | "generator" | "generator_version" | "tedit" | "layer" | "uuid" | "at"
            | "locked" | "path" | "sheetname" | "sheetfile" => {}
            "descr" | "tags" => head_meta.push(child.clone()),
            "property" => {
                let key = atom(child, 1).unwrap_or("");
                let val_ov = match key {
                    "Reference" => {
                        saw_reference = true;
                        Some(pl.refdes.as_str())
                    }
                    "Value" => {
                        saw_value = true;
                        Some(pl.value.as_str())
                    }
                    "Footprint" => Some(fp_id),
                    _ => sch_fields.get(key).copied().inspect(|_| {
                        emitted_fields.insert(key);
                    }),
                };
                props.push(transform_text_item(ch, val_ov, fp_rot, pl.side, uuids));
            }
            "fp_text" => {
                let val_ov = match atom(child, 1).unwrap_or("") {
                    "reference" => {
                        saw_reference = true;
                        Some(pl.refdes.as_str())
                    }
                    "value" => {
                        saw_value = true;
                        Some(pl.value.as_str())
                    }
                    _ => None,
                };
                body.push(transform_text_item(ch, val_ov, fp_rot, pl.side, uuids));
            }
            "attr" => {
                let mut toks = Vec::new();
                for t in ch.iter().skip(1).filter_map(SExp::as_atom) {
                    if !toks.iter().any(|x: &String| x == t) {
                        toks.push(t.to_string());
                    }
                }
                raw_attr = Some(toks);
            }
            "pad" => body.push(transform_pad(ch, pl, net_ids, uuids)),
            "model" => body.push(child.clone()),
            _ => body.push(transform_generic(child, pl.side, uuids, true)),
        }
    }

    let _ = writeln!(out, "\t(footprint \"{}\"", esc(fp_id));
    let _ = writeln!(out, "\t\t(layer \"{}\")", pl.side.cu_layer());
    let _ = writeln!(out, "\t\t(uuid \"{}\")", uuids.next());
    let _ = writeln!(
        out,
        "\t\t(at {:.6} {:.6} {})",
        nm_to_mm(pl.pos.x),
        nm_to_mm(-pl.pos.y),
        format_rot(fp_rot)
    );
    if pl.locked {
        out.push_str("\t\t(locked yes)\n");
    }
    for node in &head_meta {
        write_top_child(out, node);
    }
    for node in &props {
        write_top_child(out, node);
    }
    if !saw_reference {
        write_fp_property(out, "Reference", &pl.refdes, fab_layer, bottom, uuids);
    }
    if !saw_value {
        write_fp_property(out, "Value", &pl.value, fab_layer, bottom, uuids);
    }
    for (k, v) in &sch_fields {
        if !emitted_fields.contains(k) {
            write_fp_property(out, k, v, fab_layer, bottom, uuids);
        }
    }
    if let Some(sch) = &pl.schematic {
        write_schematic_paths(out, sch);
    }
    let attr_tokens = match raw_attr {
        Some(mut toks) => {
            for (flag, name) in [
                (pl.exclude_from_pos, "exclude_from_pos_files"),
                (pl.exclude_from_bom, "exclude_from_bom"),
                (pl.dnp, "dnp"),
            ] {
                if flag && !toks.iter().any(|t| t == name) {
                    toks.push(name.to_string());
                }
            }
            toks
        }
        None => collect_attr_tokens(fp, pl)
            .into_iter()
            .map(str::to_string)
            .collect(),
    };
    if !attr_tokens.is_empty() {
        let _ = writeln!(out, "\t\t(attr {})", attr_tokens.join(" "));
    }
    for node in &body {
        write_top_child(out, node);
    }
    out.push_str("\t)\n");
    true
}
