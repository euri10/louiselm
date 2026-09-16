# Stream accumulation — louiselm-w2j53

Original revision: `be2812da0dbba917f50258736326248ea7f9b632`, clean tree.
Campaign: **1 of 3 candidates attempted, none retained**. No runtime change.

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
