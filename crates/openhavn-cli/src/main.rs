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

use cli::{BudgetCommand, Cli, Command, McpCommand, ReceiptsCommand, SkillCommand};

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
        Command::Mcp(McpCommand::Add {
            name,
            target,
            env,
            dry_run,
            force,
            command,
        }) => commands::mcp::add(&name, target.as_deref(), &env, &command, dry_run, force),
        Command::Mcp(McpCommand::List) => commands::mcp::list(),
        Command::Mcp(McpCommand::Rm {
            name,
            target,
            force,
        }) => commands::mcp::rm(&name, target.as_deref(), force),
        Command::Mcp(McpCommand::Sync { dry_run }) => commands::mcp::sync(dry_run),
        Command::Init {
            register_mcp,
            dry_run,
        } => commands::init::run(register_mcp, dry_run),
        Command::Run(args) => commands::run::run(*args),
        Command::Watch { path, once } => commands::watch::watch(&path, once),
        Command::Skill(SkillCommand::Install {
            source,
            name,
            global,
            target,
            dry_run,
            force,
        }) => commands::skill::install(
            &source,
            name.as_deref(),
            global,
            target.as_deref(),
            dry_run,
            force,
        ),
        Command::Skill(SkillCommand::List { global }) => commands::skill::list(global),
        Command::Skill(SkillCommand::Update {
            name,
            all,
            global,
            dry_run,
        }) => commands::skill::update(name.as_deref(), all, global, dry_run),
        Command::Skill(SkillCommand::Rm { name, global }) => commands::skill::rm(&name, global),
    }
}
