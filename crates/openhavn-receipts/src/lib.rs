// SPDX-License-Identifier: Apache-2.0

//! `openhavn-receipts` — types, parsing, and validation for OCF lifecycle receipts
//! (`receipts.jsonl`), per `SPEC.md` ("5. Lifecycle Receipts") and
//! `schema/receipts.schema.json` in [aquifer-labs/ocf](https://github.com/aquifer-labs/ocf).
//!
//! `receipts.jsonl` governs the membrane between a parent loop and its subagents: a **spawn**
//! record is written before a child starts (its absence means the child may not run — fail
//! closed); a **return** record is terminal and carries the `stop_reason` (spawn acknowledgment
//! and completion are distinct events — ack != completion).
//!
//! This crate is intentionally offline and dependency-light: no daemon, no network, just typed
//! parsing ([`Receipt::from_jsonl_line`]), an append-only log ([`ReceiptLog`]), monotonic id
//! generation ([`ReceiptIdGen`]), and semantic validation ([`validate`]).

mod id;
mod log;
mod model;
mod validate;

pub use id::ReceiptIdGen;
pub use log::{parse_jsonl, resolve_receipts_path, ReceiptLog};
pub use model::{
    Artifact, BudgetEnvelope, Consumed, ContentEncoding, Distilled, GateDecision, Receipt,
    ReceiptError, ReturnReceipt, SpawnReceipt, StopReason,
};
pub use validate::{validate, BudgetDimension, Violation, XorProblem};
