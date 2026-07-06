// SPDX-License-Identifier: Apache-2.0

//! `openhavn` CLI argument grammar (clap derive).

use std::path::PathBuf;

use clap::{Parser, Subcommand};

#[derive(Debug, Parser)]
#[command(
    name = "openhavn",
    version,
    about = "OpenHavn — the Agent Harbor: fleet receipts, budget composition, distillation gates"
)]
pub struct Cli {
    #[command(subcommand)]
    pub command: Command,
}

#[derive(Debug, Subcommand)]
pub enum Command {
    /// Lifecycle receipts: validate and inspect a receipts.jsonl stream.
    #[command(subcommand)]
    Receipts(ReceiptsCommand),
    /// Budget composition and fleet observability.
    #[command(subcommand)]
    Budget(BudgetCommand),
}

#[derive(Debug, Subcommand)]
pub enum ReceiptsCommand {
    /// Validate a receipts.jsonl file (or an .ocf bundle directory) against the OCF invariants.
    Validate {
        /// Path to a receipts.jsonl file, or a `<name>.ocf` bundle directory containing one.
        path: PathBuf,
    },
    /// Render the spawn tree: role@harness, task boundary, stop reason, consumed vs budget.
    Show {
        /// Path to a receipts.jsonl file, or a `<name>.ocf` bundle directory containing one.
        path: PathBuf,
    },
}

#[derive(Debug, Subcommand)]
pub enum BudgetCommand {
    /// Render budget composition and fleet observability: granted -> consumed, context
    /// efficiency, rolled-up totals, and over-allocation flags.
    Tree {
        /// Path to a receipts.jsonl file, or a `<name>.ocf` bundle directory containing one.
        path: PathBuf,
    },
}
