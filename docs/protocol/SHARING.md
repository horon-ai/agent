# Encrypted session sharing

Language: **English** | [한국어](SHARING.ko.md)

Status: proposed design for #544. Existing Markdown export writes a local file or stdout, and the harness API exposes the same transcript. The proposed share flow starts from the session data that feeds that export. [Harness API](HARNESS_API.md)

## Decision

| Option | Consequence |
| --- | --- |
| Local file or clipboard | Works offline but provides no expiration or revocation. |
| Hosted plaintext snapshot | Supports access control but exposes selected content to the storage operator. |
| Client-encrypted immutable snapshot | Keeps content off the transport in plaintext and gives the service a single place to enforce access lifetime. Selected. |

## Projection and publication

Sharing starts disabled. A human owner selects message entries from a single committed history head. The initial selection is empty. V1 admits user and assistant text only. Exclude reasoning, tool requests and results, attachments, system text, absolute paths and internal metadata. Apply existing secret redaction to the allowed fields, then let the owner remove any remaining sensitive text. A redaction match excludes the original bytes. The preview must show the entire resulting payload and the exact expiration. Bound the serialized UTF-8 projection to 1 MiB and reject oversize or empty content without uploading. User confirmation binds to the frozen bytes, version and expiration. A change to any of these invalidates confirmation. Later turns never update the snapshot.

## Transport and lifecycle

The publishing client creates a fresh authenticated-encryption key and nonce per share and binds the format version, opaque share ID and expiration as authenticated data. The writer sends ciphertext and a hash of a separate random read token to a dedicated HTTPS share service. The service stores ciphertext and lifecycle metadata. The browser viewer obtains the token and key from the link fragment, sends the token only in an authorization header to the dedicated service and decrypts locally. The renderer treats transcript text as inert and uses no third-party scripts, remote media or analytics. Disable caching and referrer forwarding. Redact tokens and URLs from logs and receipts. Anyone who receives the full link can read until expiry or revocation. Review the viewer deployment as a trusted code origin before enabling delivery.

The service checks read authority and expiration on every fetch and returns the same unavailable response on missing, expired, revoked and unauthorized links. Default lifetime is 24 hours with a maximum of seven days. Revoke through the authenticated owner identity, independent of the read token. Acknowledgment follows a durable state update. A read authorized before that update may complete, and recipients can retain previously downloaded data. Disable further reads first and then remove ciphertext according to the service retention policy. An owner-scoped idempotency key allows a retry after an ambiguous publish outcome without creating another share. Cancelled previews create no remote state.

## Implementation boundaries

1. In `crates/session-store/src/`, produce an immutable, bounded projection with an explicit allowlist. Verify omission before serialization and share no raw events or blob references.
2. In `src/cli/sessions.rs` and `crates/harness-api/src/requests.rs`, add an explicit owner preview and publish flow. Keep existing export callers on their current contract. TUI, if added, consumes the typed API as a client.
3. Implement the transport and viewer outside the agent. Gate activation on review of hosting, retention, incident response, cryptographic dependencies and licenses. Keep live collaboration outside this snapshot protocol.

## Verification

Use the issue's regression command `cargo test --locked -p horon-agent --lib cli::`. Add an Actions fixture with an in-memory transport and injected clock before enabling any service. No network or real session data is needed for the fixture.

| Fixture action | Expected observation |
| --- | --- |
| Select `hello` and a synthetic credential, omit `private note` and a tool result, then publish | The authorized viewer gets `hello` and the redacted text only. Stored ciphertext has no plaintext fields and the preview bytes equal the decrypted bytes. |
| Fetch with no token, a wrong token, or a valid token and wrong decryption key | The first two fetches return unavailable. The third produces no rendered transcript. |
| Publish two shares expiring at clock tick 100, revoke the first at tick 99 and read both at ticks 99 and 100 | The first is unavailable at both ticks. The second is available at tick 99 and unavailable at tick 100. |
| Retry a publish after dropping its successful response | Exactly one share exists for that owner and idempotency key. |

Also check that cancel causes zero uploads and that a tampered envelope fails authentication. A terminal implementation must record a PTY receipt for preview, confirmation and cancellation with synthetic secrets only.
