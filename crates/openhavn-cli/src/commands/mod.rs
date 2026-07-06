// SPDX-License-Identifier: Apache-2.0

pub mod budget;
pub mod receipts;

use std::path::Path;

use anyhow::{Context, Result};
use openhavn_receipts::{resolve_receipts_path, Receipt};

/// Read and parse the receipts log at `path` (a direct `receipts.jsonl` file, or an `.ocf`
/// bundle directory containing one).
pub(crate) fn load(path: &Path) -> Result<Vec<Receipt>> {
    let resolved = resolve_receipts_path(path);
    let text = std::fs::read_to_string(&resolved)
        .with_context(|| format!("reading receipts log {}", resolved.display()))?;
    openhavn_receipts::parse_jsonl(&text)
        .with_context(|| format!("parsing receipts log {}", resolved.display()))
}
