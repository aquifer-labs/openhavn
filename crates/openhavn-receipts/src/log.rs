// SPDX-License-Identifier: Apache-2.0

//! Append-only JSONL reader/writer for a `receipts.jsonl` path, plus the `.ocf` bundle-dir
//! resolution shared by every CLI verb (`<dir>.ocf/receipts.jsonl` vs. a direct file path).

use std::fs::OpenOptions;
use std::io::Write;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};

use crate::model::Receipt;

/// Parse a full `receipts.jsonl` document (one record per non-blank line).
///
/// Blank lines are skipped. Each non-blank line is parsed with [`Receipt::from_jsonl_line`]; a
/// parse failure is reported with its 1-based line number.
pub fn parse_jsonl(text: &str) -> Result<Vec<Receipt>> {
    let mut records = Vec::new();
    for (idx, line) in text.lines().enumerate() {
        let line = line.trim();
        if line.is_empty() {
            continue;
        }
        let record = Receipt::from_jsonl_line(line)
            .with_context(|| format!("line {}: invalid receipt", idx + 1))?;
        records.push(record);
    }
    Ok(records)
}

/// If `input` is a directory (an `.ocf` bundle), resolve to `<input>/receipts.jsonl`; otherwise
/// treat `input` as the receipts file itself.
pub fn resolve_receipts_path(input: &Path) -> PathBuf {
    if input.is_dir() {
        input.join("receipts.jsonl")
    } else {
        input.to_path_buf()
    }
}

/// An append-only `receipts.jsonl` file: write spawn/return records as they happen, read them
/// back for validation and reporting.
#[derive(Debug, Clone)]
pub struct ReceiptLog {
    path: PathBuf,
}

impl ReceiptLog {
    /// Open a log at `path`. Accepts either a direct `receipts.jsonl` path or an `.ocf` bundle
    /// directory (resolved via [`resolve_receipts_path`]).
    pub fn new(path: impl Into<PathBuf>) -> Self {
        let path = path.into();
        let path = resolve_receipts_path(&path);
        Self { path }
    }

    /// The resolved `receipts.jsonl` path this log reads and writes.
    pub fn path(&self) -> &Path {
        &self.path
    }

    /// Append one record as a single JSONL line. Creates the file (and never truncates an
    /// existing one) so writers never clobber prior receipts.
    pub fn append(&self, record: &Receipt) -> Result<()> {
        let line = serde_json::to_string(record).context("serializing receipt")?;
        let mut file = OpenOptions::new()
            .create(true)
            .append(true)
            .open(&self.path)
            .with_context(|| format!("opening receipts log {}", self.path.display()))?;
        writeln!(file, "{line}")
            .with_context(|| format!("appending to receipts log {}", self.path.display()))?;
        Ok(())
    }

    /// Read and parse every record currently in the log, in file order.
    pub fn read_all(&self) -> Result<Vec<Receipt>> {
        let text = std::fs::read_to_string(&self.path)
            .with_context(|| format!("reading receipts log {}", self.path.display()))?;
        parse_jsonl(&text)
    }
}
