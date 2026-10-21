# Greeter protocol

The greeter connects to `/run/alphin/alphind.sock` and sends one JSON object per
connection, followed by a newline. Requests and responses use `version: 1`.
Messages larger than 16 KiB are rejected.

```text
GetState                 → State
Status                   → Status or Prompt
ListUsers                → Users
ListSessions             → Sessions
BeginAuthentication      → Status
Respond                  → Status
Cancel                   → Status
StartSession             → Status
PowerAction              → Status after an authorized logind request, otherwise Error
```

`BeginAuthentication` starts a PAM conversation for an eligible account.
`Status` returns pending PAM prompts. `Respond` supplies exactly one prompt
response. The greeter must call `Cancel` when abandoning a conversation.

After successful authentication, `StartSession` accepts a discovered session
identifier and returns `session_queued`. The greeter then exits. Alphind waits
for the private worker completion signal, opens the PAM session, and launches
the selected session without a shell.

`Error.code` is stable machine-readable text. Clients must not display a raw
PAM or system error as an authentication prompt.

Rust greeters can use `alphin::client::Client` or
`alphin::client::socket_from_environment()` instead of implementing framing.
