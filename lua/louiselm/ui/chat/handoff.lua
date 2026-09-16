local Transcript = require("louiselm.session.transcript")

---@diagnostic disable-next-line: undefined-global -- `vim` is Neovim's injected runtime API.
local nvim = vim

---@class louiselm.ui.Handoff
---@field target_session louiselm.session.Session
---@field target_session_id string
---@field source_session_id? string Durable source Session identity.
---@field source_id string Local source Session id.

---@class louiselm.ui.HandoffOptions
---@field submit fun(review: louiselm.ui.Handoff, text: string, content: string|table[]): boolean, string? Submit through the target's draft and accepted-render boundaries.
---@field focus fun(session_id: string) Focus an attached target after closing a review.

---@class louiselm.ui.Handoffs
---@field options louiselm.ui.HandoffOptions
---@field reviews table<integer, louiselm.ui.Handoff>
---@field disposed boolean
local Handoffs = {}
Handoffs.__index = Handoffs
local M = {}

---Construct a review-buffer owner without creating editor resources.
---@param options louiselm.ui.HandoffOptions
---@return louiselm.ui.Handoffs handoffs
function M.new(options)
  return setmetatable({ options = options, reviews = {}, disposed = false }, Handoffs)
end

---@param self louiselm.ui.Handoffs
---@param buffer integer
local function close_handoff(self, buffer)
  self.reviews[buffer] = nil
  if nvim.api.nvim_buf_is_valid(buffer) then
    nvim.api.nvim_buf_delete(buffer, { force = true })
  end
end

---Placeholder marking an unfilled takeover task in a Handoff review buffer;
---`submit_handoff` refuses to send while this exact text is still present.
local HANDOFF_TASK_PLACEHOLDER = "<replace with the concrete action the target must take>"

---Read the filled takeover task from a Handoff review buffer's contents, or nil
---when the line is missing, blank, or still holds the template placeholder.
---@param text string
---@return string? task
local function takeover_task(text)
  for line in text:gmatch("[^\n]+") do
    local value = line:match("^%s*%-%s*takeover task:%s*(.-)%s*$")
    if value ~= nil then
      if value == "" or value:find(HANDOFF_TASK_PLACEHOLDER, 1, true) ~= nil then
        return nil
      end
      return value
    end
  end
  return nil
end

---Open a Handoff review buffer for another session: an editable three-section
---brief — a `## Handoff` takeover-task template first, the compacted source
---transcript as `## Context`, and `## Source` metadata — that `submit_handoff`
---validates and sends.
---@param self louiselm.ui.Handoffs
---@param target_session louiselm.session.Session Session that will receive the reviewed prompt.
---@param source_state louiselm.session.State Attached source Session snapshot.
---@param entries louiselm.session.TranscriptEntry[] Source transcript snapshot.
---@return integer? buffer
---@return string? error_message Validation or session state error.
function Handoffs:open(target_session, source_state, entries)
  if self.disposed then
    return nil, "Handoff reviews are disposed"
  end
  if
    type(target_session) ~= "table"
    or type(target_session.inspect) ~= "function"
    or type(target_session.prompt) ~= "function"
  then
    return nil, "handoff requires a target session"
  end
  local source_id = source_state.id
  local target_state = target_session:inspect()
  if type(target_state) ~= "table" or type(target_state.id) ~= "string" then
    return nil, "target session has invalid state"
  end
  if target_state.id == source_id then
    return nil, "handoff target must differ from source session"
  end

  local buffer = nvim.api.nvim_create_buf(false, true)
  nvim.api.nvim_buf_set_name(buffer, "louiselm://handoff-" .. source_id .. "-" .. target_state.id)
  nvim.api.nvim_set_option_value("buftype", "nofile", { buf = buffer })
  nvim.api.nvim_set_option_value("bufhidden", "wipe", { buf = buffer })
  nvim.api.nvim_set_option_value("swapfile", false, { buf = buffer })
  nvim.api.nvim_set_option_value("filetype", "markdown", { buf = buffer })
  local user_turns = 0
  for _, entry in ipairs(entries) do
    if entry.kind == "user" then
      user_turns = user_turns + 1
    end
  end
  local context = Transcript.render_handoff(entries, source_state)
  local source_ref = source_state.acp_session_id ~= nil and (source_state.agent .. "/" .. source_state.acp_session_id)
    or source_state.agent
  local brief = table.concat({
    "## Handoff",
    "",
    "- source: `" .. source_ref .. "`",
    "- takeover task: " .. HANDOFF_TASK_PLACEHOLDER,
    "- constraints: (none)",
    "",
    "## Context",
    "",
    context,
    "## Source",
    "",
    "- agent: " .. source_state.agent,
    "- acp session: " .. (source_state.acp_session_id or "none"),
    "- user turns: " .. tostring(user_turns),
    "",
  }, "\n")
  nvim.api.nvim_buf_set_lines(buffer, 0, -1, false, nvim.split(brief, "\n", { plain = true }))
  self.reviews[buffer] = {
    target_session = target_session,
    target_session_id = target_state.id,
    source_session_id = source_state.acp_session_id and (source_state.agent .. "/" .. source_state.acp_session_id)
      or nil,
    source_id = source_id,
  }
  nvim.api.nvim_set_current_buf(buffer)
  nvim.keymap.set("n", "<C-s>", function()
    self:submit(buffer)
  end, { buffer = buffer, silent = true, desc = "Submit louiselm handoff" })
  nvim.keymap.set("n", "q", function()
    self:abandon(buffer)
  end, { buffer = buffer, silent = true, desc = "Abandon louiselm handoff" })
  nvim.keymap.set("n", "<Esc>", function()
    self:abandon(buffer)
  end, { buffer = buffer, silent = true, desc = "Abandon louiselm handoff" })
  return buffer
end

---Split a reviewed Handoff brief at its `## Context` header, or nil when the
---header is absent (the operator edited it out). The Handoff section — the
---takeover instruction — travels as the prompt's text block; everything from
---`## Context` on, including `## Source`, travels as the resource block.
---@param text string
---@return string? handoff_section
---@return string? context_section
local function split_handoff_brief(text)
  local marker = text:find("\n## Context", 1, true)
  if marker == nil then
    return nil, nil
  end
  return text:sub(1, marker - 1), text:sub(marker + 1)
end

---Build the reviewed brief's transport blocks. An edited-out Context header
---falls back to text; the host prepends staged content through the draft owner.
---@param handoff louiselm.ui.Handoff Review record.
---@param text string Reviewed brief text.
---@return string|table[] content
local function handoff_content(handoff, text)
  ---@type string|table[]
  local content = text
  if handoff.target_session:inspect().embedded_context == true then
    local handoff_section, context_section = split_handoff_brief(text)
    if handoff_section ~= nil then
      local source_ref = handoff.source_session_id or handoff.source_id
      content = {
        { type = "text", text = handoff_section },
        {
          type = "resource",
          resource = {
            uri = "louiselm://handoff/" .. source_ref,
            mimeType = "text/markdown",
            text = context_section,
          },
        },
      }
    end
  end
  return content
end

---Submit the current contents of a handoff review buffer.
---@param self louiselm.ui.Handoffs
---@param buffer integer Handoff buffer returned by `open_handoff`.
---@return boolean sent
---@return string? error_message Validation or target-session error.
function Handoffs:submit(buffer)
  local handoff = self.reviews[buffer]
  if handoff == nil or not nvim.api.nvim_buf_is_valid(buffer) then
    return false, "handoff buffer is not open"
  end
  local text = table.concat(nvim.api.nvim_buf_get_lines(buffer, 0, -1, false), "\n")
  if text:match("%S") == nil then
    return false, "handoff prompt must be a non-empty string"
  end
  if takeover_task(text) == nil then
    return false, "handoff takeover task must be filled in before submitting"
  end
  local sent, send_error = self.options.submit(handoff, text, handoff_content(handoff, text))
  if not sent then
    return false, send_error
  end
  close_handoff(self, buffer)
  self.options.focus(handoff.target_session_id)
  return true
end

---Close a handoff review buffer without sending its contents.
---@param self louiselm.ui.Handoffs
---@param buffer integer Handoff buffer returned by `open_handoff`.
---@return boolean abandoned
---@return string? error_message Validation error.
function Handoffs:abandon(buffer)
  local handoff = self.reviews[buffer]
  if handoff == nil then
    return false, "handoff buffer is not open"
  end
  close_handoff(self, buffer)
  self.options.focus(handoff.target_session_id)
  return true
end

---Close all reviews without sending or disposing either Session.
---@param self louiselm.ui.Handoffs
function Handoffs:dispose()
  self.disposed = true
  for buffer in pairs(self.reviews) do
    close_handoff(self, buffer)
  end
end

return M
