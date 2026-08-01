//! Kommentarerhaltende Config-Änderungen in-process (toml_edit) — Basis für
//! die Key-Verwaltung (`POST /v1/admin/keys`) und den Einstellungen-Tab.

use std::path::Path;

use toml_edit::{DocumentMut, Item, Table, Value};

pub struct ConfigEditor;

impl ConfigEditor {
    pub fn load(path: &Path) -> Result<DocumentMut, String> {
        let content = std::fs::read_to_string(path).map_err(|e| e.to_string())?;
        content.parse::<DocumentMut>().map_err(|e| e.to_string())
    }

    pub fn save(path: &Path, doc: &DocumentMut) -> Result<(), String> {
        std::fs::write(path, doc.to_string()).map_err(|e| e.to_string())
    }

    /// Setzt einen verschachtelten Wert (dotted key), legt Zwischentabellen an.
    pub fn set(doc: &mut DocumentMut, dotted: &str, value: &str) -> Result<(), String> {
        let parts: Vec<&str> = dotted.split('.').collect();
        if parts.is_empty() {
            return Err("empty key".into());
        }
        let mut table = doc.as_table_mut();
        for (i, part) in parts.iter().enumerate() {
            if i == parts.len() - 1 {
                table.insert(part, Item::Value(parse_value(value)?));
            } else {
                if !table.contains_key(*part) {
                    table.insert(*part, Item::Table(Table::new()));
                }
                match table.get_mut(*part) {
                    Some(Item::Table(t)) => table = t,
                    _ => return Err(format!("'{part}' ist keine Tabelle")),
                }
            }
        }
        Ok(())
    }

    /// Legt einen API-Key in `auth.keys` an (Array-of-Tables) und aktiviert auth.
    pub fn add_auth_key(
        doc: &mut DocumentMut,
        name: &str,
        hash: &str,
        daily: Option<f64>,
        monthly: Option<f64>,
    ) -> Result<(), String> {
        if !doc.contains_key("auth") {
            doc.insert("auth", Item::Table(Table::new()));
        }
        let auth = doc
            .get_mut("auth")
            .and_then(|i| i.as_table_mut())
            .ok_or("auth ist keine Tabelle")?;
        auth.insert("enabled", Item::Value(Value::from(true)));
        if !auth.contains_key("keys") {
            auth.insert("keys", Item::ArrayOfTables(toml_edit::ArrayOfTables::new()));
        }
        let keys = match auth.get_mut("keys") {
            Some(Item::ArrayOfTables(aot)) => aot,
            _ => return Err("auth.keys ist kein [[array-of-tables]]".into()),
        };
        if keys
            .iter()
            .any(|t| t.get("name").and_then(|n| n.as_str()) == Some(name))
        {
            return Err(format!("Key '{name}' existiert bereits"));
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
        keys.push(t);
        Ok(())
    }

    /// Entfernt einen API-Key nach Name; `false`, wenn er nicht existierte.
    pub fn remove_auth_key(doc: &mut DocumentMut, name: &str) -> Result<bool, String> {
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
}

fn parse_value(s: &str) -> Result<Value, String> {
    if let Ok(b) = s.parse::<bool>() {
        return Ok(Value::from(b));
    }
    if let Ok(i) = s.parse::<i64>() {
        return Ok(Value::from(i));
    }
    if let Ok(f) = s.parse::<f64>() {
        return Ok(Value::from(f));
    }
    Ok(Value::from(s))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn add_and_remove_key_preserves_rest() {
        let mut doc: DocumentMut = "# LLM-Router Configuration\n[server]\nbind = \"127.0.0.1:4123\"\n\n[backends.openrouter]\nkind = \"openrouter\"\nbase_url = \"https://x\"\n"
            .parse()
            .unwrap();
        ConfigEditor::add_auth_key(&mut doc, "pi", "sha256:abc", Some(2.0), None).unwrap();
        let s = doc.to_string();
        assert!(s.contains("name = \"pi\""));
        assert!(s.contains("sha256:abc"));
        assert!(s.contains("enabled = true"));
        assert!(s.contains("[[auth.keys]]"));
        // Rest unverändert (Kommentare erhalten)
        assert!(s.contains("LLM-Router Configuration"));
        let removed = ConfigEditor::remove_auth_key(&mut doc, "pi").unwrap();
        assert!(removed);
        let round = ConfigEditor::remove_auth_key(&mut doc, "pi").unwrap();
        assert!(!round);
        // Key ist weg, Rest unverändert (ein leerer [auth]-Header darf bleiben).
        let s2 = doc.to_string();
        assert!(!s2.contains("pi"));
        assert!(!s2.contains("sha256:abc"));
        assert!(s2.contains("bind = \"127.0.0.1:4123\""));
        assert!(s2.contains("LLM-Router Configuration"));
    }

    #[test]
    fn set_nested_creates_tables() {
        let mut doc: DocumentMut = "[server]\nbind = \"127.0.0.1:4123\"\n".parse().unwrap();
        ConfigEditor::set(&mut doc, "alerts.webhook_url", "https://example.com/hook").unwrap();
        ConfigEditor::set(&mut doc, "alerts.daily_cost_threshold_usd", "5").unwrap();
        let s = doc.to_string();
        assert!(s.contains("webhook_url = \"https://example.com/hook\""));
        assert!(s.contains("daily_cost_threshold_usd = 5"));
    }
}
