# Agent workflow

Required by [AGENTS.md](../AGENTS.md) before tracker mutations, claim decisions,
or live-instance diagnosis. Read once before applicable work.

## 2. Work Tracking

Work is tracked in beads (`br`). `.beads/` is committed; its JSONL export is the
shared record across sessions, agents, and adapters.

When asked to recommend or list the next best task(s), that is a read-only
triage answer: report issue ID(s), priority, and one-line reasoning, and stop.
Do not claim, implement, or commit work unless asked to proceed.

- Run `br robot-docs guide` for command syntax. Use the bounded query recipes in
  [AGENTS.md](../AGENTS.md) before loading full issue records.
- Discover work with `br ready`. Triage with `bvr --robot-*` flags only; bare
  `bvr` opens a blocking TUI.
- Pass `--actor "<your session id>"` to every mutating `br` command — `create`,
  `update`, `close`, `delete`, `comments add`, `dep add`, `label add` — not just
  claims. The actor is your adapter's session identifier (Claude Code exports
  `CLAUDE_CODE_SESSION_ID`; other adapters may name their session, e.g.
  `deepseek/session-<uuid>`). It must point at a transcript someone can actually
  open. If your adapter exposes no session id, ask the user before mutating. Do
  not use the OS username, `br config`'s `_computed.actor`, the br skill's
  `BR_ACTOR:-assistant` fallback, or `robot-docs`' `$AGENT_NAME` — none of
  those identify your session.

Adapter lookup recipes:

- Claude Code: use `claude/<CLAUDE_CODE_SESSION_ID>` when that variable is
  present. Claude's `--resume` and `--continue` options refer to persisted
  sessions, but do not replace the current session ID with one copied from a
  session picker.
- LouiseLM ACP sessions (including OpenCode, Codex, DeepSeek, and Copilot):
  take the ACP session id your own interaction exposes and resolve it with
  `session.identity()`, as described in Live-instance introspection below. Never ask
  which chat is focused: focus follows the maintainer's window, not the Agent
  that is calling, so a focused-chat lookup names an arbitrary Session whenever
  more than one is live (louiselm-hmmc). Codex exposes its id as
  `CODEX_SESSION_ID` in the shell it spawns for a tool call, not in its own
  process environment, and that value equals the ACP session id (verified
  2026-09-07). An adapter that exposes no id of its own has nothing to resolve;
  ask the maintainer.
- OpenCode outside LouiseLM: `OPENCODE=1`, `OPENCODE_PID`, and
  `OPENCODE_CLIENT=acp` identify the process, not the current session.
  `opencode session list --format json` is an inventory, not proof of which
  session is current; use an ID from the current interaction or ask.
- Codex outside LouiseLM: `codex agents` lists persisted local sessions, but
  a listed ID is not current-session attribution by itself. Use the current
  interaction's `codex/<id>` or ask when it is not exposed.
- DeepSeek, Copilot, and any other adapter: a historical `deepseek/<id>` or
  `copilot/<id>` actor proves only the stored shape. Use the exact ID exposed
  by the current interaction or live ACP Chat; never synthesize one from a
  process name, PID, or old Beads record.

Bare UUIDs and `assistant` actors are legacy records, not valid sources for a
new mutation. If no recipe yields an attributable current ID, stop and ask the
maintainer rather than falling back to one.
- Claim work with `br update <id> --claim --actor "<your session id>"`, never
  with a bare `--status=in_progress`. `--claim` atomically sets the assignee to
  the actor, which is the only thing that makes a claim attributable, and it
  refuses to overwrite a live claim. `bvr` suggests the bare
  `--status=in_progress` form in its `claim_command` field; do not copy it.
- Read the graph through `br` and `bvr`, never by parsing `.beads/*.jsonl`.
- File findings as beads before fixing them, including work deliberately
  deferred. File before the fix, while the failing evidence still exists: the
  log line, payload, or hung process that proves it disappears the moment it is
  repaired. A finding that survives only in a transcript is lost, and deferred
  work that is not filed becomes work that never happens.
- Emit, don't narrate: if you could not complete the work you claimed, or you
  noticed something outside it, file or update a beads issue before ending the
  session. Do not leave either as a comment, a handoff summary, or a transcript
  note — those are read by no one and no command. The test for label versus
  blocker: does something have to change before anyone can do this? Yes is a
  dependency; model it as one. No — a standing requirement or missing
  capability rather than something to unblock — is a label. `needs-capability:*`
  is the namespace for the second case (e.g. `needs-capability:image-generation`
  for work only an agent with raster image generation can do); attach it and
  move on, do not restate the requirement in prose.
- Model aggregate completion with one dependency direction. Do not combine a
  child's `parent-child` dependency on its aggregate with the aggregate's
  explicit dependency on that child: `br ready` treats both as blockers while
  `br dep cycles` does not report the mixed-edge deadlock.
- Prefer finishing **your own** in-progress work to starting new work, even
  when triage ranks an unstarted issue higher. Scoring rewards unblocking
  leverage and cannot see that a claimed issue is half-done. This preference
  covers only issues you claimed; another agent's claim is not your backlog.
- Before working an issue that is already `in_progress`, run
  `./scripts/agent-liveness-snapshot --status`. This project does not use
  Agent Mail and will not; the script supplies the reservation snapshot
  `br coordination status` needs by reading each session's transcript mtime,
  and without it every claim classifies as `no_mail_snapshot` and no claim can
  ever be released. Do not call `br coordination status` bare, and never hand
  it an empty snapshot: an empty file asserts that no session is alive, which
  makes age the only evidence and takes work away from an agent that is simply
  busy. A session counts as gone after 45 minutes of transcript silence
  (`LOUISELM_LIVENESS_WINDOW_MINUTES`). An assignee the script reports as
  unresolved on stderr has no transcript to judge, so treat its claim as live.
  A live claim belongs to its holder: pick something else. A claim is eligible
  for takeover only when its classification is `abandoned_likely` and
  `reclaim_allowed_by_policy` is true. That flag alone is not enough — br also
  sets it at `stale_candidate`, two hours in, and this project requires the
  eight-hour `abandoned_likely` bar as well, so that a takeover needs both a
  silent transcript and a silent issue. Age alone is not permission either:
  `no_mail_snapshot`, `ambiguous`, `fresh`, and
  `blocked_by_active_reservation` remain protected regardless of age. Record
  the command's evidence summary in a comment, then have the maintainer requeue
  the issue with `br update <id> --status open --assignee "" --actor "<session id>" --json`;
  the next Agent claims it normally with `--claim`, which refuses to overwrite
  an intervening holder. `br scheduler` already excludes claimed work and
  explains its ranking, so prefer it over hand-scanning
  `br list --status in_progress`. With br 0.5.12, scheduler labels are populated (verified against `br show`,
  louiselm-scheduler-labels-xw1i). Use `br ready --label-any
  needs-capability:<name> --limit 5 --json` for capability-specific discovery;
  scheduler has no label filter.
- Keep durable context in the repository, never in an agent's private memory.
  Context anchored to one issue belongs in `br comments`; a standing convention
  belongs in AGENTS.md or its applicable required reference. Anything an agent knows that another adapter cannot
  read is a defect in the record.
- Before running `br close` on a `bug`, `task`, or `feature`, route what the
  work taught: issue-local evidence and false starts to a `br comment`, a rule
  that should bind future work to AGENTS.md or its applicable required reference, an unresolved question to a
  `needs-design` question. Name the destination or say none is warranted;
  silence is the failure mode, because the lesson is legible only while the
  work is still fresh. Chores and mechanical closes are exempt from
  lesson-routing and prior-lesson search, but still need a close verdict —
  `none:mechanical` makes that exemption a checkable claim instead of a silent
  assumption; see the Close verdict gate below.
- Every `br close` on this project must carry exactly one typed verdict naming
  what makes the close checkable by someone other than its author, enforced by
  `.beads/policy.yaml`'s `close_policy.require_typed_references` gate. The five
  kinds are a partition, not a menu — pick the one that actually applies:
  `consumer:` (production code reaches this, e.g.
  `consumer:lua/louiselm/ui/chat/init.lua:11`), `gate:` (a check fails if it
  stops being true, e.g. `gate:scripts/generate-luacats`), `live:` (the
  maintainer confirmed it running, e.g.
  `live:codex/01a050f8-93e9-7f00-847f-be08e6920e77`), `inert:` (not reachable
  yet — requires a filed follow-up issue), and `none:` (nothing to verify, e.g.
  `none:mechanical`). A reason whose only typed reference is a built-in kind
  such as `commit:` does not satisfy the gate — built-ins are not in
  `required_kinds`, deliberately, so a stray commit citation cannot stand in
  for naming a verdict. A parent's verdict may be no stronger than its weakest
  child's: if any child closed `inert:`, the parent closes `inert:` too. This
  is a convention the gate cannot see, not a rule `br` enforces — a violation
  is a visible contradiction in the shared record, not a rejected command.
- Write close reasons, comments, and commit bodies in the fewest words that
  stay greppable: name the decision, state the outcome, cite `file:line`
  where one applies. Do not restate the work as prose or re-explain what the
  diff already shows. Compress, never omit — the routing rule above still
  binds, and a lesson dropped to save a line costs far more than the line
  saved.
- `br search` excludes closed issues unless passed `-a`, and lessons live on
  closed issues almost by definition. Always search prior art with `-a`, and
  never read a zero-result search as absence without it. `br list --json`
  carries no comments field at all, so it cannot scan comment text however it
  is filtered (louiselm-o2jh).
- File a design question worth a full `louiselm-grill-me` session as a beads
  `question` issue, labeled `needs-design`, titled "Grill-me needed: <question>". Open
  with a line telling the grill not to resolve the question in passing, then
  the question itself, the context/evidence that raised it, and the threads
  worth pulling — not a pre-baked answer.
- When claimed work reveals multiple independently completable deliverables,
  create child tasks instead of hiding them as serial "next slices" inside the
  original issue. If a clean slice depends on an unresolved product or
  architecture decision, create a blocking `needs-design` question and pause
  implementation for a focused grill-me session.
- Run `br sync --flush-only` before committing, and commit `.beads/` alongside
  the work it describes.
- Stage the paths you changed by name. Never `git add -A`/`git add .`. Other
  sessions work this tree at the same time, and a blanket stage silently sweeps
  their in-flight edits into your commit — the work is not lost, but it lands
  under someone else's message and their own commit then looks incomplete. This
  happened in commit `7b7dd90`, which absorbed a `tests/session/api_spec.lua`
  case belonging to the fix committed separately as `c5a7046`. Run
  `git status --short` before staging and account for every path you did not
  write.

### Live-instance introspection

When your shell is a child of the Neovim instance hosting the plugin — the
normal case here — `$NVIM` is that editor's RPC socket. Inspect live plugin
state through it before scraping environment variables, process tables, or
state files: those miss state owned by a running adapter and can reconstruct
a different answer than the live one.

```bash
nvim --server "$NVIM" --remote-expr 'luaeval("...")'
```

For multi-line queries, write a temporary Lua file and run
`luaeval("dofile('/tmp/query.lua')")` from the same remote expression.

`acp-proxy` splits one conversation across two log files. Everything sent
before the Agent returns a session id — `session/new` and its `_meta`, which is
where session construction is decided — lands in
`~/.local/state/acp-llm-adapter/proxy/{connections/<connection-id>.jsonl}`, not
in `sessions/<session-id>/log.jsonl`. Searching only the session directory
makes construction-time configuration look like it was never sent
(`louiselm-wh2l` was filed on exactly that mistake). Resolve a session back to
its connection with `grep -l <session-id> .../proxy/connections/*.jsonl`, which
works because the bind is recorded there. When checking whether a value reached
the Agent, match its structure (`'"thinking":{'`) rather than a word from it:
bare terms like `summarized` also appear in ordinary session prose and read as
false confirmation.

To learn your own Session identity, resolve the ACP session id your interaction
already exposes against the live Sessions:

```bash
nvim --headless --server "$NVIM" --remote-expr 'luaeval("(function() local id, err = require(\"louiselm.session\").identity(_A) return id or err end)()", "<your ACP session id>")'
```

`--headless` is required: without it the client starts a TUI and its terminal
queries scramble the answer on a real tty.

`session.identity()` returns `<agent>/<ACP-session-id>` for the one live Session
holding that id. Use it verbatim as the Beads actor. It reads only the id you
pass, so it stays correct while several Sessions run and while the maintainer
switches windows.

Anything else it returns is a question for the maintainer, never a reason to
name another Session. `no live Session has ACP session id <id>` means what you
passed is not the Session you are running in. The ambiguous message means two
live Sessions share that id and only the maintainer can say which one is
calling. Do not substitute an ID copied from an old Beads record, a different
Session, or a child-agent task.

A sandboxed Agent may not reach the socket at all — connecting to `$NVIM` needs
write access to a path outside the workspace, and Codex's Linux sandbox refuses
it (louiselm-lkoc). Unreachable does not license a guess. If you also have no id
of your own, you have nothing to resolve and nothing to fall back on: ask the
maintainer. Only if you do know your own id may you use `<agent>/<your id>`, and
you must say it is unverified. Never guess the Agent name: it is a LouiseLM
config key, not your adapter's name, and the two only happen to match today.

`:LouiselmSessionId` and `Chat:session_id()` answer for the **focused** chat.
That is what a bug report about the visible Session wants, and it is not caller
identity. On 2026-09-07 four Sessions were live and the focused lookup named the
one that was not even running a turn; two Sessions had already claimed the same
Beads issue under one actor that way (louiselm-hmmc). Do not attribute work with
it.
