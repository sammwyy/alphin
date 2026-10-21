# Code Style

## Comments

- Write comments in English.
- Do not add task, issue, PR, or change-history comments.
- Use comments only for non-obvious technical constraints or invariants.
- Do not leave TODO or FIXME comments in source; track deferred work in `TODO.md`.

## Quality

- Keep process execution argument-based; never use a shell for adapters or sessions.
- Validate untrusted data at the boundary and keep privileged paths root-managed.
- Run `cargo fmt`, `cargo test`, and `cargo clippy --all-targets -- -D warnings` before committing.
