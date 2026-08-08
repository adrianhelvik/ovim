# ADR 0001: Durable, bounded subagent foundation

- Status: Accepted
- Date: 2026-07-18
- Plan baseline: `f0b1bd1`
- First release: read-only, asynchronous, depth one

## 2026-08 bounded hierarchy amendment

The preview now keeps depth one as the default but permits an explicit
`max_depth` from one through four. A child with remaining depth receives the
same strict coordination contracts as the root and may dispatch read-only
explorer/reviewer descendants against its immutable snapshot. Each descendant
gets a fresh durable manifest/envelope identity, shares the root concurrency
and budget ceilings, and loses coordination tools at the depth boundary.

Direct parents own model-to-model steering. The durable root remains operator
authority over the whole visible tree, so a human can steer or interrupt a
descendant without impersonating an intermediate model. This extends the
existing scheduler, mailbox, ancestry, cancellation, and restart contracts;
it does not introduce peer-to-peer dispatch or autonomous depth growth.
Operator controls remain attached to the durable chat binding after the root
model turn ends and append a user-authored causal event. A delegated parent
waiting on mailbox updates temporarily yields its provider-concurrency slot,
then reacquires capacity before its provider session resumes; otherwise a
depth-two tree could deadlock at `max_concurrent = 1`.

## Context

Ovim already has durable run identities, an append-only event log, captured
workspace manifests, provider profiles, tool policy, and an
`AgentDispatchScheduler`. The scheduler intentionally stops at durable
allocation and lifecycle policy: it does not launch provider sessions or
create Git worktrees. Subagent execution must extend those seams without
adding a second task database or making editor state the source of truth.

Parallel agents amplify several risks: a role preset can silently override the
model requested by the dispatching model, context can drift while a child is
running, a fluent but malformed result can be mistaken for completion, and
cleanup can destroy a workspace or runtime that Ovim does not own. Source-code
isolation alone is also insufficient for projects with ports, containers,
volumes, databases, and service dependencies.

## Decision

### 1. Hierarchy and lifecycle

Every child belongs to one rooted run tree and records its Ovim `AgentId`,
parent ID, causing turn and event, workspace identity, immutable dispatch
contract, lifecycle, budgets, and completion record in the existing run log.
Provider thread/session identifiers are bindings, never agent identities.

The default has maximum depth one, at most three concurrently running children,
and read-only roles only. Configuration may raise the bounded depth to four.
The parent remains live after dispatch and learns about child progress and
completion through durable mailbox events. A child is not represented as an
ordinary chat branch. It can dispatch descendants only when the configured
depth leaves room and its durable role contains `DispatchAgents`.

Cancellation is hierarchical. Canceling a parent cancels all nonterminal
descendants, but preserves their trace and partial evidence. Terminal state is
idempotent; repeated delivery may occur, but consumption is deduplicated by
event ID.

### 2. Read-only v1

V1 children receive an immutable projection of the captured workspace
manifest. They may read, navigate, and use only commands classified as
read-only. They cannot write source, use arbitrary shell, access the network,
perform external effects other than bounded agent coordination. A command that cannot be
deterministically classified as read-only is denied rather than escalated to
an approval prompt.

Write agents, integration, custom role definitions, and dependency graphs are
later phases. Bounded read-only recursive dispatch is available as described
in the amendment above. Follow-up turns are now available for
completed or interrupted read-only children: they preserve the Ovim agent,
route, workspace, capability ceiling, and budget ceiling while recording a
fresh turn ID and generation. The existing
scheduler types that mention write workspaces are foundations, not permission
to expose write agents in the v1 product.

### 3. Model and reasoning-effort routing

The model dispatching a child must specify both a catalog model ID and a
reasoning-effort value on every dispatch. Role presets control instructions,
maximum capabilities, workspace policy, and completion contract; they do not
own execution routing.

Routing precedence is:

1. The call-site `RequestedModelRoute` (`catalog_model_id` plus
   `reasoning_effort`) is authoritative.
2. The model catalog validates that the exact model/effort pair exists,
   supports the role's tools, and is allowed by administrator, user, project,
   and root-budget policy.
3. Exact routing is used when available. Unknown or incompatible pairs fail
   before an agent is allocated.
4. A fallback is used only when dispatch policy explicitly names and permits
   it. It must preserve or narrow capabilities, and the difference is durably
   recorded and shown to the parent.
5. Role or product defaults may be advertised as suggestions. They never
   overwrite a request or silently fill a missing required field.

Both requested and resolved routes, including provider, profile, model,
effort, catalog generation, resolution kind, and fallback reason, are stored
in the dispatch snapshot. Phase 1 will separate the current role-owned model
field and implement this resolution; this ADR does not make the existing v1
snapshot the final routing contract.

### 4. Delegation context envelope

A child starts from a versioned, immutable, system-generated envelope rather
than a raw conversation fork. The envelope contains:

- objective, task name, done criteria, expected output, non-goals, relevant
  paths, and risk tripwires;
- root run, parent, causing turn/event, workspace and manifest IDs;
- resolved model/effort, effective capabilities and tools, timeout, token and
  tool-call budgets, and remaining depth;
- applicable project instructions plus a concise parent-authored brief; and
- referenced artifacts with IDs, digests, provenance, and explicit warnings
  for missing or excluded inputs.

The default does not copy the full parent conversation. Open buffers and
unsaved overlays are captured once in the manifest. Later parent edits do not
mutate a running child's view. Secret caches, credentials, sockets, devices,
and ignored/excluded sensitive files are never transferred merely because
they exist below the repository root.

### 5. Structured handoff v1

Completion requires a validated handoff artifact with version, status,
bounded summary, evidence references, changed files, verification, blockers,
follow-ups, and confidence. Evidence paths must be valid for the assigned
workspace and must use a bounded, typed shape. Status must agree with blockers,
runtime outcome, and workspace mutation state.

Malformed, oversized, contradictory, or wrongly scoped handoffs do not move an
agent to `Completed`. Failed, canceled, and timed-out children may retain a
partial handoff, but their terminal status remains explicit. The original
artifact is retained for audit; only a bounded data projection is delivered to
the parent, never inserted as a system or developer instruction.

### 6. Monotonic capabilities

Effective child authority is the intersection of:

```text
role maximum
intersection parent effective capabilities
intersection root user authorization
intersection profile tool allowlist
intersection workspace strategy
intersection project policy
intersection phase feature gates
```

Every conversion from agent capabilities into existing tool
`Capabilities`/`RequiredScope` policy goes through one tested mapping. A child
cannot gain a tool because its prompt asks for it, its chosen provider happens
to support it, or a later message requests it. Follow-ups and fallback routing
may only preserve or narrow the set. Tools outside the effective set are not
advertised.

### 7. Workspace storage and ownership

Ovim owns worktree identity and lifecycle. The default checkout location for a
future write agent is:

```text
<platform-data>/ovim/workspaces/
  <encoded-repository-id>/<encoded-run-id>/<encoded-agent-id>/checkout
```

This store is a sibling of `ovim/runs`, not inside the repository, a run-event
directory, or a cache. Repository/run/agent IDs use the same traversal-safe
encoding pattern as `RunStorageLayout`; task prose never becomes a path. The
root can be overridden through `OVIM_WORKSPACES_DIR`, then project config, but
must be canonicalized and rejected if it is inside a source worktree, a Git
common directory, or another registered worktree. A repo-adjacent
`<repo>-worktrees` layout is an explicit validated override only.

An Ovim-created worktree has owner-only parent directories, a non-secret
`workspace.json` marker, exact canonical paths, captured base commit/manifest,
and a recoverable `ovim/<short-run>/<short-agent>` branch. Cleanup uses only
the recorded workspace ID, canonical path, marker, ref, and Git worktree
registry entry. It never discovers deletion targets by glob, branch prefix, or
path-name inference.

An existing Claude, `fed ws`, or user-created worktree is adopted/unowned.
Ovim may inspect it but may not infer ownership or automatically mutate its
isolation, runtime, branch, or files. Run-log retention and workspace retention
are independent; deleting history cannot implicitly delete unresolved source.

### 8. Project runtime providers

Workspace ownership and project runtime ownership remain separate. A
versioned `ProjectRuntimeProvider` detects, describes, prepares, operates,
stops, cleans, and reconciles a runtime for an exact checkout. It does not own
Git worktrees.

Without a supported runtime manifest, `PlainRuntimeProvider` promises code
isolation plus lifecycle control only for Ovim-launched processes. It must
state that fixed ports, shared databases, volumes, daemons, and external state
may collide.

When a resolved `service-federation.yaml` is present,
`FederationRuntimeProvider` invokes the external `fed` CLI in the exact child
checkout through a versioned adapter. For a new Ovim-owned write worktree it
runs `fed isolate enable` before every other `fed` operation, records the
reported identity and allocations, and does not automatically run `fed start`.
Services and declared scripts start lazily and selectively. `fed start
--replace` is never allowed.

Pause, interruption, failure, and ordinary completion run `fed stop` for
owned live resources while retaining `.fed` state, ports, volumes, install
markers, worktree, branch, and changes. `fed clean` is terminal disposal only,
after integration, explicit discard, or approved retirement. Adopted
worktrees require approval for runtime mutations and cleanup. Ovim never reads
`.fed/secrets.cache.env`, `.fed/secrets.generated.env`, or a raw resolved
environment into prompts, events, artifacts, logs, or handoffs.

### 9. Recovery and retirement

Startup rehydrates scheduler and projections from normalized durable events.
Queued work may restart after current catalog/policy validation. Starting,
running, waiting, or effect-ambiguous work becomes conservatively interrupted
unless the exact provider session and effect boundary are provably resumable.
Completed handoffs are re-notified until their mailbox event is consumed.

Worktree recovery requires agreement among the recorded canonical path,
ownership marker, branch/ref, and `git worktree list --porcelain`. Federation
reconciliation compares Ovim's recorded ownership/intent with Federation's
recorded allocations/status. Ambiguity produces attention-required state; it
never authorizes rotate, replace, start, clean, or delete.

Retirement order is fixed and retryable:

1. block new tools;
2. capture final delta and handoff;
3. stop the runtime;
4. perform provider terminal cleanup;
5. verify no owned live resources;
6. remove the exact registered Git worktree;
7. delete only the recorded Ovim ref;
8. remove the recorded marker directory; and
9. append the terminal retirement event.

A failure preserves the workspace and records the failed step. Retention expiry
never force-deletes dirty or unintegrated work. Force discard is a separate,
explicit action with a loss preview.

## Consequences

- The run log remains the reconstructible source of truth; live supervisors,
  UI cards, trees, and API snapshots are projections.
- Explicit model and effort fields make routing visible and testable, at the
  cost of a Phase 1 catalog/refactor before live dispatch can ship.
- Immutable snapshots make read-only parallelism safe but intentionally hide
  later parent edits until a new child or follow-up turn is created.
- Structured handoffs and monotonic policy add validation work before a useful
  child can be called complete.
- Platform-data worktrees need quota, free-space, path-length, and
  cross-filesystem preflight, but avoid source-tree pollution and unsafe cache
  eviction.
- Service Federation integration can give agents an honest service-isolation
  model while keeping `fed` the authority for orchestration details.

## Verification and phase gates

Phase 0 supplies deterministic offline provider scenarios for delay,
out-of-order completion, tool failure, malformed handoff, cancellation,
timeout, and restart, plus a pinned 20–30 task evaluation corpus and baseline
capture runner. These are contract fixtures; they do not launch children.

Live read-only preview must not ship until capability escalation,
foreground-mutation, crash/recovery, cancellation, approval visibility, and
handoff completion gates pass. Write preview additionally requires fault
injection proving exact-target cleanup and absence of secret values from every
prompt/event/artifact/handoff path.

## Rejected alternatives

- A second in-memory task system: not recoverable and duplicates the run log.
- Role-owned hidden model defaults: violates explicit per-dispatch routing.
- Shared foreground writers: exclusion alone cannot protect the user's active
  checkout.
- Worktrees inside `.ovim`, run history, or a cache: unsafe search, retention,
  and eviction coupling.
- `fed ws new/rm` as Ovim's workspace backend: its path and cleanup protocol do
  not establish Ovim's exact ownership/retention contract.
- Reimplementing Federation config/secrets in Ovim: creates divergent runtime
  semantics and a new secret-exposure surface.
- Peer-to-peer or unbounded recursively self-expanding agents: ownership,
  authority, and cost become ambiguous. Bounded parent-child recursion is
  permitted only inside the durable rooted tree and configured depth ceiling.
