# Hosting one Horon per customer

Hosting is opt-in through the environment; a tenant's own `config.toml` can only narrow it.

## Modes

| `HORON_AGENT_HOSTING_MODE` | Meaning |
| --- | --- |
| unset | No hosting: the operator's own installation |
| `per-user-container` | One customer per container with its own `/data` home volume; its own MCP servers and package children are allowed |
| `shared-worker` | Shared process; every MCP server, editor-supplied server and package child is refused with one `HOSTED_POLICY` audit record |
| anything else | Startup error naming the two values |

`HORON_AGENT_TENANT_ID` is required in both modes: 1–63 lowercase letters, digits and hyphens, starting and ending with a letter or digit. It is an audit label and directory name, never a session lookup key.

## A per-user container refuses to start unless

| Condition | Reason |
| --- | --- |
| `HORON_HOME` is absolute, an owner-only (`0700`) directory, not a symlink, and not `$HOME/.horon` | The customer's state lives only on its volume |
| `HORON_AGENT_RUNTIME_DIR` is absolute, owner-only, and outside the home | Sockets stay off the volume |
| `HORON_AGENT_TENANT_ID` is valid | Every hosted process carries its label |
| `[gateway] enabled` is false | No TCP listener outside the socket boundary |

## Narrowing in `config.toml`

```toml
[hosting]
allow_shared_children = false        # refuse MCP servers and package children even where allowed
allowed_mcp_servers = ["files"]      # exact names; [] reaches none
```

`[hosting] mode` is ignored with a warning.

## Customer data

| Need | Command |
| --- | --- |
| Back up or restore a user's volume | `horon volume export` / `import` |
| Freeze for a snapshot | `horon volume quiesce`, then `resume` |
| Delete a user for good | `horon volume delete-user <user_id>` (resumable: fences writes, revokes the data key, wipes the store and snapshots); `deletion-status` reports progress |

The hosted image is built from `scripts/hosting/Containerfile`; the control plane, routing and suspend/wake design are in the developer reference at https://agent.horon.ai/reference/architecture/hosting.
