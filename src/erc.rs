use std::collections::BTreeMap;

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

struct LogicalPin<'a> {
    label: String,
    pin_type: Option<PinType>,
    pos_nm: Pt,
    _refdes: &'a str,
}

fn is_kicad_unconnected_net(net: &str) -> bool {
    net.starts_with("unconnected-(")
}

fn pin_labels(pins: &[&LogicalPin<'_>]) -> Vec<String> {
    let mut out: Vec<String> = pins.iter().map(|p| p.label.clone()).collect();
    out.sort();
    out
}

/// Runs native electrical rules checks across nets and optional footprint/placement pin types.
pub fn check_board_erc(board: &BoardIr) -> ErcReport {
    let mut violations = Vec::new();
    let mut net_pins: BTreeMap<&str, Vec<LogicalPin<'_>>> = BTreeMap::new();
    let mut typed_pins = 0usize;

    for pl in &board.placements {
        if pl.dnp {
            continue;
        }
        let Some(fp) = board.footprints.get(&pl.footprint) else {
            continue;
        };
        // Deduplicate multiple physical copper pads sharing a pin name into one logical pin.
        let mut logical_pads: BTreeMap<&str, Pt> = BTreeMap::new();
        for pad in &fp.pads {
            if pad.kind == PadKind::Npth {
                continue;
            }
            let pin_name = pad.name.trim();
            if pin_name.is_empty() {
                continue;
            }
            logical_pads
                .entry(pin_name)
                .or_insert_with(|| transform_pad_local_pt(Pt::new(0, 0), pad, pl));
        }

        for (pin_name, pos_nm) in logical_pads {
            let pin_type = pl
                .pin_types
                .get(pin_name)
                .or_else(|| fp.pin_types.get(pin_name))
                .copied();
            if pin_type.is_some() {
                typed_pins += 1;
            }
            let net = pl
                .pin_nets
                .get(pin_name)
                .map(String::as_str)
                .map(str::trim)
                .filter(|s| !s.is_empty());
            let label = if pl.refdes.trim().is_empty() {
                format!("<unnamed>.{pin_name}")
            } else {
                format!("{}.{pin_name}", pl.refdes)
            };

            match net {
                Some(net_name) if is_kicad_unconnected_net(net_name) => {}
                Some(net_name) => {
                    if pin_type == Some(PinType::Nc) {
                        violations.push(ErcViolation {
                            kind: ErcViolationKind::ConnectedNcPin,
                            severity: Severity::Error,
                            net: net_name.to_string(),
                            pins: vec![label.clone()],
                            pos_nm,
                            message: format!("nc pin {label} is connected to net '{net_name}'"),
                        });
                    }
                    net_pins.entry(net_name).or_default().push(LogicalPin {
                        label,
                        pin_type,
                        pos_nm,
                        _refdes: &pl.refdes,
                    });
                }
                None => {
                    if let Some(pt) = pin_type
                        && pt != PinType::Nc
                    {
                        violations.push(ErcViolation {
                            kind: ErcViolationKind::UnconnectedPin,
                            severity: Severity::Error,
                            net: String::new(),
                            pins: vec![label.clone()],
                            pos_nm,
                            message: format!("unconnected {} pin {label}", pt.as_str()),
                        });
                    }
                }
            }
        }
    }

    let checked_nets = net_pins.len();
    for (net_name, pins) in &net_pins {
        if pins.len() == 1 && pins[0].pin_type != Some(PinType::Nc) {
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

        let outs: Vec<&LogicalPin<'_>> = pins
            .iter()
            .filter(|p| p.pin_type == Some(PinType::Out))
            .collect();
        let power_outs: Vec<&LogicalPin<'_>> = pins
            .iter()
            .filter(|p| p.pin_type == Some(PinType::PowerOut))
            .collect();
        let tris: Vec<&LogicalPin<'_>> = pins
            .iter()
            .filter(|p| p.pin_type == Some(PinType::Tri))
            .collect();
        let ins: Vec<&LogicalPin<'_>> = pins
            .iter()
            .filter(|p| p.pin_type == Some(PinType::In))
            .collect();
        let power_ins: Vec<&LogicalPin<'_>> = pins
            .iter()
            .filter(|p| p.pin_type == Some(PinType::PowerIn))
            .collect();

        if power_outs.len() >= 2 {
            let labels = pin_labels(&power_outs);
            violations.push(ErcViolation {
                kind: ErcViolationKind::PowerOutputConflict,
                severity: Severity::Error,
                net: (*net_name).to_string(),
                pos_nm: power_outs[0].pos_nm,
                message: format!(
                    "multiple power_out pins on net '{net_name}': {}",
                    labels.join(", ")
                ),
                pins: labels,
            });
        }
        if outs.len() >= 2 {
            let labels = pin_labels(&outs);
            violations.push(ErcViolation {
                kind: ErcViolationKind::OutputConflict,
                severity: Severity::Error,
                net: (*net_name).to_string(),
                pos_nm: outs[0].pos_nm,
                message: format!(
                    "multiple out pins on net '{net_name}': {}",
                    labels.join(", ")
                ),
                pins: labels,
            });
        }
        if !outs.is_empty() && !power_outs.is_empty() {
            let mut both = outs.clone();
            both.extend_from_slice(&power_outs);
            let labels = pin_labels(&both);
            violations.push(ErcViolation {
                kind: ErcViolationKind::OutputConflict,
                severity: Severity::Error,
                net: (*net_name).to_string(),
                pos_nm: outs[0].pos_nm,
                message: format!(
                    "out and power_out pins conflict on net '{net_name}': {}",
                    labels.join(", ")
                ),
                pins: labels,
            });
        }
        if !tris.is_empty() && (!outs.is_empty() || !power_outs.is_empty()) {
            let mut mixed = outs.clone();
            mixed.extend_from_slice(&power_outs);
            mixed.extend_from_slice(&tris);
            let labels = pin_labels(&mixed);
            violations.push(ErcViolation {
                kind: ErcViolationKind::OutputTriStateConflict,
                severity: Severity::Warning,
                net: (*net_name).to_string(),
                pos_nm: tris[0].pos_nm,
                message: format!(
                    "tri-state pin shares net '{net_name}' with driven output: {}",
                    labels.join(", ")
                ),
                pins: labels,
            });
        }

        let has_signal_driver = pins.iter().any(|p| {
            matches!(
                p.pin_type,
                Some(
                    PinType::Out
                        | PinType::Bidir
                        | PinType::Tri
                        | PinType::PowerOut
                        | PinType::Passive
                )
            )
        });
        if !ins.is_empty() && !has_signal_driver {
            let labels = pin_labels(&ins);
            violations.push(ErcViolation {
                kind: ErcViolationKind::UndrivenInput,
                severity: Severity::Error,
                net: (*net_name).to_string(),
                pos_nm: ins[0].pos_nm,
                message: format!(
                    "undriven in pin(s) on net '{net_name}': {}",
                    labels.join(", ")
                ),
                pins: labels,
            });
        }

        if !power_ins.is_empty() && power_outs.is_empty() {
            let labels = pin_labels(&power_ins);
            violations.push(ErcViolation {
                kind: ErcViolationKind::UndrivenPowerInput,
                severity: Severity::Error,
                net: (*net_name).to_string(),
                pos_nm: power_ins[0].pos_nm,
                message: format!(
                    "undriven power_in pin(s) on net '{net_name}': {}",
                    labels.join(", ")
                ),
                pins: labels,
            });
        }
    }

    violations.sort_by(|a, b| {
        (a.severity, a.kind, a.net.as_str(), a.pos_nm.x, a.pos_nm.y)
            .cmp(&(b.severity, b.kind, b.net.as_str(), b.pos_nm.x, b.pos_nm.y))
            .then_with(|| a.pins.cmp(&b.pins))
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
