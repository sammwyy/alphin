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

## Session catalogue

Wayland sessions are discovered from `/usr/share/wayland-sessions`. Alphin
accepts `Application` desktop entries, ignores hidden entries, tokenizes `Exec`
without a shell, and removes desktop-entry field codes before execution.

## Security model

The greeter compositor and UI run under the dedicated `alphin` account. Their
runtime directory is `/run/alphin` with mode `0700`; adapter files and
`/etc/alphin` are root-owned and not writable by the greeter or desktop users.
The worker clears inherited environment variables and supplies only its runtime
directory, a fixed PATH, and adapter-declared variables.

The daemon socket is private to the `alphin` account and authenticates the peer
with Unix credentials before accepting protocol messages. Its versioned JSON
protocol currently exposes state, status, user listing, and session listing; unavailable
authentication and power operations are rejected explicitly. Authentication
secrets must never be copied to logs, arguments, or persistent state.

## Development

```bash
cargo test
cargo clippy --all-targets -- -D warnings
```
