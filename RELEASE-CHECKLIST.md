# Release checklist

Items a person runs by hand before a release. Record each result, with the OS and client versions, in the release PR.

## 4b real clients (v0.3.0)

CI runs no authenticated client, so these run by hand with real Claude Code 2.1.325 and Copilot CLI on macOS, Linux
and Windows (rmux there). In a project folder run `up`; from the leader spawn a Claude member A and a Copilot member B.

Wherever a row records that a member quotes a nonce, also record receipt retirement, because quoting alone does not
prove the ack: the member's `read -Ack <batch>` answers `"acked": true`, and afterwards its mailbox cursor
(`mail/<mailbox>.cursor.json` under the state root) has no `pending` batch.

In an interactive turn the delivery hook may ack the batch itself before the person runs a manual ack; that manual
ack then correctly answers `"acked": false, "duplicate": true`. Either of these counts as receipt retirement:
the delivery hook's own ack answering `"acked": true`, or a `duplicate` reply together with the cursor check (no
`pending` batch).

1. Idle Claude self-wake: let A finish its turn and sit idle; send A a message from B. Record that A starts a turn on
   its own, with no keystroke, within 10 s, and that the turn text is
   `mux-agents mail is waiting (wake <nonce>): run mux-agent read` (8 hex digits).
2. Context acceptance (interactive turn): restart A with `MUX_AGENTS_WAKE=0` in its environment (test only; it
   disables self-wake). Send A a body holding a fresh nonce (`nonce-` and 8 random hex digits), then type a prompt
   into A. Record that A quotes the nonce in its reply.
3. Fallback via the background wait: with A still under `MUX_AGENTS_WAKE=0`, tell A to finish its task. Record that A
   runs `report done`, then starts a background `read -Wait 600` before its final answer, and that a message sent
   afterwards starts its next turn. Then restart A without `MUX_AGENTS_WAKE` for the rows below.
4. Copilot report, then on call: tell B to finish its task. Record that B runs `report done`, then
   `read -Wait 600 -Ack <batch>` as its last tool call, and that the bar shows `on call` for B.
5. The hook subprocess: in A, record from the plugin log that the hook `read -Count` answers ok, not `tagged`; in a
   person session of the same multiplexer, record that the hook delivers nothing (`member:false`).
6. A client killed mid-delivery: send A a message, kill its Claude Code process right after the turn starts (before
   the ack), resume A, and record that the mail is served again exactly once and then acked.
7. A refused wake: install a second plugin that refuses the wake (drops a `prompt.submit` from a plugin origin);
   record that A shows no extra turn, acks nothing, and reads the mail at its next typed turn.
8. Copilot on call interrupted: press Ctrl-C in B during its `read -Wait 600`; record that the bar leaves `on call`
   within 10 s and that the mail sent afterwards is read when B listens again.
9. The wake turn carries no mail (PM, fix cycle 2): with A idle, send a body holding a fresh nonce. Record that A,
   on a wake, runs read and quotes the nonce in its reply, then runs `read -Ack <batch>`, and that no further wake
   comes for that mail.
