# Routing comparison decisions

LouiseLM accepts a selected, payload-free JSON comparison report. Inspect the
report, then run `:LouiselmApproveComparison {report-file}` or
`:LouiselmRejectComparison {report-file}`. A rejected or absent report does not
qualify a route. These commands record a decision only; they do not start a
trial, submit a prompt, enable Auto, or change Provider disclosure permissions.

The report has this version 1 shape (the values below are synthetic):

```json
{
  "version": 1,
  "id": "selected-comparison-1",
  "policy_revision": "policy-1",
  "workload": { "kind": "main", "id": "implementation" },
  "baseline": {
    "agent": "alpha", "provider": "OpenAI", "model": "large",
    "model_option_id": "model",
    "options": { "model": "large", "effort": "high" }
  },
  "candidate": {
    "agent": "alpha", "provider": "OpenAI", "model": "small",
    "model_option_id": "model",
    "options": { "model": "small", "effort": "low" }
  },
  "fixtures": [{
    "id": "selected-task-1",
    "source": "operator-selected snapshot",
    "digest": "sha256:aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
    "checks": [{ "id": "expected-result", "baseline": "pass", "candidate": "pass" }],
    "human_review_required": true,
    "human": { "baseline": "pass", "candidate": "pass", "reviewer": "operator" }
  }]
}
```

`workload.kind` is `main` or `reader`; its `id` is an exact workload key. Each
route names the configured Agent, resolved Provider, advertised Model, Model
option ID, and the complete supported option tuple. Each explicitly selected
fixture needs a source description, SHA-256 digest, and at least one acceptance
check. Check results are `pass`, `fail`, or `pending`. Set
`human_review_required` for fixtures that need human judgment; a missing or
pending human result prevents approval. Both baseline and candidate results
must be complete, and every candidate result must pass. A completed turn or
Good feedback is not an acceptance check. Unknown fields are rejected.

Headless callers use `require("louiselm.routing.qualification").new(path)` and
`store:decide({ action = "approve", report = selected_report }, callback)`.
Optional `economics` is a separate array of
`{route, kind, metric, value, unit, provenance}` entries: `route` identifies
`baseline` or `candidate`, `kind` is `estimated` or
`measured`, `metric` is `api_cost`, `quota`, or `latency`, and an API cost uses a
three-letter currency unit. `api_for_quota` is separate again:
`{max_extra_cost = {value, currency}, provenance}`. Its presence is the
explicit allowance to trade additional API spend for quota headroom. The
selection check bounds the approved extra cost for the compared workload; it
does not meter cumulative spending or enforce a Provider billing limit. Neither
planning estimates nor these decisions edit factual usage history. Figures
without a route remain descriptive and cannot justify automatic selection.

For example, an operator may approve the report above with separate estimated
API costs:

```lua
store:decide({
  action = "approve",
  report = selected_report,
  economics = {
    { route = "baseline", kind = "estimated", metric = "api_cost",
      value = 1, unit = "USD", provenance = "operator estimate per fixture" },
    { route = "candidate", kind = "estimated", metric = "api_cost",
      value = 0.25, unit = "USD", provenance = "operator estimate per fixture" },
  },
}, callback)
```

## Rules-only Auto selection

Configure exact workload rules beneath an Agent's baseline:

```lua
auto = {
  model = "large",
  effort = "high",
  rules = {
    implementation = {
      model = "small",
      effort = "low",
      policy_revision = "policy-1",
      require_traits = { "coding" }, -- optional configured Agent constraints
    },
  },
}
```

An Auto submission resolves its rule from the headless caller's explicit
`workload`, then a selected Skill name with a matching rule, then an explicitly
declared canonical phase. A headless example is
`session:prompt(content, callback, { workload = "implementation" })`.
Chat forwards the metadata of exactly one staged Skill, not prompt keywords,
an old turn's phase, or a Skill bypassed by a direct slash command. Inferred
phases and multiple selected Skills are uncertain. Helper `parent_turn_id`
may share this metadata but never selects a rule by itself.

The rule requires an exact approved **main** workload comparison, current
advertised baseline and candidate Model/effort choices, all other supported
options, resolved Providers, and any declared Agent constraints. Capabilities
are eligibility constraints, not Model-quality evidence. Reader approvals and
legacy recommendations do not qualify a main turn. Omit `effort` only when the
Agent advertises no thought-level option.

Economics must contain one comparable baseline/candidate API-cost pair per
currency, with the same `kind`. Money, quota and latency stay separate; incomplete,
duplicate or incompatible figures select the baseline. Explicitly approved
estimates may qualify, but are never relabelled as measured usage. A candidate
must improve cost or comparable quota without worsening either unless explicitly
permitted to trade extra API cost for quota. Extra API cost
additionally needs quota improvement and a matching, sufficient
`api_for_quota.max_extra_cost` allowance. Tokens, output size, context occupancy,
unsupported Account limits and latency alone cannot establish savings. No
credential store or Provider API is consulted.

Missing metadata, rules, qualification or economic evidence keeps the configured
baseline. A manual pair bypasses rule selection. The existing admission
transaction confirms the requested Model and effort, persists the decision,
and commits the prepared turn before dispatch. Approval/configuration changes
during preparation reject stale admission without sending or retrying the
prompt. Decisions record the selected and baseline pairs, resolved rule/workload,
approval revisions, labelled economic basis and any fallback reason, never raw
prompt or Skill content.

The production qualification store is
`$XDG_STATE_HOME/louiselm/routing-evidence.json.qualifications.json` (with the
usual shared state-root fallback). Headless owners may supply an absolute
`qualification_path` in `Session.new`'s third argument. Approvals do not enable
Auto; rules are inactive in manual mode and absent `auto` retains ordinary
submission behavior.

Failed economical attempts set Session-local recovery intent. The next
authorized Auto submission uses confirmed baseline and records
`fallback_reason = "economical_failure"`; its admission decision names the
failed turn and reason. Recovery survives supported resume and stays pending
through unsent, cancelled or failed continuations until one completes. A later
manual pin wins. Chat may release a separately queued follow-up when the Session
can safely continue, but never replays the failed prompt, reruns tools, undoes
workspace changes, restarts an Agent or creates a Handoff automatically.
Unavailable baseline or a dead Session retains input for operator action.
Operational failures never rewrite qualification approvals; completed turns
remain observations rather than correctness evidence.

## Lookup and revision checks

`store:lookup({workload, baseline, candidate, policy_revision}, expected_revision, callback)`
returns a matching approved report with its report ID, policy revision, and
durable approval revision. The scope must exactly match both routes and the
workload and current policy revision. Read `store:revision(callback)` during preparation and pass that
revision at the final admission check; any intervening decision refuses the
stale lookup. An invalid or corrupt approval file fails closed. A matching
decision's revision may be older than the current global revision; admission
records both and rechecks the global revision immediately before dispatch.
