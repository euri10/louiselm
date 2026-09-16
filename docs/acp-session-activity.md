# Session activity extension (version 1)

An ACP `session/prompt` response completes that request. It does not prove the
Agent is idle: a scheduled wakeup can start work without another client prompt.

LouiseLM advertises this opt-in capability in `initialize`:

```json
{"clientCapabilities":{"_meta":{"io.github.euri10.louiselm.sessionActivity":{"version":1}}}}
```

Supporting Agents advertise the same entry in `agentCapabilities._meta` and
forward authoritative, live root-Session activity as ordered `session/update`
notifications, including activity outside a client prompt:

```json
{
  "sessionId": "the ACP Session id",
  "update": {
    "sessionUpdate": "session_info_update",
    "_meta": {
      "io.github.euri10.louiselm.sessionActivity": {
        "version": 1,
        "state": "running"
      }
    }
  }
}
```

`state` is `running`, `idle`, or `requires_action`. The last value means the Agent
is still active; LouiseLM shows a permission prompt only when it receives an
actual permission request. Other optional fields are ignored. Unsupported
versions are ignored; malformed version-1 state is a protocol error.

The producer must use native lifecycle signals, not silence, tool completion,
usage updates, or text matching. States describe the root Session, never a
subagent, and must not replay historical activity. LouiseLM ignores activity
while loading history and after error or disposal. A loaded active Session must
publish its current state after loading completes.

LouiseLM's `running` status represents Agent activity beyond the pending client
prompt. It keeps the winbar active, queues input until ready, permits a cancel
request, and protects active work on editor exit. Native idle cannot complete a
pending prompt or dismiss an outstanding permission. Prompt completion callbacks
and recording still run once per client request; autonomous activity does not
fabricate an additional prompt or usage record. Cancellation remains a request
until the Agent reports idle; dispatch alone does not prove work stopped.

The managed Claude adapter maps SDK `session_state_changed` events to this
extension for compatible subscribers. Peers without the extension retain their
existing prompt lifecycle. See `louiselm-gmcod` for captured evidence and live
acceptance; the deferred Copilot integration is separate.
