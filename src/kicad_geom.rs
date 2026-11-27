use std::collections::{BTreeMap, HashMap};
use std::f64::consts::{FRAC_PI_2, TAU};

use viahop::sexp::SExp;

use crate::geom::{ensure_ccw, remap_layer_for_side};
use crate::ir::{Graphic, Nm, Pt, Side, mm};
use crate::shapes::circle_poly;

const CHAIN_TOL_NM: Nm = 10_000;

/// Iterative KiCad S-expression parser handling `#` atoms (e.g. `#PWR01`) and full string escapes.
pub(crate) fn parse_kicad_sexp(src: &str) -> Result<SExp, String> {
    let bytes = src.as_bytes();
    let mut pos = 0usize;
    let mut stack: Vec<Vec<SExp>> = Vec::new();
    let mut root: Option<SExp> = None;

    while pos < bytes.len() {
        while pos < bytes.len() && bytes[pos].is_ascii_whitespace() {
            pos += 1;
        }
        if pos >= bytes.len() {
            break;
        }
        match bytes[pos] {
            b'(' => {
                if root.is_some() {
                    return Err("unexpected trailing expression in KiCad S-expression".into());
                }
                pos += 1;
                stack.push(Vec::new());
            }
            b')' => {
                pos += 1;
                let list = stack
                    .pop()
                    .ok_or_else(|| "unexpected ')' in KiCad S-expression".to_string())?;
                let node = SExp::List(list);
                if let Some(parent) = stack.last_mut() {
                    parent.push(node);
                } else {
                    root = Some(node);
                }
            }
            b'"' => {
                if root.is_some() {
                    return Err("unexpected trailing string in KiCad S-expression".into());
                }
                pos += 1;
                let mut s = String::new();
                let mut closed = false;
                while pos < bytes.len() {
                    let b = bytes[pos];
                    if b == b'"' {
                        pos += 1;
                        closed = true;
                        break;
                    }
                    if b == b'\\' {
                        pos += 1;
                        if pos >= bytes.len() {
                            return Err("unterminated escape in KiCad S-expression string".into());
                        }
                        match bytes[pos] {
                            b'\\' => s.push('\\'),
                            b'"' => s.push('"'),
                            b'n' => s.push('\n'),
                            b'r' => s.push('\r'),
                            b't' => s.push('\t'),
                            _ => {
                                let ch = src[pos..].chars().next().unwrap();
                                s.push(ch);
                                pos += ch.len_utf8();
                                continue;
                            }
                        }
                        pos += 1;
                    } else {
                        let start = pos;
                        while pos < bytes.len() && bytes[pos] != b'"' && bytes[pos] != b'\\' {
                            pos += 1;
                        }
                        s.push_str(&src[start..pos]);
                    }
                }
                if !closed {
                    return Err("unterminated quoted string in KiCad S-expression".into());
                }
                let node = SExp::Atom(s);
                if let Some(parent) = stack.last_mut() {
                    parent.push(node);
                } else {
                    root = Some(node);
                }
            }
            _ => {
                if root.is_some() {
                    return Err("unexpected trailing token in KiCad S-expression".into());
                }
                let start = pos;
                while pos < bytes.len()
                    && !bytes[pos].is_ascii_whitespace()
                    && bytes[pos] != b'('
                    && bytes[pos] != b')'
                {
                    pos += 1;
                }
                let node = SExp::Atom(src[start..pos].to_string());
                if let Some(parent) = stack.last_mut() {
                    parent.push(node);
                } else {
                    root = Some(node);
                }
            }
        }
    }

    if !stack.is_empty() {
        return Err("unclosed '(' in KiCad S-expression".into());
    }
    root.ok_or_else(|| "empty KiCad S-expression".into())
}

pub(crate) fn is_ignored_kicad_field(name: &str) -> bool {
    name.trim().is_empty()
        || matches!(
            name,
            "Reference" | "Value" | "Footprint" | "Sheetname" | "Sheetfile"
        )
        || name.starts_with("ki_")
}

pub(crate) fn insert_kicad_field(fields: &mut BTreeMap<String, String>, k: &str, v: &str) {
    if is_ignored_kicad_field(k) || v.is_empty() || (k == "Datasheet" && v == "~") {
        return;
    }
    fields.insert(k.to_string(), v.to_string());
}

pub(crate) fn find_field_ci(fields: &BTreeMap<String, String>, key: &str) -> Option<String> {
    fields
        .iter()
        .find(|(k, _)| k.eq_ignore_ascii_case(key))
        .map(|(_, v)| v.clone())
}

pub(crate) fn normalize_sheetname(raw: &str) -> String {
    let trimmed = raw.trim().trim_matches('/');
    if trimmed.is_empty() {
        "/".into()
    } else {
        format!("/{trimmed}/")
    }
}

pub(crate) fn atom_at(node: &SExp, idx: usize) -> Option<&str> {
    node.as_list()?.get(idx)?.as_atom()
}

pub(crate) fn f64_at(node: &SExp, idx: usize) -> Option<f64> {
    node.as_list()?.get(idx)?.as_f64()
}

pub(crate) fn child_atom<'a>(node: &'a SExp, tag: &str) -> Option<&'a str> {
    atom_at(node.find_child(tag)?, 1)
}

pub(crate) fn has_flag(node: &SExp, tag: &str) -> bool {
    if let Some(c) = node.find_child(tag) {
        return atom_at(c, 1) != Some("no");
    }
    node.as_list()
        .is_some_and(|items| items.iter().skip(1).any(|x| x.as_atom() == Some(tag)))
}

pub(crate) fn parse_pt(node: &SExp, y_sign: i64) -> Option<Pt> {
    Some(Pt::new(mm(f64_at(node, 1)?), y_sign * mm(f64_at(node, 2)?)))
}

pub(crate) fn parse_at(node: &SExp, y_sign: i64) -> Option<(Pt, f64)> {
    Some((parse_pt(node, y_sign)?, f64_at(node, 3).unwrap_or(0.0)))
}

pub(crate) fn parse_stroke_width(node: &SExp) -> Nm {
    if let Some(stroke) = node.find_child("stroke")
        && let Some(w) = stroke.find_child("width").and_then(|n| f64_at(n, 1))
    {
        return mm(w);
    }
    node.find_child("width")
        .and_then(|n| f64_at(n, 1))
        .map(mm)
        .unwrap_or(0)
}

pub(crate) fn parse_fill(node: &SExp) -> bool {
    match node.find_child("fill").and_then(|f| atom_at(f, 1)) {
        Some("yes") | Some("solid") => true,
        Some("no") | Some("none") => false,
        _ => node
            .find_child("fill")
            .is_some_and(|f| f.as_list().is_some_and(|l| l.len() == 1)),
    }
}

fn parse_text_font(node: &SExp) -> (Nm, Nm) {
    let font = node
        .find_child("effects")
        .and_then(|e| e.find_child("font"));
    let height_nm = font
        .and_then(|f| f.find_child("size"))
        .and_then(|s| f64_at(s, 2).or_else(|| f64_at(s, 1)))
        .filter(|&v| v.is_finite() && v > 0.0)
        .map(mm)
        .unwrap_or(1_000_000);
    let stroke_nm = font
        .and_then(|f| f.find_child("thickness"))
        .and_then(|t| f64_at(t, 1))
        .filter(|&v| v.is_finite() && v > 0.0)
        .map(mm)
        .unwrap_or(150_000);
    (height_nm, stroke_nm)
}

pub(crate) fn parse_text_graphic(
    node: &SExp,
    y_sign: i64,
    layer: String,
    text: &str,
    rot_override: Option<f64>,
) -> Option<Graphic> {
    if has_flag(node, "hide") {
        return None;
    }
    let (pos, raw_rot) = node.find_child("at").and_then(|n| parse_at(n, y_sign))?;
    let (height_nm, stroke_nm) = parse_text_font(node);
    let r = rot_override.unwrap_or(raw_rot);
    let rot_deg = if r.is_finite() {
        r.rem_euclid(360.0)
    } else {
        0.0
    };
    Some(Graphic::Text {
        layer,
        text: text.to_string(),
        pos,
        height_nm,
        stroke_nm,
        rot_deg,
    })
}

pub(crate) fn parse_pts(node: &SExp, y_sign: i64) -> Vec<Pt> {
    let Some(pts) = node.find_child("pts").and_then(SExp::as_list) else {
        return Vec::new();
    };
    let mut out = Vec::new();
    for item in pts.iter().skip(1) {
        match item.tag() {
            Some("xy") => {
                if let Some(p) = parse_pt(item, y_sign) {
                    out.push(p);
                }
            }
            Some("arc") => {
                if let (Some(s), Some(m), Some(e)) = (
                    item.find_child("start").and_then(|n| parse_pt(n, y_sign)),
                    item.find_child("mid").and_then(|n| parse_pt(n, y_sign)),
                    item.find_child("end").and_then(|n| parse_pt(n, y_sign)),
                ) {
                    for p in discretize_arc(s, m, e) {
                        if out.last() != Some(&p) {
                            out.push(p);
                        }
                    }
                }
            }
            _ => {}
        }
    }
    out
}

pub(crate) fn parse_net_ref(node: &SExp, net_id_map: &HashMap<i64, String>) -> String {
    let Some(items) = node.find_child("net").and_then(SExp::as_list) else {
        return String::new();
    };
    if items.len() >= 3 {
        return items[2].as_atom().unwrap_or("").to_string();
    }
    if items.len() == 2 {
        if let Some(id) = items[1].as_i64() {
            return net_id_map.get(&id).cloned().unwrap_or_default();
        }
        return items[1].as_atom().unwrap_or("").to_string();
    }
    String::new()
}

pub(crate) fn parse_layers_list(node: &SExp, canonical_side: Option<Side>) -> Vec<String> {
    let map_layer = |raw: &str| match canonical_side {
        Some(Side::Bottom) => remap_layer_for_side(raw, Side::Bottom),
        _ => raw.to_string(),
    };
    if let Some(layers) = node.find_child("layers").and_then(SExp::as_list) {
        return layers
            .iter()
            .skip(1)
            .filter_map(SExp::as_atom)
            .map(map_layer)
            .collect();
    }
    child_atom(node, "layer")
        .map(map_layer)
        .into_iter()
        .collect()
}

pub(crate) fn discretize_arc(start: Pt, mid: Pt, end: Pt) -> Vec<Pt> {
    let (ax, ay) = (start.x as f64, start.y as f64);
    let (bx, by) = (mid.x as f64, mid.y as f64);
    let (cx, cy) = (end.x as f64, end.y as f64);
    let d = 2.0 * (ax * (by - cy) + bx * (cy - ay) + cx * (ay - by));
    if d.abs() < 1e-3 {
        return vec![start, end];
    }
    let a2 = ax * ax + ay * ay;
    let b2 = bx * bx + by * by;
    let c2 = cx * cx + cy * cy;
    let ux = (a2 * (by - cy) + b2 * (cy - ay) + c2 * (ay - by)) / d;
    let uy = (a2 * (cx - bx) + b2 * (ax - cx) + c2 * (bx - ax)) / d;
    let r = (ax - ux).hypot(ay - uy);
    let a_s = (ay - uy).atan2(ax - ux);
    let a_m = (by - uy).atan2(bx - ux);
    let a_e = (cy - uy).atan2(cx - ux);
    let d_sm = (a_m - a_s).rem_euclid(TAU);
    let d_se = (a_e - a_s).rem_euclid(TAU);
    let sweep = if d_sm <= d_se { d_se } else { d_se - TAU };
    let segs = ((sweep.abs() / FRAC_PI_2) * 16.0).ceil().clamp(4.0, 64.0) as usize;
    let mut pts = Vec::with_capacity(segs + 1);
    pts.push(start);
    for i in 1..segs {
        let ang = a_s + sweep * (i as f64 / segs as f64);
        pts.push(Pt::new(
            (ux + r * ang.cos()).round() as Nm,
            (uy + r * ang.sin()).round() as Nm,
        ));
    }
    pts.push(end);
    pts
}

pub(crate) fn parse_graphic_node(
    node: &SExp,
    y_sign: i64,
    layer_norm: Option<Side>,
) -> Vec<Graphic> {
    let raw_layer = child_atom(node, "layer").unwrap_or("");
    let layer = match layer_norm {
        Some(s) => remap_layer_for_side(raw_layer, s),
        None => raw_layer.to_string(),
    };
    let width = parse_stroke_width(node);
    let fill = parse_fill(node);
    match node.tag().unwrap_or("") {
        "fp_line" | "gr_line" => {
            if let (Some(a), Some(b)) = (
                node.find_child("start").and_then(|n| parse_pt(n, y_sign)),
                node.find_child("end").and_then(|n| parse_pt(n, y_sign)),
            ) {
                return vec![Graphic::Line { layer, a, b, width }];
            }
        }
        "fp_rect" | "gr_rect" => {
            if let (Some(s), Some(e)) = (
                node.find_child("start").and_then(|n| parse_pt(n, y_sign)),
                node.find_child("end").and_then(|n| parse_pt(n, y_sign)),
            ) {
                return vec![Graphic::Rect {
                    layer,
                    min: Pt::new(s.x.min(e.x), s.y.min(e.y)),
                    max: Pt::new(s.x.max(e.x), s.y.max(e.y)),
                    width,
                    fill,
                }];
            }
        }
        "fp_circle" | "gr_circle" => {
            if let (Some(center), Some(end)) = (
                node.find_child("center").and_then(|n| parse_pt(n, y_sign)),
                node.find_child("end").and_then(|n| parse_pt(n, y_sign)),
            ) {
                return vec![Graphic::Circle {
                    layer,
                    center,
                    radius: center.dist(end).round() as Nm,
                    width,
                    fill,
                }];
            }
        }
        "fp_poly" | "gr_poly" => {
            let verts = parse_pts(node, y_sign);
            if verts.len() >= 3 {
                return vec![Graphic::Poly {
                    layer,
                    verts,
                    width,
                    fill,
                }];
            }
        }
        "fp_arc" | "gr_arc" => {
            if let (Some(s), Some(m), Some(e)) = (
                node.find_child("start").and_then(|n| parse_pt(n, y_sign)),
                node.find_child("mid").and_then(|n| parse_pt(n, y_sign)),
                node.find_child("end").and_then(|n| parse_pt(n, y_sign)),
            ) {
                return discretize_arc(s, m, e)
                    .windows(2)
                    .map(|w| Graphic::Line {
                        layer: layer.clone(),
                        a: w[0],
                        b: w[1],
                        width,
                    })
                    .collect();
            }
        }
        "gr_text" => {
            if let Some(text) = atom_at(node, 1)
                && let Some(g) = parse_text_graphic(node, y_sign, layer, text, None)
            {
                return vec![g];
            }
        }
        _ => {}
    }
    Vec::new()
}

pub(crate) fn collect_poly_or_segment(
    node: &SExp,
    y_sign: i64,
    open: &mut Vec<Vec<Pt>>,
    closed: &mut Vec<Vec<Pt>>,
) {
    match node.tag() {
        Some("fp_line") | Some("gr_line") => {
            if let (Some(a), Some(b)) = (
                node.find_child("start").and_then(|n| parse_pt(n, y_sign)),
                node.find_child("end").and_then(|n| parse_pt(n, y_sign)),
            ) {
                open.push(vec![a, b]);
            }
        }
        Some("fp_arc") | Some("gr_arc") => {
            if let (Some(s), Some(m), Some(e)) = (
                node.find_child("start").and_then(|n| parse_pt(n, y_sign)),
                node.find_child("mid").and_then(|n| parse_pt(n, y_sign)),
                node.find_child("end").and_then(|n| parse_pt(n, y_sign)),
            ) {
                open.push(discretize_arc(s, m, e));
            }
        }
        Some("fp_rect") | Some("gr_rect") => {
            if let (Some(s), Some(e)) = (
                node.find_child("start").and_then(|n| parse_pt(n, y_sign)),
                node.find_child("end").and_then(|n| parse_pt(n, y_sign)),
            ) {
                let mut v = vec![
                    Pt::new(s.x.min(e.x), s.y.min(e.y)),
                    Pt::new(s.x.max(e.x), s.y.min(e.y)),
                    Pt::new(s.x.max(e.x), s.y.max(e.y)),
                    Pt::new(s.x.min(e.x), s.y.max(e.y)),
                ];
                ensure_ccw(&mut v);
                closed.push(v);
            }
        }
        Some("fp_poly") | Some("gr_poly") => {
            let mut v = parse_pts(node, y_sign);
            if v.len() >= 3 {
                ensure_ccw(&mut v);
                closed.push(v);
            }
        }
        Some("fp_circle") | Some("gr_circle") => {
            if let (Some(c), Some(e)) = (
                node.find_child("center").and_then(|n| parse_pt(n, y_sign)),
                node.find_child("end").and_then(|n| parse_pt(n, y_sign)),
            ) {
                let mut v = circle_poly(c, c.dist(e).round() as Nm, 32);
                ensure_ccw(&mut v);
                closed.push(v);
            }
        }
        _ => {}
    }
}

pub(crate) fn chain_segments(open: Vec<Vec<Pt>>, closed: &mut Vec<Vec<Pt>>) {
    let mut rem = open;
    while let Some(mut chain) = rem.pop() {
        if chain.is_empty() {
            continue;
        }
        let mut is_closed = false;
        loop {
            let start = chain[0];
            let tail = *chain.last().unwrap();
            if chain.len() >= 3 && tail.dist(start) <= CHAIN_TOL_NM as f64 {
                chain.pop();
                is_closed = true;
                break;
            }
            // Match either end of `chain` against either end of any remaining segment.
            let mut best: Option<(usize, bool, bool, f64)> = None;
            for (i, seg) in rem.iter().enumerate() {
                if seg.is_empty() {
                    continue;
                }
                let s0 = seg[0];
                let s1 = *seg.last().unwrap();
                for (append_tail, rev, d) in [
                    (true, false, tail.dist(s0)),
                    (true, true, tail.dist(s1)),
                    (false, true, start.dist(s0)),
                    (false, false, start.dist(s1)),
                ] {
                    if d <= CHAIN_TOL_NM as f64 && best.is_none_or(|b| d < b.3) {
                        best = Some((i, append_tail, rev, d));
                    }
                }
            }
            let Some((idx, append_tail, rev, _)) = best else {
                break;
            };
            let mut seg = rem.swap_remove(idx);
            if rev {
                seg.reverse();
            }
            if append_tail {
                chain.extend(seg.into_iter().skip(1));
            } else {
                seg.pop();
                seg.extend(chain);
                chain = seg;
            }
        }
        chain.dedup();
        if chain.len() > 1 && chain.first() == chain.last() {
            chain.pop();
            is_closed = true;
        }
        if is_closed && chain.len() >= 3 {
            ensure_ccw(&mut chain);
            closed.push(chain);
        }
    }
}
