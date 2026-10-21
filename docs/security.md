# Security model

Alphind is the authority for PAM, session creation, and power policy. It runs
with the privileges required for those operations. The greeter worker, Blair,
and Coconut run as the dedicated `alphin` account.

`/run/alphin` is created by tmpfiles with mode `0700` and ownership
`alphin:alphin`. Alphind creates `alphind.sock` as `root:alphin` with mode
`0660`. It verifies Unix peer credentials and accepts only root or the socket
group. Adapter and configuration files are root-managed.

The protocol has a fixed version and a bounded request size. It accepts only
declared message types. Clients cannot provide executable paths, shell input,
adapter values, or environment overrides. Session desktop entries are parsed
into an argument vector and are never executed through a shell.

Authentication responses are held in zeroizing buffers. They are not logged,
placed in process arguments, or written to persistent state. PAM prompts may
be delivered only over the private socket.

The daemon accepts authentication only for eligible local accounts: UID 1000
through 60000, with a login shell that is not `nologin` or `false`.

## Authorization boundaries

The greeter may request authentication, session selection, and a power action,
but it never authorizes any of them. Alphind accepts authentication only for
eligible local accounts, starts only package-discovered sessions for the
authenticated account, and permits a power request only after authentication.

Seat ownership, VT switching, suspend, hibernate, reboot, and power-off are
owned by systemd-logind. Alphind must call its logind APIs for those operations
and rely on logind and polkit for authorization; it must not invoke system
commands or treat greeter IPC membership as authorization. Until that backend
is enabled, `PowerAction` is rejected.

The greeter worker must stop its compositor before a desktop session starts.
Alphind owns the worker and accepts its completion only from a private control
file descriptor that is closed before the UI executes. A selected session is
held until that signal arrives, then runs under its PAM transaction.
