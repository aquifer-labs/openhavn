// SPDX-License-Identifier: Apache-2.0

//! Run-scoped monotonic receipt id generation: `rc_<run>_<seq:06>`.

use std::sync::atomic::{AtomicU64, Ordering};

/// Generates `receipt_id` values scoped to one run, e.g. `rc_run1_000001`, `rc_run1_000002`, ...
///
/// Sequence numbers start at 1 and are zero-padded to 6 digits, matching the examples in
/// SPEC.md ("Lifecycle Receipts") and the conformance fixtures. The counter is an `AtomicU64` so
/// a single generator can be shared across threads without external locking.
#[derive(Debug)]
pub struct ReceiptIdGen {
    run: String,
    seq: AtomicU64,
}

impl ReceiptIdGen {
    /// Create a generator for the given run id. `run` is embedded verbatim in every id.
    pub fn new(run: impl Into<String>) -> Self {
        Self {
            run: run.into(),
            seq: AtomicU64::new(0),
        }
    }

    /// The run id this generator is scoped to.
    pub fn run(&self) -> &str {
        &self.run
    }

    /// Produce the next id in sequence: `rc_<run>_<seq:06>`.
    pub fn next_id(&self) -> String {
        let n = self.seq.fetch_add(1, Ordering::SeqCst) + 1;
        format!("rc_{}_{:06}", self.run, n)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn starts_at_one_and_is_zero_padded() {
        let gen = ReceiptIdGen::new("run1");
        assert_eq!(gen.next_id(), "rc_run1_000001");
        assert_eq!(gen.next_id(), "rc_run1_000002");
    }

    #[test]
    fn is_monotonic_across_many_calls() {
        let gen = ReceiptIdGen::new("runX");
        let ids: Vec<String> = (0..1000).map(|_| gen.next_id()).collect();
        let mut sorted = ids.clone();
        sorted.sort();
        assert_eq!(
            ids, sorted,
            "ids must already be produced in increasing order"
        );
        // Zero-padding keeps lexicographic order equal to numeric order up to 999_999.
        assert_eq!(ids[0], "rc_runX_000001");
        assert_eq!(ids[999], "rc_runX_001000");
    }
}
