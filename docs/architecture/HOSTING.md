# Hosting and the per-user boundary

Language: **English** | [한국어](HOSTING.ko.md)

A hosted deployment runs one agent container or Fly Machine per customer, each with one home volume, and never puts two customers in one daemon. This page states the flag contract, the image and volume layout, the routing contract the control plane owns, the suspend and delete procedures, and the limits that remain.

## The hosting flag contract

The environment is the only authority for the hosting mode. A tenant can write its own `config.toml`, so a file must never be able to widen the mode it runs under.

| Value | Where it comes from | Meaning |
| --- | --- | --- |
| Hosting mode | `HORON_AGENT_HOSTING_MODE` | `per-user-container` or `shared-worker`. An absent variable means no hosting. Any other value is a startup error that names the two allowed values |
| Tenant id | `HORON_AGENT_TENANT_ID` | A validated audit label: 1 to 63 bytes of lowercase ASCII letters, digits and hyphens, starting and ending with a letter or digit. It is a directory name and a log label, never a session lookup key |
| `[hosting] allow_shared_children` | `config.toml` | `false` refuses MCP servers and extension children even in a mode that would allow them. `true` never widens a mode that forbids them |
| `[hosting] allowed_mcp_servers` | `config.toml` | Names of MCP servers this process may connect. Absent leaves all of them reachable, an empty list reaches none, and the match is exact. Never widens a mode; narrowed further in [hosted MCP and extension isolation](../safety/SECURITY.md) |
| `[hosting] mode` | `config.toml` | Ignored with one warning. Set `HORON_AGENT_HOSTING_MODE` instead |

The resolved policy is a single process-wide value. `HostingPolicy::mode()`, `tenant_id()`, `is_hosted()`, `is_per_user_container()`, `is_shared_worker()`, `allows_shared_children()`, `allowed_mcp_servers()`, `permits_mcp_server()` and `inherits_operator_logins()` are the supported reads; the MCP and extension gate and the non-local exposure check consume them rather than re-reading the environment.

What each mode does with an MCP server or a package child, the `HOSTED_POLICY` audit event, and the limits of the gate are in [hosted MCP and extension isolation](../safety/SECURITY.md).

## The startup boundary

A per-user container refuses to start unless all of the following hold. The daemon checks them before it binds a socket and exits with a message naming the failed condition.

| Condition | Why |
| --- | --- |
| `HORON_HOME` is set, absolute, exists, is a directory, is owner-only (`0700`) and is not a symlink | The customer's state must live on a directory only that container can enter |
| `HORON_HOME` is not `$HOME/.horon` | A hosted customer must never be served from the operator's own home |
| `HORON_AGENT_RUNTIME_DIR` is set, absolute, owner-only and not inside the home | Sockets stay off the volume, and the runtime directory is separate from durable state |
| `HORON_AGENT_TENANT_ID` is set and valid | Every hosted process carries its audit label |
| `[gateway] enabled` is false | The TCP gateway binds an address outside the container's socket boundary |

A shared worker starts without those checks so its own gate can refuse the operations it must not run: it never allows child processes or remote MCP connections for a customer, whatever `config.toml` says.

## Image, volume and runtime layout

`scripts/hosting/Containerfile` builds the hosted image from the same published release archives as the release image. It keeps UID `10001`, declares `/data` as the volume, sets `HORON_HOME=/data/home` and `HORON_AGENT_RUNTIME_DIR=/run/horon`, and starts `scripts/hosting/entrypoint.sh`, which validates the hosting variables and execs `horon-agent --no-update serve`.

The same image runs unchanged on a Fly Machine, where a Fly Volume mounted at `/data` is that customer's home. The image is not tied to one host: what the boundary needs is the volume, the environment and the absence of a TCP gateway, not a particular runtime.

| Path | Contents |
| --- | --- |
| `/data` | The tenant volume. One volume per customer |
| `/data/home` | `HORON_HOME`: sessions, transcripts, approvals, MCP config, preferences, memory and user files |
| `/run/horon` | Socket directory, separate from the volume and recreated on every start |

The image never exposes a port. The API socket and the daemon socket are files inside the container; a client outside the container reaches them only through the control plane.

## Routing contract

The control plane owns the mapping from a customer to an instance and its volume. The agent binary does not read tenant ids from requests and does not route between customers.

1. The control plane authenticates the customer and resolves `user -> instance -> volume`.
2. It starts, wakes or reuses that customer's instance. `scripts/hosting/instance.sh create` prepares the volume, `start` runs the daemon in the foreground under a supervisor, or the container runtime starts the image with the volume mounted at `/data`.
3. It forwards the customer's request to that instance's socket. The socket never leaves the container, and no agent endpoint is exposed on a network address.
4. Requests from two customers never reach one process. There is no per-session tenant check, because there is no shared process to check against.

Exposing the harness API beyond the container requires authentication, which is a separate change and is not part of this contract. Hosted images keep the gateway off and publish no port.

## Suspend, wake and restore

Idling an instance is stopping its container or sending the daemon `SIGTERM`. The daemon drains in-flight session saves before it exits (`docs/architecture/SERVER.md`), so a stop is a durable stop and the next start opens the same volume.

- Stop with `scripts/hosting/instance.sh stop <tenant>`, `docker stop`, or the supervisor's termination signal. `stop_hosted_instance` in `horon-agent-sdk` is the same operation: it signals, waits for the endpoint to stop accepting, and reports the tenant as stopped only after that.
- Wake by starting the image or the daemon again with the same volume. Sessions, approvals, MCP config and memory files are read from the volume; a session resumes through the ordinary resume path.
- Never copy a volume between tenants, and never mount one volume into two instances.

## Deleting a customer

Deletion removes one tenant and nothing else: stop the instance, then remove that tenant's directory on the volume. `delete_hosted_instance` and `scripts/hosting/instance.sh delete <tenant>` do exactly that, and both refuse a tenant directory that is a symbolic link or that does not resolve inside the volume root.

| Step | Command | Result |
| --- | --- | --- |
| Stop | `scripts/hosting/instance.sh stop <tenant>` | The daemon drains and exits; the volume is unchanged |
| Remove container | `docker rm horon-tenant-<tenant>` | The container no longer exists |
| Remove volume contents | `scripts/hosting/instance.sh delete <tenant>` | Only `<root>/<tenant>` and its derived runtime directory are removed |
| Verify | `scripts/hosting/instance.sh status <tenant>` | The tenant reports absent, and every other tenant still runs |

Deleting one instance must leave other tenants untouched. Their sessions, volumes and running daemons are separate, and the integration test starts two daemons and asserts exactly that.

## What is out of scope

- Billing, plans and quotas.
- Autoscaling and placement policy.
- A central metadata service; the control plane owns the mapping and this repository does not implement it.
- Authentication for non-local exposure.
- Tenant ids inside application code paths, sessions or the wire protocol.
- A general-purpose OS sandbox beyond the container boundary.

## Compatibility

With `HORON_AGENT_HOSTING_MODE` absent, nothing changes: the daemon serves the operator's own home, the gateway default stays off, and every existing command behaves as before. Hosted mode adds a startup check and a launcher; it does not alter sessions, the wire protocol or configuration semantics outside the `[hosting]` section.
