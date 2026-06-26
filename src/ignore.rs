//! "Ignore rules" — a reversible filter that hides trivial/junk drives from the
//! Drives list and the all-time Stats. The rules are disjunctive normal form: a
//! drive is ignored if it matches **any** rule (OR), and a rule matches when
//! **all** of its conditions are true (AND). Each condition compares a per-drive
//! metric — `miles` (GPS length) or `minutes` (drive time) — against a threshold.
//! Nothing is deleted; clearing the rules makes every drive reappear.
//!
//! Stored as JSON in the `settings` table under `ignore_rules`.

use serde::Deserialize;
use serde_json::Value;

use crate::error::AppResult;
use crate::state::AppState;

const KEY: &str = "ignore_rules";

/// Default when the rules have never been configured: hide zero-movement records
/// (engine-on-but-never-moved / no-GPS stubs all have 0 miles). Editable/removable
/// in Settings — saving `[]` turns filtering off entirely.
const DEFAULT_RULES_JSON: &str = r#"[{"conditions":[{"field":"miles","op":"lt","value":0.1}]}]"#;

#[derive(Deserialize, Clone)]
pub struct Condition {
    pub field: String, // "miles" | "minutes"
    pub op: String,    // "lt" | "le" | "gt" | "ge"
    pub value: f64,
}

#[derive(Deserialize, Clone)]
pub struct Rule {
    pub conditions: Vec<Condition>,
}

/// Load the saved rules (empty if unset/invalid).
pub async fn load_rules(state: &AppState) -> Vec<Rule> {
    let v = sqlx::query_scalar::<_, String>("SELECT value FROM settings WHERE key = ?")
        .bind(KEY)
        .fetch_optional(&state.pool)
        .await
        .ok()
        .flatten();
    match v {
        // Configured (possibly `[]` = filtering off) → use it.
        Some(s) => serde_json::from_str::<Vec<Rule>>(&s).unwrap_or_default(),
        // Never configured → the default rule.
        None => serde_json::from_str::<Vec<Rule>>(DEFAULT_RULES_JSON).unwrap_or_default(),
    }
}

/// The raw JSON value (for the settings GET) — the default rule if never configured.
pub async fn rules_json(state: &AppState) -> Value {
    let v = sqlx::query_scalar::<_, String>("SELECT value FROM settings WHERE key = ?")
        .bind(KEY)
        .fetch_optional(&state.pool)
        .await
        .ok()
        .flatten();
    match v {
        Some(s) => serde_json::from_str::<Value>(&s).unwrap_or_else(|_| Value::Array(vec![])),
        None => serde_json::from_str::<Value>(DEFAULT_RULES_JSON).unwrap_or_else(|_| Value::Array(vec![])),
    }
}

/// Validate + persist the rules (rejects unknown fields/ops).
pub async fn save_rules(state: &AppState, rules: &[Rule]) -> AppResult<()> {
    for r in rules {
        for c in &r.conditions {
            if !matches!(c.field.as_str(), "miles" | "minutes") {
                return Err(crate::error::AppError::BadRequest(format!("bad field: {}", c.field)));
            }
            if !matches!(c.op.as_str(), "lt" | "le" | "gt" | "ge") {
                return Err(crate::error::AppError::BadRequest(format!("bad op: {}", c.op)));
            }
        }
    }
    // Re-serialize from the validated structs (drops any extra fields).
    let json = serde_json::to_string(
        &rules
            .iter()
            .map(|r| {
                serde_json::json!({
                    "conditions": r.conditions.iter().map(|c| serde_json::json!({
                        "field": c.field, "op": c.op, "value": c.value
                    })).collect::<Vec<_>>()
                })
            })
            .collect::<Vec<_>>(),
    )
    .unwrap_or_else(|_| "[]".into());
    sqlx::query("INSERT INTO settings (key, value) VALUES (?, ?) ON CONFLICT(key) DO UPDATE SET value = excluded.value")
        .bind(KEY)
        .bind(json)
        .execute(&state.pool)
        .await?;
    Ok(())
}

fn cond_matches(c: &Condition, miles: f64, minutes: f64) -> bool {
    let lhs = match c.field.as_str() {
        "miles" => miles,
        "minutes" => minutes,
        _ => return false,
    };
    match c.op.as_str() {
        "lt" => lhs < c.value,
        "le" => lhs <= c.value,
        "gt" => lhs > c.value,
        "ge" => lhs >= c.value,
        _ => false,
    }
}

/// Is a drive with these metrics ignored by the rules? An empty rule (no
/// conditions) never matches, so it can't ignore everything by accident.
pub fn is_ignored(rules: &[Rule], miles: f64, minutes: f64) -> bool {
    rules.iter().any(|r| {
        !r.conditions.is_empty() && r.conditions.iter().all(|c| cond_matches(c, miles, minutes))
    })
}
