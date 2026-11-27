use std::collections::hash_map::Entry;
use std::collections::{BTreeMap, HashMap, HashSet};

use serde::{Deserialize, Serialize};

use crate::geom::local_to_board;
use crate::ir::{
    BgaShadowRule, CapProximityRule, Constraints, MatchGroupRule, MaxLengthRule, MaxViasRule,
    MinViasRule, NetDef, Placement, Pt, Side,
};
use crate::spec::BoardSpec;
use crate::spec_serde::deserialize_nets;
use crate::spec_validate::validate_net_names;

const MAX_SUBCIRCUIT_DEPTH: usize = 32;
const MAX_EXPANDED_INSTANCES: usize = 4096;

/// Reusable subcircuit block authored with local refdeses, coordinates, and port nets.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct SubcircuitSpec {
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub ports: Vec<String>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub placements: Vec<Placement>,
    #[serde(
        deserialize_with = "deserialize_nets",
        skip_serializing_if = "BTreeMap::is_empty"
    )]
    pub nets: BTreeMap<String, NetDef>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub instances: Vec<SubcircuitInstance>,
    #[serde(skip_serializing_if = "is_empty_constraints")]
    pub constraints: Constraints,
}

/// Instantiation of a `SubcircuitSpec` with a refdes/net prefix, port bindings, and board transform.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct SubcircuitInstance {
    pub subcircuit: String,
    pub prefix: String,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub ports: BTreeMap<String, String>,
    #[serde(default)]
    pub pos: Pt,
    #[serde(default)]
    pub rot_deg: f64,
    #[serde(default)]
    pub side: Side,
}

fn is_empty_constraints(c: &Constraints) -> bool {
    *c == Constraints::default()
}

pub(crate) struct ExpandedSpec {
    pub placements: Vec<Placement>,
    pub nets: BTreeMap<String, NetDef>,
    pub constraints: Constraints,
    pub sub_net_pins: Vec<(String, String, String)>,
}

#[derive(Default)]
struct ExpandScope {
    prefix: String,
    ports: BTreeMap<String, String>,
    pos: Pt,
    rot_deg: f64,
    side: Side,
    depth: usize,
}

struct NetScope {
    board_nets: HashSet<String>,
    private_nets: HashMap<String, String>,
}

impl NetScope {
    fn resolve(
        &mut self,
        local_net: &str,
        prefix: &str,
        ports: &BTreeMap<String, String>,
    ) -> Result<String, String> {
        let trimmed = local_net.trim();
        if trimmed.is_empty() {
            return Ok(String::new());
        }
        if let Some(bound) = ports.get(trimmed) {
            return Ok(bound.clone());
        }
        let resolved = join_prefix(prefix, trimmed);
        if !prefix.is_empty() {
            if self.board_nets.contains(&resolved) {
                return Err(format!(
                    "private net '{trimmed}' in '{prefix}' collides with board net '{resolved}'"
                ));
            }
            match self.private_nets.entry(resolved.clone()) {
                Entry::Occupied(e) if e.get() != prefix => {
                    let owner = e.get();
                    return Err(format!(
                        "private net '{trimmed}' in '{prefix}' collides with private net '{resolved}' owned by '{owner}'"
                    ));
                }
                Entry::Vacant(e) => {
                    e.insert(prefix.to_string());
                }
                _ => {}
            }
        }
        Ok(resolved)
    }

    fn resolve_all(
        &mut self,
        nets: &[String],
        prefix: &str,
        ports: &BTreeMap<String, String>,
    ) -> Result<Vec<String>, String> {
        nets.iter()
            .map(|n| self.resolve(n, prefix, ports))
            .collect()
    }
}

struct ExpandCtx {
    out: ExpandedSpec,
    nets: NetScope,
    match_group_names: HashSet<String>,
    instance_count: usize,
}

fn normalize_prefix(raw: &str) -> Result<&str, String> {
    let trimmed = raw.trim().trim_end_matches('_');
    if trimmed.is_empty() || trimmed.chars().any(char::is_whitespace) {
        return Err(format!(
            "subcircuit instance prefix '{raw}' must be non-empty without whitespace"
        ));
    }
    Ok(trimmed)
}

fn join_prefix(prefix: &str, name: &str) -> String {
    let p = prefix.trim().trim_end_matches('_');
    let n = name.trim();
    if p.is_empty() {
        n.to_string()
    } else {
        format!("{p}_{n}")
    }
}

fn compose_rot_side(
    parent_rot: f64,
    parent_side: Side,
    child_rot: f64,
    child_side: Side,
) -> (f64, Side) {
    match parent_side {
        Side::Top => ((parent_rot + child_rot).rem_euclid(360.0), child_side),
        Side::Bottom => (
            (parent_rot - child_rot).rem_euclid(360.0),
            child_side.flip(),
        ),
    }
}

fn is_default_net_class(class: &str) -> bool {
    let c = class.trim();
    c.is_empty() || c == "Default"
}

fn validate_instance_slice(
    scope_label: &str,
    instances: &[SubcircuitInstance],
    subcircuits: &BTreeMap<String, SubcircuitSpec>,
) -> Result<(), String> {
    let mut seen_prefixes = HashSet::with_capacity(instances.len());
    for inst in instances {
        let (p, sub_name) = (&inst.prefix, &inst.subcircuit);
        let target = subcircuits
            .get(sub_name)
            .ok_or_else(|| format!("{scope_label} instantiates unknown subcircuit '{sub_name}'"))?;
        if !seen_prefixes.insert(normalize_prefix(p)?) {
            return Err(format!("{scope_label} has duplicate instance prefix '{p}'"));
        }
        if !inst.rot_deg.is_finite() {
            return Err(format!("subcircuit instance '{p}' rot_deg must be finite"));
        }
        let declared: HashSet<&str> = target.ports.iter().map(String::as_str).collect();
        if let Some(k) = inst.ports.keys().find(|k| !declared.contains(k.as_str())) {
            return Err(format!(
                "subcircuit instance '{p}' of '{sub_name}' binds unknown port '{k}'"
            ));
        }
        for port in &target.ports {
            if inst.ports.get(port).is_none_or(|b| b.trim().is_empty()) {
                return Err(format!(
                    "subcircuit instance '{p}' of '{sub_name}' must bind port '{port}' to a non-empty net"
                ));
            }
        }
    }
    Ok(())
}

fn check_subcircuit_dag<'a>(
    name: &'a str,
    subs: &'a BTreeMap<String, SubcircuitSpec>,
    state: &mut HashMap<&'a str, u8>,
    memo: &mut HashMap<&'a str, usize>,
) -> Result<usize, String> {
    if state.get(name) == Some(&1) {
        return Err(format!(
            "subcircuit recursion cycle detected involving '{name}'"
        ));
    }
    if let Some(&depth) = memo.get(name) {
        return Ok(depth);
    }
    state.insert(name, 1);
    let mut max_depth = 0usize;
    if let Some(sub) = subs.get(name) {
        for inst in &sub.instances {
            let child_depth = check_subcircuit_dag(&inst.subcircuit, subs, state, memo)? + 1;
            if child_depth > MAX_SUBCIRCUIT_DEPTH {
                return Err(format!(
                    "subcircuit nesting exceeds max depth {MAX_SUBCIRCUIT_DEPTH} at '{name}'"
                ));
            }
            max_depth = max_depth.max(child_depth);
        }
    }
    state.insert(name, 2);
    memo.insert(name, max_depth);
    Ok(max_depth)
}

fn validate_subcircuits(spec: &BoardSpec) -> Result<(), String> {
    for (name, sub) in &spec.subcircuits {
        if name.trim().is_empty() {
            return Err("subcircuit name cannot be empty".into());
        }
        validate_net_names(&sub.nets)?;
        let mut seen_ports = HashSet::with_capacity(sub.ports.len());
        for port in &sub.ports {
            if port.trim().is_empty() || port != port.trim() {
                return Err(format!(
                    "subcircuit '{name}' has empty or untrimmed port '{port}'"
                ));
            }
            if !seen_ports.insert(port.as_str()) {
                return Err(format!("subcircuit '{name}' has duplicate port '{port}'"));
            }
        }
        let mut seen_refs = HashSet::with_capacity(sub.placements.len());
        for pl in &sub.placements {
            let r = pl.refdes.trim();
            if r.is_empty() {
                return Err(format!(
                    "subcircuit '{name}' contains a placement with empty refdes"
                ));
            }
            if !seen_refs.insert(r) {
                return Err(format!(
                    "subcircuit '{name}' has duplicate local placement refdes '{}'",
                    pl.refdes
                ));
            }
        }
        validate_instance_slice(
            &format!("subcircuit '{name}'"),
            &sub.instances,
            &spec.subcircuits,
        )?;
    }
    validate_instance_slice("board", &spec.instances, &spec.subcircuits)?;
    let mut state = HashMap::with_capacity(spec.subcircuits.len());
    let mut memo = HashMap::with_capacity(spec.subcircuits.len());
    for name in spec.subcircuits.keys() {
        check_subcircuit_dag(name, &spec.subcircuits, &mut state, &mut memo)?;
    }
    Ok(())
}

fn expand_constraints(
    sub_constraints: &Constraints,
    eff_prefix: &str,
    scope_ports: &BTreeMap<String, String>,
    ctx: &mut ExpandCtx,
) -> Result<(), String> {
    for r in &sub_constraints.max_lengths {
        let nets = ctx.nets.resolve_all(&r.nets, eff_prefix, scope_ports)?;
        ctx.out.constraints.max_lengths.push(MaxLengthRule {
            nets,
            max_nm: r.max_nm,
        });
    }
    for r in &sub_constraints.match_groups {
        let mg_name = join_prefix(eff_prefix, &r.name);
        if !ctx.match_group_names.insert(mg_name.clone()) {
            return Err(format!("duplicate match_group name '{mg_name}'"));
        }
        let nets = ctx.nets.resolve_all(&r.nets, eff_prefix, scope_ports)?;
        ctx.out.constraints.match_groups.push(MatchGroupRule {
            name: mg_name,
            nets,
            tolerance_nm: r.tolerance_nm,
            target_nm: r.target_nm,
        });
    }
    for r in &sub_constraints.cap_proximity {
        let net = if r.net.trim().is_empty() {
            String::new()
        } else {
            ctx.nets.resolve(&r.net, eff_prefix, scope_ports)?
        };
        ctx.out.constraints.cap_proximity.push(CapProximityRule {
            cap_refdes: join_prefix(eff_prefix, &r.cap_refdes),
            ic_refdes: join_prefix(eff_prefix, &r.ic_refdes),
            net,
            max_dist_nm: r.max_dist_nm,
        });
    }
    for r in &sub_constraints.min_vias {
        let net = ctx.nets.resolve(&r.net, eff_prefix, scope_ports)?;
        ctx.out.constraints.min_vias.push(MinViasRule {
            net,
            refdes: r.refdes.as_ref().map(|rf| join_prefix(eff_prefix, rf)),
            min_vias: r.min_vias,
        });
    }
    for r in &sub_constraints.max_vias {
        let nets = ctx.nets.resolve_all(&r.nets, eff_prefix, scope_ports)?;
        ctx.out.constraints.max_vias.push(MaxViasRule {
            nets,
            max_vias: r.max_vias,
        });
    }
    for r in &sub_constraints.bga_shadows {
        ctx.out.constraints.bga_shadows.push(BgaShadowRule {
            bga_refdes: join_prefix(eff_prefix, &r.bga_refdes),
            min_clearance_nm: r.min_clearance_nm,
            allow_passives: r.allow_passives,
        });
    }
    Ok(())
}

fn expand_instance(
    spec: &BoardSpec,
    inst: &SubcircuitInstance,
    scope: &ExpandScope,
    ctx: &mut ExpandCtx,
) -> Result<(), String> {
    ctx.instance_count += 1;
    if ctx.instance_count > MAX_EXPANDED_INSTANCES {
        return Err(format!(
            "subcircuit expansion exceeds max {MAX_EXPANDED_INSTANCES} instances"
        ));
    }
    if scope.depth > MAX_SUBCIRCUIT_DEPTH {
        return Err(format!(
            "subcircuit expansion exceeds max depth {MAX_SUBCIRCUIT_DEPTH}"
        ));
    }
    let sub = &spec.subcircuits[&inst.subcircuit];
    let eff_prefix = join_prefix(&scope.prefix, normalize_prefix(&inst.prefix)?);
    let mut child_ports = BTreeMap::new();
    for (port, raw_target) in &inst.ports {
        child_ports.insert(
            port.clone(),
            ctx.nets.resolve(raw_target, &scope.prefix, &scope.ports)?,
        );
    }

    let world_origin = local_to_board(
        inst.pos,
        scope.pos.x,
        scope.pos.y,
        scope.rot_deg,
        scope.side,
    );
    let (world_rot, world_side) =
        compose_rot_side(scope.rot_deg, scope.side, inst.rot_deg, inst.side);
    let start_pl = ctx.out.placements.len();

    for pl in &sub.placements {
        let mut new_pl = pl.clone();
        new_pl.refdes = join_prefix(&eff_prefix, &pl.refdes);
        new_pl.pos = local_to_board(
            pl.pos,
            world_origin.x,
            world_origin.y,
            world_rot,
            world_side,
        );
        (new_pl.rot_deg, new_pl.side) =
            compose_rot_side(world_rot, world_side, pl.rot_deg, pl.side);
        for net in new_pl.pin_nets.values_mut() {
            if !net.trim().is_empty() {
                *net = ctx.nets.resolve(net, &eff_prefix, &child_ports)?;
            }
        }
        ctx.out.placements.push(new_pl);
    }

    expand_constraints(&sub.constraints, &eff_prefix, &child_ports, ctx)?;
    let child_scope = ExpandScope {
        prefix: eff_prefix.clone(),
        ports: child_ports.clone(),
        pos: world_origin,
        rot_deg: world_rot,
        side: world_side,
        depth: scope.depth + 1,
    };
    for child in &sub.instances {
        expand_instance(spec, child, &child_scope, ctx)?;
    }

    let subtree_refs: HashSet<&str> = ctx.out.placements[start_pl..]
        .iter()
        .map(|p| p.refdes.as_str())
        .collect();
    for (local_net, sub_nd) in &sub.nets {
        if local_net.trim().is_empty() {
            continue;
        }
        if let Some((r, _)) = sub_nd
            .pins
            .iter()
            .find(|(r, _)| !subtree_refs.contains(join_prefix(&eff_prefix, r).as_str()))
        {
            return Err(format!(
                "subcircuit '{}' net '{local_net}' references unknown local placement '{r}'",
                inst.subcircuit
            ));
        }
        let resolved_net = ctx.nets.resolve(local_net, &eff_prefix, &child_ports)?;
        let entry = ctx
            .out
            .nets
            .entry(resolved_net.clone())
            .or_insert_with(|| NetDef {
                name: resolved_net.clone(),
                ..Default::default()
            });
        if !is_default_net_class(&sub_nd.class) {
            if is_default_net_class(&entry.class) {
                entry.class = sub_nd.class.clone();
            } else if entry.class != sub_nd.class {
                return Err(format!(
                    "net '{resolved_net}' has conflicting net classes '{}' and '{}'",
                    entry.class, sub_nd.class
                ));
            }
        }
        entry.ignore_route |= sub_nd.ignore_route;
        entry.power_driven |= sub_nd.power_driven;
        for (r, p) in &sub_nd.pins {
            let exp_ref = join_prefix(&eff_prefix, r);
            entry.pins.push((exp_ref.clone(), p.clone()));
            ctx.out
                .sub_net_pins
                .push((resolved_net.clone(), exp_ref, p.clone()));
        }
    }
    Ok(())
}

pub(crate) fn expand_subcircuits(spec: &BoardSpec) -> Result<ExpandedSpec, String> {
    validate_net_names(&spec.nets)?;
    validate_subcircuits(spec)?;

    let board_nets = spec
        .nets
        .keys()
        .map(String::as_str)
        .chain(
            spec.placements
                .iter()
                .flat_map(|p| p.pin_nets.values().map(String::as_str)),
        )
        .chain(
            spec.instances
                .iter()
                .flat_map(|i| i.ports.values().map(String::as_str)),
        )
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(str::to_string)
        .collect();

    let mut ctx = ExpandCtx {
        out: ExpandedSpec {
            placements: spec.placements.clone(),
            nets: spec.nets.clone(),
            constraints: spec.constraints.clone(),
            sub_net_pins: Vec::new(),
        },
        nets: NetScope {
            board_nets,
            private_nets: HashMap::new(),
        },
        match_group_names: spec
            .constraints
            .match_groups
            .iter()
            .map(|r| r.name.trim().to_string())
            .collect(),
        instance_count: 0,
    };

    let root = ExpandScope::default();
    for inst in &spec.instances {
        expand_instance(spec, inst, &root, &mut ctx)?;
    }

    let mut seen_refs = HashSet::with_capacity(ctx.out.placements.len());
    for p in &ctx.out.placements {
        let r = p.refdes.trim();
        if !r.is_empty() && !seen_refs.insert(r) {
            return Err(format!("duplicate placement refdes '{}'", p.refdes));
        }
    }
    Ok(ctx.out)
}
