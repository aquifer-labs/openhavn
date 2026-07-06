// SPDX-License-Identifier: Apache-2.0

//! The equipment log (`~/.openhavn/equipment.jsonl`): an append-only, OCF qualify-record-shaped
//! trail of every MCP admission-gate decision (admit or reject) plus every `sync`/`rm` event —
//! the same file `commands::skill::equipment` appends to (`slot` tells the two apart), so
//! `~/.openhavn/equipment.jsonl` is one governed trail across every equipment kind.

use std::io::Write as _;

use anyhow::{Context, Result};
use chrono::{SecondsFormat, Utc};
use serde::Serialize;

use super::gate::Rejection;
use super::targets::Roots;

/// One `equipment.jsonl` line.
#[derive(Debug, Clone, Serialize)]
pub struct EquipmentRecord {
    pub ts: String,
    /// `"mcp:<name>"` — the OCF "unit" this decision is about.
    pub unit_ref: String,
    pub admitted: bool,
    pub decision: Decision,
    /// Always `"mcp"` — distinguishes these records from `commands::skill::equipment`'s
    /// `"skill"`-slot records in the same file.
    pub slot: &'static str,
    pub score: f64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
    pub extra: Extra,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Decision {
    Admit,
    Reject,
    Update,
    Remove,
}

#[derive(Debug, Clone, Default, Serialize)]
pub struct Extra {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub command: Option<String>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub targets: Vec<String>,
}

impl EquipmentRecord {
    pub fn admit(name: &str, command: &str, targets: &[String]) -> Self {
        Self {
            ts: now_ts(),
            unit_ref: format!("mcp:{name}"),
            admitted: true,
            decision: Decision::Admit,
            slot: "mcp",
            score: 1.0,
            reason: Some("admitted".to_string()),
            extra: Extra {
                name: Some(name.to_string()),
                command: Some(command.to_string()),
                targets: targets.to_vec(),
            },
        }
    }

    pub fn reject(name: &str, rejection: &Rejection) -> Self {
        Self {
            ts: now_ts(),
            unit_ref: format!("mcp:{name}"),
            admitted: false,
            decision: Decision::Reject,
            slot: "mcp",
            score: 0.0,
            reason: Some(rejection.to_string()),
            extra: Extra {
                name: Some(name.to_string()),
                ..Extra::default()
            },
        }
    }

    pub fn update(name: &str, command: &str, targets: &[String]) -> Self {
        Self {
            ts: now_ts(),
            unit_ref: format!("mcp:{name}"),
            admitted: true,
            decision: Decision::Update,
            slot: "mcp",
            score: 1.0,
            reason: Some("reconciled to the lock's command/args".to_string()),
            extra: Extra {
                name: Some(name.to_string()),
                command: Some(command.to_string()),
                targets: targets.to_vec(),
            },
        }
    }

    pub fn remove(name: &str, targets: &[String]) -> Self {
        Self {
            ts: now_ts(),
            unit_ref: format!("mcp:{name}"),
            admitted: false,
            decision: Decision::Remove,
            slot: "mcp",
            score: 0.0,
            reason: Some("removed".to_string()),
            extra: Extra {
                name: Some(name.to_string()),
                targets: targets.to_vec(),
                ..Extra::default()
            },
        }
    }
}

/// Append one record to `<home>/.openhavn/equipment.jsonl` (created, and its parent directories,
/// on first use; never truncated).
pub fn append(roots: &Roots, record: &EquipmentRecord) -> Result<()> {
    let path = roots.home.join(".openhavn").join("equipment.jsonl");
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)
            .with_context(|| format!("creating directory {}", parent.display()))?;
    }
    let line = serde_json::to_string(record).context("serializing equipment record")?;
    let mut file = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(&path)
        .with_context(|| format!("opening {}", path.display()))?;
    writeln!(file, "{line}").with_context(|| format!("appending to {}", path.display()))?;
    Ok(())
}

fn now_ts() -> String {
    Utc::now().to_rfc3339_opts(SecondsFormat::Secs, true)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn roots(dir: &std::path::Path) -> Roots {
        Roots {
            home: dir.to_path_buf(),
            project_root: dir.to_path_buf(),
        }
    }

    #[test]
    fn append_writes_one_jsonl_line_per_record_with_expected_shape() {
        let dir =
            std::env::temp_dir().join(format!("openhavn-mcp-equipment-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let roots = roots(&dir);

        append(
            &roots,
            &EquipmentRecord::admit("demo", "/bin/echo", &["claude-project".to_string()]),
        )
        .unwrap();
        append(
            &roots,
            &EquipmentRecord::reject(
                "Bad Name!",
                &Rejection::UnsafeName {
                    name: "Bad Name!".to_string(),
                },
            ),
        )
        .unwrap();

        let path = dir.join(".openhavn").join("equipment.jsonl");
        let text = std::fs::read_to_string(&path).unwrap();
        let lines: Vec<&str> = text.lines().collect();
        assert_eq!(lines.len(), 2);

        let first: serde_json::Value = serde_json::from_str(lines[0]).unwrap();
        assert_eq!(first["unit_ref"], "mcp:demo");
        assert_eq!(first["admitted"], true);
        assert_eq!(first["decision"], "admit");
        assert_eq!(first["slot"], "mcp");
        assert_eq!(first["extra"]["command"], "/bin/echo");
        assert_eq!(first["extra"]["targets"][0], "claude-project");

        let second: serde_json::Value = serde_json::from_str(lines[1]).unwrap();
        assert_eq!(second["decision"], "reject");
        assert_eq!(second["admitted"], false);

        std::fs::remove_dir_all(&dir).ok();
    }
}
