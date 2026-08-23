# LouiseLM

![People, ideas, tools, and agents connected through a red-and-black decentralized mesh](assets/louiselm-hero.webp)

**LouiseLM Organizes Unruled Intelligent Systems into Emancipated Language
Meshes.**

> LouiseLM turns ideas into reality.

Ideas arrive wild and unordered. LouiseLM gives them enough structure to become
real without forcing them into one prescribed path: chaos with order, directed
by the person whose idea started it.

> [!IMPORTANT]
> LouiseLM is alpha software. Today it provides an ACP-backed Neovim chat and a
> headless session foundation. The complete idea-to-reality loop described
> below is the vision being built incrementally, and APIs and workflows may
> change as first-hand use exposes better designs.

## Why LouiseLM

An idea can appear while gardening, walking, or already concentrating on
something else. Capturing it should take seconds and should not steal the
attention needed to finish the current activity. Understanding whether it
matters, challenging it, and deciding what to build are slower processes that
deserve focused thought.

Implementation is slower too, but increasingly little of it needs continuous
human attention. As [Julien Danjou argues](https://julien.danjou.info/blog/the-human-is-the-new-bottleneck/),
the scarce resource in software development is becoming the attention needed
to decide what deserves to exist and whether the result is right. LouiseLM aims
to preserve that attention for creativity, taste, and judgment while agents
handle more of the mechanical work.

The project's anti-capitalist position and the reasoning behind its permissive
licensing posture are set out separately in the
[political statement](POLITICAL_STATEMENT.md). Use of the LouiseLM name and
associated marks is covered by the [trademark policy](TRADEMARK_POLICY.md).

## From an idea to reality

LouiseLM's envisioned loop is:

1. **Capture without context switching.** Record a few words, text, or speech
   wherever an idea appears. Acknowledge it quietly and let the current focused
   session continue.
2. **Organize and resurface.** Preserve the original capture, enrich it with
   relationships, and rank it using explainable signals. Configurable policies
   decide when to resurface a recommendation; finishing the current focused
   session is the default boundary.
3. **Deliberate.** Give the idea sustained attention through a conversation
   that questions assumptions, explores alternatives, and reaches an explicit
   agreement with the user.
4. **Plan.** Turn that agreement into dependency-shaped work that agents and
   people can inspect, prioritize, and execute.
5. **Implement.** Let a swarm of agents select ready work, write code, run
   quality gates, and review one another. Different phases can use different
   agents or models according to the judgment, cost, and throughput they need.
6. **Stay in control.** Surface uncertainty, policy violations, failures, and
   decisions that need a person. A project control plane should make the work
   graph, agent activity, blockers, progress, and attention points legible
   without requiring line-by-line supervision.

Automation may organize ideas and advance explicitly permitted work, but
implementation begins only after the user agrees that the idea and plan are
ready. Emancipation here means freeing ideas, workflows, and human creativity;
it never means giving agents unbounded authority.

## A language mesh, not a fixed pipeline

LouiseLM connects people, agents, models, skills, tools, and capture surfaces
through language and observable workflow boundaries. It should provide one
coherent experience out of the box while keeping its stages replaceable. The
default, which is in a sense my louislm workflow configuration is a useful starting point, not a definition of the only correct way to
work.

Ideas are personal data, so their storage should remain local and user-owned by
default. Cloud models, synchronization, and remote services are choices rather
than requirements.

Neovim is LouiseLM's deliberate home: a programmable cockpit for thinking,
observing, and intervening, as well as a runtime for headless automation.
Future voice, mobile, or dedicated capture devices may feed the workflow, but
none is required and no editor-independent service is assumed.

LouiseLM is focused on the concrete promise of turning ideas into reality, in my case software but this in hopefully more generalizable. The
same machinery might eventually help other kinds of ideas become outcomes, but
it is not trying to become a general notes application or second brain.

## One possible workflow

One workflow already works manually and is beginning to move into LouiseLM:

1. Use a strong deliberative model and the `grill-me` skill to interrogate an
   idea until the user and agent share an understanding.
2. Turn the agreement into a plan, then use the `to-beads` skills to split it
   into epics, features, and independently executable tasks.
3. Use `br` and `bvr` to inspect the dependency graph, recommend the next
   useful task, or expose parallel execution tracks.
4. Give well-specified tasks to coding agents that can implement and verify
   them with less supervision, escalating the decisions that still need human
   judgment.

This is a configuration of LouiseLM, not LouiseLM itself. Those skills, task
trackers, and models are examples; users should be able to compose different
and smarter workflows from the same foundation.

## What exists today

LouiseLM currently provides the first layer of that experience: an ACP-first
Neovim plugin with validated agent configuration, process and protocol
handling, multiple headless sessions, typed events, permission policies, skill
discovery, context providers, and a streaming chat buffer. You can already talk
to an ACP agent inside Neovim and drive the same session API without a UI.

Idea capture, background triage, autonomous task-graph execution, and the
operator control plane are not implemented yet. The sections below document
the foundation that exists now.

## Development

Install the pinned `mini.test` dependency from `mini.nvim`:

```sh
./scripts/install-test-deps
```

The installer uses `mini.nvim` `v0.18.0` at commit
`1345d191bb3da9c7b0e977f4387c5761f9bff68d`. Run the test suite with:

```sh
nvim --headless --noplugin -u "./tests/minimal_init.lua" -c 'lua MiniTest.run()' -c 'qa!'
```

Run one test file:

```sh
nvim --headless --noplugin -u "$PWD/tests/minimal_init.lua" \
  -c 'lua MiniTest.run_file("tests/schema/dsl_spec.lua")' -c 'qa!'
```

For interactive debugging, start `nvim -u ./tests/minimal_init.lua` and run
`:lua MiniTest.run()`. Without `--headless`, Neovim intentionally stays open.

Run `:checkhealth louiselm` after setup to revalidate the configuration and
check configured agent executables, versions, and skill paths. Agent version
results arrive asynchronously because external processes must not block the
editor; unavailable executables and invalid skill paths are reported directly
in the health buffer.

## ACP client

For a minimal manual session, run this from the repository root:

```sh
nvim -u ./manual_init.lua
```

The config installs this checkout with `vim.pack.add`. Verify it loaded with
`:lua print(require("louiselm.acp").PROTOCOL_VERSION)`.
It registers `:LouiselmChat` and the other chat commands. After
`louiselm.setup()` they use the configured agents and skills; before setup they
fall back to the default launcher described below.

ACP agents communicate over newline-delimited JSON-RPC on stdio. The client
starts the configured process, negotiates protocol version 1, then exposes
session requests and streamed notifications without requiring a UI:

```lua
local acp = require("louiselm.acp")
local client = assert(acp.connect({ command = "claude-agent-acp", args = {} }))

client:initialize(nil, function(result, err)
  if err ~= nil then
    return
  end
  client:new_session({ cwd = vim.fn.getcwd(), mcpServers = {} }, function(session, session_err)
    if session_err == nil then
      client:prompt({ sessionId = session.sessionId, prompt = { { type = "text", text = "Hello" } } })
    end
  end)
end)
```

Use `on_notification` and `on_request` connection callbacks for streamed
updates and agent permission requests.

## Headless sessions

The session API manages multiple named ACP sessions without requiring a UI:

```lua
local sessions = assert(require("louiselm.session").new({ claude = { command = "claude-agent-acp", args = {} }, }))

local session = assert(sessions:create_session("claude", { cwd = vim.fn.getcwd() }, function(value, err)
  assert(err == nil, err)
end))
session:on(function(event)
  if event.type == "chunk" then
    print(event.data.content.text)
  end
end)
session:prompt("Review this project")
```

Discover persisted conversations from configured agents that advertise ACP
`sessionCapabilities.list`. Pass `cwd` for an exact workspace filter or omit it
to list every workspace; each result includes the configured agent name needed
for loading:

```lua
sessions:discover_sessions({ cwd = vim.fn.getcwd() }, function(found, errors)
  for _, item in ipairs(found) do
    print(item.agent, item.session_id, item.cwd, item.title, item.updated_at)
  end
end)
```

To load a persisted ACP conversation, use its agent-side session id. The agent
must advertise `loadSession` during initialization; loading replays the stored
history through the session's normal events before the ready callback runs:

```lua
local session = assert(sessions:load_session("claude", "sess_789xyz", {
  cwd = vim.fn.getcwd(),
}))
```

Each session is addressable through `get_session(id)`, inspectable with
`session:inspect()`, and exposes `prompt`, `cancel`, and `dispose` methods.
Events include streamed chunks, tool-call start/finish, permission requests,
turn completion, and agent/process errors. Call `sessions:dispose()` when the
owner is finished to terminate all remaining agent processes.

## Chat UI

The optional chat controller renders a session into a scratch markdown buffer,
shows streamed chunks and tool calls, and keeps one buffer per session:

```lua
local chat = assert(require("louiselm.ui.chat").new(sessions, {
  agents = { "claude" },
}))
chat:new_session()
```

For a quick interactive check, run `nvim -u ./tests/minimal_init.lua` and use
`:LouiselmChat`. By default it launches the DeepSeek ACP agent with:

```sh
/home/lotso/code/acp-llm-adapter/acp-debug.sh \
  acp-llm-adapter serve --backend deepseek
```

The wrapper preserves ACP JSON-RPC on stdout and records debug logs under
`$XDG_STATE_HOME/acp-llm-adapter` (or `~/.local/state/acp-llm-adapter`). Set
`DEEPSEEK_API_KEY` for the adapter; the bootstrap passes it as `LLM_API_KEY`.
Alternatively, set `LOUISELM_AGENT_COMMAND` to use another ACP executable
without implicit arguments.

Use `chat:switch("session-1")` for another attached session. Prompts entered in
the buffer, including slash commands, are passed to the session unchanged.
While a turn is active, Enter queues one prompt for that session and marks it
`Queued for next turn`; editing it returns it to a draft until Enter recommits
it. The queued prompt sends after the active turn completes or is cancelled.
Call `chat:dispose()` to remove its buffers and event listeners; it does not
dispose the sessions it displays.

When an agent returns supported ACP session options, a new chat opens an
overview before the first prompt. Select and boolean options remain available
while the session is idle; dismissing the overview keeps the agent defaults.
The interactive controls are:

- `:LouiselmSessionOptions` — inspect or change all supported options
- `:LouiselmPermissions` — inspect and revoke remembered permission rules
- `:LouiselmInline` — ask the agent to replace the current selection, or insert at the cursor
- `:LouiselmCancel` — cancel the active turn
- `:LouiselmNewSession` — start another session without stopping existing ones
- `:LouiselmResume` — discover and load a session from the current workspace
- `:LouiselmResume!` — discover and load a session from any workspace
- `:LouiselmSwitchSession` — switch using compact session telemetry rows
- `:LouiselmHandOff` — hand the current session's reviewed transcript off to another configured agent, keeping the source attached
- `:LouiselmRenameSession` — give the current session a human-readable name
- `:LouiselmSessionId` — copy the current agent-scoped ACP session identifier
- `:LouiselmCloseSession` — dispose the current session and remove its buffer
- `:LouiselmPickSkill` — pick a configured skill and queue its invocation
- `:LouiselmPickFile [root]` — pick a file and queue its path as context
- `:LouiselmMentionBuffer` — queue the source buffer as context
- `:LouiselmSendSelection` — queue the source buffer's visual selection as context

LouiseLM sends picker choices through `vim.ui.select`, so the active UI provider
owns their layout. If Snacks reports several choices while only one row appears
usable, disable wrapping for its `select` source. Long rows otherwise consume
multiple display lines even though they are separate items:

```lua
require("snacks").setup({
  picker = {
    sources = {
      select = { win = { list = { wo = { wrap = false } } } },
    },
  },
})
```

This source-specific override can coexist with `wrap = true` for other Snacks
picker lists. LouiseLM intentionally does not detect or reconfigure a
`vim.ui.select` provider.

`:LouiselmPickSkill` tags its call with `opts.kind = "louiselm_skill"` and
gives each item a `file` field pointing at that skill's `SKILL.md`, both
standard `vim.ui.select` extension points a provider may opt into. Snacks, for
example, renders a live SKILL.md preview once a `kinds.louiselm_skill` entry
exists for its `select` source:

```lua
require("snacks").setup({
  picker = {
    sources = {
      select = {
        kinds = {
          -- The `select` layout hides the preview pane by default; switch to
          -- a layout that shows one to see it.
          louiselm_skill = { preview = "file", layout = { preset = "default" } },
        },
      },
    },
  },
})
```

Each chat begins with a contiguous diagnostic block that stays copyable as
plain text:

```text
# codex/ACP_SESSION_ID · session-1
Session: status=ready · display=Your turn · skills=native
ACP options: Mode=agent · Model=gpt-5.6-sol · Fast mode=false
Telemetry: context=148222/258400 (57%) · cost=1.5 USD
```

The identity line keeps the ACP and local session identifiers. `Session` keeps
the raw lifecycle state beside its friendly label and reports the exact
`native`, `inject`, or `off` skills policy. `ACP options` preserves the agent's
names, typed current values, and order. Tool activity stays in chronological
`[tool]` transcript messages instead of the diagnostic block.

The window bar persists the friendly lifecycle label, reported context counts,
LouiseLM's derived percentage, and cumulative cost while the transcript is
scrolled. Context and cost segments are omitted until the agent reports them;
an omitted cost in a later update retains the last reported cumulative value,
while an explicit ACP null clears it. A model-option change marks the retained
percentage `stale` until fresh usage arrives. LouiseLM does not estimate token
counts, prices, or compaction state, and it does not display a context-pressure
classification.

Adapter values use the user-overridable `LouiselmAcpValue` highlight and derived
percentage/staleness use `LouiselmDerivedValue`. Lifecycle labels use
`LouiselmStatusReady`, `LouiselmStatusActive`, `LouiselmStatusWarning`, and
`LouiselmStatusError`; their text remains meaningful without color. Completed
turns append only usage fields reported by the agent. The session-options picker
annotates values with locally observed average tokens per turn and
agent-reported cost when history exists; values without measurements remain
unannotated. This history is stored under Neovim's state directory and contains
option identity, measured counters, and timestamps, never prompts or tool data.

The inline assistant uses the same headless session API. It sends the current
buffer location and selected text as context, then replaces that selection with
the streamed response; with no selection, the response is inserted at the
cursor.

Sessions accept an optional `name` in their headless options. `inspect()` also
reports whether the session was `new` or `loaded` and exposes `acp_session_id`
once the agent establishes it; named sessions use that name in chat headers and
the session switcher.

When reporting a session-specific bug, run `:LouiselmSessionId` and include the
copied `<agent>/<ACP-session-id>` value with the reproduction steps and expected
and actual behavior. The identifier contains no prompt or tool payload content.

Headless consumers can call `session:set_config_option(id, value, callback)`
while `session:inspect().status == "ready"`. Snapshots include
`config_options`, optional `context`, `cost`, `usage`, and `activity` fields;
the event stream adds `state_changed`, `config_options_changed`, and
`usage_updated`. A model-category change marks existing context telemetry stale
until the agent sends another `usage_update`.

## Permissions

Sessions default to an `ask-human` policy: agent permission requests are held
until an event consumer responds. For explicitly scoped automation, pass a
policy object when creating a session:

```lua
local permission = require("louiselm.permission")
local policy = assert(permission.auto_approve_scoped({
  paths = { vim.fn.getcwd() },
  commands = { { "git", "status" } },
}))
local session = assert(sessions:create_session("claude", {
  permission_policy = policy,
}))
```

File edits and command argv are checked without shell expansion. Unknown ACP
permission requests remain askable and are never auto-approved.

For typed ACP choices, LouiseLM gives their lifetimes literal semantics:
`allow_once`/`reject_once` are not saved, `allow_session`/`reject_session` stay
in memory until that local session is disposed, and
`allow_always`/`reject_always` persist in
`stdpath("state")/louiselm/permissions.json`. Labels never determine lifetime.
Rules are keyed by the configured agent, exact adapter command and args, and
canonical session workspace. A command rule stores the complete selected argv
as an exact prefix; a file rule stores one exact normalized path. This means a
remembered choice cannot silently spread to another adapter, workspace,
command prefix, or file.

Remembered decisions replay only through a matching typed once-only option. If
the adapter later omits one, LouiseLM asks again instead of guessing from a
label. Malformed or unreadable state also fails closed: the request remains
askable, an attempted persistent choice is cancelled with an actionable error,
and the state file is not replaced.

When an ask-human request is a file edit with a diff or replacement content,
the chat UI opens a read-only `louiselm-diff://` buffer before responding. Press
`a` to allow the edit, or `d`/`q` to reject it. When the adapter offers several
typed lifetimes, a picker asks which one to use. The review records the file
content it displayed and rejects a local apply if the file changed meanwhile;
the ACP agent remains responsible for writing the file after approval.

Command and unknown permission requests send the ACP options through
`vim.ui.select`. Command picker prompts include the exact normalized command as
a JSON array so argument boundaries and embedded shell text remain inspectable;
malformed or unknown requests say that details are unavailable. Use the active
provider's normal navigation and confirm the highlighted choice. Typed
rejection choices are listed before approvals so the initial choice fails
closed. Dismissing the picker sends a cancellation response. Permission UI work
is scheduled onto Neovim's main loop, and a choice arriving after chat disposal
cancels the request instead of granting it.

Agents ask for several permissions at once when they run parallel tool calls. A
session publishes those requests one at a time and stays in
`waiting_permission` until the last outstanding request is answered, so a
picker or diff review never has to host two decisions at once. Each request is
answered exactly once; answering an already-answered request fails with an
error instead of writing a second response. Remembered rules are consulted when
a request is published rather than when it arrives, so an `always` choice also
covers the requests still queued behind it. Cancelling the turn answers every
outstanding request with the ACP cancelled outcome, which releases an agent
that is blocked waiting for a decision.

The chat controller applies the same rule across sessions: one picker or diff
review is presented at a time for every attached session, and the next decision
opens when the current one is answered. Disposing the chat answers whatever it
can no longer host with a cancellation. Because a review buffer is
`bufhidden=wipe`, navigating away from it abandons the review and cancels its
request rather than leaving the agent waiting.

While a decision is presented, commands that open their own picker — file,
skill, session switch, session resume, agent choice, session options, and
remembered-permission management — refuse with `a louiselm permission decision
is open; answer it first` rather than replacing the open picker. They are
refused instead of queued because a decision can open a nested picker of its
own, and a queued command would surface long after the keypress. Closing the
session stays available as the way out of a decision you cannot answer, and
cancelling the turn is not a picker at all.

Headless consumers can inspect and revoke through
`sessions:list_permissions()` and `sessions:revoke_permission(rule_id)`. Tests
or isolated embedders can pass an explicit store as the third constructor
argument to `Session.new`; set its `permission_store` field to
`permission.store(path)`.

## Chat context

Context providers stay thin: they point the agent at files and skills while
visual selections carry their selected text. `:LouiselmChat` exposes them as
`:LouiselmMentionBuffer`, `:LouiselmSendSelection`, `:LouiselmPickFile`, and
`:LouiselmPickSkill`. A headless `Chat` you construct yourself queues context
on an attached view the same way, before submitting its prompt:

```lua
chat:mention_buffer()
chat:send_selection()
chat:pick_file()
chat:pick_skill()
```

Pending manual contexts appear as compact chips on the editable prompt. After
ACP accepts a non-slash prompt, every attached context moves into one closed
native Neovim fold immediately before the ordinary visible user prompt. The
fold summary lists context labels in transport order; opening it reveals the
exact text bodies and resource-link fields that were sent. `zo`, `zc`, and
other normal fold commands affect presentation only and can never add context
to a later prompt. These folds exist only in the live `nofile`, swap-disabled
chat buffer; session reload and Neovim restart do not reconstruct them.

Pass configured roots to the chat controller to rediscover skills whenever the
picker opens. Relative roots resolve against the attached session's working
directory:

```lua
local chat = assert(require("louiselm.ui.chat").new(sessions, {
  skill_paths = { skill_root },
}))
```

Passing a pre-discovered `skills` array remains useful for an explicit static
catalog. The picker still rereads the selected `SKILL.md` at selection time.

Set `context.instructions_file` to attach the project's governing instructions
file (e.g. `AGENTS.md`) to a brand-new session's first prompt, as an ACP
`resource_link` — a name and URI, not the file body. The agent decides whether
to fetch it; this is a hint, not a guarantee. Empty (the default) disables it.
Resumed sessions never receive it, and it is sent at most once per session:

```lua
require("louiselm").setup({
  context = { instructions_file = "AGENTS.md" },
})
```

To use an existing `mini.nvim` checkout instead of `.deps/mini.nvim`:

```sh
MINI_NVIM_PATH=/path/to/mini.nvim nvim --headless --noplugin \
  -u "$PWD/tests/minimal_init.lua" -c 'lua MiniTest.run()' -c 'qa!'
```

## Dogfooding the skills workflow

Agent Skills specifies the `SKILL.md` format. ACP transports prompts and
advertised commands, but does not define skill discovery, invocation, or
filesystem access. Native skill loading and Codex-like catalog fallback are
adapter or host behavior.

`skills.policy` defaults to `native`. Choose a policy for an unfamiliar adapter
from its documented behavior:

- `native` when the adapter advertises that it discovers and invokes Agent
  Skills. LouiseLM sends no catalog or skill body automatically.
- `inject` when the adapter has no native integration but its agent can read
  the local absolute paths advertised by LouiseLM.
- `off` when neither mechanism is wanted or the agent cannot be trusted with
  configured skill paths. This also disables `:LouiselmPickSkill`.

Codex and Claude adapters should normally inherit `native`; custom DeepSeek or
GLM adapters can opt into `inject`. Paths stay global. An agent may override
only the policy:

```lua
require("louiselm").setup({
  agents = {
    codex = { command = "codex-acp", capabilities = { "image-generation" } },
    claude = { command = "claude-agent-acp" },
    deepseek = {
      command = "acp-llm-adapter",
      args = { "serve", "--backend", "deepseek" },
      skills = { policy = "inject" },
    },
    glm = {
      command = "acp-llm-adapter",
      args = { "serve", "--backend", "glm" },
      skills = { policy = "inject" },
    },
  },
  skills = {
    paths = { vim.fn.expand("~/.config/agentskills") },
    policy = "native",
  },
})
```

`capabilities` declares what an agent can do (e.g. `"image-generation"`); it is
optional, defaults to none, and is never inferred. LouiseLM only stores and
surfaces the declaration through `:checkhealth louiselm` — it does not read
beads or route work. Matching a capability string against a `needs-capability:*`
beads label is done by whatever is selecting work, per AGENTS.md.

Policies are copied into each session when it is created and never inferred
from the adapter name or changed by runtime events. Natural-language activation
in `native` belongs entirely to the adapter. In `inject`, LouiseLM sends one
bounded catalog for the first accepted prompt so the agent can choose a skill.
The picker is deliberate activation: native mode sends one matching advertised
command plus the task, while inject mode attaches the selected `SKILL.md` body
once. LouiseLM never combines native and injected activation, so one selection
cannot be double-sent. Explicit-only skills remain picker-visible but are never
placed in the implicit injected catalog. `off` does not intercept user-authored
slash prompts.

LouiseLM-managed local discovery requires the `lyaml` LibYAML binding so
`SKILL.md` metadata follows the Agent Skills YAML format instead of a
LouiseLM-specific subset. Install LibYAML and LuaRocks with the operating
system's package manager, then build `lyaml` for Neovim's Lua 5.1 ABI. Common
prerequisites are `libyaml-dev` plus `luarocks` on Debian/Ubuntu, or `libyaml`
plus `luarocks` with Homebrew on macOS:

```sh
luarocks --lua-version 5.1 install lyaml
```

A `SKILL.md` may declare which workflow phase its skill belongs to, as either a
bare phase name or a mapping with one primary and ordered secondary phases:

```yaml
phase:
  primary: qa
  secondary:
    - review
```

The canonical phases are `design`, `planning`, `implementation`, `review`, `qa`,
and `mechanical`. A skill that declares none has its phase inferred from
hyphen-separated segments of its own name at lower confidence, so `qa-review`
resolves to primary `qa` with secondary `review` whether or not it says so; a
name naming no phase, such as `grill-me`, yields no phase rather than a guess. A
declared phase outside the canonical list is an error and the skill is rejected,
because a phase with no built-in profile would rank candidates against nothing.

When a phase-tagged skill completes a turn, the Chat UI records the operational
outcome locally and offers `Good`, `Skip`, or `Poor` phase feedback; `Poor` asks
for a short context note. After feedback or dismissal, the idle Session gets a
short ranked recommendation picker. A recommendation is never opened during an
active turn. The picker always includes the current choice as safe `CONTINUE`,
followed by eligible Model changes and Handoffs with their action, reasons, and
confidence.

Approval is explicit and phase-scoped. `CONTINUE` leaves the Session unchanged;
a Model choice changes the advertised model option in that Session; a Handoff
creates a new Session and opens the reviewed transcript while retaining the
source Session. After selecting a recommendation, approve it or reject and
suppress it for the phase. Dismissing either picker makes no decision. The
coordinator can reconsider suppressed candidates and invalidate a phase when
fresh discovery changes the routing context.

The built-in profiles use existing Agent capability tags:

| Phase | Preferred capability | Minimum free context |
| --- | --- | ---: |
| design | `reasoning` | 50% |
| planning | `reasoning` | 40% |
| implementation | `coding` | 30% |
| review | `reasoning`, `coding` | 30% |
| qa | `coding` | 20% |
| mechanical | `speed` | 10% |

Skills without phase metadata do not trigger routing; LouiseLM does not guess a
workflow phase from the prompt. Evidence contains only bounded operational
outcomes, model/options, and optional phase-end feedback — never prompts,
transcripts, tool payloads, or environments. The routing estimator remains a
local pure boundary; process and UI execution happen only after approval.

The dependency is loaded only when configured local skill files need parsing.
Install it in Lua's standard search path. If LuaRocks reports it installed but
Neovim cannot find it, start Neovim from a POSIX shell with the LuaRocks paths:

```sh
eval "$(luarocks path --lua-version 5.1 --no-bin)"
nvim
```

The asdf Lua plugin configures its own Lua executable, while Neovim embeds a
separate LuaJIT runtime. For a persistent zsh setup, add this to `~/.zshrc`:

```zsh
if (( $+commands[luarocks] )); then
  eval "$(luarocks path --lua-version 5.1 --no-bin)"
fi
```

Restart the shell afterward. GUI launchers do not necessarily read `.zshrc`;
they must inherit the resulting `LUA_PATH` and `LUA_CPATH` from their launcher
or desktop session.

Missing `lyaml` does not affect sessions whose effective policy is `off` and
does not block native session startup. It prevents the managed local picker
from opening and blocks `inject` chat creation when an index is required.
LouiseLM never installs it automatically. The removed `skills.full_content`
setting is a configuration error; use the `inject` policy.

After setup, run `:checkhealth louiselm`. It reports each agent's effective
policy, dependency failures, the cwd used for relative roots, invalid metadata,
duplicate shadowing, invocation-metadata conflicts, line warnings, and symlink
escapes. If any agent uses `inject`, it also reports catalog bytes and the exact
skill names whose descriptions were shortened or whose entries were omitted.
It cannot verify whether an ACP agent can read local files; ACP exposes no such
capability.

### Manual adapter smoke checks

Start `nvim -u ./manual_init.lua` from the repository root and evaluate the
setup example above with installed commands and credentials.

For Codex and Claude, test each adapter separately:

1. Run `:LouiselmChat`, select the adapter, and wait for
   `skills=native` in the session header.
2. Submit a natural-language request that matches an adapter-installed skill
   and confirm that the adapter invokes it once.
3. Run `:LouiselmPickSkill`, choose a configured skill, submit a task, and
   confirm one advertised native command is sent. No injected catalog or
   selected `SKILL.md` body should appear in the transcript.

For a custom DeepSeek/GLM adapter, set its override to `inject`:

1. Confirm the header says `skills=inject`; in a fresh session, submit a normal
   prompt matching an implicitly invokable skill and confirm it is selected.
2. Pick an explicit-only skill and submit a task. Open the resulting context
   fold with `zo` and verify that exactly one selected `SKILL.md` body was sent.
3. Confirm the hidden `<available_skills>` catalog is invisible before
   submission and appears afterward only inside the closed context fold. Run
   `:checkhealth louiselm` to inspect its budget outcome.

The deterministic mock-adapter smoke for injected transport requires no
credentials:

```sh
nvim --headless --noplugin -u ./tests/minimal_init.lua \
  -c 'lua MiniTest.run_file("tests/ui/command_spec.lua")' -c 'qa!'
```

Installed-adapter checks are deliberately manual and are not CI requirements.
The default `ask-human` permission policy remains active during them.

## Skills

Discover Agent Skills metadata from configured directories and build a bounded
catalog when an agent does not provide native skill loading:

```lua
local skills = require("louiselm.skills")
local found, errors = skills.discover({ vim.fn.expand("~/.config/agentskills") })
local policy = assert(skills.policy("inject")) -- native, inject, or off
local catalog = assert(skills.inject(found))
```

Discovery validates the standard `name`, `description`, `license`,
`compatibility`, `metadata`, and `allowed-tools` fields. `allowed-tools` remains
metadata and never grants or bypasses LouiseLM ACP permissions. Skills marked
by `disable-model-invocation: true` or by
`agents/openai.yaml`'s `policy.allow_implicit_invocation: false` remain available
to the picker and are marked explicit-only for the policy layer. The injected
catalog uses a fixed instruction preamble and the `<available_skills>` XML
shape. It contains only each implicitly invokable skill's name, description,
and absolute configured-alias `SKILL.md` path. Its complete hidden block is
capped at 8,000 UTF-8 bytes:
descriptions are shortened fairly first, then a deterministic sorted tail is
omitted when minimum metadata cannot fit. `catalog.truncated` and
`catalog.omitted` expose both outcomes. This conservative budget and fallback
shape are inspired by Codex behavior; they are not part of Agent Skills or ACP.
Reconsider them if a future shared specification standardizes discovery or
skill transport.

The catalog is held locally and sent as the first hidden text block of the
first accepted non-slash prompt in a brand-new inject session. Slash commands
defer it, and resumed or externally attached sessions never receive a guessed
catalog; start a new session when catalog provenance is uncertain.
Deliberately picked skills remain available even when explicit-only and attach
their exact selected `SKILL.md` body once. Use
`skills.overlap(native_skill_dir, configured_paths)` to detect
native/configured skill trees that resolve to the same directory.

Implicit injected activation gives the agent only an absolute path, so it
requires agent-side file access and reveals the local directory layout to that
agent. A picker-attached, instruction-only skill can work without agent-side
file access because LouiseLM sends its body. Any referenced scripts,
references, or assets still require suitable agent tools. `allowed-tools` is
advisory metadata and never expands LouiseLM permissions. Neither ACP nor
`:checkhealth` can prove those tools or filesystem access exist.

Configured roots are processed in order; valid skills within each root are
ordered by name and configured path. The first valid duplicate name wins.
Canonical file and directory aliases are traversed once, while advertised
paths retain the configured root alias. Canonical aliases, cycles, and symlinks
outside a root remain usable but produce at most one layout warning per root.
`.git`, `target`, `node_modules`, and `_build` subtrees are pruned unless that
directory itself contains a `SKILL.md`. `~` expands normally, and relative roots
use the session working directory (`:checkhealth louiselm` instead uses and
reports Neovim's current working directory). Configuring a root trusts its skill
folders and any followed symlink targets; a relative root also trusts the
active workspace to supply that path. LouiseLM does not add project roots
automatically and has no trust database.

## Licence

LouiseLM is licensed under the [MIT License](LICENSE). MIT was chosen to serve
reach: the project's usefulness depends on being installable and adaptable
anywhere without licensing friction.

If reach stops being the project's binding constraint, the licence choice for
future versions will be revisited; versions already released keep their
existing terms. The separate [trademark policy](TRADEMARK_POLICY.md) governs
use of the LouiseLM name, and the [political statement](POLITICAL_STATEMENT.md)
explains the position behind this permissive posture.
