// SPDX-License-Identifier: Apache-2.0

//! `openhavn mcp serve` — the OpenHavn MCP server, over stdio. Exposes fleet receipts, budgets,
//! and gates as four MCP tools: `receipts.validate`, `receipts.show`, `budget.tree`,
//! `fleet.status`.
//!
//! Every tool hands back DATA (`serde_json::Value` / typed structs derived from it), reusing the
//! exact same parsing (`openhavn_receipts::{parse_jsonl, validate}`) and tree-building
//! (`render::build_forest`, `treedata`) the CLI's text verbs use — text rendering itself stays in
//! `commands::receipts` / `commands::budget`, so this module never prints.

use std::collections::HashSet;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use openhavn_receipts::Receipt;
use rmcp::handler::server::router::tool::ToolRouter;
use rmcp::handler::server::wrapper::{Json, Parameters};
use rmcp::model::{Implementation, ServerCapabilities, ServerInfo};
use rmcp::transport::stdio;
use rmcp::{schemars, tool, tool_handler, tool_router, ErrorData, ServerHandler, ServiceExt};
use serde::{Deserialize, Serialize};
use serde_json::Value;

use super::{count_kinds, load};
use crate::render::build_forest;
use crate::treedata;

const INSTRUCTIONS: &str = "OpenHavn fleet governor: receipts, budgets, gates. Validate and \
inspect OCF receipts.jsonl streams — receipts.validate checks the OCF lifecycle-receipt \
invariants (duplicate/unknown spawns, budget composition, artifact shape); receipts.show and \
budget.tree return the spawn tree as structured data (the latter adds per-node context \
efficiency and fleet-wide granted/consumed totals); fleet.status recursively scans a directory \
for receipts*.jsonl files and reports open spawns and violations per file.";

const FLEET_STATUS_MAX_DEPTH: usize = 3;

// ---------------------------------------------------------------------------------------------
// receipts.validate
// ---------------------------------------------------------------------------------------------

#[derive(Debug, Deserialize, schemars::JsonSchema)]
pub struct PathArg {
    /// Path to a receipts.jsonl file, or a `<name>.ocf` bundle directory containing one.
    pub path: String,
}

#[derive(Debug, Serialize, schemars::JsonSchema)]
pub struct ViolationInfo {
    pub code: String,
    pub message: String,
}

#[derive(Debug, Serialize, schemars::JsonSchema)]
pub struct ValidateResponse {
    pub ok: bool,
    pub records: usize,
    pub spawns: usize,
    pub returns: usize,
    pub violations: Vec<ViolationInfo>,
}

fn validate_data(path: &Path) -> Result<ValidateResponse> {
    let records = load(path)?;
    let violations = openhavn_receipts::validate(&records);
    let (spawns, returns) = count_kinds(&records);
    Ok(ValidateResponse {
        ok: violations.is_empty(),
        records: records.len(),
        spawns,
        returns,
        violations: violations
            .iter()
            .map(|v| ViolationInfo {
                code: v.code().to_string(),
                message: v.to_string(),
            })
            .collect(),
    })
}

// ---------------------------------------------------------------------------------------------
// receipts.show
// ---------------------------------------------------------------------------------------------

#[derive(Debug, Serialize, schemars::JsonSchema)]
pub struct ShowResponse {
    pub tree: Vec<Value>,
}

fn show_data(path: &Path) -> Result<ShowResponse> {
    let records = load(path)?;
    let forest = build_forest(&records);
    Ok(ShowResponse {
        tree: forest.iter().map(treedata::node_to_json).collect(),
    })
}

// ---------------------------------------------------------------------------------------------
// budget.tree
// ---------------------------------------------------------------------------------------------

#[derive(Debug, Serialize, schemars::JsonSchema)]
pub struct BudgetTreeResponse {
    pub tree: Vec<Value>,
    pub totals: Value,
}

fn budget_tree_data(path: &Path) -> Result<BudgetTreeResponse> {
    let records = load(path)?;
    let forest = build_forest(&records);
    Ok(BudgetTreeResponse {
        tree: forest.iter().map(treedata::node_to_budget_json).collect(),
        totals: treedata::fleet_totals_json(&forest),
    })
}

// ---------------------------------------------------------------------------------------------
// fleet.status
// ---------------------------------------------------------------------------------------------

#[derive(Debug, Deserialize, schemars::JsonSchema)]
pub struct RootArg {
    /// Root directory to scan recursively (max depth 3) for `receipts*.jsonl` files.
    pub root: String,
}

#[derive(Debug, Default, Serialize, schemars::JsonSchema)]
pub struct FleetFileStatus {
    pub path: String,
    pub records: usize,
    pub spawns: usize,
    pub returns: usize,
    pub open_spawns: usize,
    pub violations: usize,
    /// Set (and every count field left at zero) when the file could not be read or parsed; the
    /// scan continues past it rather than failing the whole `fleet.status` call.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

#[derive(Debug, Default, Serialize, schemars::JsonSchema)]
pub struct FleetTotals {
    pub files: usize,
    pub records: usize,
    pub spawns: usize,
    pub returns: usize,
    pub open_spawns: usize,
    pub violations: usize,
}

#[derive(Debug, Serialize, schemars::JsonSchema)]
pub struct FleetStatusResponse {
    pub files: Vec<FleetFileStatus>,
    pub totals: FleetTotals,
}

fn is_receipts_file(path: &Path) -> bool {
    path.file_name()
        .and_then(|name| name.to_str())
        .is_some_and(|name| name.starts_with("receipts") && name.ends_with(".jsonl"))
}

/// Recursively collect `receipts*.jsonl` files under `root`, descending at most `max_depth`
/// directory levels below it (`root` itself is depth 0).
fn find_receipt_files(root: &Path, max_depth: usize) -> Vec<PathBuf> {
    let mut out = Vec::new();
    collect_receipt_files(root, 0, max_depth, &mut out);
    out.sort();
    out
}

fn collect_receipt_files(dir: &Path, depth: usize, max_depth: usize, out: &mut Vec<PathBuf>) {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_dir() {
            if depth < max_depth {
                collect_receipt_files(&path, depth + 1, max_depth, out);
            }
        } else if is_receipts_file(&path) {
            out.push(path);
        }
    }
}

fn count_open_spawns(records: &[Receipt]) -> usize {
    let mut spawn_ids: HashSet<&str> = HashSet::new();
    let mut returned_refs: HashSet<&str> = HashSet::new();
    for record in records {
        match record {
            Receipt::Spawn(spawn) => {
                spawn_ids.insert(spawn.receipt_id.as_str());
            }
            Receipt::Return(ret) => {
                returned_refs.insert(ret.spawn_ref.as_str());
            }
        }
    }
    spawn_ids.difference(&returned_refs).count()
}

fn fleet_status_data(root: &Path) -> Result<FleetStatusResponse> {
    anyhow::ensure!(
        root.is_dir(),
        "fleet.status root {} is not a directory",
        root.display()
    );

    let mut files = Vec::new();
    let mut totals = FleetTotals::default();
    for file in find_receipt_files(root, FLEET_STATUS_MAX_DEPTH) {
        totals.files += 1;
        let display_path = file.display().to_string();
        let parsed = std::fs::read_to_string(&file)
            .context("reading file")
            .and_then(|text| openhavn_receipts::parse_jsonl(&text));
        match parsed {
            Ok(records) => {
                let (spawns, returns) = count_kinds(&records);
                let open_spawns = count_open_spawns(&records);
                let violations = openhavn_receipts::validate(&records).len();
                totals.records += records.len();
                totals.spawns += spawns;
                totals.returns += returns;
                totals.open_spawns += open_spawns;
                totals.violations += violations;
                files.push(FleetFileStatus {
                    path: display_path,
                    records: records.len(),
                    spawns,
                    returns,
                    open_spawns,
                    violations,
                    error: None,
                });
            }
            Err(err) => files.push(FleetFileStatus {
                path: display_path,
                error: Some(format!("{err:#}")),
                ..Default::default()
            }),
        }
    }
    Ok(FleetStatusResponse { files, totals })
}

// ---------------------------------------------------------------------------------------------
// rmcp server wiring
// ---------------------------------------------------------------------------------------------

fn tool_error(err: anyhow::Error) -> ErrorData {
    ErrorData::internal_error(format!("{err:#}"), None)
}

#[derive(Debug, Clone)]
pub struct OpenhavnServer {
    tool_router: ToolRouter<Self>,
}

impl OpenhavnServer {
    pub fn new() -> Self {
        Self {
            tool_router: Self::tool_router(),
        }
    }
}

impl Default for OpenhavnServer {
    fn default() -> Self {
        Self::new()
    }
}

#[tool_router]
impl OpenhavnServer {
    #[tool(
        name = "receipts.validate",
        description = "Validate a receipts.jsonl file (or an .ocf bundle directory) against the OCF lifecycle-receipt invariants. Returns ok, record/spawn/return counts, and typed violations."
    )]
    async fn receipts_validate(
        &self,
        Parameters(PathArg { path }): Parameters<PathArg>,
    ) -> Result<Json<ValidateResponse>, ErrorData> {
        validate_data(Path::new(&path))
            .map(Json)
            .map_err(tool_error)
    }

    #[tool(
        name = "receipts.show",
        description = "Return the spawn tree for a receipts.jsonl file (or .ocf bundle directory) as structured data: receipt_id, role, harness, task_boundary, stop_reason, consumed, budget, children."
    )]
    async fn receipts_show(
        &self,
        Parameters(PathArg { path }): Parameters<PathArg>,
    ) -> Result<Json<ShowResponse>, ErrorData> {
        show_data(Path::new(&path)).map(Json).map_err(tool_error)
    }

    #[tool(
        name = "budget.tree",
        description = "Return the spawn tree plus per-node context_efficiency (distilled/consumed tokens) and fleet-wide granted (top-level) / consumed (fleet) totals."
    )]
    async fn budget_tree(
        &self,
        Parameters(PathArg { path }): Parameters<PathArg>,
    ) -> Result<Json<BudgetTreeResponse>, ErrorData> {
        budget_tree_data(Path::new(&path))
            .map(Json)
            .map_err(tool_error)
    }

    #[tool(
        name = "fleet.status",
        description = "Recursively scan a directory (max depth 3) for receipts*.jsonl files and report per-file records/spawns/returns/open_spawns/violations plus fleet-wide totals."
    )]
    async fn fleet_status(
        &self,
        Parameters(RootArg { root }): Parameters<RootArg>,
    ) -> Result<Json<FleetStatusResponse>, ErrorData> {
        fleet_status_data(Path::new(&root))
            .map(Json)
            .map_err(tool_error)
    }
}

#[tool_handler(router = self.tool_router)]
impl ServerHandler for OpenhavnServer {
    fn get_info(&self) -> ServerInfo {
        ServerInfo::new(ServerCapabilities::builder().enable_tools().build())
            .with_instructions(INSTRUCTIONS)
            .with_server_info(Implementation::new("openhavn", env!("CARGO_PKG_VERSION")))
    }
}

/// `openhavn mcp serve`: run the OpenHavn MCP server over stdio until the client disconnects.
pub async fn serve() -> Result<()> {
    let server = OpenhavnServer::new();
    server
        .serve(stdio())
        .await
        .context("starting the openhavn MCP server over stdio")?
        .waiting()
        .await
        .context("running the openhavn MCP server")?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use super::*;

    fn fixture(name: &str) -> PathBuf {
        PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("../openhavn-receipts/tests/fixtures")
            .join(name)
    }

    #[test]
    fn validate_data_valid_fixture_is_ok_with_no_violations() {
        let response = validate_data(&fixture("valid.jsonl")).unwrap();
        assert!(response.ok);
        assert_eq!(response.records, 5);
        assert_eq!(response.spawns, 3);
        assert_eq!(response.returns, 2);
        assert!(response.violations.is_empty());
    }

    #[test]
    fn validate_data_over_budget_fixture_reports_one_typed_violation() {
        let response = validate_data(&fixture("over-budget.jsonl")).unwrap();
        assert!(!response.ok);
        assert_eq!(response.violations.len(), 1);
        assert_eq!(
            response.violations[0].code,
            "OVER_BUDGET_WITHOUT_BUDGET_STOP"
        );
        assert!(response.violations[0].message.contains("rc_run2_000002"));
    }

    #[test]
    fn validate_data_missing_file_is_an_error_not_a_panic() {
        let missing = std::env::temp_dir().join("openhavn-mcp-test-does-not-exist.jsonl");
        assert!(validate_data(&missing).is_err());
    }

    #[test]
    fn show_data_builds_tree_with_running_root_and_two_children() {
        let response = show_data(&fixture("valid.jsonl")).unwrap();
        assert_eq!(response.tree.len(), 1);
        let root = &response.tree[0];
        assert_eq!(root["receipt_id"], "rc_run1_000001");
        assert_eq!(root["stop_reason"], "running");
        let children = root["children"].as_array().unwrap();
        assert_eq!(children.len(), 2);
        assert_eq!(children[0]["stop_reason"], "done");
        assert_eq!(children[1]["stop_reason"], "budget_tokens");
    }

    #[test]
    fn budget_tree_data_adds_context_efficiency_and_fleet_totals() {
        let response = budget_tree_data(&fixture("valid.jsonl")).unwrap();
        let child = &response.tree[0]["children"][0];
        let efficiency = child["context_efficiency"].as_f64().unwrap();
        assert!((efficiency - (410.0 / 61212.0)).abs() < 1e-9);
        assert_eq!(response.totals["consumed_fleet"]["tokens"], 121756.0);
    }

    #[test]
    fn fleet_status_data_scans_recursively_and_reports_open_spawns_and_violations() {
        let dir =
            std::env::temp_dir().join(format!("openhavn-mcp-fleet-status-{}", std::process::id()));
        std::fs::create_dir_all(dir.join("nested")).unwrap();
        std::fs::copy(fixture("valid.jsonl"), dir.join("receipts.jsonl")).unwrap();
        std::fs::copy(
            fixture("over-budget.jsonl"),
            dir.join("nested").join("receipts-run2.jsonl"),
        )
        .unwrap();
        // Not a receipts file — must be ignored.
        std::fs::write(dir.join("notes.txt"), "ignore me").unwrap();

        let response = fleet_status_data(&dir).unwrap();
        std::fs::remove_dir_all(&dir).ok();

        assert_eq!(response.files.len(), 2);
        assert_eq!(response.totals.files, 2);
        assert_eq!(response.totals.records, 7);
        assert_eq!(response.totals.violations, 1);
        // valid.jsonl's root spawn has no return yet -> exactly one open spawn fleet-wide.
        assert_eq!(response.totals.open_spawns, 1);
    }

    #[test]
    fn fleet_status_data_rejects_a_non_directory_root() {
        assert!(fleet_status_data(&fixture("valid.jsonl")).is_err());
    }
}
