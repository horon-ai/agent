/// Split an optional `server:` prefix from a debug command.
///
/// The old `client` and `tester` namespaces belonged to the terminal UI and are
/// gone; naming one now returns an error instead of silently running it as a
/// server command. Every other namespace runs on the server, so commands like
/// `clients:map` or `queue_interrupt:<text>` pass through unchanged.
pub(super) fn parse_namespaced_command(command: &str) -> Result<&str, String> {
    let trimmed = command.trim();
    let Some(idx) = trimmed.find(':') else {
        return Ok(trimmed);
    };
    let namespace = &trimmed[..idx];
    let rest = &trimmed[idx + 1..];
    match namespace {
        "server" => Ok(rest),
        "client" | "tester" => Err(format!(
            "Unknown debug namespace '{namespace}'. The client and tester namespaces were removed with the terminal UI."
        )),
        _ => Ok(trimmed),
    }
}

pub(super) fn debug_help_text() -> String {
    r#"Debug socket commands (namespaced):

SERVER COMMANDS (server: prefix or no prefix):
  state                    - Get agent state
  history                  - Get conversation history
  tools                    - List available tools (names only)
  tools:full               - List tools with full definitions (input_schema)
  mcp:servers              - List configured + connected MCP servers
  last_response            - Get last assistant response
  message:<text>           - Send message to agent
  message_async:<text>     - Send message async (returns job id)
  swarm_message:<text>     - Plan and run subtasks via swarm workers, then integrate
  swarm_message_async:<text> - Async swarm message (returns job id)
  tool:<name> <json>       - Execute tool directly
  cancel                   - Cancel in-flight generation (urgent interrupt)
  clear                    - Clear conversation history
  agent:info               - Get comprehensive agent internal state
  agent:context            - Inspect assembled prompt, mode, current and locked provider tools (no inference)
  agent:context:prepare    - Prepare/freeze actual provider tool snapshot without inference (updates tool cache)
  agent:memory             - Get process + session memory breakdown
  allocator                - Get allocator info and jemalloc stats, if available
  allocator:purge          - Release retained heap (jemalloc arena purge / glibc malloc_trim)
  allocator:decay:<ms>     - Set jemalloc dirty/muzzy decay for all arenas to <ms>
  allocator:profile:on     - Enable jemalloc sampling at runtime (jemalloc-prof builds)
  allocator:profile:off    - Disable jemalloc sampling at runtime (jemalloc-prof builds)
  allocator:profile:prefix:<prefix> - Set jemalloc heap dump filename prefix
  allocator:profile:dump [path] - Write jemalloc heap profile to default or explicit path
  jobs                     - List async debug jobs
  job_status:<id>          - Get async job status/output
  job_wait:<id>            - Wait for async job to finish
  job_cancel:<id>          - Cancel a running job
  jobs:purge               - Remove completed/failed jobs
  jobs:session:<id>        - List jobs for a session
  background:tasks         - List background tasks
  server:memory-incident   - Fast cause classification + prescribed next actions
  server:memory            - Get global server memory breakdown
  server:memory-history    - Recent server process memory samples
  memory-judge             - No-LLM memory-mode conversion + degradation rates
  embeddings:stats         - Get embedding model/cache runtime stats
  embeddings:load          - Force-load the shared embedding model
  embeddings:unload        - Force-unload the shared embedding model and cache
  sessions                 - List all sessions (with full metadata)
  clients:map              - Map connected clients to sessions
  server:info              - Server identity, health, uptime
  swarm                    - List swarm members + status (alias: swarm:members)
  swarm:help               - Full swarm command reference
  create_session                - Create headless session
  create_session:<path>         - Create session with working dir
  destroy_session:<id>     - Destroy a session
  set_model:<model>        - Switch model (may change provider)
  set_provider:<name>      - Switch provider (claude/openai/openrouter/cursor/copilot/gemini/antigravity)
  trigger_extraction       - Force end-of-session memory extraction
  available_models         - List all available models
  reload                   - Trigger server reload with current binary

SWARM COMMANDS (swarm: prefix):
  swarm:members            - List all swarm members with details
  swarm:list               - List all swarms with member counts
  swarm:info:<swarm_id>    - Full info for a swarm
  swarm:coordinators       - List all coordinators
  swarm:roles              - List all members with roles
  swarm:plans              - List all swarm plans
  swarm:plan_version:<id>  - Show plan version for a swarm
  swarm:proposals          - List pending plan proposals
  swarm:context            - List all shared context
  swarm:touches            - List all file touches
  swarm:conflicts          - Files touched by multiple sessions
  swarm:channels           - List channel subscriptions
  swarm:broadcast:<msg>    - Broadcast to swarm members
  swarm:notify:<sid> <msg> - Send DM to specific session
  swarm:help               - Full swarm command reference

AMBIENT COMMANDS (ambient: prefix):
  ambient:status              - Ambient + schedule runner state, counts, next due items
  ambient:queue               - Scheduled queue contents with target/session metadata
  ambient:trigger             - Manually trigger an ambient cycle
  ambient:log                 - Recent transcript summaries
  ambient:permissions         - List pending permission requests
  ambient:approve:<id>        - Approve a permission request
  ambient:deny:<id> [reason]  - Deny a permission request (optional reason)
  ambient:start               - Start/restart ambient mode
  ambient:stop                - Stop ambient mode
  ambient:help                - Ambient command reference

EVENTS COMMANDS (events: prefix):
  events:recent            - Get recent events (default 50)
  events:recent:<N>        - Get recent N events
  events:since:<id>        - Get events since event ID
  events:count             - Event count and latest ID
  events:types             - List available event types
  events:subscribe         - Subscribe to all events (streaming)
  events:subscribe:<types> - Subscribe filtered (e.g. status_change,member_change)

Examples:
  {"type":"debug_command","id":1,"command":"state"}
  {"type":"debug_command","id":2,"command":"set_provider:openai","session_id":"..."}
  {"type":"debug_command","id":3,"command":"swarm:info:/home/user/project"}"#
        .to_string()
}

pub(super) fn swarm_debug_help_text() -> String {
    r#"Swarm debug commands (swarm: prefix):

MEMBERS & STRUCTURE:
  swarm                    - List all swarm members (alias for swarm:members)
  swarm:members            - List all swarm members with full details
  swarm:list               - List all swarm IDs with member counts and coordinators
  swarm:info:<swarm_id>    - Full info: members, coordinator, plan, context, conflicts

COORDINATORS & ROLES:
  swarm:coordinators            - List all coordinators (swarm_id -> session_id)
  swarm:coordinator:<id>        - Get coordinator for specific swarm
  swarm:clear_coordinator:<id>  - Admin: forcibly clear coordinator so any session can self-promote
  swarm:roles                   - List all members with their roles

PLANS (server-scoped plan items):
  swarm:plans              - List all swarm plans with item counts
  swarm:plan:<swarm_id>    - Get plan items for specific swarm
  swarm:plan_version:<id>  - Show current plan version for a swarm
  swarm:clear_plan:<id>    - Admin: delete a swarm's plan (memory + persisted state)

PLAN PROPOSALS (pending approval):
  swarm:proposals          - List all pending proposals across swarms
  swarm:proposals:<swarm>  - List proposals for a specific swarm (with items)
  swarm:proposals:<sess>   - Get detailed proposal from a session

SHARED CONTEXT (key-value store):
  swarm:context            - List all shared context entries
  swarm:context:<swarm_id> - List context for specific swarm
  swarm:context:<swarm_id>:<key> - Get specific context value

FILE TOUCHES (conflict detection):
  swarm:touches            - List all file touches (path, session, op, age, timestamp)
  swarm:touches:<path>     - Get touches for specific file
  swarm:touches:swarm:<id> - Get touches filtered by swarm members
  swarm:conflicts          - List files touched by multiple sessions

NOTIFICATIONS:
  swarm:broadcast:<msg>    - Broadcast message to all members of your swarm
  swarm:broadcast:<swarm_id> <msg> - Broadcast to specific swarm
  swarm:notify:<session_id> <msg> - Send direct message to specific session

EXECUTION STATE:
  swarm:session:<id>       - Detailed session state (interrupts, provider, usage)
  swarm:interrupts         - List pending interrupts across all sessions

CHANNELS:
  swarm:channels           - List channel subscriptions per swarm

OPERATIONS (debug-only, bypass tool:communicate):
  swarm:set_context:<sess> <key> <value> - Set shared context as session
  swarm:approve_plan:<coord> <proposer>  - Approve plan proposal (coordinator only)
  swarm:reject_plan:<coord> <proposer> [reason] - Reject plan proposal

UTILITIES:
  swarm:id:<path>          - Compute swarm_id for a path and show provenance

REAL-TIME EVENTS:
  events:recent            - Get recent 50 events
  events:recent:<N>        - Get recent N events
  events:since:<id>        - Get events since event ID (for polling)
  events:count             - Get event count and latest ID
  events:types             - List available event types
  events:subscribe         - Subscribe to all events (streaming, keeps connection open)
  events:subscribe:<types> - Subscribe filtered (e.g. events:subscribe:status_change,member_change)

Examples:
  {"type":"debug_command","id":1,"command":"swarm:list"}
  {"type":"debug_command","id":2,"command":"swarm:info:/home/user/myproject"}
  {"type":"debug_command","id":3,"command":"swarm:plan:/home/user/myproject"}
  {"type":"debug_command","id":4,"command":"swarm:broadcast:Build complete, ready for review"}
  {"type":"debug_command","id":5,"command":"swarm:notify:session_fox_123 Please review PR #42"}"#
        .to_string()
}
