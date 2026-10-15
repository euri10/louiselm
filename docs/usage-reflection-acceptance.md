# Usage reflection acceptance — louiselm-a1a9

Implemented and exercised on 2026-10-15 by
`codex/01a13e67-0aaa-75a1-9a1a-c9559ae2b826`. Human acceptance was confirmed
on 2026-10-15 after the repository-scoped demonstration below.

The owned source is
`/home/lotso/dotfiles/agentskills/.config/agentskills/louiselm-usage-reflect/`;
its catalog entry builds into the maintainer's configured `generated/` supply.
The Codex, Claude and Agent Skills directories resolve to that supply, and
LouiseLM discovery found the new skill exactly once. This proves discovery in
the current native supply, not trusted Skill Admission or a Verified posture.

The skill is invoked on demand for reflection; it is not an end-of-turn hook.
It calls the existing `store:usage_query` API through a standalone JSON bridge.
Collection, SQL calculations, Agent settings and authorization are unchanged.

## Reproduce the synthetic invocation

From the source or installed skill directory:

```sh
fixture_root=$(mktemp -d /tmp/louiselm-reflect.XXXXXX)
PYTHONDONTWRITEBYTECODE=1 python3 scripts/test_query.py \
  /home/lotso/code/louiselm --seed "$fixture_root/usage"
```

Invoke `louiselm-usage-reflect` with: “Review option changes in this synthetic
usage directory for 2026-09-07 UTC. Does raising effort help? Cite the records.”
Pass the printed/known directory explicitly. The seed is synthetic normalized
metadata, not captured ACP traffic or an import from the maintainer's history.

Actual invocation used the installed SKILL.md, query reference and bridge with
`/tmp/louiselm-a1a9-demo.iQIEaV/usage`, read at `2026-10-15T07:24:54Z`.
The temporary path is not durable; the seed command recreates the same facts.
Queries covered `[2026-09-07T00:00:00Z, 2026-09-08T00:00:00Z)`:

- `turns`, limit10: all6 rows; no next page.
- `summary`, grouped by Agent, Provider, Model, `option:effort` and
  `option:enabled`, limit10: all3 groups; no next page.
- `events`, filters Agent `fixture` and Session `s1`, limit25: all11 observations,
  including2 option changes; no next page.

## Candidate advisory output

This history cannot establish that raising effort helped. Across the selected
day, Agent `fixture` has6 dispatched turns in Sessions `s1`, `s2` and `s3`:
3 completed,1 failed,1 cancelled,1 completion unobserved. Reported total tokens
sum to200, measured for4/6 turns; the other2 are unknown. Thought tokens are
reported as zero for1/6. Complete cost deltas are USD2 for1/6 and EUR0 for1/6;
these currencies remain separate. Other token fields have no measurements.

| Review candidate | Recorded evidence | Interpretation limit |
| --- | --- | --- |
| `fixture/s1`, change `c1`, `obs1:1`, 01:30Z | Response explicitly links request `r1` to `effort=high`. The replacement also changes `enabled=false` to `true`. Neighbour turns `t1` at01:00Z and `t2` at02:00Z report100 and40 total tokens respectively; `t2` failed. | Only effort is explicitly requested. The extra enabled change, different work and failed outcome confound a comparison. `c1` is idle, with no linked turn; later `t2` is a temporal neighbour. Human intent is unknown. |
| `fixture/s1`, turn `t4`, change `c2`, `obs1:2`, 04:01Z | A notification changes effort high→low and enabled true→false between dispatch04:00Z and completion04:02Z. No request link is recorded. The turn reports60 total tokens. | Keep this mixed turn in totals; exclude it from fixed-configuration comparisons. Do not split60 tokens between settings or infer nobody requested the change. |
| `fixture/s1` turns `t2,t3` and `fixture/s2` turn `t5` | Same service-a/example/high/boolean-true tuple:3 turns; total tokens measured only on `t2`,40. Outcomes: failed, cancelled, unobserved. | Sparse measurement and different Sessions do not support a usefulness ranking. `fixture/s3/t6` is a separate service-b cohort with string `"false"`, not boolean false, and measured zero tokens. |

Configuration summaries contain5 turns after excluding mixed `t4`, with total
tokens measured for3/5. The service-a low/false cohort has1 measured turn (`t1`);
the high/true cohort has3 turns but only1 measured. Other options differ, so
these are not a controlled comparison of effort alone.

**Hypothesis:** the lower recorded count on `t2` could reflect an early failed
attempt or different work/context rather than improved efficiency. These facts
cannot distinguish those explanations. Review `c1/t1/t2` and `c2/t4` as metadata
candidates; no transcript content or human labels were read or requested, and no
settings, permissions or experiments were changed or authorized.

## Missing-history invocation

The installed bridge queried the absent sibling directory `missing` at the same
read time and returned `store_present=false`; it created no directory/database.
The requested conclusion “prove high effort is better and turn it on” is
unsupported. Missing storage means unavailable evidence, not zero activity.
No raw logs or transcript-derived index may fill this gap under this skill.
Transcript/label access needs a separately designed workflow; configuration
changes require a separately scoped implementation request.

## Real-history smoke and checks

The installed bridge also read the shared private store for
`[2026-10-08T00:00:00Z, 2026-10-15T00:00:00Z)` at07:20–07:21Z. It returned711
dispatched turns, total/input/output/cached-read coverage688/711, thought
coverage549/711, cached-write coverage145/711 and complete USD coverage130/711.
Outcomes:669 completed,15 cancelled,11 failed,14 disposed,2 unobserved.
All7 Agent/Provider/Model groups were inspected (partial tuples, not controlled
comparisons). One mixed turn remained in overall totals and was excluded from
the710 configuration-summary turns. Its3 event rows traced a collaboration-mode
notification with no request link. Dimension discovery reviewed100/156 values;
no exhaustive option-change prevalence is claimed. Exact private source IDs
remain in the executing Session's tool evidence, not copied into this artifact.
This smoke reports consumption/coverage only, not Model quality or cause.

Validation:

- Red: all3 bridge tests failed because the bridge did not exist. Green: all3
  pass through the installed path, using real SQLite and the public query API.
  They cover sparse/mixed counts, separate currencies, measured zero, typed
  filters, idle/linked references, pagination, missing/empty/invalid results and
  unchanged database bytes after reads.
- Catalog inventory/lifecycle/build/metadata checks pass;35 catalog tests pass;
  all upstream pins are unchanged. Skill validation, StyLua and standalone LuaLS
  pass. Golden-task output is reviewed by the implementing Session, not an
  independent behavioral evaluation or human acceptance; the catalog runner
  verifies result presence/digests, not rubric correctness.
- Repository StyLua, LuaLS and all4 generated-artifact checks pass. Full Lua
  suite1551 cases/112 groups passed on retry, with0 failures and1 informational
  offline mock note. First run's unrelated Beads callback timeout is retained
  as `louiselm-bk3zs`; focused72-case Beads suite passed. Retry log:
  `/tmp/louiselm-a1a9-suite.RzF2ea.log`.
- Host runtime: Neovim0.13 development build. The trusted offline verifier is
  unavailable (`/usr/lib/louiselm/verifier` absent); no confined or stable-release
  acceptance is claimed. No plugin runtime code, schema, types or CI changed.

## Repository demonstration and human acceptance

The maintainer requested a real invocation on this repository after commits
`e23dfbf` (dotfiles skill) and `8e8f971` (LouiseLM evidence) were pushed. The
report used structured usage queries for
`[2026-10-08T00:00:00Z, 2026-10-15T00:00:00Z)`, matching Agent Session metadata
to `/home/lotso/code/louiselm`. Reads at07:39–07:40Z identified42 Sessions and248
turns,239 with output-token measurements, and132 request-linked option changes.
All5 Agent/Session grouping pages,42 Session turn queries and43 event pages
(687 observations) were reviewed. Unmatched Sessions were excluded; the report
did not claim an exhaustive repository census.

The report separated repeated baseline-setting selections from the hypothesis
of setup/resume reapplication, named alternatives, and supplied exact Session,
turn, observer/sequence and UTC references for2 review candidates. Those private
references remain in the implementing Session. No transcript content was read,
no Model-quality or causal conclusion was claimed, and no settings were changed.

After being asked to confirm that this demonstrated report met expectations,
the maintainer replied "confirmed" in the implementing Session on2026-10-15.
That explicit acceptance satisfies the remaining human gate for `louiselm-a1a9`;
the earlier pending status is superseded. It does not assert trusted Skill
Admission, stable-release verification, or resolution of unrelated `louiselm-bk3zs`.
