# Compatibility

| Component | Status | Requirements |
| --- | --- | --- |
| Blair greeter adapter | Supported | `blair --profile greeter`, system-only config, Wayland socket readiness |
| Coconut greeter adapter | Contract defined | IPC version 1, layer-shell client package pending |
| Wayland desktop sessions | Supported | Valid desktop entry, available `TryExec` when declared |
| X11 sessions | Unsupported | No adapter is installed |
| Cage | Unsupported | No tested adapter is installed |

Compatibility requires an adapter package and an end-to-end worker lifecycle
test. Opening a Wayland socket alone is not sufficient.
