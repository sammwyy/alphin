# Compatibility

| Component | Status | Requirements |
| --- | --- | --- |
| Blair greeter adapter | Supported | `blair --profile greeter` (`/etc/blair/profiles/greeter.toml`), Wayland socket readiness |
| Coconut greeter adapter | Available | `coconut-greeter`, IPC version 1, CreamUI Wayland overlay |
| Wayland desktop sessions | Supported | Valid desktop entry, available `TryExec` when declared |
| X11 sessions | Unsupported | No adapter is installed |
| Cage | Unsupported | No tested adapter is installed |

Compatibility requires an adapter package and an end-to-end worker lifecycle
test. Opening a Wayland socket alone is not sufficient.
