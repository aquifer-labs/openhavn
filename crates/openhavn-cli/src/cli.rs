// SPDX-License-Identifier: Apache-2.0

//! `openhavn` CLI argument grammar (clap derive).

use std::path::PathBuf;

use clap::{Args, Parser, Subcommand};

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
    /// Model Context Protocol server.
    #[command(subcommand)]
    Mcp(McpCommand),
    /// Detect installed agent harnesses and (optionally) register the OpenHavn MCP server.
    Init {
        /// Register `openhavn mcp serve` as an MCP server for every detected harness that
        /// supports it (claude, codex, zed). Idempotent: re-running never duplicates an entry.
        #[arg(long)]
        register_mcp: bool,
        /// Preview what `--register-mcp` would write without touching any file.
        #[arg(long)]
        dry_run: bool,
    },
    /// Govern an arbitrary command with a spawn/return receipt pair — the harness-agnostic entry
    /// point. Writes a spawn receipt before launching, then exactly one return receipt after the
    /// child exits; exits with the child's own exit code (killed by a signal -> 130). If none of
    /// --budget-tokens/--budget-tool-calls/--budget-time-ms/--budget-cost is given, defaults to
    /// a budget of max_wall_time_ms=86400000 (24h) and prints a warning; pass --fail-closed to
    /// refuse to launch instead.
    //
    // A `Box<RunArgs>` (rather than inline struct-variant fields) purely to keep `Command`
    // itself small — see `clippy::large_enum_variant`; every other variant is a few bytes.
    Run(Box<RunArgs>),
    /// Tail a receipts.jsonl stream (or a directory of them) and print new records and
    /// violations as they're appended.
    Watch {
        /// Path to a receipts.jsonl file, or a directory to search (recursively, max depth 3)
        /// for `receipts*.jsonl` files. New files are picked up on each rescan.
        path: PathBuf,
        /// Single pass: print current records + violations, then exit (0 = clean, 1 =
        /// violations found) — CI mode. Without this flag, poll every 500ms until interrupted.
        #[arg(long)]
        once: bool,
    },
    /// Governed cross-harness skill logistics: install / update / list / remove skills, with a
    /// provenance lockfile, drift detection, and a deterministic admission gate (never
    /// prompt-based) whose decisions are appended to `~/.openhavn/equipment.jsonl`.
    #[command(subcommand)]
    Skill(SkillCommand),
}

#[derive(Debug, Subcommand)]
pub enum SkillCommand {
    /// Fetch a skill (a local directory, or a git URL) and, once it clears the admission gate,
    /// install it into every selected harness's skill directory.
    Install {
        /// A local directory containing a `SKILL.md`, or a git source: a URL ending `.git`, or
        /// one starting `https://github.com/` (optionally with `/<subdir>` after the repo path
        /// naming where the skill lives inside that repo).
        source: String,
        /// Name to install under. Defaults to the `name:` in the fetched SKILL.md frontmatter.
        #[arg(long)]
        name: Option<String>,
        /// Install into (and lock via) the global scope — `~/.openhavn/skills.lock` and each
        /// harness's `~/.<harness>/...` skill directory — instead of the project scope.
        #[arg(long)]
        global: bool,
        /// Comma-separated harnesses to install into (`claude,codex,opencode`). Defaults to
        /// every harness detected as installed on this machine.
        #[arg(long, value_delimiter = ',')]
        target: Option<Vec<String>>,
        /// Print the install plan without fetching admission-gate decisions to disk or writing
        /// any file.
        #[arg(long = "dry-run")]
        dry_run: bool,
        /// Overwrite a target directory even if it already exists and isn't owned by this
        /// skill's lock entry.
        #[arg(long)]
        force: bool,
    },
    /// List locked skills and, for each installed target, whether it is `OK` (content hash
    /// matches the lock), `MODIFIED` (drifted), or `MISSING`.
    List {
        /// Read `~/.openhavn/skills.lock` instead of the project-scoped `openhavn.lock`.
        #[arg(long)]
        global: bool,
    },
    /// Refetch a locked skill's source and reinstall it if the content changed (pin+notify:
    /// content only ever changes via an explicit `update`, never silently on `install`).
    Update {
        /// Skill name to update. Omit and pass `--all` to update every locked skill instead.
        name: Option<String>,
        /// Update every locked skill.
        #[arg(long)]
        all: bool,
        #[arg(long)]
        global: bool,
        /// Print what would change without writing anything.
        #[arg(long = "dry-run")]
        dry_run: bool,
    },
    /// Remove a skill: deletes only the directories its lock entry owns, then drops the entry.
    Rm {
        name: String,
        #[arg(long)]
        global: bool,
    },
}

#[derive(Debug, Args)]
pub struct RunArgs {
    /// Role recorded on the spawn receipt (e.g. "worker", "reviewer").
    #[arg(long)]
    pub role: Option<String>,
    /// Harness recorded on the spawn receipt. Defaults to the launched command's basename.
    #[arg(long)]
    pub harness: Option<String>,
    /// Model recorded on the spawn receipt.
    #[arg(long)]
    pub model: Option<String>,
    /// Task boundary recorded on the spawn receipt. Defaults to the launched command line,
    /// truncated to 200 characters.
    #[arg(long)]
    pub task: Option<String>,
    /// Budget: max tokens.
    #[arg(long = "budget-tokens")]
    pub budget_tokens: Option<u64>,
    /// Budget: max tool calls.
    #[arg(long = "budget-tool-calls")]
    pub budget_tool_calls: Option<u64>,
    /// Budget: max wall-clock time, in milliseconds.
    #[arg(long = "budget-time-ms")]
    pub budget_time_ms: Option<u64>,
    /// Budget: max cost, in USD.
    #[arg(long = "budget-cost")]
    pub budget_cost: Option<f64>,
    /// `parent` receipt_id recorded on the spawn receipt. Defaults to the literal "root".
    #[arg(long)]
    pub parent: Option<String>,
    /// Receipts file, or a directory treated as an `.ocf` bundle (`<dir>/receipts.jsonl`).
    /// Defaults to `./.openhavn/runs/<run-id>/receipts.jsonl`.
    #[arg(long)]
    pub receipts: Option<PathBuf>,
    /// Run id embedded in generated receipt ids and in the default receipts path. Defaults to
    /// `run-<current UTC timestamp, compact>`.
    #[arg(long = "run-id")]
    pub run_id: Option<String>,
    /// Refuse to launch when no `--budget-*` flag is given, instead of defaulting to a 24h
    /// wall-time budget (with a warning).
    #[arg(long = "fail-closed")]
    pub fail_closed: bool,
    /// The command to launch, and its arguments (everything after `--`).
    #[arg(last = true, required = true)]
    pub command: Vec<String>,
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

#[derive(Debug, Subcommand)]
pub enum McpCommand {
    /// Run the OpenHavn MCP server over stdio.
    Serve,
}
