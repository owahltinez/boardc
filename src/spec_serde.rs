use std::collections::BTreeMap;

use serde::Deserialize;

use crate::ir::NetDef;
use crate::spec::FootprintSpec;

#[derive(Deserialize)]
#[serde(untagged)]
enum RawPinRef {
    Str(String),
    Pair([String; 2]),
}

fn parse_pin_pair(raw_ref: &str, raw_pin: &str) -> Result<(String, String), String> {
    let r = raw_ref.trim();
    let p = raw_pin.trim();
    if r.is_empty() || p.is_empty() {
        return Err("pin reference refdes and pin must be non-empty".into());
    }
    Ok((r.to_string(), p.to_string()))
}

fn deserialize_net_pins<'de, D: serde::Deserializer<'de>>(
    de: D,
) -> Result<Vec<(String, String)>, D::Error> {
    let raw = Vec::<RawPinRef>::deserialize(de)?;
    raw.into_iter()
        .map(|item| {
            let res = match item {
                RawPinRef::Str(ref s) => match s.split_once('.') {
                    Some((r, p)) => parse_pin_pair(r, p),
                    None => Err(format!("invalid pin reference '{s}': expected 'REF.PIN'")),
                },
                RawPinRef::Pair([ref r, ref p]) => parse_pin_pair(r, p),
            };
            res.map_err(serde::de::Error::custom)
        })
        .collect()
}

#[derive(Deserialize)]
#[serde(default)]
struct SpecNetDef {
    name: String,
    class: String,
    #[serde(deserialize_with = "deserialize_net_pins")]
    pins: Vec<(String, String)>,
    ignore_route: bool,
    power_driven: bool,
}

impl Default for SpecNetDef {
    fn default() -> Self {
        Self {
            name: String::new(),
            class: crate::ir::default_net_class(),
            pins: Vec::new(),
            ignore_route: false,
            power_driven: false,
        }
    }
}

impl From<SpecNetDef> for NetDef {
    fn from(s: SpecNetDef) -> Self {
        Self {
            name: s.name,
            class: s.class,
            pins: s.pins,
            ignore_route: s.ignore_route,
            power_driven: s.power_driven,
        }
    }
}

struct NetEntry(NetDef);

impl<'de> Deserialize<'de> for NetEntry {
    fn deserialize<D: serde::Deserializer<'de>>(de: D) -> Result<Self, D::Error> {
        struct NetEntryVisitor;
        impl<'de> serde::de::Visitor<'de> for NetEntryVisitor {
            type Value = NetEntry;

            fn expecting(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
                f.write_str("a pin array or a NetDef table")
            }

            fn visit_seq<A: serde::de::SeqAccess<'de>>(
                self,
                seq: A,
            ) -> Result<Self::Value, A::Error> {
                let pins = deserialize_net_pins(serde::de::value::SeqAccessDeserializer::new(seq))?;
                Ok(NetEntry(NetDef {
                    pins,
                    ..Default::default()
                }))
            }

            fn visit_map<A: serde::de::MapAccess<'de>>(
                self,
                map: A,
            ) -> Result<Self::Value, A::Error> {
                let nd =
                    SpecNetDef::deserialize(serde::de::value::MapAccessDeserializer::new(map))?;
                Ok(NetEntry(nd.into()))
            }
        }
        de.deserialize_any(NetEntryVisitor)
    }
}

pub(crate) fn deserialize_nets<'de, D: serde::Deserializer<'de>>(
    de: D,
) -> Result<BTreeMap<String, NetDef>, D::Error> {
    let raw = BTreeMap::<String, NetEntry>::deserialize(de)?;
    Ok(raw.into_iter().map(|(k, NetEntry(nd))| (k, nd)).collect())
}

#[derive(Deserialize)]
#[serde(untagged)]
enum PinTargets {
    One(String),
    Many(Vec<String>),
}

pub(crate) fn deserialize_pin_names<'de, D: serde::Deserializer<'de>>(
    de: D,
) -> Result<BTreeMap<String, Vec<String>>, D::Error> {
    let raw = BTreeMap::<String, PinTargets>::deserialize(de)?;
    Ok(raw
        .into_iter()
        .map(|(k, v)| {
            let list = match v {
                PinTargets::One(s) => vec![s],
                PinTargets::Many(vs) => vs,
            };
            (k, list)
        })
        .collect())
}

pub(crate) fn deserialize_footprints<'de, D: serde::Deserializer<'de>>(
    de: D,
) -> Result<BTreeMap<String, FootprintSpec>, D::Error> {
    let raw = BTreeMap::<String, serde_json::Value>::deserialize(de)?;
    let mut out = BTreeMap::new();
    for (name, mut val) in raw {
        if let Some(map) = val.as_object_mut()
            && !map.contains_key("kind")
            && !map.is_empty()
            && map
                .keys()
                .all(|k| k == "pin_types" || k == "courtyard" || k == "pin_names")
        {
            map.insert("kind".into(), serde_json::Value::String("lib".into()));
        }
        let fp: FootprintSpec = serde_json::from_value(val)
            .map_err(|e| serde::de::Error::custom(format!("footprint '{name}': {e}")))?;
        out.insert(name, fp);
    }
    Ok(out)
}
