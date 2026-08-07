# LouiseLM

![People, ideas, tools, and agents connected through a red-and-black decentralized mesh](assets/louiselm-hero.webp)

**LouiseLM Organizes Unruled Intelligent Systems into Emancipated Language
Meshes.**

> LouiseLM turns ideas into software.

Ideas arrive wild and unordered. LouiseLM gives them enough structure to become
real without forcing them into one prescribed path: chaos with order, directed
by the person whose idea started it.

The acronym is recursive, and so is the project. LouiseLM is being used to build
LouiseLM: its own development is the first test of the experience it wants to
provide.

> [!IMPORTANT]
> LouiseLM is alpha software. Today it provides an ACP-backed Neovim chat and a
> headless session foundation. The complete idea-to-software loop described
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

## From an idea to software

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
default is a useful starting point, not a definition of the only correct way to
work.

Ideas are personal data, so their storage should remain local and user-owned by
default. Cloud models, synchronization, and remote services are choices rather
than requirements.

Neovim is LouiseLM's deliberate home: a programmable cockpit for thinking,
observing, and intervening, as well as a runtime for headless automation.
Future voice, mobile, or dedicated capture devices may feed the workflow, but
none is required and no editor-independent service is assumed.

LouiseLM is focused on the concrete promise of turning ideas into software. The
same machinery might eventually help other kinds of ideas become outcomes, but
it is not trying to become a general notes application or second brain.

## One possible workflow

One workflow already works manually and is beginning to move into LouiseLM:

1. Use a strong deliberative model and the `grill-me` skill to interrogate an
   idea until the user and agent share an understanding.
2. Turn the agreement into a plan, then use the `to-beads` skills to split it
   into epics, features, and independently executable tasks.
3. Use `br`, `bv`, or `bvr` to inspect the dependency graph, recommend the next
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
It also registers `:LouiselmChat`. After `louiselm.setup()` the command uses
the configured agents and skills; before setup it falls back to the default
launcher described below.

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
local sessions = assert(require("louiselm.session").new({
  claude = { command = "claude-agent-acp", args = {} },
}))

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
Call `chat:dispose()` to remove its buffers and event listeners; it does not
dispose the sessions it displays.

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

When an ask-human request is a file edit with a diff or replacement content,
the chat UI opens a read-only `louiselm-diff://` buffer before responding. Press
`a` to allow the edit, or `d`/`q` to reject it. The review records the file
content it displayed and rejects a local apply if the file changed meanwhile;
the ACP agent remains responsible for writing the file after approval.

Command and unknown permission requests use a chat picker for the ACP options;
dismissing the picker sends a cancellation response. Permission UI work is
scheduled onto Neovim's main loop, and late choices after chat disposal are
ignored.

## Chat context

Context providers stay thin: they point the agent at files and skills while
visual selections carry their selected text. Queue context on an attached chat
view before submitting its prompt:

```lua
chat:mention_buffer()
chat:send_selection()
chat:pick_file()
chat:pick_skill()
```

Pass discovered skills to the chat controller to enable the skill picker:

```lua
local found, errors = require("louiselm.skills").discover({ skill_root })
assert(#errors == 0, "skill discovery failed")
local chat = assert(require("louiselm.ui.chat").new(sessions, { skills = found }))
```

To use an existing `mini.nvim` checkout instead of `.deps/mini.nvim`:

```sh
MINI_NVIM_PATH=/path/to/mini.nvim nvim --headless --noplugin \
  -u "$PWD/tests/minimal_init.lua" -c 'lua MiniTest.run()' -c 'qa!'
```

## Dogfooding the skills workflow

The reproducible manual recipe for the current P1 workflow can construct the
session and chat controllers explicitly. The canonical `:LouiselmChat` command
also consumes configured `agents` (or the singular `agent` compatibility shape)
and `skills.paths` after setup. Set `skills.policy` to `inject` to queue the
metadata-only skill index for each new chat session; `native` leaves loading to
the agent, and `off` disables the skill picker and index.

Run `nvim -u ./manual_init.lua`, then evaluate this setup from the repository
root (replace the agent command if a different ACP launcher is intended):

```lua
local Session = require("louiselm.session")
local Chat = require("louiselm.ui.chat")
local Context = require("louiselm.ui.context")
local Skills = require("louiselm.skills")

local skill_root = vim.fn.expand("~/.config/agentskills")
local found, discovery_errors = Skills.discover({ skill_root })
assert(#found > 0, "no skills discovered")
local skill_index = assert(Skills.inject(found))
local grill_me
for _, skill in ipairs(found) do
  if skill.name == "grill-me" then
    grill_me = skill
    break
  end
end
assert(grill_me, "grill-me skill not discovered")
for _, discovery_error in ipairs(discovery_errors) do
  vim.notify(discovery_error.path .. ": " .. discovery_error.message, vim.log.levels.WARN)
end
local sessions = assert(Session.new({
  claude = { command = "claude-agent-acp", args = {} },
}))
local chat = assert(Chat.new(sessions, { agents = { "claude" }, skills = found }))

assert(sessions:create_session("claude", { cwd = vim.fn.getcwd() }, function(session, err)
  vim.schedule(function()
    assert(err == nil, err)
    assert(chat:attach(session))
    assert(chat:queue_context({ label = "skill-index", text = skill_index }))
    assert(chat:queue_context(Context.skills.context(grill_me)))
    assert(chat:submit("Start the grill-me workflow for this project."))
  end)
end))
```

Continue the conversation in the chat buffer. After agreement, send
`/to-beads` and let the agent route to the appropriate `to-beads-*` skill. The
default `ask-human` permission policy remains active; file edits open the diff
review, while command and unknown permission requests currently require a
custom event consumer to answer them.

This recipe was exercised against `claude-agent-acp` 0.64.2. LouiseLM
initialized the real ACP session, the injected index exposed `grill-me`,
`to-beads`, `to-beads-epic`, `to-beads-feature`, and `to-beads-tasks`, and a
bounded chat prompt completed successfully. The configured skill tree also
contains two `.system` skills whose nested `metadata` frontmatter is currently
reported as malformed; the workflow skills themselves discover successfully.

Decision: continue P1 dogfooding and defer P2 comfort work. The session and
manual chat path are viable, but the canonical command and permission UI gaps
must be resolved before treating the workflow as a daily-driver exit criterion.

## Skills

Discover Agent Skills metadata from configured directories and inject a thin
index when an agent does not provide native skill loading:

```lua
local skills = require("louiselm.skills")
local found, errors = skills.discover({ vim.fn.expand("~/.config/agentskills") })
local policy = assert(skills.policy("inject")) -- native, inject, or off
local index = assert(skills.inject(found))
```

The injected index contains only each skill's name, description, and
`SKILL.md` path. Use `skills.overlap(native_skill_dir, configured_paths)` to
detect native/configured skill trees that resolve to the same directory.
