// SPDX-License-Identifier: Apache-2.0

//! OpenHavn — the Agent Harbor.
//!
//! See docs/design.md for the receipts/budgets/gates design this binary implements.

mod cli;
mod commands;
mod render;
mod treedata;

use anyhow::{Context, Result};
use clap::Parser;

use cli::{BudgetCommand, Cli, Command, McpCommand, ReceiptsCommand};

fn main() {
    let cli = Cli::parse();
    match run(cli) {
        Ok(code) => std::process::exit(code),
        Err(err) => {
            eprintln!("error: {err:#}");
            std::process::exit(2);
        }
    }
}

/// Dispatch a parsed CLI invocation, returning the process exit code on success (I/O / parse
/// failures are reported as `Err` instead; only *semantic* results — e.g. validation violations
/// — are encoded as a nonzero `Ok` exit code).
fn run(cli: Cli) -> Result<i32> {
    match cli.command {
        Command::Receipts(ReceiptsCommand::Validate { path }) => {
            commands::receipts::validate(&path)
        }
        Command::Receipts(ReceiptsCommand::Show { path }) => commands::receipts::show(&path),
        Command::Budget(BudgetCommand::Tree { path }) => commands::budget::tree(&path),
        Command::Mcp(McpCommand::Serve) => {
            let runtime = tokio::runtime::Runtime::new().context("building tokio runtime")?;
            runtime.block_on(commands::mcp::serve())?;
            Ok(0)
        }
        Command::Init {
            register_mcp,
            dry_run,
        } => commands::init::run(register_mcp, dry_run),
    }
}
