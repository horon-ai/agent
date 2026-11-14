# Swarm as a task DAG (design)

Language: **English** | [한국어](SWARM_TASK_GRAPH.ko.md)

Status: complete (it replaces the earlier agent-first shape). The DAG engine,
deep/light modes, gates, the growth machinery, and all four communication migration
steps are done: artifact dataflow, subtree-scoped broadcast, moving flows off the chat
surface, and removing topic channels and the shared-context store.

This document holds the plan for reworking the swarm module from an agent-centered
model into a **task DAG (directed acyclic graph)**. The DAG becomes the primary
object, and an agent becomes a replaceable worker that executes, decomposes, and
verifies nodes. Building on the design discussion, it records the architecture, the
data model, completion and coverage guarantees, the bias budget, and the tool surface.

---

## 1. Motivation and the core reframing

Today swarm is **agent-first**. You start agents and drive work by talking to them
(DM, channels, roles), and `VersionedPlan` of `PlanItem` sits alongside. The
dependency graph is already inside (`PlanItem.blocked_by` edges,
`summarize_plan_graph`, `next_runnable_item_ids`, `run_plan`/`fill_slots`). But it
stays an implementation detail. Coverage and thoroughness are left to whoever happens
to be driving the work.

The reframing takes the **task DAG as the primary abstraction**.

- Declare a task graph with dependency edges and a per-node spec.
- The scheduler walks the DAG. A node becomes runnable once its dependencies finish,
  is assigned to a worker (reused or newly started), and on completion automatically
  releases the nodes that depend on it.
- An agent is a worker taken from a pool, not an object to be handled one at a time.
- The coordinator and worktree-manager roles move from user-visible concepts down to
  **scheduler policy**.

```text
Now (agent-first)
  Coordinator -> Agent -> plan item

DAG-first (reframe)
  worker pool ...executes...> Task A
  Task A -> Task B \
  Task A -> Task C --> Task D
```

The existing `horon-agent-plan` graph code is the foundation. This is not a rewrite;
it evolves that code.

---

## 1a. The two modes: deep (exhaustive) and light (fan-out)

The DAG engine runs in one of two **modes**. It is deliberately **one engine with two
presets**, not two separate systems. Both modes use the same DAG data model, the same
scheduler, the same edge dataflow, and the same member-cap mechanism. The only
differences are whether the strictness devices (mandatory decomposition +
critique/verify gates + recursion) are on, and how large the member cap is.

The core shape is this: **light mode is deep mode with the structural pressure off and
the cap small.** Build one engine with one mode switch; do not duplicate the scheduler
or the dataflow.

- **Deep mode (exhaustive):** everything in this document. The goal is to leave no
  corner untouched. It is a tree that deepens itself through recursion, decomposition
  is mandatory (composite by default), every node passes a critique/verify gate before
  it closes, recursion is encouraged with no depth limit, and the whole typed handoff
  schema including `what_i_did_not_check` is enforced. It scales up to a cap of 1000
  agent members. Cost and latency are high, so it is used deliberately. Examples:
  "investigate multi-monitor support in scrollwm", a large and risky refactor.

- **Light mode (fan-out):** a cheaper preset that parallelizes work for speed and
  raises quality modestly. It does not go to the extreme. The goal is only to run
  independent units in parallel. It is mostly flat (a single fan-out step),
  decomposition is optional (agent discretion), the critique/verify gates are off or
  at most a single final check, and only the root may start agents, so recursive growth
  is off. Handoff artifacts may be light and free-form. The worker cap is small (for
  example 4-16). Cost and latency are low. In effect it keeps today's start-and-fan-out
  behavior cheaply. Example: "run five independent edits in parallel".

| Dimension                   | Deep (exhaustive)                            | Light (fan-out)                     |
| --------------------------- | -------------------------------------------- | ----------------------------------- |
| Goal                        | leave no corner untouched                    | parallelize for speed and quality   |
| Shape                       | a tree that deepens itself through recursion | mostly flat, one-step fan-out       |
| Decomposition               | mandatory (composite by default)             | optional, agent discretion          |
| Critique/verify gates       | required before every node closes            | off (or one final check)            |
| Recursion                   | encouraged, unlimited depth                  | suppressed or off                   |
| Handoff artifact            | the full typed schema, `what_i_did_not_check` | light, free-form allowed            |
| Member cap                  | up to 1000 agents                            | small (e.g. 4-16 workers)           |
| Cost / latency              | high, deliberate                             | low, fast                           |

What the two modes share is the DAG data model, the scheduler, edge dataflow (typed or
lightweight artifacts), and the member-cap mechanism (only the number differs). The
strictness sections of this document (6, 7) describe **deep mode**. Light mode only
turns those gates off.

---

## 2. An ownership tree over the dependency graph

The trap of "everyone edits one shared graph" is that there is only one shared
`VersionedPlan`. Concurrent free-form edits break consistency, which is why plan
changes are currently limited to a single coordinator. The fix is not to add a lock but
to change **what a change is**.

Model it as an **ownership tree layered on the dependency graph**. The unit of change
is *extending a node you own*, never editing an arbitrary node.

- Writes are **divided by owner**. A node adds children only below itself, so two
  owners never write the same region. That removes the coordinator bottleneck without a
  lock while keeping one global graph.
- The graph is still the **server-owned, versioned** single source of truth (it reuses
  `VersionedPlan`). What changes is that a change becomes an **append-style operation**
  (add node, add edge, complete node) instead of a last-writer-wins blob, and the server
  validates acyclicity and ownership.
- A new edge may point only at an **existing upstream** node. Acyclicity is therefore
  structurally guaranteed.

---

## 3. Node kinds: atomic and composite

When an agent picks up a node, the node goes one of two ways. The state flips during
execution, not at draft time. There is **no** need to declare a node composite in
advance. An agent at any depth can extend the node assigned to it.

- **Atomic:** a worker executes the task directly and writes a handoff artifact.
- **Composite:** the assigned agent decides the task is too big. Instead of executing,
  it **decomposes** the node into a child sub-DAG and now owns that sub-DAG. The
  original node becomes a **join/synthesis point**. It stays in progress until every
  child finishes; then the owner wakes again, reads the child artifacts, and writes one
  synthesized result for whoever depends on this node.

The owner of a composite node is only a **planner and integrator** (map, then reduce).
The owner decomposes, the children do the work, and the owner synthesizes. The owner
does not execute leaf work itself. That leaves each node with a clean responsibility and
sharp ownership boundaries.

```text
Task A (atomic) --\
                  +--> Task D (composite) --> child D.1 --> child D.2
Task B (atomic) --/                                    --> D join / synthesize --> Task E
```

In **deep mode** recursion has no fixed depth and no per-node fan-out limit. Growth is
bounded by a configurable live-worker budget and the absolute total member cap (1000
agents per swarm, section 10). Light mode turns recursive spawning off. Only the root
can create a flat worker fan-out.

---

## 4. Node kinds by terminal behavior

The DAG is agnostic about task type. The structure (decomposition, gates, typed
handoffs) is the same regardless of task. Only the **artifact kind and the definition of
"done"** change. A real task usually **explores, then acts**, and that order is just one
dependency edge. An exploration node feeds an implementation node.

| Kind        | Artifact (output)                       | "Done" contract                 |
| ----------- | --------------------------------------- | ------------------------------- |
| `explore`   | findings (the product of research)      | passes the critique gate        |
| `implement` | diff / commit reference + what changed  | passes the verify gate          |
| `verify`    | pass/fail + the concrete failure        | the check run completed         |
| `fix`       | a patch                                 | re-verification passes          |

- **Verification and fixing** make the system act and repair itself instead of only
  describing the system. A failed verification creates a fix node (the graph grows),
  just as a critique creates gap nodes.
- The final `synthesize` node is **optional**. In a pure exploration task it is the
  deliverable; in an implementation task the merged and verified code is the deliverable
  and the report is a thin summary of what shipped.

```text
explore: how does X work --findings--> plan changes
plan changes --> implement: module A --\
plan changes --> implement: module B --+--> verify: build + tests
verify --fails--> fix node --> verify (loop)
verify --passes--> working feature, committed
```

---

## 5. Dataflow: how a finished dependency passes information

The key insight is that **a dependency edge is also a data channel**. Today a completion
report goes back to the *spawner* (control flow). In a DAG it must also flow toward the
*dependent* node (data flow).

- On completion each node stores a structured **handoff artifact** attached to the node.
- When a downstream task becomes runnable (all dependencies done), the scheduler
  **assembles its input**. It adds the merged artifacts of every dependency to the
  task's own prompt in the worker's starting context. Fan-out (one dependency releasing
  several) and fan-in (several dependencies feeding one) follow naturally.
- An artifact is by default **a reference, not a value**. "I created the API in
  `crates/foo/api.rs`, types are X/Y, commit is `abc123`." The repository and git are the
  shared medium, and the downstream agent reads the files directly. Values carry only
  what the repository does not hold (decisions, designs, analyses). That keeps context
  small, which matters more the deeper you go.
- A composite node's handoff is **decompose then synthesize** (map-reduce). When children
  finish, the owner wakes with their artifacts and writes one consolidated/synthesized
  report. The parent does not concatenate the children's noise; it produces a clean
  summary for the next layer.

---

## 6. Completion and coverage: making thoroughness structural

The goal is that completion is comprehensive enough that the chance of missing a corner
of the task is very low. Comprehensiveness must be a **structural property of the
graph**, not a request in a prompt. Three pressures reinforce each other.

### 6.1 Mandatory decomposition (breadth)

An exploration node is **composite by default**. The agent's first job is not to answer
but to **enumerate the surface** and split it into child nodes. Coverage then becomes
visible and auditable in the graph instead of buried in prose and taken on faith (was
there a hotplug node? a DPI node?).

### 6.2 Critique/verify gates (adversarial gap hunting)

Certain nodes get an automatically inserted **critique** (for exploration) or **verify**
(for code) dependency before the parent may synthesize or close.

- The gate is adversarial. "What corner did this miss?" / "Does it actually work?"
- When it finds a gap or a failure, it puts **new child nodes** back into the graph
  (recursion), and the parent cannot close until those are exhausted.
- That is how "very low chance of having missed something" comes about: as long as an
  open gap or a failed check exists, the graph literally does not allow the parent to
  complete.

```text
explore X --> enumerate facets
enumerate facets --> facet: hotplug --\
enumerate facets --> facet: DPI --------+--> critique
critique --gap found--> facet: cursor crossing --> critique (loop)
critique --no gaps--> synthesize report
```

### 6.3 A typed artifact that names what was not checked (exposing shallowness)

The handoff artifact has a **required schema**, so shallow output shows up structurally.

- `findings`
- `evidence` (file:line references / commit references, not unsupported claims)
- `edge_cases_considered`
- `validation` (verification results for code)
- `open_questions`
- `confidence`
- `what_i_did_not_check`

`what_i_did_not_check` is the trick. Making the agent list what it did *not* investigate
surfaces the corners it did not investigate, and the critique/scheduler turns them into
new nodes. On a complex task, an empty `open_questions` and `what_i_did_not_check` is
itself the risk signal an auditor checks.

So comprehensiveness now means two things, and gates enforce both: is the surface
covered (critique), and does it actually work (verify). Both turn gaps and failures into
new nodes.

### 6.4 The enforcement that exists (2026-07: growth machinery)

Those pressures are implemented as hard engine rules in the `dag` module of
`horon-agent-plan`, not as prompt requests.

- **Root gate (whole-plan audit).** For every deep-mode `seed`, a parentless gate
  (`plan::gate`) is inserted automatically, and it depends on every root-level node.
  Even a flat seed whose nodes all execute atomically cannot reach a terminal state
  without a final adversarial pass, and that pass can `inject_gap` new top-level nodes
  (growth at the top of the tree). Re-seeding widens the root gate's scope and reopens a
  gate that had already passed.
- **Enumeration gate coverage.** A passing deep gate artifact must address every
  completed node in the audit scope by id (the scope is the gate's non-gate
  `depends_on`; one rule for composite gates and the root gate alike). The enumeration
  cap is 20. Past the cap, enumeration is relaxed only for HIGH confidence nodes, and
  medium/low/unreadable confidence nodes must still be addressed by id. "All good, no
  gaps" is structurally refused (`UncoveredSiblings`). The rule that catches a stale
  scope (`StaleGateScope`) refuses a pass when a node entered the scope after the gate
  was dispatched.
- **A turn does not end without an artifact.** Deep mode has no auto-complete. A worker
  turn that ends with its node still running re-queues that node for a new worker once
  (`no_artifact_requeues`), and fails on a repeat. The only ways a deep node closes are
  `expand_node` (decomposition) and `complete_node` (a verified artifact).
- **Growth accounting.** Every node records its origin (`seed`/`expand`/`gap`/`gate`).
  `PlanGraphStatus` carries `seeded_count`/`grown_count`, and `plan_status`/`run_plan`
  print a growth line. So a deep plan that never outgrew its seed shows the
  under-exploration plainly.

---

## 7. Bias budget: what is fixed and what emerges

The core tension is this. Too much prior bias hardcodes a workflow that breaks easily;
too little loses the coverage guarantee. The split is as follows.

### What the first agent decides (a deliberately small seed)
1. The root task's shape (inherited from the user prompt).
2. The first-level decomposition: the initial facet/child nodes and their edges.

Even that first decomposition is **provisional**. Every child can be decomposed again,
and a gate can insert siblings the first agent never imagined. The first agent sets the
*seed*, not the *shape*.

### What the system fixes (structure, not the first agent's choice)
- **Gate discipline:** every composite node gets a critique/verify dependency before it
  closes. There is no option to skip the audit.
- **Handoff contract:** a typed artifact carrying `what_i_did_not_check`/
  `open_questions` is enforced on every node.
- **Recursion right:** any sub-node can extend its own node. The first agent cannot
  "lock" the shape it drew.
- **Gap/failure -> new node:** critique and verification turn what was missed into
  graph, independent of the original plan.

So the coverage guarantee does **not** depend on the first agent being smart. An ordinary
first decomposition still gets caught and expanded.

```text
fully scripted <--> seeded <--> fully emergent
(system dictates facets)   (first agent drafts,    (agent decides everything,
                            gates+recursion          no gates)
                            correct it)
```

We are at **B (seed + structural gates)**.

- **Content / domain knowledge:** almost all of it comes from agents (the first agent
  lays the seed, sub-agents refine it). The system knows nothing about the domain (say,
  scrollwm).
- **Process / strictness / coverage:** the first agent chooses almost none of it. It is
  structural and uniform across the tree.
- **Final shape:** mostly emergent. The first agent's draft is usually a small fraction
  of the final node count, and most nodes are born from re-decomposition and the gaps and
  fixes the gates produce.

**Invariant to hold:** domain assumptions must not leak into the structural layer. Gates
must stay **domain-neutral**. A critique asks "against the scope and artifacts this task
declares, what was not explored", and a verification runs "the acceptance checks this
task declares". Bias toward thoroughness is deliberate; bias toward specific content
should be near zero. Re-running the same task should produce similar first-level facets
(a stable seed) with different deep structure (adaptive exploration).

---

## 8. Interface: an enforced graph API, not an agent script

The interface choice really decides how much strictness can be enforced. The options
considered were:

- **A. The reworked swarm tool (graph operations).** The server owns the graph and
  *enforces* the invariants (acyclicity, ownership, mandatory gates, typed handoffs) on
  every change. This option is the only one that makes "comprehensiveness is structure"
  true.
- **B. The agent writes a script.** It looks powerful, but a script that runs start to
  finish cannot express a graph that grows through runtime discovery. It has to wait for
  node results and re-enter, so it ends up an imperative driver wrapping the same API.
  Meanwhile strictness lives in unverified agent code and the gates can be bypassed. That
  is the under-biased failure mode.
- **C. Primitive tool operations plus optional declarative sugar.** The tools are the
  verified base, a static part is accepted as a spec submitted at once, and runtime
  growth still goes through tool calls.

**Decision: build on A and add C's sugar.** The graph is a server-side object changed by
verified operations, not an agent-side script. The agent keeps complete freedom in
*deciding* the graph (arbitrary reasoning and tool use) and zero freedom in *enforcing*
it, where the structural gates cannot be skipped.

```text
agent reasoning (arbitrary)
  -- submit spec / expand_node --> swarm tool (= graph ops)
  -- validate: acyclic, owned, gated, typed --> server-owned DAG
  -- scheduler: fill_slots / run_plan --> workers (input hydrated from upstream artifacts)
  -- workers: complete_node + artifact --> swarm tool (loop)
```

### The proposed tool surface (the evolution of `swarm`)
- `swarm task_graph {nodes:[...], edges:[...]}` - seeds the initial DAG in one call (the
  first agent's draft). It is the batch form of the operations and gets the same
  validation.
- `swarm expand_node {node_id, children:[...], edges:[...]}` - runtime decomposition
  (recursion). It checks ownership and acyclicity.
- `swarm complete_node {node_id, artifact:{findings, evidence, validation,
  edge_cases_considered, open_questions, confidence, what_i_did_not_check}}` - the typed
  handoff the gates inspect.
- `swarm run` - hands control to the scheduler.
- `spawn` / `dm` / `broadcast` remain as low-level escape hatches.

The "more control" an agent actually wants is per-node prompts, computed fan-out, and
conditional expansion. That is provided (a) by computing the node list the way the agent
wants and *then* submitting it as a verified spec, and (b) by runtime `expand_node` calls.
It is not provided as a scripting language that bypasses enforcement.

---

## 8a. Communication rework: dataflow first, chat later

Before the rework the swarm tool gave agents a rich human-facing chat surface: DM,
whole-swarm broadcast, topic channels (subscriptions/members), a shared key-value
context store, delivery modes (`notify`/`interrupt`/`wake`), and `await_members`. It
bolted a human collaboration metaphor onto agents. For the DAG model that was too much
and the wrong shape. The rework was **subtraction, not addition**, and it is finished:
what remains is DM, subtree-scoped broadcast, `await_members`, and the artifact edges
the scheduler fills.

### Why the current model does not fit
1. **It is chat, not dataflow.** Every existing channel is push-notification messaging
   *between agents*. In a DAG the primary information path is **node -> dependent through
   artifacts on edges**, and that does not yet exist as a primitive. The most important
   "communication" in the new model is what the current tools cannot express, so agents
   must DM each other to imitate dataflow: the lossy coordination we are trying to
   replace.
2. **There are too many overlapping primitives.** DM, broadcast, channel, and shared
   context fan-out are four ways to push text into another agent, and `message` already
   auto-routes among three of them. The codebase even has a synonym-normalization layer
   because the model kept inventing new verbs. That is the smell of an oversized surface.
   More actions means more model errors.
3. **Broadcast must not scale to the member cap.** At a 1000-member cap (section 10), a
   whole-swarm fan-out becomes a 1000-way notification storm on every send. So
   broadcast-style delivery is subtree-scoped (`handle_comm_message`). A sender reaches
   only the subtree it spawned, and only the coordinator keeps whole-swarm reach.

### The goal is a two-layer model
Keep two layers and drop the middle.

- **Layer 1 - structural dataflow (new, primary).** Handoff artifacts on edges. On
  completion a node's typed artifact flows automatically through the scheduler to the
  dependent nodes, and the scheduler fills a newly runnable node's input with upstream
  artifacts. This replaces most of what DM is used for today ("here is what I found, now
  you go"). Unlike fire-and-forget DM, it is typed, durable, reference-based, and
  survives a reload.
- **Layer 2 - exception channel (kept, thin).** Agents contact each other directly only
  in genuinely unstructured cases the graph cannot model: conflict resolution ("we are
  both editing `foo.rs`") and confirmation questions raised up the ownership tree. That
  is **DM + subtree-scoped broadcast**, nothing more.

### What went away
- **The shared context key-value store:** it largely duplicated the repository (the real
  shared medium) and typed artifacts, so it was removed as a second source of truth.
  Pending plan proposals, its one control-state consumer, moved to a typed store
  (`crates/app-core/src/server/plan_proposals.rs`) where `take` is atomic-once.
- **Whole-swarm broadcast:** replaced by a **subtree-scoped broadcast** that reaches only
  the children an agent owns, which is what keeps it from being a storm at the member
  cap. Whole-swarm reach stays with the coordinator only.
- **General topic channels:** unnecessary once dataflow is the structure. Channels are
  how *humans* organize ad-hoc collaboration; agents collaborate through graph edges.

### Alignment with the DAG model
The dependency edge becomes the primary communication channel (typed artifacts), and
agent-to-agent messaging shrinks to a small exception path (DM + subtree broadcast). That
aligns communication with the DAG, removes the broadcast-storm risk at the member cap,
and shrinks an error-prone tool surface.

### Staged migration (nothing was ripped out up front)
Cutting channels and shared context was a real behavior change for existing swarm flows,
so it was staged. All four steps are done.
1. **Done.** Artifact dataflow. Completion artifacts flow to dependents and fill their
   input.
2. **Done.** Broadcasts scoped to the subtree the sender spawned. Whole-swarm broadcast
   remains only as a coordinator escape hatch.
3. **Done.** Flows moved off channels and shared context: the tool schema stopped
   offering them and the prompt stopped recommending them, so nothing new was written
   against the retired surface.
4. **Done.** The duplicate chat primitives were removed in one pass, because the daemon
   wire is same-binary and has no external producers:

   | Removed | What replaced it |
   | --- | --- |
   | `swarm` actions `share`, `share_append`, `read` | node artifacts on graph edges, and `status`/`summary` to observe the swarm |
   | `swarm` actions `channel`, `list_channels`, `channel_members`, `subscribe_channel`, `unsubscribe_channel` | `message` with `to_session` (DM) or without it (subtree broadcast) |
   | the `channel` argument on `swarm message` | the same two forms |
   | wire requests `CommShare`, `CommRead`, `CommListChannels`, `CommChannelMembers`, `CommSubscribeChannel`, `CommUnsubscribeChannel` | none; they existed only to serve the removed actions |
   | events `CommContext`, `CommChannels`, `ContextUpdate`, notification `SharedContext`, the notification `channel` field | none |
   | debug commands `swarm:context*`, `swarm:set_context`, `swarm:channels` | `swarm:proposals*` and `swarm:info` |

   A new call naming a removed action is rejected with a message that names the
   replacement instead of silently degrading to a broadcast, and a stored session
   written before the removal still replays its calls (both are pinned by tests:
   `tool::communicate::tests::input_format::retired_channel_and_shared_context_actions_are_rejected`
   and `session::tests::retired_swarm_actions`). The public Harness API never carried
   this surface, so no client-visible version bump was needed; `docs/protocol/HARNESS_API.md`
   records the removal under protocol 1.21.

---

## 8b. Named agent definitions

A spawn can name a role instead of restating it. `swarm spawn` accepts
`agent: "<name>"`, resolved against Markdown files with YAML frontmatter:

- `<workspace>/.horon/agents/*.md` first, then `~/.horon/agents/*.md`.
- The first file carrying an exact name wins, so a project definition shadows a
  home definition of the same name.
- No definitions means spawn behaves exactly as it did before.

Frontmatter keys follow this repository's spawn vocabulary rather than another
tool's: `name`, `description`, `tools` (allow list), `model`, `effort`,
`output_schema` (JSON Schema). The Markdown body is the role prompt. `tools`
accepts a YAML sequence or a comma-delimited string.

Resolution happens before any session exists:

- Explicit spawn parameters override definition values, so a caller can reuse a
  role and change its model for one run.
- An unknown name fails and lists the names that do exist.
- A file that cannot be parsed is reported under its own name rather than
  hidden, while other definitions stay usable.

The definition's `tools` reaches the child as an allow list. It narrows the
child's registry and never widens it: a definition naming three tools gets those
three, not the configured profile's surface. `model` is a plain model name, not
a model role.

`swarm` action `list_agent_defs` lists the definitions visible from the current
workspace. It is a separate action from `list`, which still means "list swarm
members" and keeps `list_agents` as its alias.

## 8c. Fan-out: `spawn_many`

A parent that fans work out pays one model round trip per child when it calls
`swarm spawn` once per child. `swarm spawn_many` starts several children from one
call:

- `tasks`: a list of `{label, initial_message?, agent?, model?, effort?, isolation?, output_schema?}`.
  `prompt` is accepted as an alias for `initial_message`, the same way the single
  spawn accepts it.
- `context`: shared background prepended to each child's first message.

The rules that keep a batch honest:

- Labels must be unique within one call, compared case-insensitively. A
  duplicate fails the call before any child exists.
- An empty `tasks` list is rejected.
- Each task runs the single-spawn request path, and the tasks start
  concurrently, one socket request each.
- Call-level `model`, `effort`, `output_schema`, `agent`, `isolation`,
  `working_dir` and `spawn_mode` act as defaults for the tasks that omit them.
  A task value wins.
- The result keeps input order and carries one entry per task: the child's
  session id, or the error that stopped that task. One failure never cancels the
  others.
- Both spawn caps still reject rather than queue: `agents.swarm_max_concurrent_agents`
  for concurrency and `MAX_SWARM_MEMBERS` for total live members. A caller must
  not assume that three tasks always produce three children.

`context` is prepended to the child's first user message, not rendered as a
system-prompt section, because the child session exists before its first turn.

## 8d. Consult: `source_session`

A parent can ask a question of an earlier conversation instead of reading its
transcript by hand. `swarm spawn` accepts `source_session: "<id>"`:

- The child starts from a copy of that session's transcript, made the same way
  `split` forks a session. The source session is never written to.
- The question is the child's first new user message, and the answer comes back
  through `agent://` like any other child result.
- The child's registry is restricted to `read`, `agentgrep`, `ls` and `yield`.
  The restriction is applied where the child session is created, so a wire
  client naming a source session directly cannot widen it.
- An unknown id fails before a child session exists. So does an id outside the
  caller's spawn tree: that tree is the set of sessions sharing a top-most
  ancestor, and a `history://<id>` read uses the same rule (8e).
- Plan mode needs no rule of its own. `swarm` declares no effects of its own,
  so it keeps the default write barrier and a read-only plan state already
  refuses the spawn.

The fork names the source as its parent rather than the caller, so the child sits
in the source's tree. Its model, effort and tool policy still come from the spawn
parameters, not from the source session.

## 8e. Child results and `history://<id>`

`agent://` answers from the in-memory cache first and from a durable copy second.
When a child completes, its rendered result is written atomically to
`<owning-session>/child-results/<child>.md`, plus `<child>.json` when the child
yielded structured output. Both are redacted with the same `redact_secrets` pass
the cache read applies, so a secret a child echoed is no more readable from the
file than from the cache. The copy outlives the process and the retention bound:
an id the cache evicted still answers, and `agent://` lists it as `stored`.

The copies belong to the session that spawned the children, so removing that
session's directory removes them with it. The child's own event log stays the
authoritative record; the stored result is only the projection written at
completion, and a read never serves bytes that disagree with it.

`history://<session-id>` renders the stored transcript of a session in this
session's spawn tree: the same boundary a consult fork uses, so a coordinator can
re-read a child's conversation after it finished without being able to browse
anyone else's. `history://current` and `history://<id>` both follow
`[tools] context_notes`, and a selector applies to either, so `history://<id>:40-80`
is a bounded peek.

---

## 9. Example: how the graph changes over time

Task: "investigate multi-monitor support in scrollwm". Status legend: pending, running,
composite (decomposing / waiting on children), critique, done.

### T0 - the first agent draws the top-level skeleton
The root agent does not answer. It lays a seed: explore, gate, synthesize.

```text
explore multimonitor (composite) --\
                                    +--> critique --> synthesize report
explore multimonitor (composite) --/
```

### T1 - the root expands explore into facets (composite -> children)
```text
facet: geometry/layout -----------\
facet: hotplug/disconnect ---------+
facet: DPI/scaling ----------------+--> critique --> synthesize
facet: focus/cursor crossing ------+
facet: workspace to output map ----+
facet: existing code touchpoints --/
```

### T2 - the scheduler dispatches the ready facets (fan-out, parallel workers)

### T3 - one facet decomposes itself (recursion)
`w2` reports that hotplug is deep and expands its own node. It now owns a sub-DAG.

```text
hotplug (owner: w2, composite) --> critique
  --> udev/event source --\
  --> reflow on remove ---+--> hotplug synth --> critique
  --> restore on re-add --/
```

### T4 - the atomic facets finish. Now the edges carry artifacts
`F1,F3,F4,F6` complete with typed artifacts. The critique is blocked on `F2`.

### T5 - the hotplug children finish. The owner wakes to synthesize (reduce)
`w2` reads H1/H2/H3 and writes one clean hotplug report. The composite node closes.

### T6 - the critique finds gaps and grows a new graph
The auditor reads every facet's `what_i_did_not_check`. Nobody covered fullscreen on a
single output or mixed refresh rates. The auditor injects new nodes and a re-critique,
and synthesize stays blocked.

```text
critique (gaps found) --> fullscreen on one output --\
critique (gaps found) --> mixed refresh rate --------+--> re-critique --> synthesize
```

### T7 - the gap nodes finish, re-critique passes, and synthesize runs
Synthesize collects every upstream artifact (by reference) and writes the final report.

What the example shows: breadth (coverage exposed as facets), recursion (hotplug
decomposing itself), dataflow on edges (artifacts filling dependent input), map-reduce
per composite (the owner synthesizes), and comprehensiveness as a gate (the critique
turns what was missed into graph, and the parent cannot close with an open gap). The
graph is not drawn once and finished. It grows wherever depth or a gap is found, and
attention shrinks as subtrees fold into synthesized artifacts.

---

## 10. Data model changes (against `horon-agent-plan`)

It reuses `VersionedPlan` / `PlanItem` (which already has `blocked_by` edges,
`summarize_plan_graph`, `next_runnable_item_ids`, `newly_ready_item_ids`,
`run_plan`/`fill_slots`). It adds:

- `PlanItem`: `owner_session`, `kind: atomic | composite` (and the terminal-behavior
  kind: `explore | implement | verify | fix`), `parent_node`,
  `output: Option<HandoffArtifact>`.
- `HandoffArtifact`: the typed schema of section 6.3.
- New operation-based changes: `expand_node(node_id, children, edges)` and
  `complete_node(node_id, artifact)`. They apply ownership checking, acyclicity
  checking, and versioning. `task_graph` is the batch seed form.
- Scheduler: hydrate worker input from upstream `output` at dispatch. At a composite
  join, wake the owner again to synthesize. Auto-insert critique/verify dependencies
  according to gate discipline.
- Roles (coordinator / worktree manager) become scheduler policy rather than
  user-visible entities.

### Runaway protection: one total member cap
Runaway protection is one cap, not a matrix of limits. A swarm may hold at most
**`MAX_SWARM_MEMBERS` = 1000** live members (agents). There is deliberately **no depth
cap and no per-node breadth/fan-out cap**. The spawn tree may nest and fan out freely
until the swarm reaches 1000 members, at which point an additional spawn is refused with
a clear error. This is implemented in `ensure_spawn_coordinator_swarm`
(`server/comm_session.rs`) by counting the swarm's live members and refusing a spawn at
the cap. The old `MAX_SWARM_SPAWN_DEPTH` depth limit has been removed.

### Honest trade-offs and limits
- The single member cap is the only regulator. It bounds total concurrency and cost, but
  it cannot stop a lopsided tree (say, one greedy node eating much of the budget). That
  judgment is left to the agents and the gate discipline.
- The graph **orders** work but does not mutually exclude it. Two subtrees editing the
  same file is still the "talk it over by DM, no lock" case, as today.
- Domain bias must stay out of the structure/gate layer (section 7 invariant).

---

## 11. Recommended implementation order

1. Land the typed `HandoffArtifact` schema and the `PlanItem` field additions in
   `horon-agent-plan`.
2. Add the verified `expand_node` / `complete_node` / `task_graph` operations (ownership
   + acyclicity + automatic gate insertion).
3. Extend the scheduler to hydrate downstream input from upstream artifacts and to wake a
   composite owner to synthesize.
4. Build a text-based simulator to watch the graph change (like section 9) and to check
   the scheduler/critique/verification machinery before wiring it to a live swarm.
5. Rework the tool surface (`task_graph`/`expand_node`/`complete_node`/`run`) around the
   graph-first view (before the headless switch, this item also changed the TUI to the
   same view). `spawn`/`dm` remain escape hatches.