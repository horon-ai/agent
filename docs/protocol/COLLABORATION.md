# Live session invitations

Language: **English** | [한국어](COLLABORATION.ko.md)

Status: proposed design for #545. The harness API currently supports attaching to sessions and provides structured answers and approval requests. An invitation delegates a limited role on one live session. It conveys no general daemon access. [Harness API](HARNESS_API.md)

## Decision

| Option | Consequence |
| --- | --- |
| Reuse device pairing credentials | Extends a device's authority beyond one session and makes removal affect unrelated work. |
| Independent owner-scoped invitations | Allows revocation of one participant and preserves the original owner channel. Selected. |
| Read-only snapshot | Suits asynchronous sharing but cannot show live presence or accept a writer's input. |

## Authority and visibility

The owner opens collaboration for one session and creates an invitation with a chosen role and expiration. The request returns a single-use random redemption secret. Initial defaults are 15 minutes to redeem and one hour of total validity. Both deadlines are chosen at creation and may only be shortened. Bind redeemed access to the authenticated transport identity, session ID and invitation generation. Redemption has one atomic winner. Access is unavailable when no authenticated collaboration transport is configured. Connect through the existing authenticated connection boundary without forwarding a device-pairing credential. The transport authenticates the peer and preserves the delegated identity through to authorization. A viewer or writer can never act as a direct daemon owner.

| Operation | Viewer | Writer | Owner |
| --- | --- | --- | --- |
| Read the collaboration transcript and roster | Yes | Yes | Yes |
| Submit a normal user message to the invited session | No | Yes | Yes |
| Answer a structured question or settle approval, acknowledge a receipt, steer or abort a turn | No | No | Yes |
| Invite or revoke another participant, change configuration, access files or export history | No | No | Existing owner permissions |
| Leave the shared session | Yes | Yes | Owner can end collaboration |

The shared transcript begins at redemption and includes only visible user and assistant text and turn status. Omit earlier history, reasoning, tool payloads, approvals and other private events. The roster shows each participant's display name, role and presence. Attribute accepted writer inputs to their participant identity. Treat those inputs as ordinary user content, with agent tool effects still subject to owner approvals. Actor identity is separate metadata supplied by the server. Escape names and content in clients. Keep names and secrets out of command arguments and logs. Allow at most eight guests per session. Existing owner sessions keep their normal behavior.

## Departure and durable state

An authenticated grant can reconnect until it expires or is revoked. The owner sees online and offline presence. Explicit leave invalidates the grant, and ending collaboration invalidates every grant on that session. Store grant lifecycle independently from the durable conversation. Apply expiry and revocation at the point where a new input would enter the daemon's queue. Serialize that decision with admissions for the same grant. Quarantine that grant's already queued but unstarted inputs when revocation takes effect. A running turn and previously committed messages remain intact, and the owner can decide whether to abort a running turn. Stop subsequent replay and live delivery to the guest, then detach its stream. A disconnect alone preserves the grant until expiration. Restart must fail closed for any grant state it cannot restore. Previously seen text remains in the recipient's hands.

## Implementation boundaries

1. Define opt-in invitation, redemption, leave, revoke and roster messages in `crates/harness-api/src/requests.rs` and companion event types. Make role and session scope visible in the typed connection contract. Existing peers receive no collaboration frames without negotiation.
2. In `crates/harness-api-server/src/`, enforce an explicit allowlist for guest requests and a separate guest event projection. Ensure the daemon owns admission and revocation ordering wherever an input can outlive the bridge. Test cross-session rejection and owner-only approvals before enabling remote transport. Review identity binding, expiry and authorization at both boundaries.
3. Wire `crates/tui/src/app.rs` through the SDK to show roles and participants and to detach on leave. Role-based controls only reflect server authority. Keep guest policy and networking out of the terminal presentation code.

## Verification

Run `cargo test --locked -p horon-agent --lib cli::` for the existing CLI contract. Add a GitHub Actions fixture using an isolated daemon, owner, viewer and writer, with a controlled clock and no live provider.

| Fixture action | Expected observation |
| --- | --- |
| A viewer tries message, answer and approval requests over the wire | All return a refusal. The session gets no input or approval decision. |
| A writer sends one input and then tries to approve a pending tool call | The input is attributed once to the writer. Only the owner can settle the approval. |
| The owner revokes a writer before its next admission | The revoked stream stops and new writer requests fail. The owner stays attached and the previous committed message remains readable after restart. |
| A grant races redemption twice or arrives at its deadline | One redemption succeeds before the deadline, and the same grant fails at or after expiration. |

Record admission and event delivery at the revoke boundary. Test another session ID, missing transport identity, reconnect and explicit leave. The terminal review must include a PTY receipt showing the roster, viewer restrictions and writer removal with synthetic identities.
