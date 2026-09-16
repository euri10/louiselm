# LouiseLM onboarding

This guide gets a first-time Neovim user from zero to a working LouiseLM chat.
LouiseLM does not install Agents or create credentials for you. Choose an
Agent, install it using its own documentation, authenticate it, then point
LouiseLM at the executable.

If you want to explore Sessions, permission review, Resume, limits, and
Handoff before installing anything, [try LouiseLM in your
browser](https://louiselm.com/demo/). Its Agent behavior is scripted and no
Provider is connected.

The shortest path is Codex. The included quickstart tracks LouiseLM `main`, so
it is intended for this alpha project's current onboarding flow.

## Prerequisites

You need:

- Neovim 0.12 or newer.
- Git and network access for the first plugin download.
- One installed and authenticated ACP Agent.

The quickstart does not modify your normal Neovim configuration. It downloads
one Lua file and starts Neovim with that file as its configuration.

## Quick start: Codex

If `codex-acp` is already available, keep it and continue. Otherwise install it
by following its [official installation instructions](https://github.com/agentclientprotocol/codex-acp#installation).
Its default login is the provider-managed ChatGPT login; the adapter also
documents `CODEX_API_KEY`/`OPENAI_API_KEY` authentication as alternatives.
Complete one of those authentication paths before starting LouiseLM.

Download the quickstart file:

```sh
curl -fL https://raw.githubusercontent.com/euri10/louiselm/main/examples/quickstart.lua \
  -o quickstart.lua
```

Start the isolated onboarding Neovim:

```sh
nvim --clean -u quickstart.lua
```

Then run:

```vim
:checkhealth louiselm
:LouiselmTutor
:LouiselmChat
```

`:LouiselmTutor` is the in-editor tour. It explains the first prompt,
permissions, context, cancellation, and Sessions without requiring an Agent
to be configured. `:LouiselmChat` opens the actual chat after the health check
shows that `codex-acp` is available.

If downloading a file is inconvenient, create `quickstart.lua` and paste this
minimal version into it:

```lua
local nvim = vim

nvim.pack.add({
  {
    src = "https://github.com/euri10/louiselm.git",
    name = "louiselm.nvim",
  },
}, { confirm = false })

assert(require("louiselm").setup({
  agents = { codex = { command = "codex-acp", provider = "OpenAI" } },
}))
```

## Choosing another Agent

The quickstart keeps Codex enabled so a newly downloaded file has one known
default. To use another provider, install and authenticate it first, then
replace the `codex` entry in `quickstart.lua` with one of the profiles below.

### Claude Agent ACP: API key path

The official adapter package currently requires Node.js 22 or newer and
installs the `claude-agent-acp` executable:

```sh
npm install --global @agentclientprotocol/claude-agent-acp
export ANTHROPIC_API_KEY=your-key
```

Use this LouiseLM profile:

```lua
agents = {
  claude = { command = "claude-agent-acp", provider = "Anthropic" },
}
```

The key is read from the environment inherited by the adapter; it is not put
in the Lua file. See Anthropic's
[authentication documentation](https://platform.claude.com/docs/en/manage-claude/authentication)
for key handling and the adapter's
[package metadata](https://raw.githubusercontent.com/agentclientprotocol/claude-agent-acp/main/package.json)
for the executable and Node requirement.

The adapter also advertises Claude subscription and Console login flows through
ACP terminal authentication. LouiseLM does not currently advertise the ACP
terminal-auth client capability, so those flows are not presented as a
working LouiseLM setup recipe. Use the API-key path above until that client
capability is implemented.

### DeepSeek through `acp-llm-adapter`

The official adapter documents a Rust installation and a DeepSeek backend:

```sh
cargo install acp-llm-adapter
export DEEPSEEK_API_KEY=your-key
```

Use this LouiseLM profile:

```lua
local nvim = vim

agents = {
  deepseek = {
    provider = "DeepSeek",
    command = "acp-llm-adapter",
    args = { "serve", "--backend", "deepseek" },
    env = { LLM_API_KEY = assert(nvim.env.DEEPSEEK_API_KEY) },
  },
}
```

The adapter maps `DEEPSEEK_API_KEY` to the `LLM_API_KEY` it consumes. See its
[official installation and editor setup](https://github.com/euri10/acp-llm-adapter#editor-setup)
for the current command, backend name, and requirements.

### Google Antigravity

Use Google's **Antigravity ACP server**, distributed through the
[ACP registry](https://github.com/agentclientprotocol/registry/blob/main/antigravity-acp/agent.json).
Download the archive for your platform and extract both files together:
`agy_acp_server.par` and `localharness_external` on Linux. The server is a
separate distribution from the `agy` terminal application.

For Linux x86-64, the checked version is 1.1.1:

```sh
curl -fL https://dl.google.com/agy-extensions/releases/linux/agy-acp-server-agy_acp_server_1.1.1-linux-x86_64.zip \
  -o antigravity-acp.zip
mkdir -p ~/.local/share/antigravity-acp/1.1.1
unzip -n antigravity-acp.zip -d ~/.local/share/antigravity-acp/1.1.1
chmod u+x ~/.local/share/antigravity-acp/1.1.1/agy_acp_server.par \
  ~/.local/share/antigravity-acp/1.1.1/localharness_external
```

Use this profile; Linux requires the empty `--uid=` argument listed in the
registry. Other platforms use their registry entry's executable and arguments.

```lua
agents = {
  antigravity = {
    provider = "Google Antigravity",
    command = vim.fn.expand("~/.local/share/antigravity-acp/1.1.1/agy_acp_server.par"),
    args = { "--uid=" },
  },
}
```

For personal Google-account access, set the server's own
`~/.gemini/antigravity-acp/settings.json` to the following, preserving any
other existing settings:

```json
{ "auth": { "type": "oauth-personal" } }
```

The `.gemini` directory is Google's storage location for Antigravity. This
setting selects the login method; it contains no credentials. The server
opens browser sign-in when it needs authentication, then caches credentials
locally. See Google's [Antigravity authentication guidance](https://antigravity.google/docs/ide/extensions#authentication--licensing)
for account eligibility and plans.

First login can exceed LouiseLM's normal 20-second startup deadline. In an
editor with LouiseLM loaded, run this one-time command with a three-minute
deadline and complete the browser sign-in:

```vim
lua << EOF
local api = assert(require("louiselm.session").new({
  antigravity = {
    provider = "Google Antigravity",
    command = vim.fn.expand("~/.local/share/antigravity-acp/1.1.1/agy_acp_server.par"),
    args = { "--uid=" },
  },
}))
local session, err = api:create_session("antigravity", { start_timeout_ms = 180000 }, function(_, failure)
  api:dispose()
  vim.schedule(function() vim.notify(failure or "Antigravity sign-in completed") end)
end)
if not session then api:dispose(); error(err) end
EOF
```

Then select `antigravity` with `:LouiselmSessionNew` and check its executable
with `:checkhealth louiselm`. The server advertises session loading/listing,
image/audio prompts, and embedded context. A successful handshake alone does
not verify authentication, tool permissions, or cancellation.

Checked with Antigravity ACP server 1.1.1 on 2026-09-16: authenticated text
response, a bounded command with allow-once permission, cancellation, session
loading, and disposal through LouiseLM's headless Session API. Image/audio
support is advertised but not tested. Keep permission choices explicit in the
Agent's supported controls and LouiseLM policy; this profile does not force
auto-approval.

## If the quickstart cannot download

The quickstart needs network access because `vim.pack` fetches LouiseLM on its
first run. In a restricted environment, clone or otherwise place LouiseLM in a
local Neovim package directory using your normal system or plugin-management
procedure, then use the same `require("louiselm").setup` block from the
copy/paste example. Agent installation and authentication remain separate
steps handled by each Agent's documentation.

## After the first run

Once the isolated quickstart works, move the setup into your normal Neovim
configuration or plugin manager. A minimal existing-configuration setup is:

```lua
vim.pack.add({
  {
    src = "https://github.com/euri10/louiselm.git",
    name = "louiselm.nvim",
  },
})

require("louiselm").setup({
  agents = { codex = { command = "codex-acp", provider = "OpenAI" } },
})
```

Keep any Agent environment setup outside the Lua file where practical, and
use:

```vim
:checkhealth louiselm
:help louiselm
```

Verification date for the Codex, Claude, and DeepSeek recipes: 2026-08-28. Provider
installers, executable names, authentication flows, and links can change while
LouiseLM and these adapters are alpha software.
