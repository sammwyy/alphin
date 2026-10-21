# Adapters

Adapters are TOML files installed by packages under
`/usr/lib/alphin/compositors` or `/usr/lib/alphin/greeters`. Administrators
select installed adapter files; untrusted clients cannot submit adapters.

```toml
id = "blair"
executable = "/usr/bin/blair"
arguments = ["--profile", "greeter"]
shutdown_timeout_seconds = 10

[environment]
EXAMPLE = "value"

[readiness]
kind = "wayland_socket"
timeout_seconds = 20
```

Greeter adapters also declare `ipc_version = 1` and any supported capabilities:
`users`, `sessions`, `authentication`, `power`, `languages`, and
`accessibility`.

`id` contains lowercase ASCII letters, digits, and hyphens. `executable` is an
absolute path. Arguments and environment values are literal values passed to
`exec`; no shell expansion is performed. Readiness and shutdown timeouts are
bounded to 1 through 120 seconds.
