---@class louiselm.ui.ChatDraft
---@field contexts louiselm.ui.ContextItem[] Ordered context snapshots; callers may read, but only draft operations mutate them.
---@field context_prefix string Visible context markers, independent of buffer coordinates.
---@field skill_catalog? string Hidden catalog pending for the first accepted model prompt.
---@field pending_skill? louiselm.skills.Skill Selected native skill, resolved only at submission.
---@field queued_prompt? string Text committed for the next turn, without context markers.
---@field add_context fun(self: louiselm.ui.ChatDraft, item: louiselm.ui.ContextItem)
---@field select_skill fun(self: louiselm.ui.ChatDraft, skill: louiselm.skills.Skill, native: boolean): string?
---@field set_catalog fun(self: louiselm.ui.ChatDraft, catalog: string)
---@field cache_native_content fun(self: louiselm.ui.ChatDraft, content: string)
---@field cache_context_content fun(self: louiselm.ui.ChatDraft, index: integer, content: string)
---@field prompt_text fun(self: louiselm.ui.ChatDraft, text: unknown): string?, string?
---@field context_content fun(self: louiselm.ui.ChatDraft, embedded_context?: boolean): table[], louiselm.ui.ContextItem[]
---@field content fun(self: louiselm.ui.ChatDraft, text: string, command_name?: string, embedded_context?: boolean): string|table[], louiselm.ui.ContextItem[]
---@field clear_context fun(self: louiselm.ui.ChatDraft)
---@field queue fun(self: louiselm.ui.ChatDraft, text?: string)
---@field staged_context fun(self: louiselm.ui.ChatDraft): louiselm.ui.StagedContext

---@class louiselm.ui.StagedContext
---@field contexts integer Number of queued context items.
---@field pending_skill boolean Whether a native-mode skill selection is pending.
---@field queued_prompt boolean Whether a prompt is queued behind the active turn.

local M = {}
local Draft = {}
Draft.__index = Draft

---Construct empty staged content for one Chat view; performs no I/O.
---@return louiselm.ui.ChatDraft draft
function M.new()
  return setmetatable({ contexts = {}, context_prefix = "" }, Draft)
end

---@param self louiselm.ui.ChatDraft
---@param label string
local function add_chip(self, label)
  self.context_prefix = self.context_prefix .. "[context: " .. label .. "] "
end

---Snapshot one validated context and append its presentation marker.
---@param self louiselm.ui.ChatDraft
---@param item louiselm.ui.ContextItem Validated at the Chat input boundary.
function Draft:add_context(item)
  self.contexts[#self.contexts + 1] = { label = item.label, text = item.text, uri = item.uri }
  add_chip(self, item.label)
end

---Stage a selected skill or its injected body; retain unread bodies for retry.
---Takes ownership of the selection built by the picker boundary.
---@param self louiselm.ui.ChatDraft
---@param skill louiselm.skills.Skill
---@param native boolean Whether invocation uses an advertised native command.
---@return string? error_message A missing injected body is staged but needs a read before submission.
function Draft:select_skill(skill, native)
  local label = "skill: " .. skill.name
  add_chip(self, label)
  if native then
    self.pending_skill = skill
  else
    self.contexts[#self.contexts + 1] = { label = label, text = skill.content, skill_path = skill.path }
    if skill.content == nil then
      return "could not read selected skill: " .. skill.path
    end
  end
  return nil
end

---Hold the hidden catalog for a newly created inject Session.
---@param self louiselm.ui.ChatDraft
---@param catalog string Validated non-empty catalog.
function Draft:set_catalog(catalog)
  self.skill_catalog = catalog
end

---Retain a successful boundary read for the pending native selection, including across failed sends.
---@param self louiselm.ui.ChatDraft
---@param content string Resolved skill body.
function Draft:cache_native_content(content)
  self.pending_skill.content = content
end

---Retain a successful boundary read for an injected selection, including across failed sends.
---@param self louiselm.ui.ChatDraft
---@param index integer Existing context index.
---@param content string Resolved skill body.
function Draft:cache_context_content(index, content)
  self.contexts[index].text = content
end

---Remove the staged prefix from visible input and validate that something can be submitted.
---@param self louiselm.ui.ChatDraft
---@param text unknown Buffer text or an explicit Chat submit argument.
---@return string? text User-authored text without the prefix.
---@return string? error_message Invalid or empty input without a staged context or skill.
function Draft:prompt_text(text)
  if type(text) ~= "string" then
    return nil, "prompt must be a non-empty string"
  end
  local prefix = self.context_prefix
  if prefix ~= "" and text:sub(1, #prefix) == prefix then
    text = text:sub(#prefix + 1)
  end
  if text == "" and #self.contexts == 0 and self.pending_skill == nil then
    return nil, "prompt must be a non-empty string"
  end
  return text
end

---@param item louiselm.ui.ContextItem
---@return table block
local function context_block(item)
  if item.uri ~= nil then
    return { type = "resource_link", uri = item.uri, name = item.label }
  end
  return { type = "text", text = item.text }
end

---Assemble resolved contexts in transport order without I/O or state changes.
---@param self louiselm.ui.ChatDraft
---@param embedded_context? boolean Current Session capability.
---@return table[] content Fresh transport blocks.
---@return louiselm.ui.ContextItem[] contexts Exact attached contexts in transport order; entries are read-only.
function Draft:context_content(embedded_context)
  local content, contexts = {}, {}
  if self.skill_catalog ~= nil then
    local catalog = { label = "skill-index", text = self.skill_catalog }
    contexts[#contexts + 1] = catalog
    if embedded_context then
      content[#content + 1] = {
        type = "resource",
        resource = { uri = "louiselm://skills/index", mimeType = "text/plain", text = self.skill_catalog },
      }
    else
      content[#content + 1] = context_block(catalog)
    end
  end
  for _, item in ipairs(self.contexts) do
    contexts[#contexts + 1] = item
    content[#content + 1] = context_block(item)
  end
  return content, contexts
end

---Assemble a prompt from resolved inputs without consuming staged content.
---The caller reads missing skill bodies and resolves the native command against
---the current Session immediately before calling this pure transformation.
---@param self louiselm.ui.ChatDraft
---@param text string Validated user-authored text.
---@param command_name? string Resolved native command for this submission.
---@param embedded_context? boolean Current Session capability.
---@return string|table[] content Slash commands bypass all staged content.
---@return louiselm.ui.ContextItem[] contexts Exact attached contexts in transport order.
function Draft:content(text, command_name, embedded_context)
  if text:sub(1, 1) == "/" then
    return text, {}
  end
  local final_text = text
  if command_name ~= nil then
    final_text = text == "" and ("/" .. command_name) or ("/" .. command_name .. " " .. text)
  end
  local content, contexts = self:context_content(embedded_context)
  if #content == 0 then
    return final_text, contexts
  end
  if final_text ~= "" then
    content[#content + 1] = { type = "text", text = final_text }
  end
  return content, contexts
end

---Consume staged contexts only after an ordinary prompt or Handoff is accepted.
---Slash prompts leave them pending; queue cancellation is a separate transition.
---@param self louiselm.ui.ChatDraft
function Draft:clear_context()
  self.contexts = {}
  self.context_prefix = ""
  self.skill_catalog = nil
  self.pending_skill = nil
end

---Replace or cancel the one committed next-turn prompt; does not consume contexts.
---@param self louiselm.ui.ChatDraft
---@param text? string New queued text, or nil to cancel/release it.
function Draft:queue(text)
  self.queued_prompt = text
end

---Report the staged material that would be lost with this view.
---@param self louiselm.ui.ChatDraft
---@return louiselm.ui.StagedContext staged Fresh summary, excluding the hidden catalog.
function Draft:staged_context()
  return {
    contexts = #self.contexts,
    pending_skill = self.pending_skill ~= nil,
    queued_prompt = self.queued_prompt ~= nil,
  }
end

return M
