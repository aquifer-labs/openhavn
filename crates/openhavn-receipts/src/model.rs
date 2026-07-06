// SPDX-License-Identifier: Apache-2.0

//! Types mirroring `schema/receipts.schema.json` in the OCF spec (aquifer-labs/ocf) exactly.
//!
//! Every record type carries a flattened `extra` map so unknown fields survive a
//! parse -> serialize -> parse round trip untouched (OCF "permissive consumption": producers may
//! extend, consumers must tolerate — SPEC.md, "Permissive consumption").

use std::fmt;

use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};

/// A single `receipts.jsonl` line: either a spawn record or a return record, dispatched on `kind`.
///
/// Deserialization is hand-dispatched (see [`Receipt::from_jsonl_line`]) rather than relying on
/// serde's internally-tagged enum support, so an unrecognized `kind` produces a typed
/// [`ReceiptError`] instead of a generic serde message, matching the "typed rejection, never
/// silent accept" invariant in `docs/design.md`.
#[derive(Debug, Clone, PartialEq)]
pub enum Receipt {
    Spawn(SpawnReceipt),
    Return(ReturnReceipt),
}

impl Receipt {
    /// The record's own `receipt_id`, regardless of kind.
    pub fn receipt_id(&self) -> &str {
        match self {
            Receipt::Spawn(s) => &s.receipt_id,
            Receipt::Return(r) => &r.receipt_id,
        }
    }

    /// Parse one `receipts.jsonl` line. Dispatches on the required `kind` field.
    pub fn from_jsonl_line(line: &str) -> Result<Receipt, ReceiptError> {
        let value: Value = serde_json::from_str(line)?;
        let kind = value
            .get("kind")
            .and_then(Value::as_str)
            .ok_or(ReceiptError::MissingKind)?;
        match kind {
            "spawn" => Ok(Receipt::Spawn(serde_json::from_value(value)?)),
            "return" => Ok(Receipt::Return(serde_json::from_value(value)?)),
            other => Err(ReceiptError::UnknownKind(other.to_string())),
        }
    }
}

impl Serialize for Receipt {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: serde::Serializer,
    {
        // Serialize the inner record, then splice in the `kind` discriminant so it round-trips
        // through `from_jsonl_line`. `kind` is not a field on SpawnReceipt/ReturnReceipt itself —
        // it lives only here, on the envelope — so there is exactly one place that writes it.
        let (kind, value) = match self {
            Receipt::Spawn(s) => ("spawn", serde_json::to_value(s)),
            Receipt::Return(r) => ("return", serde_json::to_value(r)),
        };
        let mut value = value.map_err(serde::ser::Error::custom)?;
        if let Value::Object(map) = &mut value {
            map.insert("kind".to_string(), Value::String(kind.to_string()));
        }
        value.serialize(serializer)
    }
}

impl<'de> Deserialize<'de> for Receipt {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        let value = Value::deserialize(deserializer)?;
        let kind = value
            .get("kind")
            .and_then(Value::as_str)
            .ok_or_else(|| serde::de::Error::missing_field("kind"))?;
        match kind {
            "spawn" => Ok(Receipt::Spawn(
                serde_json::from_value(value).map_err(serde::de::Error::custom)?,
            )),
            "return" => Ok(Receipt::Return(
                serde_json::from_value(value).map_err(serde::de::Error::custom)?,
            )),
            other => Err(serde::de::Error::custom(format!(
                "unknown receipt kind {other:?} (expected \"spawn\" or \"return\")"
            ))),
        }
    }
}

/// Typed parse errors for a single `receipts.jsonl` line.
#[derive(Debug)]
pub enum ReceiptError {
    /// The line is not valid JSON, or does not deserialize into the record shape for its `kind`.
    Json(serde_json::Error),
    /// The `kind` field (required by every record) is absent.
    MissingKind,
    /// `kind` was present but was neither `"spawn"` nor `"return"`.
    UnknownKind(String),
}

impl fmt::Display for ReceiptError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            ReceiptError::Json(e) => write!(f, "invalid receipt JSON: {e}"),
            ReceiptError::MissingKind => {
                write!(f, "receipt is missing the required \"kind\" field")
            }
            ReceiptError::UnknownKind(k) => {
                write!(
                    f,
                    "unknown receipt kind {k:?} (expected \"spawn\" or \"return\")"
                )
            }
        }
    }
}

impl std::error::Error for ReceiptError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            ReceiptError::Json(e) => Some(e),
            ReceiptError::MissingKind | ReceiptError::UnknownKind(_) => None,
        }
    }
}

impl From<serde_json::Error> for ReceiptError {
    fn from(e: serde_json::Error) -> Self {
        ReceiptError::Json(e)
    }
}

/// A spawn record: written before a child starts. Its absence means the child may not run
/// (fail-closed autonomy — SPEC.md "Lifecycle Receipts").
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SpawnReceipt {
    pub receipt_id: String,
    pub ts: String,
    /// `receipt_id` of the parent's spawn record, or the literal string `"root"`.
    pub parent: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub role: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub harness: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model: Option<String>,
    pub task_boundary: String,
    pub budget: BudgetEnvelope,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tool_allowlist: Option<Vec<String>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub schema_hash: Option<String>,
    #[serde(flatten)]
    pub extra: Map<String, Value>,
}

/// A return record: terminal, exactly one per spawn. Spawn acknowledgment and completion are
/// distinct events — only a return receipt carries a `stop_reason` (ack != completion).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ReturnReceipt {
    pub receipt_id: String,
    pub ts: String,
    /// `receipt_id` of the spawn record this return terminates.
    pub spawn_ref: String,
    pub stop_reason: StopReason,
    pub consumed: Consumed,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub distilled: Option<Distilled>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub artifacts: Option<Vec<Artifact>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub trace_ref: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub gate: Option<GateDecision>,
    #[serde(flatten)]
    pub extra: Map<String, Value>,
}

/// Why a subagent stopped. Serde strings match the OCF schema enum exactly:
/// `done`, `budget_tokens`, `budget_tool_calls`, `budget_time`, `budget_cost`, `gate_rejected`,
/// `error`, `killed`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum StopReason {
    Done,
    BudgetTokens,
    BudgetToolCalls,
    BudgetTime,
    BudgetCost,
    GateRejected,
    Error,
    Killed,
}

impl StopReason {
    /// Whether this stop reason is one of the four `budget_*` variants — a budget-typed stop.
    pub fn is_budget(self) -> bool {
        matches!(
            self,
            StopReason::BudgetTokens
                | StopReason::BudgetToolCalls
                | StopReason::BudgetTime
                | StopReason::BudgetCost
        )
    }
}

impl fmt::Display for StopReason {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let s = match self {
            StopReason::Done => "done",
            StopReason::BudgetTokens => "budget_tokens",
            StopReason::BudgetToolCalls => "budget_tool_calls",
            StopReason::BudgetTime => "budget_time",
            StopReason::BudgetCost => "budget_cost",
            StopReason::GateRejected => "gate_rejected",
            StopReason::Error => "error",
            StopReason::Killed => "killed",
        };
        f.write_str(s)
    }
}

/// A budget envelope authorizing a child's execution: `{ max_tokens?, max_tool_calls?,
/// max_wall_time_ms?, max_cost_usd? }`. The JSON schema requires at least one dimension
/// (`minProperties: 1`); callers should treat an envelope with all four fields absent as invalid
/// (see [`crate::validate`]'s `MissingBudgetDimension`).
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct BudgetEnvelope {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_tokens: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_tool_calls: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_wall_time_ms: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_cost_usd: Option<f64>,
    #[serde(flatten)]
    pub extra: Map<String, Value>,
}

/// Actuals in the same dimensions as a spawn's [`BudgetEnvelope`]: `{ tokens?, tool_calls?,
/// wall_time_ms?, cost_usd? }`.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct Consumed {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tokens: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tool_calls: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub wall_time_ms: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cost_usd: Option<f64>,
    #[serde(flatten)]
    pub extra: Map<String, Value>,
}

/// What the parent actually received after the distillation gate: `{ tokens, ref }`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Distilled {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tokens: Option<u64>,
    #[serde(rename = "ref", default, skip_serializing_if = "Option::is_none")]
    pub ref_: Option<String>,
    #[serde(flatten)]
    pub extra: Map<String, Value>,
}

/// The distillation-gate decision recorded on a return record: typed, never silent.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct GateDecision {
    pub admitted: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
    #[serde(flatten)]
    pub extra: Map<String, Value>,
}

/// `plain` (default) or `base64` — how [`Artifact::content`] is encoded.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ContentEncoding {
    Plain,
    Base64,
}

/// A typed payload a child returned. Mirrors the ACP message-part model: exactly one of
/// `content` / `content_url` must be set (see `$defs/artifact` in `receipts.schema.json`).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Artifact {
    pub name: String,
    pub content_type: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub content: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub content_url: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub content_encoding: Option<ContentEncoding>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub metadata: Option<Map<String, Value>>,
    #[serde(flatten)]
    pub extra: Map<String, Value>,
}

impl Artifact {
    /// `true` when exactly one of `content` / `content_url` is set, per the schema's `oneOf`.
    pub fn satisfies_content_xor(&self) -> bool {
        self.content.is_some() != self.content_url.is_some()
    }
}
