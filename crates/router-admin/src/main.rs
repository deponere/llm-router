use std::io::Read;

use anyhow::{bail, Context, Result};
use serde_json::Value as J;
use toml_edit::{value, Array, ArrayOfTables, DocumentMut, InlineTable, Item, Table, Value};

fn main() -> Result<()> {
    let args: Vec<String> = std::env::args().collect();
    match args.get(1).map(String::as_str) {
        Some("dump") => {
            let path = args.get(2).context("usage: router-admin dump <toml-path>")?;
            dump(path)
        }
        Some("apply") => {
            let path = args.get(2).context("usage: router-admin apply <toml-path>")?;
            apply(path)
        }
        Some("auth") => auth_cmd(&args[2..]),
        Some("alerts") => alerts_cmd(&args[2..]),
        other => bail!(
            "unknown command '{other:?}' — expected dump|apply <path> | auth add|list|rm <path> … | alerts test"
        ),
    }
}

/// `router-admin auth add <toml-path> <name> [--daily X] [--monthly Y]`
/// Erzeugt einen API-Key: SHA-256-Hash in die Config ([[auth.keys]]), Plaintext genau einmal ausgeben.
fn auth_cmd(args: &[String]) -> Result<()> {
    let (sub, path) = match (args.first().map(String::as_str), args.get(1)) {
        (Some(s), Some(p)) => (s, p.clone()),
        _ => bail!("usage: router-admin auth <add|list|rm> <toml-path> [name] [--daily X] [--monthly Y]"),
    };
    match sub {
        "list" => {
            let cfg = router_config::Config::load(&path)
                .with_context(|| format!("loading config from {path}"))?;
            println!("auth.enabled = {}", cfg.auth.enabled);
            for k in &cfg.auth.keys {
                let prefix: String = k.hash.chars().take(16).collect();
                println!(
                    "  {}  {}…  daily={:?} monthly={:?} profile={:?}",
                    k.name, prefix, k.daily_budget_usd, k.monthly_budget_usd, k.profile
                );
            }
            if cfg.auth.keys.is_empty() {
                println!("  (keine Keys konfiguriert)");
            }
            Ok(())
        }
        "add" => {
            let name = args
                .get(2)
                .map(String::as_str)
                .filter(|s| !s.is_empty())
                .context("usage: router-admin auth add <path> <name> [--daily X] [--monthly Y] [--profile P]")?;
            let mut daily = None;
            let mut monthly = None;
            let mut profile = None;
            let mut i = 3;
            while i < args.len() {
                match args[i].as_str() {
                    "--daily" => {
                        daily = Some(args.get(i + 1).context("--daily braucht einen Wert")?.parse::<f64>()?);
                        i += 2;
                    }
                    "--monthly" => {
                        monthly = Some(args.get(i + 1).context("--monthly braucht einen Wert")?.parse::<f64>()?);
                        i += 2;
                    }
                    "--profile" => {
                        profile = Some(args.get(i + 1).context("--profile braucht einen Wert")?.clone());
                        i += 2;
                    }
                    other => bail!("unbekanntes Argument '{other}'"),
                }
            }
            let (plain, hash) = generate_key();
            let existing = std::fs::read_to_string(&path)
                .with_context(|| format!("reading {path}"))?;
            let mut doc: DocumentMut = existing.parse().context("TOML malformed")?;
            add_auth_key(&mut doc, name, &hash, daily, monthly, profile.as_deref())?;
            std::fs::write(format!("{path}.bak"), &existing)?;
            std::fs::write(&path, doc.to_string())?;
            println!("Key '{name}' angelegt (auth.enabled = true gesetzt).");
            println!("Plaintext-Key — JETZT kopieren, wird nie wieder angezeigt:");
            println!("{plain}");
            Ok(())
        }
        "rm" => {
            let name = args
                .get(2)
                .map(String::as_str)
                .context("usage: router-admin auth rm <path> <name>")?;
            let existing = std::fs::read_to_string(&path)
                .with_context(|| format!("reading {path}"))?;
            let mut doc: DocumentMut = existing.parse().context("TOML malformed")?;
            let removed = remove_auth_key(&mut doc, name)?;
            if !removed {
                bail!("Key '{name}' nicht gefunden");
            }
            std::fs::write(format!("{path}.bak"), &existing)?;
            std::fs::write(&path, doc.to_string())?;
            println!("Key '{name}' entfernt.");
            Ok(())
        }
        other => bail!("unbekanntes auth-Kommando '{other}' (erwartet add|list|rm)"),
    }
}

/// `router-admin alerts test` — feuert einen Test-Alert über den laufenden Router.
fn alerts_cmd(args: &[String]) -> Result<()> {
    match args.first().map(String::as_str) {
        Some("test") => {
            let url = "http://127.0.0.1:4123/v1/admin/alerts/test";
            println!("Test-Alert an {url} …");
            let out = std::process::Command::new("curl")
                .args(["-s", "-X", "POST", url])
                .output()
                .context("curl nicht verfügbar?")?;
            println!("{}", String::from_utf8_lossy(&out.stdout).trim());
            if !out.status.success() {
                bail!("Router nicht erreichbar? (status {})", out.status);
            }
            Ok(())
        }
        other => bail!("usage: router-admin alerts test (anderes: '{other:?}')"),
    }
}

fn generate_key() -> (String, String) {
    use std::time::{SystemTime, UNIX_EPOCH};
    use sha2::{Digest, Sha256};
    const CHARS: &[u8] = b"0123456789ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz";
    let seed = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_nanos() as u64)
        .unwrap_or(0x9e37_79b9)
        ^ std::process::id() as u64;
    let mut state = seed.max(1);
    let mut plain = String::with_capacity(27);
    plain.push_str("rk_");
    for _ in 0..24 {
        state ^= state << 13;
        state ^= state >> 7;
        state ^= state << 17;
        plain.push(CHARS[(state % 62) as usize] as char);
    }
    let mut h = Sha256::new();
    h.update(plain.as_bytes());
    let hex: String = h.finalize().iter().map(|b| format!("{b:02x}")).collect();
    (plain, format!("sha256:{hex}"))
}

fn add_auth_key(doc: &mut DocumentMut, name: &str, hash: &str, daily: Option<f64>, monthly: Option<f64>, profile: Option<&str>) -> Result<()> {
    if !doc.contains_key("auth") {
        doc.insert("auth", Item::Table(Table::new()));
    }
    let auth = doc
        .get_mut("auth")
        .and_then(|i| i.as_table_mut())
        .context("auth ist keine Tabelle")?;
    auth.insert("enabled", Item::Value(Value::from(true)));
    if !auth.contains_key("keys") {
        auth.insert("keys", Item::ArrayOfTables(ArrayOfTables::new()));
    }
    let keys = match auth.get_mut("keys") {
        Some(Item::ArrayOfTables(aot)) => aot,
        _ => bail!("auth.keys ist kein [[array-of-tables]]"),
    };
    if keys.iter().any(|t| t.get("name").and_then(|n| n.as_str()) == Some(name)) {
        bail!("Key '{name}' existiert bereits");
    }
    let mut t = Table::new();
    t.insert("name", Item::Value(Value::from(name)));
    t.insert("hash", Item::Value(Value::from(hash)));
    if let Some(d) = daily {
        t.insert("daily_budget_usd", Item::Value(Value::from(d)));
    }
    if let Some(m) = monthly {
        t.insert("monthly_budget_usd", Item::Value(Value::from(m)));
    }
    if let Some(p) = profile {
        t.insert("profile", Item::Value(Value::from(p)));
    }
    keys.push(t);
    Ok(())
}

fn remove_auth_key(doc: &mut DocumentMut, name: &str) -> Result<bool> {
    let Some(keys) = doc
        .get_mut("auth")
        .and_then(|i| i.as_table_mut())
        .and_then(|t| t.get_mut("keys"))
        .and_then(|i| i.as_array_of_tables_mut())
    else {
        return Ok(false);
    };
    let before = keys.len();
    keys.retain(|t| t.get("name").and_then(|n| n.as_str()) != Some(name));
    Ok(keys.len() < before)
}

/// Load + validate against the real router schema, then emit as JSON so Swift never has to understand TOML.
fn dump(path: &str) -> Result<()> {
    let cfg = router_config::Config::load(path)
        .with_context(|| format!("loading config from {path}"))?;
    let json = serde_json::to_string_pretty(&cfg)?;
    println!("{json}");
    Ok(())
}

/// Read the desired config as JSON on stdin, merge into the existing TOML document in place (comments + ordering survive), and write it back.
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

    // Backup before overwriting — never eat a config without a way back.
    std::fs::write(format!("{path}.bak"), &existing)?;
    std::fs::write(path, doc.to_string())?;
    Ok(())
}

/// Recursively merge a JSON object into a toml_edit table; unchanged subtrees (and their comments) are left untouched, only differing leaves rewritten.
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
            // Array of objects -> [[table]] blocks; merge element-wise into an existing array so its header decor survives, only rebuild if it wasn't one before.
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

/// Replace a scalar/array leaf while preserving the key's inline decor; keeps float typing when the JSON round-trip demoted a float to an integer (e.g. weight 1.0 -> 1).
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

/// Merge into an existing array-of-tables in place: recurse into overlapping entries (keeping decor), append new ones, drop trailing removed ones.
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
