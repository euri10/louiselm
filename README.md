# LouiseLM

![People, ideas, tools, and agents connected through a red-and-black decentralized mesh](assets/louiselm-hero.webp)

**LouiseLM Organizes Unruled Intelligent Systems into Emancipated Language
Meshes.**

**[Try LouiseLM in your browser](/demo/)** — no installation, Agent, Provider,
credentials, or project files required. The guided Agent behavior is clearly
labelled and scripted; the Neovim and LouiseLM UI are real.

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

1. Use a strong deliberative model and the `louiselm-grill-me` skill to
   interrogate an idea until the user and agent share an understanding.
2. Turn the agreement into a plan, then use the `louiselm-to-beads` skills to
   split it into epics, features, and independently executable tasks.
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

## Reference

After installing the plugin, use `:help louiselm` for the generated command
and configuration reference. `:checkhealth louiselm` reports the active
environment's configuration and tool status; it is troubleshooting evidence,
not a replacement for the reference. For the headless Session API, typed
events, permission policies, and Agent configuration used when embedding
LouiseLM in your own tooling, see the generated
[API appendix](doc/api.md); `AGENTS.md` remains the sole Agent-facing (AX)
contract.

## Development

Install the pinned MyST CLI and build the public landing page, documentation,
API appendix, and blog as one strictly checked static artifact:

```sh
npm ci
npm run site:build
```

The deployable directory is `_build/html`. The build accepts only the pages
listed in `myst.yml`; Markdown downloads must match those curated sources
byte-for-byte. It rejects conversation exports, unlisted Markdown sources,
state, plans, and environment files before that directory can be published.

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

Regenerate the committed Vimdoc after changing configuration or user commands:

```sh
./scripts/generate-vimdoc
./scripts/generate-vimdoc --check
```

Regenerate the committed API appendix after changing LuaCATS annotations in
the headless Session API, typed events, permission policies, or Agent
configuration (requires `lua-language-server` on `PATH`):

```sh
./scripts/generate-api-appendix
./scripts/generate-api-appendix --check
```

`lua/louiselm/types.lua` is generated from the configuration schema and ships
with the plugin, so `lua-language-server` completes and type-checks the table
you pass to `setup({...})`. Regenerate it after any change to `config.lua`:

```sh
./scripts/generate-luacats
./scripts/generate-luacats --check
```

For interactive debugging, start `nvim -u ./tests/minimal_init.lua` and run
`:lua MiniTest.run()`. Without `--headless`, Neovim intentionally stays open.

Run `:checkhealth louiselm` after setup to revalidate the configuration and
check configured agent executables, versions, and skill paths. Agent version
results arrive asynchronously because external processes must not block the
editor; unavailable executables and invalid skill paths are reported directly
in the health buffer.

## Quick start

The fastest first run uses Codex as the default Agent. LouiseLM does not
install the Agent or create credentials; install and authenticate `codex-acp`
using its [official instructions](https://github.com/agentclientprotocol/codex-acp#installation),
then download the alpha quickstart:

```sh
curl -fL https://raw.githubusercontent.com/euri10/louiselm/main/examples/quickstart.lua \
  -o quickstart.lua
nvim --clean -u quickstart.lua
```

Inside Neovim, run the health check, read the short in-editor lesson, and open
your first chat. The lesson is also available as
[docs/tutorial.md](docs/tutorial.md):

```vim
:checkhealth louiselm
:LouiselmTutor
:LouiselmChat
```

The quickstart tracks `main` because LouiseLM is alpha software. Use plain
`nvim -u quickstart.lua` if you intentionally want your normal configuration
loaded as well. If you need another Agent, cannot use the download path, or
want to move the setup into your normal configuration, see
[docs/onboarding.md](docs/onboarding.md). The complete command and
configuration reference is available with `:help louiselm`.

## Licence

LouiseLM is licensed under the [MIT License](LICENSE). MIT was chosen to serve
reach: the project's usefulness depends on being installable and adaptable
anywhere without licensing friction.

If reach stops being the project's binding constraint, the licence choice for
future versions will be revisited; versions already released keep their
existing terms. The separate [trademark policy](TRADEMARK_POLICY.md) governs
use of the LouiseLM name, and the [political statement](POLITICAL_STATEMENT.md)
explains the position behind this permissive posture.
