# Alphin

Alphin is the display manager for the distribution. It owns greeter lifecycle,
session selection, authentication, and the handoff to a user session.

The current MVP foundation provides package-managed adapters, Wayland session
cataloguing, and a greeter worker. The worker starts a configured compositor,
waits for its Wayland socket, starts the configured greeter without invoking a
shell, and terminates the compositor process group after the greeter exits.

## Adapter installation

Adapters belong to root-managed package paths:

```text
/usr/lib/alphin/compositors/*.toml
/usr/lib/alphin/greeters/*.toml
```

An adapter has an absolute executable path, literal arguments, a restricted
environment, Wayland readiness timeout, and shutdown timeout. It does not
accept commands from clients or per-user configuration. The included Blair
adapter uses `blair --profile greeter`; that profile reads only
`/etc/blair/greeter.toml`, never user Blair configuration.

`/etc/alphin/config.toml` selects the socket, runtime directory, session
catalogue, worker executable, and installed adapters. Every configured path
must be absolute.

## Session catalogue

Wayland sessions are discovered from `/usr/share/wayland-sessions`. Alphin
accepts `Application` desktop entries, ignores hidden entries, tokenizes `Exec`
without a shell, and removes desktop-entry field codes before execution.

## Security model

See [the security model](docs/security.md) and [adapter format](docs/adapters.md).
The greeter IPC contract is in [the protocol reference](docs/protocol.md).

The greeter compositor and UI run under the dedicated `alphin` account. Their
runtime directory is `/run/alphin` with mode `0700`; adapter files and
`/etc/alphin` are root-owned and not writable by the greeter or desktop users.
`alphind` runs with the privileges required by PAM and session setup. Its socket
is group-readable and writable only by the dedicated `alphin` group, and it
verifies Unix peer credentials before processing a request. The worker clears
inherited environment variables and supplies only its runtime directory, a
fixed PATH, and adapter-declared variables.

The system package creates the `alphin` account through `sysusers` and starts
the worker as that account. Alphind owns the worker process and receives its
private completion signal after the greeter compositor has stopped.

The daemon socket is private to the `alphin` account and authenticates the peer
with Unix credentials before accepting protocol messages. Its versioned JSON
protocol currently exposes state, status, user listing, session listing, and
PAM authentication. Session start is queued until the daemon confirms that the
worker has stopped the greeter compositor; power operations remain unavailable
until logind policy is available. Authentication secrets must never be copied
to logs, arguments, or persistent state.

`BeginAuthentication`, `Respond`, and `Cancel` run a Linux-PAM conversation.
The distribution package must install `/etc/pam.d/alphin` using its established
system authentication stack. The bundled Fedora/authselect profile delegates to
`system-auth` and invokes `pam_systemd`. Authentication succeeds only after PAM
authentication and account management both succeed.

## Development

```bash
cargo test
cargo clippy --all-targets -- -D warnings
```
