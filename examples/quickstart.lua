---@diagnostic disable-next-line: undefined-global -- `vim` is Neovim's injected runtime API.
local nvim = vim

nvim.pack.add({
  {
    src = "https://github.com/euri10/louiselm.git",
    name = "louiselm.nvim",
  },
}, { confirm = false })

assert(require("louiselm").setup({
  agents = {
    codex = {
      command = "codex-acp",
    },

    -- Optional Claude Agent ACP profile.
    -- Install with Node.js 22+: npm install --global @agentclientprotocol/claude-agent-acp
    -- Authenticate with an environment variable before launching Neovim:
    --   export ANTHROPIC_API_KEY=...
    -- claude = { command = "claude-agent-acp" },

    -- Optional DeepSeek profile.
    -- Install with: cargo install acp-llm-adapter
    -- Authenticate before launching Neovim:
    --   export DEEPSEEK_API_KEY=...
    -- deepseek = {
    --   command = "acp-llm-adapter",
    --   args = { "serve", "--backend", "deepseek" },
    --   env = { LLM_API_KEY = assert(nvim.env.DEEPSEEK_API_KEY) },
    -- },
  },
}))

-- Optional provider commands and environment mappings were verified against
-- upstream sources on 2026-08-28. See docs/onboarding.md for links and limits.
