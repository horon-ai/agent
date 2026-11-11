# Safety and approvals

Language: **English** | [한국어](SAFETY.ko.md)

The safety system, and approval waits and automatic failover when nobody is at the screen.

## Safety system

> **Status:** design record
> **Updated:** 2026-02-08
> **How to read:** this is a design record written on 2026-02-08 and is not the current implementation state. In the body below, what exists in code carries a `file:line` reference, and what exists only as design and is not in the tree says so. To check current behavior, read `crates/base/src/safety.rs` and `crates/app-core/src/tool/ambient.rs`.

A human-in-the-loop safety layer for agent actions that run without supervision. It is designed as an independent subsystem that any horon agent feature can attach to. Only ambient mode uses it today, but it was separated on purpose so later features can use it again.

### Overview

When the agent acts without the user watching directly (ambient mode, for example), it needs a way to:
1. **know what is allowed without asking**
2. **request permission** for actions that need human approval
3. **tell the user** that a request is pending
4. **wait or move on** while the user reviews
5. **report what it did** after the session ends

The safety system provides all of this. There are only two tiers: auto-allowed and requires permission. There is no "always denied" tier. Once the user explicitly approves, the agent may do the work. The core principle is that **anything that communicates with other people or leaves a trace outside the local sandbox needs permission**.

---

### Structure

```text
Agent (e.g. Ambient Mode)
  agent wants to take action
    -> Action Classifier -> action classification
         safe         -> auto-allowed: execute immediately -> Transcript Logger
         needs review -> requires permission: request_permission tool -> Review Queue
Review Queue (queue.json)
  -> Notification Dispatcher -> ntfy.sh push, Desktop Notification
  -> user review: phone notification / record_decision API, approve or deny
  -> decision -> back to the agent
Transcript Logger -> Session Reporter
```

There is no review screen in this build. The TUI review panel was deleted with the TUI crate (the deletion list in `AGENTS.md`), and there is no CLI subcommand such as `horon safety review`. The user decides by answering a notification or by calling `SafetySystem::record_decision` (`crates/base/src/safety.rs:597`).

---

### Action classification

Every action the agent wants to take is classified into one of two tiers. There is no "always denied" tier. Once the user approves, the agent may do the work. The safety system's job is to guarantee that the user is asked, not to block actions outright.

Classification is name based. `classify` lowercases the action name and compares it with the auto-allowed list, and anything not on the list is treated as requiring permission (`crates/base/src/safety.rs:364-374`). The auto-allowed list holds eleven names: `read`, `glob`, `grep`, `ls`, `memory`, `todo`, `todowrite`, `todoread`, `conversation_search`, `session_search`, `codesearch` (`crates/base/src/safety.rs:306-317`).

#### Tier 1: auto-allowed (no permission needed)

Actions that happen locally, can be undone, and do not affect anything outside the project sandbox.

| Action | Why |
|--------|-----------|
| Reading files inside the project | Read-only, so there are no side effects |
| Reading git history and status | Read-only |
| Running tests (read-only) | Only verifies, changes no state |
| Memory operations (within the per-cycle cap) | Local data, and reversible |
| Creating local branches and git worktrees | Local only, and easy to delete |
| Writing ambient's own logs and state files | Internal records |
| Embeddings and similarity search | Pure computation |
| Analyzing sessions for extraction | Read-only analysis |

#### Tier 2: requires permission (ask the user)

Actions that leave a trace outside the local sandbox, affect shared state, or are not easy to undo. **The general rule: anything that communicates directly with other people always requires permission, without exception.**

| Action | Why |
|--------|-----------|
| **Communication with people (always tier 2)** | |
| Sending email | Irreversible, and other people see it |
| Submitting coursework | It has academic consequences |
| Posting to a messenger or chat | Other people see it |
| Creating GitHub issue and PR comments | Publicly visible |
| Any form of direct communication with a person | Cannot be taken back after sending |
| **Code changes** | |
| Modifying repository code (must go through a worktree and PR) | Needs review before merge |
| Pushing to a remote | Collaborators see it |
| Opening a pull request | Collaborators see it |
| Modifying CI/CD pipelines | Affects shared infrastructure |
| **System changes** | |
| Installing system packages | Changes system state |
| Editing dotfiles and system settings | Affects other tools |
| Starting network services and opening ports | Affects security |
| **Deployment** | |
| Deploying to any environment | Affects users and services |
| **Data** | |
| Deleting files outside the project sandbox | May be unrecoverable |
| Dropping databases and clearing large caches | Risk of data loss |
| **Money and accounts** | |
| Purchases and billing changes | Financial consequences |
| Changing passwords, API keys, authentication | Security consequences |
| Revoking tokens and editing permissions | Access consequences |

#### Custom rules

This build has no rule table that promotes or demotes actions. There are no subtables such as `[safety.rules]` or `[safety.notifications]` either. Every field of `SafetyConfig` sits flat under `[safety]` (`crates/config-types/src/lib.rs:1543-1550`), and the approval-wait policy lives under `[ambient]` (`:1473-1510`).

```toml
[safety]
desktop_notifications = true
# ntfy_topic = "horon-agent-ambient-your-secret-topic"
# ntfy_server = "https://ntfy.sh"

[ambient]
# approval_timeout_secs = 1800
# approval_default_on_timeout = "deny"
# approval_for_clientless_runs = false
```

`approval_timeout_secs` defaults to 1800 seconds (`crates/config-types/src/lib.rs:1584`, `:1598-1600`). When it elapses, `approval_default_on_timeout` applies, and its default is `"deny"` (`:1589`, `:1602-1604`). A pending request that expires leaves a record, and a history that cannot be audited closes approvals toward denial (`crates/base/src/safety.rs:340-360`).

---

### Permission request flow

```text
1. Agent -> Classifier   "I want to create a PR"
2. Classifier            classify action -> Tier 2
3. Classifier -> Agent   permission required
4. Agent -> Review Queue request_permission({action, description, rationale})
5. Review Queue          store the pending request in queue.json
6. Review Queue -> Notifier dispatch notification
7. Notifier -> User      ntfy / desktop notification
8. Agent decides         wait: block on this action, continue other work
                         move on: skip this action, continue with the next task
9. User -> Review Queue  approve (record_decision)
10. Review Queue -> Agent permission granted
11. Agent                 waited: execute the action now
                          moved on: execute on the next ambient cycle
```

#### The `request_permission` tool

This tool is not part of the core 17 tools. The ambient runner registers it, and the session's tool allowlist has to admit it for the call to happen. For unattended runs with no client attached, it joins the allowlist only when `[ambient] approval_for_clientless_runs` is on (`crates/base/src/config.rs:752-757`, `:1153-1170`). The default is off, and the core set stays at 17 (`crates/base/src/config.rs:779-799`, tests in `crates/base/src/config_tool_tiers_tests.rs:26-40`).

Calling it outside an ambient session is refused with the error "request_permission is only available to ambient sessions" (`crates/app-core/src/tool/ambient.rs:105`).

```rust
// Tool: request_permission
{
    "action": "create_pull_request",
    "description": "Create PR for ambient/fix-auth-tests branch with 3 test fixes",
    "rationale": "Found 3 failing tests in auth module. Fixed them on a worktree branch.",
    "urgency": "low",           // "low" | "normal" | "high"
    "wait": false,              // should the agent block until approved?
    "context": {                // optional reviewer context
        "summary": "...",
        "why_permission_needed": "...",
        "files": ["src/auth.rs"],
        "risks": ["..."],
        "rollback_plan": "..."
    }
}
```

The schema's required fields are `action`, `description`, and `rationale`; the rest are optional (`crates/app-core/src/tool/ambient.rs:542-600`). Execution queues the request, writes the pending record to disk, and then sends the notification (`:689-800`). The notification goes out only after the record exists, because whoever answers has to be able to find the record the notification points at right away.

**Responses:**
```rust
// If wait=true and user responds:
{ "approved": true, "message": "looks good" }

// If wait=true and timeout:
{ "approved": false, "reason": "timeout", "timeout_minutes": 30 }

// If wait=false:
{ "queued": true, "request_id": "req_abc123" }
```

#### Agent behavior while waiting

When the agent requests permission with `wait: true`, it behaves like this.
- It does not block the whole cycle. It moves on to other ambient work.
- Once the user approves, the action is scheduled for the next cycle. If the current cycle is still running, it goes into the current cycle.
- If no response arrives within `approval_timeout_secs`, the request expires and is handled according to `approval_default_on_timeout`.

When the agent requests permission with `wait: false`:
- The request goes into the queue for user review.
- The agent continues without that action.
- If it is approved later, the next ambient cycle handles it.

---

### Notification system

#### Channels

```text
Notification Dispatcher
  -> ntfy.sh push
  -> Desktop notification (notify-send / macOS broker)
```

A failed send does not block the flow. The error goes to the log and work continues (`crates/app-core/src/notifications.rs:1-7`).

**Notification contents:**
- What the agent wants to do (action and description)
- Why it wants to do it (rationale)
- How to approve or deny (reply instructions)
- Urgency

#### Configuration

```toml
[safety]
# Turn desktop notifications on (default: true)
desktop_notifications = true

# ntfy.sh push
# ntfy_topic = "horon-agent-ambient-your-secret-topic"
# ntfy_server = "https://ntfy.sh"
```

This build has two notification channels: ntfy.sh and desktop notifications. Email, messenger bot, and cloud relay channels were removed, and `SafetyConfig` has no keys for them (`crates/config-types/src/lib.rs:1674-1682`). If an old config file still contains those keys, it loads and ignores them as unknown fields.

Preferences such as bundling intervals, quiet hours, and minimum urgency do not exist in this build either.

---

### Session records and summaries

The safety system produces a report at the end of every ambient cycle, or at the end of every agent session that ran unattended.

#### Transcript

The full record of every action performed, with its context.

```json
{
    "session_id": "ambient-2026-02-08-143022",
    "started_at": "2026-02-08T14:30:22Z",
    "ended_at": "2026-02-08T14:35:18Z",
    "provider": "openai",
    "model": "5.2-codex-xhigh",
    "token_usage": { "input": 12400, "output": 3200 },
    "actions": [
        {
            "type": "memory_consolidation",
            "description": "Merged 2 duplicate memories about dark mode preference",
            "tier": "auto_allowed",
            "details": { "merged": ["mem_abc", "mem_def"], "into": "mem_ghi" }
        },
        {
            "type": "permission_request",
            "description": "Create PR for 3 auth test fixes",
            "tier": "requires_permission",
            "status": "pending",
            "request_id": "req_abc123"
        }
    ],
    "pending_permissions": 1,
    "scheduled_next": "2026-02-08T15:05:00Z"
}
```

The record types are `AmbientTranscript` and `ActionLog` (`crates/base/src/safety.rs:265-300`). `save_transcript` handles persistence (`:678`).

#### Summary

A human-readable summary sent to the configured notification channel. `generate_summary` builds the string (`crates/base/src/safety.rs:634`).

```
Ambient cycle completed (4m 56s)

Done:
- Merged 2 duplicate memories (dark mode preference)
- Pruned 1 stale memory (confidence: 0.02)

Needs your review:
- [Approve/Deny] Create PR for auth test fixes (3 files changed)

Next cycle: ~35 minutes
```

#### Delivery

- **Always:** the queue and history files are left behind.
- **With notifications on:** the summary and approval requests go out through ntfy.sh and desktop notifications. A failed send is only logged and does not block the cycle.
- **Replies:** this build has no path that records a decision from a notification reply. Decisions are recorded with `record_decision` or `record_permission_via_file`.

---

### Review queue

#### Storage location

```
~/.horon/safety/
├── queue.json              # Pending permission requests
└── history.json            # Past decisions
```

`queue_path` and `history_path` decide the paths (`crates/base/src/safety.rs:698-704`).

#### Review interface

**1. File-based decision recording**

When an external tool records a decision, it uses `record_permission_via_file` (`crates/base/src/safety.rs:907`). Expiry cleanup is handled by `expire_stale_permissions_via_file` (`:929`).

**2. Library call**

`SafetySystem::record_decision` records a decision inside the process and wakes a waiting caller (`crates/base/src/safety.rs:597`).

This build has no TUI review panel and no `horon safety review` CLI.

#### Decision history

Past decisions are stored so they can be audited.

```json
{
    "request_id": "req_abc123",
    "action": "create_pull_request",
    "decision": "approved",
    "decided_at": "2026-02-08T14:42:00Z",
    "decided_via": "cli",
    "response_time_seconds": 420
}
```

If the history is corrupt and cannot be read, approvals close as denial. The point is not to let an approval through in a state that cannot be audited (`crates/base/src/safety.rs:340-360`, `:504`).

---

### Filesystem path policy

The path policy in this section and the `ask` contract below arrived in the same change. Both filter tool calls themselves, separately from permission requests.

#### Deny-first hook

A session can register one predicate that `read`, `write`, `edit`, `replace`, `apply_patch`, `ls`, and `agentgrep` consult when they touch a path (`crates/app-core/src/tool/path_policy.rs:1-6`). The predicate can only deny. There is no path that flips a denial into an allowance. A session that registers nothing behaves exactly as it did before this hook existed.

```rust
pub type PathDenyPredicate = Arc<dyn Fn(&Path) -> Option<String> + Send + Sync>;
```

Returning `Some(reason)` denies the call and shows that reason to the caller. `None` falls through to the tool's own checks. There is no way for the predicate to allow a path the tool would otherwise deny, and that is what "deny-first" means (`crates/app-core/src/tool/path_policy.rs:23-28`). Registration goes through `register_session_path_policy` (`:63`), and when the registration token is dropped the session's policy disappears (`:43-60`).

The limits are written in the source. `bash` reaches every path through a shell and is outside this hook, and a symlink can point at an allowed path while resolving to a denied one. This is a guard against mistakes, not a sandbox (`crates/app-core/src/tool/path_policy.rs:9-11`).

---

### The `ask` tool contract

`ask` is the tool that offers the user choices. In this build that tool **waits only while a client that can answer is attached to the session. That wait is cut off at 300 seconds.**

The daemon is headless. It cannot guarantee a client screen is attached. So `ask` first checks whether a reply channel is registered on the session (`crates/app-core/src/tool/ask.rs:357`). The channel is installed when a connection attached to the session advertises `supports_ask_requests`, and it is withdrawn when the connection drops or reattaches to a different session (`crates/app-core/src/server/client_lifecycle.rs:2045-2053`, `crates/app-core/src/tool/ask_bridge.rs:72-88`). The channel's existence is itself the signal that "this connection can show a question and answer it" (`crates/app-core/src/tool/ask_bridge.rs:6-13`). The advertisement is negotiated through `supports_ask_requests` in the harness API `Hello` (`crates/harness-api/src/requests.rs:42-45`).

- No channel: after validating the request, it decides the answer with `[tools] ask_policy` and returns immediately. It does not wait (`crates/app-core/src/tool/ask.rs:372-377`).
- Channel present: it emits `ask_requested` and waits for `ask_response` (`crates/app-core/src/server/client_ask.rs:80`, `crates/app-core/src/tool/ask.rs:386-408`). The cap is `CLIENT_ANSWER_TIMEOUT`, 300 seconds (`crates/app-core/src/tool/ask.rs:31`). So a session with a client attached can have a turn delayed by five minutes in the worst case.
- No answer (timeout, disconnection, or a response with no usable option): it falls back to the policy and states the reason in the tool text (`crates/app-core/src/tool/ask.rs:216-236`, `:361-369`). With `ask_policy = "error"` the call fails at that point (`:362-367`).

Choices sent by the client are not trusted as-is either. Labels that are not among the question's options are dropped. A question with no usable label is filled with the recommendation and marked as chosen automatically (`crates/app-core/src/tool/ask.rs:238-280`).

#### The two policies

The policy governs what happens when the client cannot answer. `AskPolicy` has two variants (`crates/tool-types/src/lib.rs:110-116`).

```rust
pub enum AskPolicy {
    /// Answer every question with its recommended option, or the first one.
    #[default]
    Recommended,
    /// Refuse the call with an explanatory tool error.
    Error,
}
```

- `recommended` (the default): answers each question with the recommendation the model produced. If the model produced none, it uses the first option. The tool text states that the choice was automatic so the model knows (`crates/app-core/src/tool/ask.rs:203-214`).
- `error`: when no client can answer, or no answer arrived, it refuses the call with an explanation. The model then decides for itself and continues (`crates/app-core/src/tool/ask.rs:362-367`, `:374-376`).

The config field's comment documents the same distinction. `recommended` picks the model's recommendation and says so. `error` refuses the call. The comment also covers the attached-client case. It asks a client that advertised `supports_ask_requests` first, waits up to 300 seconds, and then falls back to the policy (`crates/base/src/config.rs:1034-1036`). The field is `pub ask_policy: AskPolicy` (`:1037`), and the default is `AskPolicy::Recommended` (`:1119`). The type is defined in `crates/tool-types` and re-exported from `crates/base/src/config.rs:17`.

#### Request limits

One call takes up to four questions, and each question has two to five options (`crates/app-core/src/tool/ask.rs:33-35`). Requests outside these limits are rejected during validation. Too many questions is caught at `:100-105`, and an option-count violation at `:118-120`.

#### When it is available

`ask` is one of the core 17 tools (`crates/base/src/config.rs:779-796`). So it is always reachable in the default headless `serve` and `run`. A default `core` session does not put the `ask` schema in the request and only lists its name in the system prompt index; the model calls it through `toolbox`. It differs from `request_permission`, which asks about actions that need permission. `request_permission` is ambient-only and joins the allowlist only for clientless unattended runs, while `ask` is always reachable on the tool surface. So what `ask` does in an unattended run is not obtain an approval but keep the turn moving to the next judgment even when no answer is available.

---

### Integration API

The safety system offers a simple API so any horon agent feature can use it. Below are the signatures in the tree (`crates/base/src/safety.rs`).

```rust
pub struct SafetySystem {
    queue: Mutex<Vec<PermissionRequest>>,
    history: Mutex<Vec<Decision>>,
    actions: Mutex<Vec<ActionLog>>,
    // ...
}

impl SafetySystem {
    /// Classify an action name into one of the two tiers.
    pub fn classify(&self, action: &str) -> ActionTier;

    /// Queue the request and leave it on disk. Sends no notification.
    pub fn request_permission(&self, request: PermissionRequest) -> PermissionResult;

    /// Queue the request with an explicit wait window.
    pub fn request_permission_with_timeout(
        &self,
        request: PermissionRequest,
        timeout: Duration,
    ) -> PermissionResult;

    /// Record a decision and wake a waiting caller.
    pub fn record_decision(&self, /* ... */) -> RecordDecisionOutcome;

    /// The list of pending requests.
    pub fn pending_requests(&self) -> Vec<PermissionRequest>;

    /// A human-readable summary.
    pub fn generate_summary(&self) -> String;

    /// Persist a cycle transcript.
    pub fn save_transcript(&self, transcript: &AmbientTranscript) -> Result<()>;
}

pub enum PermissionResult {
    Approved { message: Option<String> },
    Denied { reason: Option<String> },
    Queued { request_id: String },
    Timeout,
}

pub enum Urgency {
    Low,
    Normal,
    High,
}
```

Notifications are handled by separate functions. `dispatch_permission_notification` sends to the channels (`crates/base/src/safety.rs:71`), and `register_permission_notifier` and `register_decision_notifier` attach callbacks (`:56`, `:86`).

---

### Implementation stages

This section is the plan from design time. It splits what is in the tree now from what is not.

#### Stage 1: foundation
- [x] Action classifier (auto-allowed list and requires-permission tiers, `crates/base/src/safety.rs:306-374`)
- [x] Review queue (persistent storage, `crates/base/src/safety.rs:698-708`)
- [x] `request_permission` tool for the agent (`crates/app-core/src/tool/ambient.rs:535-620`)
- [x] Transcript logger and session summary (`crates/base/src/safety.rs:627-690`)

#### Stage 2: notification channels
- [x] Desktop notifications (`crates/app-core/src/notifications.rs:1-7`)
- [x] ntfy.sh push
- [ ] Webhook support
- [ ] Notification bundling and quiet hours
- [ ] SMS (Twilio and similar)

#### Stage 3: review interface
- [ ] Desktop review panel
- [ ] CLI commands (`horon safety review/list/approve/deny/log`)

#### Stage 4: configuration
- [x] `[safety]` config section (`crates/base/src/config/default_file.rs:817`)
- [x] `[ambient]` approval policy (`crates/config-types/src/lib.rs:1580-1595`)
- [ ] Custom classification rules (promote or demote actions)
- [ ] Per-project overrides

#### Stage 5: intelligence
- [x] Decision history tracking (`crates/base/src/safety.rs:702-704`)
- [ ] Pattern detection (suggest promotions automatically)
- [ ] Inferring urgency from context

## Unattended approvals and automatic failover

> **Status:** implemented (remaining limits are listed under [Limits](#limits))
> **Updated:** 2026-10-18
>
> Both features are off by default. Approval waits are turned on with `[ambient] approval_for_clientless_runs = true`, and
> automatic failover with `[provider] autonomous_failover = true`. An approval nobody answers is closed with the
> default (deny) after 1800 seconds, and failover runs up to 3 times per run chain. Sessions used in front of a screen
> have both values off and behave exactly as before.

This document covers only two things that run on top of ambient mode: approval waits and automatic failover.

### Approval flow

1. **Request.** The model calls the `request_permission` tool. The implementation is
   `execute_request_permission` in `crates/app-core/src/tool/ambient.rs`. The entry gate is
   `ensure_ambient_session`, and it passes when the session is registered as ambient or
   `approval_for_clientless_runs` is on. The request goes into the safety queue, and `expires_at` is now plus
   `approval_timeout_secs`. This stage only queues the request and sends no notification.
2. **Waiting record.** `BlockedApprovalStore::add` then writes the waiting record for this request to disk.
   That record is exactly what an approver has to release the moment the notification arrives, so it has to exist
   before the notification.
3. **Notification.** Only after the record is written does `safety::dispatch_permission_notification` send the request
   to the registered dispatchers. If the record was not written, no notification goes out, so an approver who gets a
   notification never receives "no such request". A turn with `wait = true` then emits the session event
   `ApprovalPending { request_id, action, expires_at }` to the session sink and moves into waiting.
4. **Wait.** With `wait = true`, `wait_for_decision` waits on `SafetySystem::await_decision` for `approval_timeout_secs`.
   While waiting it renews the waiter lease every 30 seconds, and if the lease moves to another process this turn
   ends as `Superseded` and the parent turn is aborted. With `wait = false` it returns `Queued` and leaves only a
   record with no waiter, so a runner picks it up later.
5. **Decision or timeout.** The decision is observed through two paths, the in-process notification and a re-read of
   `history.json`, so a decision another process wrote to the file, such as a CLI or a channel, is reflected too. When
   time runs out, `apply_timeout_default` records the request's default through `record_decision` and leaves
   `via = "timeout_default"`. If `history.json` is corrupt, the result is recorded as denial regardless of the
   default (`corrupt_history_deny`).
6. **One resume.** The ambient runner's `resume_blocked_approvals` judges each record with
   `classify_blocked_approval`, and on approval `run_blocked_resume` continues the session for one turn. A denial
   closes with zero resumes.

What makes exactly-once is the session's event log. `Session::append_resume_key` appends a
`MetadataChanged` event carrying `resume_key` and returns only after that append has reached fsync. Calling again
with the same key adds nothing. However, the key and the turn are not made durable together in one ACK. The key
becomes durable first and the turn is created after, so a "key present, turn missing" state really can happen, and
the exec claim path recovers and runs that state exactly once (the path covers it rather than atomicity doing so; the
details and the remaining window are under [Limits](#limits)). Attempt counts are bounded by two kinds of claim
file. `~/.horon/approvals/resume-claims/<request_id>.json` bounds turn-creation attempts at 2, and
`resume-exec-claims/<request_id>.json` bounds execution attempts of an already created turn at 2 (the run lease
defaults to 30 minutes). If the caps are used up without finishing, the record closes as `needs-human`, and a
`RunNeedsHumanResume` event and a `dispatch_needs_human_resume` notification go out once. It is not dropped
silently.

### Failover flow

1. **Provider refusal.** When the provider refuses, a typed `ProviderFailoverPrompt` arrives on the error.
   Before turning the error into a string, the turn loop extracts this prompt with `failover_required_from_error`.
2. **Unattended check.** Automatic failover applies only when `[provider] autonomous_failover` is on and
   `session_has_attached_client` is false. With a client attached it exits as `NotApplicable`, the original provider
   error returns to the turn unchanged, and the client decides whether to switch.
3. **Budget.** `crates/app-core/src/agent/failover_budget.rs` records attempt counts in
   `~/.horon/failover-budget/<safe-session-id>.json`. The budget unit is `chain_id`, and an attempt is consumed
   **before** the route changes. That is the deliberate choice so a refused switch does not leave the attempt
   unused; the cost is that a session whose targets are all ineligible spends budget without switching.
4. **Route change.** `apply_autonomous_failover` in `crates/app-core/src/server/provider_control.rs` resolves the
   prompt's `target_route` into an actual route. It looks in the live route catalog first, and otherwise parses the
   spec and accepts it only if round-tripping produces the same string. It then checks pool eligibility for
   automatic failover and applies the route.
5. **Resend.** Once the switch is applied, the turn loop resends the same turn with `ProviderErrorAction::Resend`.
   `FailoverSwitched { chain_id, from, to, reason, attempt, max }` is written to `~/.horon/failover-events.json`
   before it goes to the session sink.

A new user prompt opens a new chain with `begin_failover_chain_for_prompt`. Retries inside one turn use that chain
as-is, so the cap applies to the run chain a single prompt opens, not to the process.

### Configuration

| Key | Section | Default | Behavior |
| --- | --- | --- | --- |
| `approval_timeout_secs` | `[ambient]` | `1800` | Seconds a `wait = true` call waits for a decision. When it passes, the default below applies. |
| `approval_default_on_timeout` | `[ambient]` | `"deny"` | Result to apply on timeout. `"approve"` and `"allow"` read as approval, ignoring case and surrounding whitespace; any other value, typos included, is treated as denial, and `"deny"` is both the default and the failure direction. |
| `approval_for_clientless_runs` | `[ambient]` | `false` | Allows `request_permission` even in sessions not registered as ambient (clientless unattended runs). |
| `autonomous_failover` | `[provider]` | `false` | Switches routes on its own in response to provider refusals when no client is attached. |
| `autonomous_failover_max_attempts` | `[provider]` | `3` | Cap on automatic switches per run chain. `0` turns switching off while leaving `autonomous_failover` on. |

All five keys default to off or deny. So a session with unchanged configuration, especially one used in front of a
screen, follows the same path as before this change. The three approval-wait keys live in `AmbientConfig` and the two
failover keys in `ProviderConfig` (`crates/config-types/src/lib.rs`).

- **`config.toml` has to be created by hand.** The binary does not create `~/.horon/config.toml` (or
  `$HORON_HOME/config.toml`) for you. The comment template in
  `crates/base/src/config/default_file.rs` documents the keys below, but no command writes that template to disk
  (`create_default_config_file` is called only in tests). So the template is reference material, not a file `serve`
  produces. There is no command that creates a config file either, so write the file by hand, putting the three
  approval keys under `[ambient]` and `retry_backoff_cap_secs` under `[provider]`. Leaving keys out applies the
  defaults: `approval_for_clientless_runs` is `false`, `approval_timeout_secs` is `1800`, and
  `retry_backoff_cap_secs` is `30`.

### Limits

- **Old prompts are not applied automatically.** A `ProviderFailoverPrompt` without `target_route` names no route,
  so `apply_autonomous_failover` refuses it. In that case a person has to pick the route.
- **The budget is not a global cap.** Its unit is the run chain, so a new prompt opens a new chain and counts from 0.
  It does not limit total switching for a session or a process.
- **The server does not switch on its own for sessions with a client attached.** The server reads
  `cross_provider_failover` only as the mode gate that decides whether switching is allowed
  (`crates/app-core/src/server/provider_control.rs`), and with a client attached it returns the original provider
  error to the turn unchanged. Showing a 3-second countdown and accepting a cancel is the client's job, and this
  headless build has no client that draws that screen (the TUI left in the headless transition). An attached client
  that wants a different model picks it with `set_model`, and there is no separate route-switch request in this
  build. The server switching by itself happens only in unattended runs.
- **`FailoverSwitched` stays until it is acked.** `failover-events.json` holds up to 100 entries and re-emits them on
  restart or reconnect. Re-emission can duplicate an entry under the same key, so consumers must ignore duplicates
  by `(session_id, chain_id, from, to, attempt)`. Past 100 entries the oldest are dropped, and that notification never
  goes out again. The clearing path is a client ack. When the client sends
  `Request::FailoverAck { chain_id, from, to, attempt }`, the server clears only the entry that matches that key
  exactly with `try_clear_acked_failover_event` and answers with `Ack`. If any key part differs it clears nothing,
  and if clearing fails because the lock cannot be taken it answers with `Error` instead of `Ack`. The harness API
  relays the same ack as a `failover_ack` frame. This build still has no client that sends the ack, so until a
  consumer sends one it behaves exactly as if no ack existed. An old daemon that does not know the ack tag rejects
  the request as an unknown type.
- **`ApprovalResolved` is also cleared only by ack.** The event is first written into the record as
  `pending_event` and then emitted, and it is re-emitted on restart. When the client sends
  `Request::ApprovalAck { request_id }`, the server clears only that record's `pending_event` and answers with `Ack`.
  If the record is already gone it treats that as success (the runner may delete it first and the ack arrive late).
  Even when a record is closed as `resumed` or `needs-human`, the field remains until the ack, so a consumer that
  does not ack receives that session's unconfirmed events again on every restart. Consumers must deduplicate approval
  events by `request_id` and failover events by `(session_id, chain_id, from, to, attempt)`. This build has no client
  that sends acks automatically, the same as for the failover ack. An old daemon that does not know the
  `approval_ack` tag answers the request with an `Invalid request` error.
- **Claim files remain after `Done`.** `~/.horon/approvals/resume-claims/<request_id>.json` and
  `~/.horon/approvals/resume-exec-claims/<request_id>.json` are records that bind a resume to exactly once and count
  attempts, so deleting them releases both idempotence and the `RESUME_ATTEMPT_CAP` judgment. The runner therefore
  only changes the state to `Done` and does not delete the files, and one pair accumulates per resumed approval. For
  a request that will not be looked at again an operator may delete them, but a deleted request starts counting
  attempts from 0 again.
- **429 honours only the parsed value.** `Retry-After` is the value the runtime parses into
  `StreamError::retry_after_secs`; the turn loop does not read the raw header. The wait is capped at
  `retry_backoff_cap_secs`, and if the header is longer than that and automatic failover is on, it switches without
  waiting. With no prompt to switch to it waits only up to the cap.
- **An untrustworthy budget blocks rather than resets.** A corrupt record, a missing primary file with a backup
  left behind, and a dangling symlink where a backup should be are all treated as UNTRUSTED. Switching stops and the
  last error records the reason. The attempt count is not reset to 0.
- **One known edge.** If the record is written exactly once and the primary file is then deleted, there is no
  backup and the state is indistinguishable from a first run. The code comments list this case as a limit too.
- **Unfinished tool calls are unknown.** A `ToolUse` block with no result block is listed in the resume prompt as
  "unknown whether a side effect happened" and is not re-executed automatically. Only read-only calls may be
  re-executed, and when that cannot be confirmed the turn ends with a `[resume-unverifiable]` line and waits for a
  human verdict.
- **Conflict row P16 is not implemented.** The plan's P16 assumes a state where only `ToolStarted` remains and
  `ToolFinished` was not recorded, but the session store event vocabulary on this branch has no kind with that name.
  That vocabulary belongs to the `session-event-compaction-sync` plan, so this plan's Must-NOT kept it out, and the
  `ToolResolved` judgment event P16 waits for is not in the tree either. The runner does not replay the event log's
  tool calls to continue; it continues by sending a message to the session with `resume_session_with_message`, so
  there is no path that could auto-re-execute the way P16 forbids. The same protection is computed from the
  transcript. `unresolved_tool_calls` collects calls without results, tells the resume prompt not to re-execute them,
  and raises `needs-human` when that cannot be confirmed. The per-call verdict mechanism (P18) does exist. When
  `BlockedApproval.tool_verdicts` is non-empty, `classify_blocked_approval` returns
  `Resume { origin: HumanVerdict }`, and a `needs-human` record with no verdict stays `Idle`
  (`crates/app-core/src/ambient/resume.rs`).
- **`wait = false` creates no wait.** It leaves the request in the queue and the record and returns `Queued` right
  away, so continuing is the runner's job.
- **The resume marker lives in the session event log.** The record that "this request already created a resume turn"
  is a `MetadataChanged` event, not a journal entry. The payload copies the session metadata the head was already
  projecting at that point and adds a single optional field, `resume_key` (`#[serde(default)]`). So no reader knows
  this event, and a reader that does not know it just re-reads metadata. The key does not go in `MessageAppended`
  because the marker is not a transcript item. Doing that would invent an invisible message that every reader
  replaying the log, provider transcripts included, would see. The implementation is
  `crates/base/src/session/resume_marker.rs`. Whether a resume happened is now decided by reading only the event log,
  with no second file a checkpoint could swap out.
- **The key append and the turn creation are not one ACK.** The marker becomes durable before the turn
  (`append_resume_key` at `crates/app-core/src/ambient/resume.rs:370`, then `resume_session_with_message` at
  `crates/app-core/src/ambient/resume.rs:427`). So the plan's requirement that "turn creation and key recording be
  made durable together in one ACK" is not met by this implementation. Instead it guarantees two things. First, the
  key is already on disk before any turn exists (`writer.append` → fsync). Second, the exec claim path recovers and
  runs a "key only, no turn" state exactly once (`crates/app-core/src/ambient/resume.rs:381-411`, pinned by
  `crash_after_the_key_is_durable_recovers_with_one_turn`). The remaining window is between the marker's ACK and the
  turn creation, and the exec claim covers that span rather than atomicity.
- **The marker event does not move the session's `updated_at`.** The resume decision uses
  `session.updated_at > recorded_at` to separate "a turn was created and actually ran" from "a turn exists but did not
  run". If the marker moved the timestamp the two cases would be indistinguishable. For the same reason appending the
  same key twice leaves the recorded time at the first value.
- **A busy store waits instead of failing the resume.** The old journal lock waited for a checkpoint or a concurrent
  append to finish. The event store takes a per-session exclusive lock, so `append_resume_key` retries every 25 ms for
  up to 30 seconds while the lock is held. Failing immediately would spend one of `RESUME_ATTEMPT_CAP`'s attempts on
  a condition that clears itself.
- **Sessions created before the event store have no marker.** Old builds wrote the key to the journal, and that
  journal is not read at runtime. A request whose resume had not finished at that point stays without a marker, so it
  can attempt one more resume. No path reads the legacy journal back as a marker source, in order not to revive the
  problem of a checkpoint swapping the journal.
- **Installing a moved session deletes the legacy journal lock unconditionally.** The install path of `cloud move`
  deletes the journal together with its lock sidecar. This build has no journal writer, so no process holds that
  lock, which is why deleting without checking is right in this build. During a mixed period when an old build that
  still writes the journal runs in the same home, however, that process may hold the lock. Deleting the file then
  makes a third process create a new lock file and believe it took the same lock. The window is narrow (while the old
  process runs during the move) and the outcome is just two owners believing they hold the lock, but it can happen
  unless only this build runs in the environment.
- **The budget guards' baseline is the 2026-11-05 tree.** The baseline files for `check_code_size.py`,
  `check_test_size.py`, `check_panics.py`, and `check_swallowed_errors.py` (`scripts/ci/*_budget.json`) were
  `--update`d to that day's tree when the repository moved to the public host. The large files this feature created
  (`agent/failover_budget.rs`, `ambient/persistence.rs`, `notifications.rs`, `server/state.rs`,
  `tool/ambient.rs`) went into the baseline then too. So all seven guards pass on `dev`, and later changes fail if
  they grow past this baseline. `--update` to raise the baseline is used only after intended cleanup.
- **Clippy run from the root does not check member crates' test targets.** The root `Cargo.toml` is both
  `[package]` and `[workspace]` and has no `default-members`. So running
  `cargo clippy --locked --all-targets --all-features -- -D warnings` from the root lints member crates' library
  code but not their test and example targets. The command that checks the whole workspace is
  `cargo clippy --locked --workspace --all-targets --all-features -- -D warnings`, and it passed with exit 0 on
  `dev` on 2026-11-05.
- **CI still runs the root form.** The `Run clippy with warnings denied` step in `.github/workflows/ci.yml` and
  `scripts/ci/check_all.sh` run without `--workspace`. Lint regressions in member crates' test code are not caught in
  CI, so if you changed that code, run the `--workspace` command above locally.

### Files to read first

If you are reviewing a change, the four files below are the whole decision path.

- `crates/app-core/src/agent/failover_budget.rs`: per-chain budget, trust judgment, blocking rules
- `crates/app-core/src/agent/turn_failover.rs`: refusal judgment, budget consumption, route change, resend decision
- `crates/app-core/src/ambient/resume.rs`: resume judgment, marker ACK, claims, `needs-human` promotion
- `crates/app-core/src/tool/ambient.rs`: request creation, waiting, timeout defaults, lease renewal