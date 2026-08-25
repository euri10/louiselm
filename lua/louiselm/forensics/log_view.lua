---Read-only, non-mutating inspection of ACP JSON-RPC log files (the traffic
---logs `acp-llm-adapter` and `acp-proxy` write to disk, one message per line).
---
---A raw line is often too long to read -- a `session/update` chunk or tool-call
---payload routinely runs to hundreds of characters. `M.enable` turns every line
---into a closed, one-line Neovim fold whose foldtext is a compact human summary;
---opening a fold (`zo`) reveals the exact untouched raw line. The buffer is never
---edited: this is display-only, driven by 'foldmethod'/'foldexpr'/'foldtext'.
---
---Delegating fold *boundaries* to the jsonl/json treesitter grammar's own fold
---query was tried and does not apply here: every ACP JSON-RPC record is exactly
---one physical line, and Neovim's fold model only ever folds a node whose start
---and end line differ -- a same-line node never registers as a fold. Forcing a
---fold boundary at every line via `M.foldexpr` is the only way to get this
---one-fold-per-record shape.

local Protocol = require("louiselm.acp.protocol")

local M = {}

---@diagnostic disable-next-line: undefined-global -- `vim` is Neovim's injected runtime API.
local nvim = vim

---Longest a rendered summary line may run, in characters. A `foldtext` line
---has no wrapping, so an unbounded summary would overflow the window width;
---the original payload is always one `zo` away in full.
local LINE_LIMIT = 180

---@param value string
---@return string collapsed
local function single_line(value)
  local lines = nvim.split(value, "\n", { plain = true })
  for index, one_line in ipairs(lines) do
    lines[index] = table.concat(nvim.split(one_line, "\r", { plain = true }), "")
  end
  return nvim.trim(table.concat(lines, " "))
end

---@param value string
---@return string shortened
local function truncate(value)
  if nvim.fn.strchars(value) <= LINE_LIMIT then
    return value
  end
  return nvim.fn.strcharpart(value, 0, LINE_LIMIT) .. "…"
end

---@param value unknown
---@return string
local function preview(value)
  if type(value) == "string" then
    return truncate(single_line(value))
  end
  local ok, encoded = pcall(nvim.json.encode, value)
  if not ok then
    return "?"
  end
  return truncate(single_line(encoded))
end

---@param value unknown
---@return string? text
local function chunk_text(value)
  if type(value) ~= "table" then
    return nil
  end
  if type(value.content) == "table" and type(value.content.text) == "string" then
    return value.content.text
  end
  return nil
end

---@param update table
---@return string
local function summarize_update(update)
  local kind = update.sessionUpdate
  if kind == "agent_message_chunk" then
    return "» " .. preview(chunk_text(update) or "")
  end
  if kind == "agent_thought_chunk" then
    return "… " .. preview(chunk_text(update) or "")
  end
  if kind == "user_message_chunk" then
    return "« " .. preview(chunk_text(update) or "")
  end
  if kind == "tool_call" or kind == "tool_call_update" then
    local title = type(update.title) == "string" and update.title or tostring(update.toolCallId or "tool")
    local status = type(update.status) == "string" and update.status or "?"
    return "⚙ " .. preview(title) .. " [" .. status .. "]"
  end
  if kind == "available_commands_update" then
    local commands = update.availableCommands
    local count = type(commands) == "table" and #commands or 0
    return "commands (" .. count .. ")"
  end
  return preview(kind) .. " " .. preview(update)
end

---Render one decoded JSON-RPC message as a single human-readable line.
---@param message unknown
---@return string
function M.summarize(message)
  if type(message) ~= "table" then
    return preview(message)
  end
  if type(message.method) == "string" then
    if
      message.method == "session/update"
      and type(message.params) == "table"
      and type(message.params.update) == "table"
    then
      return summarize_update(message.params.update)
    end
    local direction = message.id ~= nil and ("→ #" .. tostring(message.id) .. " ") or "· "
    return direction .. message.method .. " " .. preview(message.params)
  end
  if message.error ~= nil then
    local rpc_error = message.error
    local code = type(rpc_error) == "table" and rpc_error.code or "?"
    local text = type(rpc_error) == "table" and rpc_error.message or "?"
    return "← #" .. tostring(message.id) .. " ERROR " .. tostring(code) .. " " .. preview(text)
  end
  return "← #" .. tostring(message.id) .. " " .. preview(message.result)
end

---Decode and summarize one raw JSON-RPC log line. A line that fails to decode
---(a different log format, a partially-written line) still renders -- a bounded
---preview of the raw text plus the decode error, never a crash.
---@param raw_line string
---@return string rendered
function M.render_line(raw_line)
  if raw_line == "" then
    return ""
  end
  local message, decode_error = Protocol.decode(raw_line)
  if message == nil then
    return preview(raw_line) .. "  [" .. (decode_error or "undecodable") .. "]"
  end
  return M.summarize(message)
end

---Per-window fold options this module overrides, saved so `M.disable` can
---restore exactly what was there before -- never assumed to be Neovim defaults,
---since the window may already have its own fold setup.
---@type table<integer, table<string, unknown>>
local saved_options = {}

local FOLD_OPTIONS = { "foldmethod", "foldexpr", "foldtext", "foldenable", "foldlevel", "foldminlines" }

---'foldexpr' callback: force every line to start its own fold, so a fold never
---spans more than one raw JSON-RPC line.
---@return string
function M.foldexpr()
  return ">1"
end

---Rendered summary per buffer per line, keyed against the exact raw line it
---was computed from. `foldtext` runs on every redraw of every visible closed
---fold -- without this, decoding and re-summarizing a large tool-call payload
---on every scroll or `za` is what makes folding feel sluggish on a real log.
---@type table<integer, table<integer, {raw: string, rendered: string}>>
local render_cache = {}

---'foldtext' callback: render the closed fold's one line as its human summary.
---@return string
function M.foldtext()
  local buffer = nvim.api.nvim_get_current_buf()
  local lnum = nvim.v.foldstart
  local raw_line = nvim.fn.getline(lnum)

  local buffer_cache = render_cache[buffer]
  if buffer_cache == nil then
    buffer_cache = {}
    render_cache[buffer] = buffer_cache
  end
  local cached = buffer_cache[lnum]
  if cached ~= nil and cached.raw == raw_line then
    return cached.rendered
  end

  local ok, rendered = pcall(M.render_line, raw_line)
  if not ok then
    rendered = raw_line
  end
  buffer_cache[lnum] = { raw = raw_line, rendered = rendered }
  return rendered
end

---@param win integer
---@return boolean
function M.is_enabled(win)
  return saved_options[win] ~= nil
end

---Turn on folded rendering for one window. A no-op if already enabled.
---@param win? integer Defaults to the current window.
function M.enable(win)
  win = win or nvim.api.nvim_get_current_win()
  if saved_options[win] ~= nil then
    return
  end
  local previous = {}
  for _, option in ipairs(FOLD_OPTIONS) do
    previous[option] = nvim.wo[win][option]
  end
  saved_options[win] = previous

  -- foldminlines defaults to 1, which keeps single-line folds open -- the
  -- opposite of what a one-fold-per-line renderer needs.
  nvim.wo[win].foldminlines = 0
  nvim.wo[win].foldmethod = "expr"
  nvim.wo[win].foldexpr = "v:lua.require'louiselm.forensics.log_view'.foldexpr()"
  nvim.wo[win].foldtext = "v:lua.require'louiselm.forensics.log_view'.foldtext()"
  nvim.wo[win].foldenable = true
  nvim.wo[win].foldlevel = 0
end

---Restore the window's fold options to what they were before `M.enable`. A
---no-op if not enabled.
---@param win? integer Defaults to the current window.
function M.disable(win)
  win = win or nvim.api.nvim_get_current_win()
  local previous = saved_options[win]
  if previous == nil then
    return
  end
  for _, option in ipairs(FOLD_OPTIONS) do
    nvim.wo[win][option] = previous[option]
  end
  saved_options[win] = nil
end

---@param win? integer Defaults to the current window.
function M.toggle(win)
  win = win or nvim.api.nvim_get_current_win()
  if M.is_enabled(win) then
    M.disable(win)
  else
    M.enable(win)
  end
end

return M
