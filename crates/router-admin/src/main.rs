use std::io::Read;

use anyhow::{bail, Context, Result};
use serde_json::Value as J;
use toml_edit::{value, Array, ArrayOfTables, DocumentMut, InlineTable, Item, Table, Value};

fn main() -> Result<()> {
    let args: Vec<String> = std::env::args().collect();
    let (cmd, path) = match (args.get(1).map(String::as_str), args.get(2)) {
        (Some(c), Some(p)) => (c, p.clone()),
        _ => bail!("usage: router-admin <dump|apply> <toml-path>"),
    };

    match cmd {
        "dump" => dump(&path),
        "apply" => apply(&path),
        other => bail!("unknown command '{other}' (expected dump|apply)"),
    }
}

/// Load + validate against the real router schema, then emit as JSON so the
/// Swift side never has to understand TOML.
fn dump(path: &str) -> Result<()> {
    let cfg = router_config::Config::load(path)
        .with_context(|| format!("loading config from {path}"))?;
    let json = serde_json::to_string_pretty(&cfg)?;
    println!("{json}");
    Ok(())
}

/// Read the desired config as JSON on stdin, merge it into the existing TOML
/// document in place (comments + ordering survive), and write it back.
fn apply(path: &str) -> Result<()> {
    let mut stdin = String::new();
    std::io::stdin().read_to_string(&mut stdin)?;

    // Validate the payload really is a config before we touch the file.
    let _: router_config::Config =
        serde_json::from_str(&stdin).context("stdin is not a valid router config")?;
    let incoming: J = serde_json::from_str(&stdin)?;
    let obj = incoming.as_object().context("config JSON must be an object")?;

    let existing = std::fs::read_to_string(path)
        .with_context(|| format!("reading {path}"))?;
    let mut doc: DocumentMut = existing.parse().context("existing TOML is malformed")?;

    merge_table(doc.as_table_mut(), obj);

    // Backup before overwriting — struct-based tools should never eat a config
    // without a way back.
    std::fs::write(format!("{path}.bak"), &existing)?;
    std::fs::write(path, doc.to_string())?;
    Ok(())
}

/// Recursively merge a JSON object into a toml_edit table. Unchanged subtrees
/// (and their comments) are left untouched; only differing leaves are rewritten.
fn merge_table(dst: &mut Table, src: &serde_json::Map<String, J>) {
    // Drop keys the incoming config no longer has (e.g. a removed backend).
    let stale: Vec<String> = dst
        .iter()
        .map(|(k, _)| k.to_string())
        .filter(|k| !src.contains_key(k))
        .collect();
    for k in stale {
        dst.remove(&k);
    }

    for (k, v) in src {
        match v {
            // null / empty list == "not set"; keep the file lean.
            J::Null => {
                dst.remove(k);
            }
            J::Array(a) if a.is_empty() => {
                dst.remove(k);
            }
            // Array of objects -> [[table]] blocks. Merge element-wise into an
            // existing array so its header decor (the comments above the first
            // [[table]]) survives; only rebuild if it wasn't one before.
            J::Array(a) if a.iter().all(J::is_object) => match dst.get_mut(k) {
                Some(Item::ArrayOfTables(aot)) => merge_aot(aot, a),
                _ => {
                    dst.insert(k, array_of_tables(a));
                }
            },
            J::Object(o) => match dst.get_mut(k) {
                Some(Item::Table(t)) => merge_table(t, o),
                Some(Item::Value(Value::InlineTable(it))) => merge_inline(it, o),
                _ => {
                    dst.insert(k, new_table_item(o));
                }
            },
            _ => set_scalar(dst, k, v),
        }
    }
}

fn merge_inline(dst: &mut InlineTable, src: &serde_json::Map<String, J>) {
    let stale: Vec<String> = dst
        .iter()
        .map(|(k, _)| k.to_string())
        .filter(|k| !src.contains_key(k))
        .collect();
    for k in stale {
        dst.remove(&k);
    }
    for (k, v) in src {
        match v {
            J::Null => {
                dst.remove(k);
            }
            _ => {
                dst.insert(k, json_to_value(v));
            }
        }
    }
}

/// Replace a scalar/array leaf while preserving the key's inline decor (trailing
/// comments). Keeps float typing when the original was a float but the JSON
/// round-trip demoted it to an integer (e.g. weight 1.0 -> 1).
fn set_scalar(dst: &mut Table, k: &str, v: &J) {
    let mut nv = json_to_value(v);
    if let Some(Item::Value(existing)) = dst.get_mut(k) {
        if existing.is_float() {
            if let Value::Integer(i) = &nv {
                nv = Value::from(*i.value() as f64);
            }
        }
        let decor = existing.decor().clone();
        *existing = nv;
        *existing.decor_mut() = decor;
    } else {
        dst.insert(k, value(nv));
    }
}

fn new_table_item(o: &serde_json::Map<String, J>) -> Item {
    let mut t = Table::new();
    merge_table(&mut t, o);
    Item::Table(t)
}

/// Merge into an existing array-of-tables in place: recurse into overlapping
/// entries (keeping their decor), append new ones, drop trailing removed ones.
fn merge_aot(aot: &mut ArrayOfTables, src: &[J]) {
    while aot.len() > src.len() {
        aot.remove(aot.len() - 1);
    }
    for (i, e) in src.iter().enumerate() {
        let Some(o) = e.as_object() else { continue };
        if let Some(t) = aot.get_mut(i) {
            merge_table(t, o);
        } else {
            let mut t = Table::new();
            merge_table(&mut t, o);
            aot.push(t);
        }
    }
}

fn array_of_tables(a: &[J]) -> Item {
    let mut aot = ArrayOfTables::new();
    for e in a {
        if let J::Object(o) = e {
            let mut t = Table::new();
            merge_table(&mut t, o);
            aot.push(t);
        }
    }
    Item::ArrayOfTables(aot)
}

fn json_to_value(v: &J) -> Value {
    match v {
        J::Bool(b) => Value::from(*b),
        J::Number(n) => n
            .as_i64()
            .map(Value::from)
            .unwrap_or_else(|| Value::from(n.as_f64().unwrap_or(0.0))),
        J::String(s) => Value::from(s.clone()),
        J::Array(a) => {
            let mut arr = Array::new();
            for e in a {
                arr.push(json_to_value(e));
            }
            Value::Array(arr)
        }
        J::Object(o) => {
            let mut it = InlineTable::new();
            for (k, e) in o {
                it.insert(k, json_to_value(e));
            }
            Value::InlineTable(it)
        }
        J::Null => Value::from(""),
    }
}
