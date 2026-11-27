use std::collections::{BTreeMap, HashMap, HashSet};
use std::fmt;

use serde::{Deserialize, Serialize};

use crate::drc::Severity;
use crate::ir::{BoardIr, PadKind, PinType, Pt};
use crate::shapes::transform_pad_local_pt;

/// Categories of electrical rule check (ERC) violations.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ErcViolationKind {
    SinglePinNet,
    OutputConflict,
    PowerOutputConflict,
    OutputTriStateConflict,
    UndrivenInput,
    UndrivenPowerInput,
    UnconnectedPin,
    ConnectedNcPin,
}

/// Atomic ERC diagnostic on a net or component pin.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ErcViolation {
    pub kind: ErcViolationKind,
    pub severity: Severity,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub net: String,
    pub pins: Vec<String>,
    pub pos_nm: Pt,
    pub message: String,
}

/// Summary report emitted by `check_board_erc`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ErcReport {
    pub violations: Vec<ErcViolation>,
    pub error_count: usize,
    pub warning_count: usize,
    pub passed: bool,
    pub checked_nets: usize,
    pub typed_pins: usize,
}

struct LogicalPin {
    label: String,
    pin_type: Option<PinType>,
    pos_nm: Pt,
}

fn push_group<'a>(
    violations: &mut Vec<ErcViolation>,
    kind: ErcViolationKind,
    severity: Severity,
    net: &str,
    pins: impl IntoIterator<Item = &'a LogicalPin>,
    prefix: fmt::Arguments<'_>,
) {
    let mut ordered: Vec<&LogicalPin> = pins.into_iter().collect();
    ordered.sort_by(|a, b| {
        a.label
            .cmp(&b.label)
            .then_with(|| (a.pos_nm.x, a.pos_nm.y).cmp(&(b.pos_nm.x, b.pos_nm.y)))
    });
    let Some(first) = ordered.first() else {
        return;
    };
    let pos_nm = first.pos_nm;
    let labels: Vec<String> = ordered.into_iter().map(|p| p.label.clone()).collect();
    let message = format!("{prefix}: {}", labels.join(", "));
    violations.push(ErcViolation {
        kind,
        severity,
        net: net.to_string(),
        pins: labels,
        pos_nm,
        message,
    });
}

fn is_unconnected_net(net: &str) -> bool {
    const PREFIX: &[u8] = b"unconnected-(";
    let bytes = net.as_bytes();
    bytes.len() >= PREFIX.len() && bytes[..PREFIX.len()].eq_ignore_ascii_case(PREFIX)
}

/// Runs native electrical rules checks across nets and optional footprint/placement pin types.
pub fn check_board_erc(board: &BoardIr) -> ErcReport {
    let mut violations = Vec::new();
    let mut net_pins: BTreeMap<&str, Vec<LogicalPin>> = BTreeMap::new();
    let mut physical_net_pins: HashMap<&str, usize> = HashMap::new();
    let mut typed_pins = 0usize;

    // Fallback pin-to-net index from board.nets so direct BoardIr JSON without mirrored pin_nets still works.
    let mut net_def_pins: HashMap<(&str, &str), &str> = HashMap::new();
    for (net_name, nd) in &board.nets {
        if net_name.trim().is_empty() {
            continue;
        }
        for (refdes, pin) in &nd.pins {
            net_def_pins
                .entry((refdes.as_str(), pin.as_str()))
                .and_modify(|cur| {
                    if is_unconnected_net(cur) {
                        *cur = net_name.as_str();
                    }
                })
                .or_insert(net_name.as_str());
        }
    }

    let copper_nets: HashSet<&str> = board
        .pours
        .iter()
        .map(|p| p.net.as_str())
        .chain(board.filled_pours.iter().map(|p| p.net.as_str()))
        .chain(board.tracks.iter().map(|t| t.net.as_str()))
        .filter(|s| !s.trim().is_empty())
        .collect();

    for pl in &board.placements {
        let Some(fp) = board.footprints.get(&pl.footprint) else {
            continue;
        };
        let refdes = if pl.refdes.trim().is_empty() {
            "<unnamed>"
        } else {
            pl.refdes.as_str()
        };
        let mut seen_pins: HashSet<&str> = HashSet::new();

        for pad in &fp.pads {
            if pad.kind == PadKind::Npth || pad.name.is_empty() || !seen_pins.insert(&pad.name) {
                continue;
            }
            let pin_name = pad.name.as_str();
            let net = pl
                .pin_nets
                .get(pin_name)
                .map(String::as_str)
                .or_else(|| net_def_pins.get(&(pl.refdes.as_str(), pin_name)).copied())
                .filter(|s| !s.trim().is_empty());

            // Count DNP pads physically (avoiding false single-pin nets) but skip them in electrical checks.
            if let Some(net_name) = net {
                *physical_net_pins.entry(net_name).or_insert(0) += 1;
            }
            if pl.dnp {
                continue;
            }

            let pin_type = pl
                .pin_types
                .get(pin_name)
                .or_else(|| fp.pin_types.get(pin_name))
                .copied();
            if pin_type.is_some() {
                typed_pins += 1;
            }

            match net {
                Some(net_name) => {
                    let pos_nm = transform_pad_local_pt(Pt::new(0, 0), pad, pl);
                    let label = format!("{refdes}.{pin_name}");
                    net_pins.entry(net_name).or_default().push(LogicalPin {
                        label,
                        pin_type,
                        pos_nm,
                    });
                }
                None => {
                    if let Some(pt) = pin_type
                        && pt != PinType::Nc
                    {
                        let pos_nm = transform_pad_local_pt(Pt::new(0, 0), pad, pl);
                        let label = format!("{refdes}.{pin_name}");
                        let message = format!("unconnected {} pin {label}", pt.as_str());
                        violations.push(ErcViolation {
                            kind: ErcViolationKind::UnconnectedPin,
                            severity: Severity::Error,
                            net: String::new(),
                            pins: vec![label],
                            pos_nm,
                            message,
                        });
                    }
                }
            }
        }
    }

    let mut checked_nets = 0usize;
    for (net_name, pins) in &net_pins {
        let power_driven = board.nets.get(*net_name).is_some_and(|nd| nd.power_driven);
        // Only treat single-pin, copper-free, non-power_driven unconnected-(...) nets as intentional nc.
        if is_unconnected_net(net_name)
            && physical_net_pins.get(net_name).copied().unwrap_or(0) <= 1
            && !copper_nets.contains(net_name)
            && !power_driven
        {
            continue;
        }
        checked_nets += 1;

        if pins.len() == 1
            && physical_net_pins.get(net_name).copied().unwrap_or(0) == 1
            && !copper_nets.contains(net_name)
            && pins[0].pin_type != Some(PinType::Nc)
        {
            let p = &pins[0];
            violations.push(ErcViolation {
                kind: ErcViolationKind::SinglePinNet,
                severity: Severity::Error,
                net: (*net_name).to_string(),
                pins: vec![p.label.clone()],
                pos_nm: p.pos_nm,
                message: format!("single-pin net '{net_name}' connected only to {}", p.label),
            });
        }

        let mut outs = Vec::new();
        let mut power_outs = Vec::new();
        let mut tris = Vec::new();
        let mut open_collectors = Vec::new();
        let mut ins = Vec::new();
        let mut power_ins = Vec::new();
        let mut has_signal_driver = power_driven;
        let mut has_untyped = false;

        for p in pins {
            match p.pin_type {
                Some(PinType::Out) => {
                    outs.push(p);
                    has_signal_driver = true;
                }
                Some(PinType::PowerOut) => {
                    power_outs.push(p);
                    has_signal_driver = true;
                }
                Some(PinType::Tri) => {
                    tris.push(p);
                    has_signal_driver = true;
                }
                Some(PinType::OpenCollector) => {
                    open_collectors.push(p);
                    has_signal_driver = true;
                }
                Some(PinType::Bidir | PinType::Passive) => {
                    has_signal_driver = true;
                }
                Some(PinType::In) => ins.push(p),
                Some(PinType::PowerIn) => power_ins.push(p),
                Some(PinType::Nc) => {
                    violations.push(ErcViolation {
                        kind: ErcViolationKind::ConnectedNcPin,
                        severity: Severity::Error,
                        net: (*net_name).to_string(),
                        pins: vec![p.label.clone()],
                        pos_nm: p.pos_nm,
                        message: format!("nc pin {} is connected to net '{net_name}'", p.label),
                    });
                }
                None => has_untyped = true,
            }
        }

        if power_outs.len() >= 2 {
            push_group(
                &mut violations,
                ErcViolationKind::PowerOutputConflict,
                Severity::Error,
                net_name,
                power_outs.iter().copied(),
                format_args!("multiple power_out pins on net '{net_name}'"),
            );
        }

        if (!power_outs.is_empty() || power_driven)
            && (!outs.is_empty() || !tris.is_empty() || !open_collectors.is_empty())
        {
            let prefix = if power_outs.is_empty() {
                format_args!(
                    "driven output/tri-state/open-collector conflicts with power_driven rail '{net_name}'"
                )
            } else {
                format_args!(
                    "driven output/tri-state/open-collector conflicts with power_out on net '{net_name}'"
                )
            };
            push_group(
                &mut violations,
                ErcViolationKind::OutputConflict,
                Severity::Error,
                net_name,
                power_outs
                    .iter()
                    .chain(&outs)
                    .chain(&tris)
                    .chain(&open_collectors)
                    .copied(),
                prefix,
            );
        } else {
            // Push-pull out conflicts with open_collector (crowbar short when out drives high and open_collector sinks low).
            if !outs.is_empty() && (outs.len() >= 2 || !open_collectors.is_empty()) {
                let prefix = if open_collectors.is_empty() {
                    format_args!("multiple out pins on net '{net_name}'")
                } else {
                    format_args!("driven out pin conflicts with open_collector on net '{net_name}'")
                };
                push_group(
                    &mut violations,
                    ErcViolationKind::OutputConflict,
                    Severity::Error,
                    net_name,
                    outs.iter().chain(&open_collectors).copied(),
                    prefix,
                );
            }
            if !outs.is_empty() && !tris.is_empty() {
                push_group(
                    &mut violations,
                    ErcViolationKind::OutputTriStateConflict,
                    Severity::Warning,
                    net_name,
                    outs.iter().chain(&tris).copied(),
                    format_args!("tri-state pin shares net '{net_name}' with driven output"),
                );
            }
        }

        // Suppress undriven errors when unannotated pins share the net (partial pin_types adoption).
        if !has_untyped && !ins.is_empty() && !has_signal_driver {
            push_group(
                &mut violations,
                ErcViolationKind::UndrivenInput,
                Severity::Error,
                net_name,
                ins.iter().copied(),
                format_args!("undriven in pin(s) on net '{net_name}'"),
            );
        }
        if !has_untyped && !power_ins.is_empty() && power_outs.is_empty() && !power_driven {
            push_group(
                &mut violations,
                ErcViolationKind::UndrivenPowerInput,
                Severity::Error,
                net_name,
                power_ins.iter().copied(),
                format_args!("undriven power_in pin(s) on net '{net_name}'"),
            );
        }
    }

    violations.sort_by(|a, b| {
        (
            a.severity,
            a.kind,
            a.net.as_str(),
            a.pins.as_slice(),
            a.pos_nm.x,
            a.pos_nm.y,
        )
            .cmp(&(
                b.severity,
                b.kind,
                b.net.as_str(),
                b.pins.as_slice(),
                b.pos_nm.x,
                b.pos_nm.y,
            ))
    });
    let error_count = violations
        .iter()
        .filter(|v| v.severity == Severity::Error)
        .count();
    let warning_count = violations.len() - error_count;
    ErcReport {
        violations,
        error_count,
        warning_count,
        passed: error_count == 0,
        checked_nets,
        typed_pins,
    }
}
