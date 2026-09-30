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
`{kind, metric, value, unit, provenance}` entries: `kind` is `estimated` or
`measured`, `metric` is `api_cost`, `quota`, or `latency`, and an API cost uses a
three-letter currency unit. `api_for_quota` is separate again:
`{max_extra_cost = {value, currency}, provenance}`. Its presence is the
explicit allowance to trade additional API spend for quota headroom. Neither
planning estimates nor these decisions edit factual usage history.

`store:lookup({workload, baseline, candidate, policy_revision}, expected_revision, callback)`
returns a matching approved report with its report ID, policy revision, and
durable approval revision. The scope must exactly match both routes and the
workload and current policy revision. Read `store:revision(callback)` during preparation and pass that
revision at the final admission check; any intervening decision refuses the
stale lookup. An invalid or corrupt approval file fails closed. Report
production and automatic routing are separate tasks.
