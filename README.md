<!-- SPDX-License-Identifier: Apache-2.0 -->

# OpenHavn

**The Agent Harbor — a fleet governor for the agents you already run.**

> Status: **pre-alpha** — charter and design phase. The receipts/budget/gate design is in
> [`docs/design.md`](docs/design.md); nothing here is stable yet.

Your coding agents already work: Claude Code, Codex, OpenCode, Zed. What none of them gives you is
proof of what a fleet of them did: what each subagent received, what it was allowed to spend, why
it stopped, what came back, and what was admitted into the parent's context. Orchestrators spawn
and observe; **nobody governs the membrane between parent and child**. OpenHavn is that governor.

A harbor does not replace ships. Agents dock, get equipped, get a fuel allocation, sail, and
return — and everything that crosses the quay is on a manifest.

## Three pillars

1. **Fleet** — every spawn produces a **lifecycle receipt** (role, model, budget granted, task
   boundary → stop reason, budget consumed, tokens returned); budgets **compose down the agent
   tree** (tokens, tool-arity, wall-time, cost — fail-closed: no declared envelope, no autonomy);
   subagent results pass a **distillation gate** before entering the parent's context (parent sees
   the distilled summary by default, the child's full trace stays in the receipt).
2. **Equipment** — one verb set to install / update / sync / version skills and MCP servers across
   every harness (`~/.claude/skills`, `~/.codex/…`, OpenCode, Zed, 14+ path targets), with
   provenance, drift detection, and an admission gate — governed logistics, not file copying.
3. **Deck** — a desktop app (macOS first): the fleet, its receipts and budgets, and the memory
   graph underneath, live — reading and writing the same transactional substrate as every agent.

## How it composes

OpenHavn embeds [Artesian](https://github.com/aquifer-labs/artesian) — the **Context Governor**
for a single agent loop — as its context layer, and both speak
[OCF](https://github.com/aquifer-labs/ocf), the open format for governed agent state (committed
snapshot + qualify log; extending to lifecycle receipts). Artesian governs one loop's context;
OpenHavn governs the fleet above it. Both **complement** the harnesses you already use — they
never replace them.

## Install

```
brew install aquifer-labs/tap/openhavn   # CLI + MCP server, one binary
openhavn init --register-mcp             # detect harnesses, wire MCP, done
```

One binary, no daemon, MCP drop-in. Today it ships: `openhavn run -- <cmd>` (a receipt pair for
any command), `receipts validate|show`, `budget tree` (context-efficiency per subagent),
`watch [--once]`, `init [--register-mcp]`, and `mcp serve` (4 tools).

## License

Apache-2.0 © Aquifer Labs.
