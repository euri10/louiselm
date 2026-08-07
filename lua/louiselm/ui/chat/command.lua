---@diagnostic disable-next-line: undefined-global -- `vim` is Neovim's injected runtime API.
local nvim = vim

local M = {}

local DEFAULT_ADAPTER_DEBUG_SCRIPT = "/home/lotso/code/acp-llm-adapter/acp-debug.sh"

---@return louiselm.agent.Definition definition
local function default_agent_definition()
  local command = nvim.env.LOUISELM_AGENT_COMMAND
  if command ~= nil and command ~= "" then
    return { command = command, args = {} }
  end

  local environment
  local api_key = nvim.env.DEEPSEEK_API_KEY
  if api_key ~= nil and api_key ~= "" then
    environment = { LLM_API_KEY = api_key }
  end
  return {
    command = DEFAULT_ADAPTER_DEBUG_SCRIPT,
    args = { "acp-llm-adapter", "serve", "--backend", "deepseek" },
    env = environment,
  }
end

---Register the interactive chat command.
---@return boolean registered Always true after the command is registered.
function M.register()
  local chat

  nvim.api.nvim_create_user_command("LouiselmChat", function()
    if chat ~= nil and chat:buffer() ~= nil then
      nvim.api.nvim_set_current_buf(chat:buffer())
      return
    end

    local sessions, session_errors = require("louiselm.session").new({
      default = default_agent_definition(),
    })
    if sessions == nil then
      nvim.notify("louiselm: invalid agent configuration (" .. #session_errors .. " errors)", nvim.log.levels.ERROR)
      return
    end
    chat = assert(require("louiselm.ui.chat").new(sessions, { agents = { "default" } }))
    local _, session_error = chat:new_session()
    if session_error ~= nil then
      nvim.notify("louiselm: " .. session_error, nvim.log.levels.ERROR)
    end
  end, { desc = "Open the louiselm chat buffer", force = true })
  return true
end

return M
