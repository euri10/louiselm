# Explicit selected-content reader

`api:read_selected_content(job, callback)` answers one explicit question in a
distinct headless Session using complete caller-selected line snapshots. It
never opens their paths or discovers additional files. No main Auto setting is
required. The maintained `acp-llm-adapter` must be rebuilt with the new contract;
unsupported runtimes fail before content is sent. Plan mode permits independent
reads and is insufficient. Ordinary Agent approval policies remain unchanged.

```lua
local reader, err = api:read_selected_content({
  scope = {
    workload = { kind = "reader", id = "status-summary" },
    baseline = baseline_route,
    candidate = reader_route,
    policy_revision = "reader-policy-1",
  },
  parent_turn_id = parent_admission_id,
  parent_allowance_ms = 30000,
  question = "What status does this selected source report?",
  sources = {
    { id = "status", path = "status.txt", first_line = 10,
      lines = { "Idle" }, provenance = "operator-selected snapshot" },
  },
}, function(result, failure)
  if failure then return end -- stable code and payload-free message
  -- result.answer, references and missing_context are bounded.
end)
-- reader:dispose() cancels; api:dispose() also disposes owned jobs.
```

Routes are complete `ComparisonRoute` records: configured Agent, resolved
Provider, advertised Model and Model option ID, plus the complete supported
option tuple. The exact pair, reader workload and policy revision require a
current durable qualification approval. The candidate is the worker. Every
option is confirmed through ACP before dispatch; the complete tuple and
resolved Provider must match. Approval is revalidated after configuration.
This production API infers no experimental trial authority from a caller flag,
feedback or an unapproved comparison. Separate trial authorization and
qualification must precede use of an unqualified route.

Optional closed `limits` defaults to:

```lua
{ version = 1, input_bytes = 131072, output_bytes = 65536,
  max_tokens = 4096, timeout_ms = 30000 }
```

Positive integer limits can be narrowed within those byte/time ceilings;
output tokens have a maximum of 8192. The input limit includes the entire
encoded instruction, question and snapshots. Oversized input is refused rather
than truncated. Questions have a 4096-byte ceiling; at most 16 sources have
unique IDs, display paths, positive first-line numbers, dense individual-line
arrays and explicit provenance. The caller owns its remaining parent allowance;
the whole job, including qualification/startup, cannot outlive its local timeout
or that allowance. This bounds local latency, not Provider-side billing, and is
not a hard real-time scheduling guarantee.

The adapter advertises version 1 under
`agentCapabilities._meta["io.github.euri10.louiselm.selectedContent"]`.
LouiseLM sends exact limits in `session/new._meta` under the same key and
requires an exact acknowledgement before sending content. The immutable
Session contract permits one prompt attempt and one Model request, with no
advertised or executable tools, MCP servers, or additional directories.
Mode/option changes cannot grant tools or remove the output-token ceiling.
A tool delta, combined answer/thought overflow, or streaming deadline ends the
attempt without a follow-up request. This is confinement in the maintained
tool harness, not kernel containment of an arbitrary executable.

The worker returns closed JSON with `answer`, `references` and `missing_context`.
Each reference names a selected `source_id` and inclusive first/last line.
LouiseLM validates membership and attaches the selected path, provenance and
SHA-256 of the complete selected lines joined with newline bytes. At most 64
references and 16 bounded missing-context descriptions are accepted. Valid
references establish provenance, not semantic truth; missing evidence remains
missing. Non-JSON, extra fields and forged references are refused. Thoughts
count against output limits but are not answers.

Results also include worker/parent admission IDs, qualification report/revision,
confirmed Agent/Provider/Model/options and observed usage. Ordinary recording
preserves helper correlation even if the parent never dispatches. Default
LouiseLM recording contains no question, snapshot, answer, thought or tool
content. Adapter helpers persist no raw history or source-derived title;
explicit protocol logging follows the existing logging policy. Missing usage
remains unknown, and cancellation is not evidence of zero billing.

Caller/API disposal releases the owned timer and worker, settling once. Late
callbacks cannot revive it; cleanup failure prevents success and remains
visible on repeated reader disposal. Main Model, pin, history and permissions
are untouched. There is no automatic interception, UI or retry.

`tests/routing/reader_spec.lua` exercises the actual headless consumer with
asynchronous ACP doubles; the adapter's `selected_content` tests exercise actual
turn and registry boundaries with a fake Provider. Full suites, static analysis
and generated docs checks remain required. These offline gates neither certify
paid Model quality nor authorize a live paid call or Provider disclosure.
