---@diagnostic disable-next-line: undefined-global -- `vim` is Neovim's injected runtime API.
local nvim = vim
local project_root = nvim.fn.getcwd()

nvim.pack.add({
  {
    src = project_root,
    name = "louiselm.nvim",
  },
}, { confirm = false })
nvim.opt.rtp:prepend(project_root)

local deepseek_env = { ACP_LOG = "1" }
if nvim.env.DEEPSEEK_API_KEY ~= nil and nvim.env.DEEPSEEK_API_KEY ~= "" then
  deepseek_env.LLM_API_KEY = nvim.env.DEEPSEEK_API_KEY
end

assert(require("louiselm").setup({
  agents = {
    claude = {
      provider = "Anthropic",
      command = "acp-proxy",
      args = { "--", "claude-agent-acp" },
      version = { command = "claude-agent-acp", args = { "--version" } },
    },
    deepseek = {
      provider = "DeepSeek",
      command = "acp-llm-adapter",
      args = { "serve", "--backend", "deepseek" },
      env = deepseek_env,
      version = { command = "acp-llm-adapter", args = { "--version" } },
    },
  },
}))
