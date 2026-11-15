# Stream accumulation — louiselm-w2j53

Original revision: `be2812da0dbba917f50258736326248ea7f9b632`, clean tree.
Campaign: **2 of 3 candidates attempted, none retained**. No runtime change.

## Reuse

Run from the repository root:

```sh
nvim --headless --noplugin -u NONE -l docs/performance/stream-accumulation-20260916/benchmark.lua
```

The two baseline JSON files contain raw samples, medians and ranges. The workload
uses unique 32-byte chunks, a newline every 128 bytes, 8/64/256-KiB responses,
two warmups and nine samples. It measures recording, rendering and their
scheduled combination (128 events per drain), including snapshot and completion
echo matching. Highlighting and network are excluded. GC stays enabled, with a
full collection before each sample. Sampled Lua heap includes garbage and is
neither total allocations nor whole-process peak RSS.

Environment: Neovim 0.12.5 Release / LuaJIT 2.1.1774638290; Linux
7.1.8+deb13-amd64; Ryzen AI 7 PRO 350, 16 logical CPUs. No concurrent campaign
gates; ordinary desktop load (0.51/0.40/0.33), CPU frequency unpinned.

256-KiB combined medians: **234.718 / 234.687 ms**; recorder:
**137.068 / 136.786 ms**; renderer: **153.996 / 152.601 ms**.
Original acceptance required >10% and >20 ms combined improvement, nonoverlapping
sample ranges and no timing/memory regression beyond baseline noise.

## Outcome

Deferring transcript concatenation passed 1,097 tests but failed LuaLS:
the open-tool map held `TranscriptEntry`, incompatible with the proposed
`TranscriptBlock` array. The former skill rule forced rollback and a stop.
No candidate timing was captured. Full details and lesson remain in Beads.

After measurement, the harness gained required Session fixture fields and
replaced `unpack` with `nvim.list_extend`; the timing loop stayed unchanged.
**Refresh the baseline with the current harness before another comparison.**
Preserve the consumed count of 1 when continuing.

User-approved cleanup removed success logs, rejected patches and the temporary
snapshot test; production code and existing tests are unchanged from the baseline.
The original suite passed 1,096 tests; restoration before cleanup passed 1,097
with the temporary test, plus StyLua, LuaLS and all three generated checks.

## Continuation: 2026-11-15

Budget remains **1 of 3 passes consumed, 0 accepted**; at most two remain. The
refreshed baseline used main at `2bc7bcf6a469047be4e38f49b57b6e2692945632`.
The relevant production and benchmark files had no dirty diff. The only relevant
dirty change during sampling was the added snapshot-isolation characterization in
`tests/session/transcript_spec.lua` (diff SHA-256
`91d51876ee9a3d66842cf8c7f34b5f39f49fa8e2c0d10febe7c9a542fda4726e`).
Other Sessions' Beads, documentation, Rust and blog changes were outside the
measured paths. A later passing chat-buffer characterization covers echo matches
spanning chunks, late completion and reset on a replayed prompt.

Using the unchanged harness and protocol above, the raw runs are
`baseline-20261115-a.json` and `baseline-20261115-b.json`. Neovim 0.12.5
Release / LuaJIT 2.1.1774638290, Linux 7.2.11+deb13-amd64, Ryzen AI 7 PRO 350;
CPU frequency unpinned, ordinary shared desktop load. Heavy checks and sampling
were serialized with the batch lock. The initial stable-Neovim suite attempt
inherited a nightly `VIMRUNTIME` and failed collection; setting it to the 0.12.5
runtime fixed the environment. The focused snapshot test passed 31/31; the
full suite passed 1,676 cases, 0 failures, 1 informational note. StyLua, LuaLS,
the API, LuaCATS, vimdoc and version checks, and `git diff --check` passed.
The focused chat-buffer suite then passed 17/17. The offline confined verifier
requires a task-owned disposable guest; its result is recorded below when known.

For the 256-KiB scheduled pair, medians were **330.621 / 313.908 ms**, with
ranges **319.920–347.966 / 310.146–425.459 ms**. Recorder medians were
171.489 / 175.798 ms; renderer medians were 205.283 / 206.902 ms. The two
scheduled medians differ by **16.713 ms**; the original >20 ms and >10% useful
gain bars already exceed that observed median drift. To reject the broad outlier
range as noise, a candidate must also have its entire sample range below both
baseline ranges, and show no timing or sampled-heap regression beyond baseline
noise at any workload size. This workstation's shared load makes small gains
indistinguishable; no gain is claimed from these baseline samples alone.
Specifically, both candidate medians must beat the lower baseline median by
more than 10%, 20 ms and 16.713 ms, and their maximum must be below 310.146 ms.
For each size/mode, compare each candidate median for time and sampled
heap with the maximum of the 18 baseline samples for the same size/mode; a
median above that observed range is a regression. The heap measure is sampled
Lua heap including garbage, not whole-process peak memory.

Pass 2 hypothesis (not yet applied): retain subsequent chunks by transcript
block index and concatenate only when producing a snapshot. The recorder alone
accounts for roughly 171–176 ms at 256 KiB, and `append_text` currently recopies
the growing block for every chunk. Keep the existing entry type for both ordered
blocks and the open-tool map, as the first attempt's LuaLS failure requires.
The caller-visible snapshot remains a deep copy with plain text; tool updates,
Handoff provenance, compaction boundaries and event scheduling must stay intact.
Start from the production-clean baseline above; rollback is the pass's own
`transcript.lua` patch only. No pruning candidate is justified independently.

### Paused before pass 2

The task-owned guest was prepared, but the offline verifier never started: the
batch heavy-check lock remained held by another Session for more than 22 minutes
while this gate waited. Only this task's queued process was stopped and its
disposable guest removed. The other lock holder and guest were untouched. The
new chat-buffer test passed its focused suite but was added after the full host
suite; it still needs the complete suite and LuaLS. The offline confined gate
also remains unrun. No production candidate or candidate measurement occurred,
so this is a blocked continuation, not a measured no-gain result. **1/3 passes
remain consumed; 0 accepted.**

To resume, recheck the current branch, ownership and measured source hashes;
refresh the baseline if the input or machine conditions changed. Run all
baseline gates, including the offline confined verifier in a newly prepared
task-owned guest under the shared lock, before applying the hypothesis above.
Retain the existing two-pass remainder and the recorded acceptance thresholds.

## Resumed: current baseline

At `901971b4a39ddba5d14f59f462df880c016cfeda`, the production and benchmark
paths were clean and their hashes matched the prior baseline. The relevant dirty
diff was the two characterization tests (combined diff SHA-256
`c29c7133ab8282e3824d70a09fa64e793ea837fc044882c0c04a6aa674c046f6`).
The full host suite passed 1,677 cases with 0 failures and 1 informational note;
both focused suites, StyLua, LuaLS, all generated checks and `git diff --check`
passed. A newly prepared task-owned guest passed both verifier-toolchain suites
and `privileged_confined_lua_gates_from_clean_snapshot` (1 test, 0 failures).

With the same harness, tool versions and sampling protocol, the fresh raw runs
are `baseline-resume-a.json` and `baseline-resume-b.json`. The 256-KiB scheduled
medians were **291.040 / 280.449 ms**, ranges **277.615–295.266 /
257.296–299.764 ms**; median drift was **10.591 ms**. Recorder medians were
212.087 / 156.295 ms and renderer medians were 225.107 / 188.712 ms, with
substantial desktop-load variation in the first run. CPU frequency remained
unpinned; load average before sampling was 2.22/2.68/2.67. Candidate acceptance
still requires both medians to improve the lower scheduled median by more than
10%, 20 ms and the observed 10.591-ms median drift, with all candidate samples
below 257.296 ms. For every mode and size, neither median time nor sampled heap
may exceed the maximum among that metric's 18 baseline samples. This demands a
large distinguishable improvement, not a conclusion from the noisy first run.

The campaign remains **1/3 consumed, 0 accepted** before applying the pass-2
hypothesis above.

### Pass 2: rejected

The block-indexed chunk-storage candidate is preserved as `candidate-2.patch.gz`
(uncompressed patch SHA-256 `6de9a6c0bdab61f08e7543ffc4271d764076c6cf07105a9ba7d8852a01a76545`);
its measured source SHA-256 was
`39e4ddce00c1896507aa27fa34209f646888ce563b6bf7f77edde623a6ccba24`.
Focused transcript tests, the 1,677-case host suite (0 failures, 1 informational
note), StyLua, LuaLS, generated checks and the confined verifier (1 test passed)
all passed before measurement. Raw samples are `candidate-2-a.json` and
`candidate-2-b.json`.

| 256-KiB workload | Baseline medians, ms | Candidate medians, ms | Candidate ranges, ms |
|---|---:|---:|---:|
| Recorder | 212.087 / 156.295 | 0.511 / 0.546 | 0.462–0.855 / 0.532–0.823 |
| Scheduled pair | 291.040 / 280.449 | 240.999 / 282.039 | 227.266–301.473 / 248.818–352.465 |

The recorder improvement is measurable, but the scheduled pair fails the
predeclared acceptance: only the first candidate median beats the lower baseline
median by more than 10% and 20 ms, and both candidate ranges overlap baseline.
The second median is 1.590 ms slower than that baseline median. The unchanged
renderer-only workload also varied beyond baseline noise: its 8-KiB median was
3.713 ms versus a 2.996-ms baseline maximum, and its 64/256-KiB sampled-heap
medians in one run exceeded the corresponding baseline maxima. Those renderer
observations cannot be attributed to the recorder change, but they show that
the shared desktop measurements did not isolate the end-to-end effect reliably.

The candidate was reverted exactly. **2/3 passes consumed, 0 accepted**; the
campaign stops because measurement noise prevents distinguishing a qualifying
end-to-end gain. Pass 1 also produced no qualifying gain. No production change
is retained, and no end-to-end speedup or live acceptance is claimed. The
snapshot and cross-chunk completion characterizations remain as test coverage.
Restoration verified the baseline transcript SHA-256
`54dc6925a5752e0708946eb5ae387aaf0b7f4c80955781e65773050b39d74c95`, no
production diff, both focused suites (31 and 17 cases), formatting and
`git diff --check`. The full host and confined baseline gates above exercised
these identical restored production and characterization bytes.
