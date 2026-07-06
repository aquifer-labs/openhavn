<!-- SPDX-License-Identifier: Apache-2.0 -->

# OpenHavn design — receipts, budgets, gates

> Draft v0. Decisions here are grounded in a 73-source landscape review (Jul 2026, stored in the
> project memory); the load-bearing findings are cited inline as [F-n] and listed at the bottom.

## The problem, precisely

Multi-agent orchestrators (agent-orchestrator, Codeman, squad, codexia, mission-control,
multi-agent-shogun, gajae-code) all spawn, isolate, observe, and route text. Across every tool
surveyed [F-1]:

- the parent↔child return channel is **freeform prose** — no typed, validated result;
- budget governance peaks at a **run-count cap of 5** or blunt per-session token thresholds —
  nothing composes budgets down a tree, nothing enforces at spawn time;
- **no lifecycle proof exists** — no record of role, budget granted/consumed, task boundary,
  stop reason, returned tokens;
- result **distillation on return appears in zero tools** — parents ingest raw child output or a
  self-reported summary, unverified.

Anthropic's advisor tool legitimizes escalate-at-decision-points but is single-vendor,
single-session, receipt-less [F-2]. Dashboards (mission-control 5.5k★) render telemetry but admit
nothing and reject nothing — observability without enforcement [F-3]. That gate is the product.

## Design invariants

1. **Deterministic gates.** Gates are code, not prompt conventions — code gates transfer across
   models; prompts do not (+16.7pp scaffold-only evidence) [F-4].
2. **Validate-before-accept, typed rejection.** Every artifact crossing the membrane is validated
   against a pinned schema *before* acceptance; invalid → typed rejection, never silent accept
   (adopted from gajae-code's `workflow_gate`: run-scoped monotonic IDs, `schema_hash` pinning,
   idempotency keys) [F-5].
3. **Fail-closed autonomy.** A budget envelope — `{max_tokens, max_tool_calls, max_wall_time_ms,
   max_cost_usd}` + scopes + action allowlist — is an **entry condition** for unattended
   operation, not a mid-run meter (adopted from gajae-code `negotiate_unattended`) [F-5].
4. **Ack ≠ completion.** Spawn acknowledgment and terminal receipt are distinct events; only a
   terminal receipt carries a stop reason.
5. **Parent sees summary; child keeps trace.** The distilled result goes up; the full trace stays
   addressable in the receipt (the dominant 2026 context-firewall pattern, made the default) [F-6].
6. **Tool-arity is a budget.** Tool-selection accuracy degrades below 90% past 10–15 tools
   (weaker models) / 20–30 (stronger) — composition allocates tool slots, not just tokens; the
   OpenHavn MCP surface itself stays ≤15 tools [F-7].
7. **Complement, never replace.** OpenHavn wraps native primitives (Claude Code agent teams via
   `~/.claude/teams/`, Codex sessions, OpenCode) and governs them; it does not ship its own agent
   runtime.
8. **No consensus gates.** A distillation gate never uses multi-model agreement as its truth
   signal — correlated model errors make ensemble consensus look like confidence when it is not
   ("Consensus is Not Verification", arXiv:2603.06612). Gates rest on deterministic validation or
   explicit human escalation [F-10].

## Protocol positioning

OpenHavn is a **governance layer over the live agent protocols, not a new wire protocol**. The
protocols split cleanly: MCP answers *what can an agent do* (tools), A2A *which agent takes the
task* (delegation across boundaries), ACP *how do local agents exchange messages*, ANP *how does a
message find its agent*. None of them owns governance: the protocol survey arXiv:2606.31498
scores MCP v1.1, A2A v1.0.1, ACP, ANP, and ERC-8004 against a six-dimension taxonomy (G1
Membership, G2 Deliberation, G3 Voting, G4 Dissent preservation, G5 Human escalation, G6
Audit/replay) and finds G3–G5 *"absent across all five protocols"* and G6 substrate-inherited at
best, concluding: *"agent community governance is a missing architectural layer, not a missing
feature within existing protocols."* [F-11]

OpenHavn's receipts and gates are that layer's enforcement: receipts instantiate **G1** (a spawn
record admits a subagent with role + budget) and **G6** (append-only, deterministic
reconstruction); the qualify/distillation gates instantiate **G4** (rejected candidates retained
with reasons) and **G5** (human-escalation paths). Concretely:

- **Over A2A** — receipt support is advertised as a standard opt-in extension in
  `AgentCard.capabilities.extensions` (`uri:
  https://aquifer-labs.github.io/ocf/extensions/receipts/v1`, `required: false`), so non-OCF
  clients interoperate unchanged.
- **Over MCP** — the OpenHavn MCP server exposes receipts/budgets as tools and bundle resources
  (≤15 tools, per the arity ceiling [F-7]).
- **ACP-shaped payloads** — return-receipt `artifacts[]` copies ACP's MIME message-part model
  (`name` + `content_type` + `content` XOR `content_url`), so code/diffs/data cross the membrane
  typed, not as prose.
- **Typed contracts at every membrane crossing** — structured output contracts between agents cut
  tokens 10–30% and eliminate failed-parse retry loops; selective context forwarding cuts payloads
  60–80%; coordination overhead is the dominant multi-agent cost (59.4% of ChatDev tokens are
  spent on coordination, and a 3-agent pipeline can carry 2.9× the token cost of one agent on the
  same task) [F-12]. Receipts + gates are how that overhead becomes visible and governable.

## Receipts (to be specified in OCF v0.2 as `receipts.jsonl`)

Two record kinds, one append-only stream per run, `schema_hash`-pinned:

```jsonc
// spawn receipt — written before the child starts; its absence = the child may not run
{
  "kind": "spawn",
  "receipt_id": "rc_<run>_<seq>",        // run-scoped, monotonic
  "ts": "…",
  "parent": "rc_<run>_<parent-seq>|root",
  "role": "worker",
  "harness": "claude-code|codex|opencode|…",
  "model": "…",
  "task_boundary": "one-sentence contract of what the child owns",
  "budget": { "max_tokens": 0, "max_tool_calls": 0, "max_wall_time_ms": 0, "max_cost_usd": 0.0 },
  "tool_allowlist": ["…"],               // arity counts against the parent's tool budget
  "schema_hash": "sha256:…"              // pins the return-schema the child must satisfy
}

// return receipt — terminal; exactly one per spawn
{
  "kind": "return",
  "receipt_id": "rc_<run>_<seq>",
  "spawn_ref": "rc_<run>_<spawn-seq>",
  "ts": "…",
  "stop_reason": "done|budget_tokens|budget_time|budget_cost|gate_rejected|error|killed",
  "consumed": { "tokens": 0, "tool_calls": 0, "wall_time_ms": 0, "cost_usd": 0.0 },
  "distilled": { "tokens": 0, "ref": "…" },   // what the parent actually received
  "trace_ref": "…",                            // full child trace, addressable, never inlined
  "gate": { "admitted": true, "reason": "…" }  // distillation-gate decision, typed
}
```

Budget composition rule: `sum(children.budget) ⊆ parent.budget` per dimension, checked at spawn
(fail-closed), reconciled at return (consumed roll-up). Receipts give fleet telemetry nobody else
can render honestly — per-agent context-efficiency is a free by-product [F-8].

## Architecture

- **Phase A (now):** new Rust workspace; `openhavn` binary (CLI + MCP server in one, ≤15 tools);
  depends on artesian crates (`flume` delegation/lanes/budgets, `headgate` qualify-gate reuse for
  distillation, `aquifer` substrate) as rev-pinned git dependencies. Artesian repo unchanged.
- **Phase B (operator-gated, artesian 0.6.0):** orchestration crates (`flume`, `basin`,
  `headrace`, `sandbox`, `artesian-process-agent`) physically migrate here; artesian slims to the
  Context Governor (aquifer, headgate, gauge, core, cli, mcp).
- **Deck:** Tauri v2 (Rust core reuses the same crates in-process; macOS first, Windows/Linux
  kept open). Bidirectional sync is free by construction: Deck is just another writer on
  Artesian's transactional substrate (optimistic concurrency, commit-log watch) — an edit made
  from Zed via MCP appears live in Deck and vice versa.
- **No daemon.** One-shot CLI + self-terminating background workers (the cavemem pattern);
  "no daemon" is a marketed feature in this space [F-9].

## Findings index

- [F-1] Orchestrator survey: agent-orchestrator 8.1k★, Codeman, squad, codexia, MASFactory,
  sous-chef (best budget = 5-run cap; receipts absent everywhere).
- [F-2] Anthropic advisor tool docs (platform + Claude Code): validates the governor pattern;
  single-vendor/single-session/no receipts.
- [F-3] builderz-labs/mission-control: telemetry/RBAC dashboard, no admission or enforcement.
- [F-4] joelniklaus harness-optimization: +16.7pp held-out from deterministic scaffold gates;
  code transfers across model families, prompt playbooks don't.
- [F-5] gajae-code `docs/rpc.md` / `bridge.md`: `workflow_gate` (schema_hash, validate-before-
  accept, typed rejection, idempotency) and `negotiate_unattended` (fail-closed budget envelope).
- [F-6] "You don't have a context problem, you have a harness problem" (LOCA-bench synthesis):
  subagent context firewalls — parent sees summary only — as the dominant production pattern.
- [F-7] arXiv 2606.30317 (MCP server architecture patterns): tool-selection accuracy cliff at
  10–15/20–30 tools; Proxy Aggregator pattern.
- [F-8] Fleet-observability gap: no surveyed product shows per-agent context-efficiency; receipts
  make it a by-product.
- [F-9] Install-UX bar: cavemem (`install --ide`, `status`/`doctor`, self-exiting worker),
  contextplus (`init <harness>` writes MCP config), squad (brew, daemonless as a feature).
- [F-10] arXiv:2603.06612 "Consensus is Not Verification": polling/ensemble aggregation gives no
  consistent truthfulness gain without external verifiers, even at 25× inference cost — model
  errors are correlated.
- [F-11] arXiv:2606.31498 "Governance Gaps in Agent Interoperability Protocols": G1–G6 taxonomy
  across MCP/A2A/ACP/ANP/ERC-8004; G3/G4/G5 absent everywhere, G6 substrate-inherited only.
  Adjacent evidence: openswarm (742★ Electron fleet UI) has display-only cost tracking — no
  envelopes, no receipts, no gates — confirming no OSS prior art on the governed membrane.
- [F-12] Token-efficient multi-agent communication (zylos.ai, Jun 2026): 59.4% of ChatDev tokens
  on coordination; 2.9× three-agent pipeline overhead; structured contracts −10–30% tokens;
  selective context forwarding −60–80% payload; model tiering −50–83% cost.
