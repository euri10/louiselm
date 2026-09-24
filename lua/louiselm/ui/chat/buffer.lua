local Status = require("louiselm.ui.chat.status")
local Tables = require("louiselm.ui.chat.tables")

---@diagnostic disable-next-line: undefined-global -- Neovim injects its runtime API.
local nvim = vim

---@class louiselm.ui.ChatBufferOptions
---@field markdown_highlighting boolean Enable Markdown highlighting explicitly.
---@field on_prompt_edit fun() Clear caller-owned queued draft state when its prompt is edited.
---@field prompt_prefix fun(): string Current caller-owned visible context prefix.
---@field on_enter fun() Mark the owning view as seen.
---@field submit fun() Submit through Chat coordination.

---@class louiselm.ui.ChatBuffer
---@field buffer integer Scratch buffer for the session.
---@field window integer Window displaying the session.
---@field package prompt_line integer Zero-based prompt line.
---@field package prompt_mark integer Extmark tracking the prompt boundary through buffer edits.
---@field package prompt_namespace integer Extmark namespace for the prompt boundary.
---@field package transcript_tail integer? Zero-based last rendered transcript line.
---@field package response_tail integer? Zero-based last streamed response line.
---@field package response_started boolean Whether the assistant has rendered response text for this turn.
---@field package pending_terminal_completion? string Text from a terminal completion tool, flushed at turn end or immediately if it arrives after turn end.
---@field package turn_prose string Assistant chunk text rendered during the current turn; suppresses a terminal-completion echo only when that exact text was already shown.
---@field package turn_done_fired boolean Whether `turn_done` already ran for the current turn; a terminal completion arriving after this flushes immediately instead of waiting for a `turn_done` that already passed.
---@field package last_block_kind ("prose"|"tool"|"reasoning")? Kind of the most recently rendered transcript block; separates adjacent prose, reasoning, and tool blocks with a blank line.
---@field package trailing_blank boolean Whether the line at `transcript_tail` is already a blank separator, counted as part of `transcript_tail` itself; meaningful only while `last_block_kind == "prose"`, since a completed prose block is the only thing that always leaves one behind.
---@field package tool_lines table<string, integer> Zero-based rendered tool lines by ID.
---@field package tool_ids table<integer, string> Tool-call IDs by zero-based rendered line.
---@field package compaction_lines table<string, integer> Compaction rows by ID, separate from tool IDs.
---@field package compaction_ids table<integer, string> Compaction IDs by zero-based rendered line.
---@field package tool_statuses table<string, string> Latest tool status by ID.
---@field package tool_titles table<string, string> Tool titles by ID.
---@field package context_folds louiselm.ui.ContextFold[] Submitted context fold ranges in this live buffer.
---@field package fold_counts table<integer, integer> Number of context folds installed in each window.
---@field package tool_folds louiselm.ui.ToolFold[] Completed tool-call fold ranges in this live buffer.
---@field package tool_fold_counts table<integer, integer> Number of tool folds installed in each window.
---@field package tool_fold_run louiselm.ui.ToolFoldRun? Contiguous rendered tool paragraph awaiting a boundary.
---@field package thought_folds louiselm.ui.ThoughtFold[] Reasoning fold ranges in this live buffer.
---@field package thought_run louiselm.ui.ThoughtFoldRun? Contiguous reasoning paragraph awaiting a boundary; first is its header line, last its final content line.
---@field package queue_mark integer? Extmark showing queued prompt state.
---@field package queue_namespace integer Extmark namespace for queued prompt state.
---@field package replay_prompt_mark integer? Range extmark for the current replayed user turn.
---@field package header_namespace integer Header highlight namespace.
---@field package tables louiselm.ui.ChatTables? Owned Markdown table presentation.
---@field header fun(self: louiselm.ui.ChatBuffer, state: louiselm.session.State)
---@field append fun(self: louiselm.ui.ChatBuffer, lines: string[])
---@field usage fun(self: louiselm.ui.ChatBuffer, line: string)
---@field prompt_text fun(self: louiselm.ui.ChatBuffer): string
---@field replace_prompt fun(self: louiselm.ui.ChatBuffer, text: string)
---@field reconcile fun(self: louiselm.ui.ChatBuffer)
---@field close_reasoning fun(self: louiselm.ui.ChatBuffer)
---@field clear_queue_indicator fun(self: louiselm.ui.ChatBuffer)
---@field queue_indicator fun(self: louiselm.ui.ChatBuffer)
---@field set_prefix fun(self: louiselm.ui.ChatBuffer, prefix: string, text: string)
---@field accept_prompt fun(self: louiselm.ui.ChatBuffer, text: string, contexts: louiselm.ui.ContextItem[], next_prefix: string, focus: boolean)
---@field render fun(self: louiselm.ui.ChatBuffer, event: louiselm.session.GenericEvent, replay_active?: boolean, continuing_prompt?: boolean)
---@field compaction fun(self: louiselm.ui.ChatBuffer, entity: louiselm.session.Compaction): boolean
---@field finish_turn fun(self: louiselm.ui.ChatBuffer)
---@field reset_response fun(self: louiselm.ui.ChatBuffer)
---@field error fun(self: louiselm.ui.ChatBuffer, message: string)
---@field tool_at_cursor fun(self: louiselm.ui.ChatBuffer): string?
---@field compaction_at_cursor fun(self: louiselm.ui.ChatBuffer): string?
---@field show fun(self: louiselm.ui.ChatBuffer, window: integer, start_insert: boolean)
---@field dispose fun(self: louiselm.ui.ChatBuffer)

---@class louiselm.ui.ContextFold
---@field first integer Zero-based first folded line.
---@field last integer Zero-based last folded line.

---@class louiselm.ui.ToolFoldRun
---@field first integer Zero-based first rendered tool line.
---@field last integer Zero-based last rendered tool line.

---@class louiselm.ui.ToolFold
---@field first integer Zero-based first folded line.
---@field last integer Zero-based last folded line.

---@class louiselm.ui.ThoughtFoldRun
---@field first integer Zero-based rendered reasoning header line.
---@field last integer Zero-based last rendered reasoning content line.

---@class louiselm.ui.ThoughtFold
---@field first integer Zero-based first folded line.
---@field last integer Zero-based last folded line.

local M = {}
local Buffer = {}
Buffer.__index = Buffer

-- Submitted context ends at the next marked user-prompt range.
local CONTEXTS_HEADER_PREFIX = "> [contexts:"
local HEADER_LINE_COUNT = 4

---@param value string
---@return string line
local function single_line(value)
  return (value:gsub("[\r\n]", " "))
end

---@param value unknown
---@return string? text
local function field(value, name)
  if type(value) == "table" and type(value[name]) == "string" and value[name] ~= "" then
    return value[name]
  end
  return nil
end

---@param buffer integer
---@param line integer
---@param value string
local function set_line(buffer, line, value)
  nvim.api.nvim_buf_set_lines(buffer, line, line + 1, false, { value })
end

---Update diagnostic header values from a Session snapshot.
---@param self louiselm.ui.ChatBuffer
---@param state louiselm.session.State
function Buffer:header(state)
  local lines, highlights = Status.session_header(state)
  nvim.api.nvim_buf_set_lines(self.buffer, 0, HEADER_LINE_COUNT, false, lines)
  nvim.api.nvim_buf_clear_namespace(self.buffer, self.header_namespace, 0, HEADER_LINE_COUNT)
  for _, highlight in ipairs(highlights) do
    nvim.api.nvim_buf_add_highlight(
      self.buffer,
      self.header_namespace,
      highlight.group,
      highlight.line,
      highlight.start_col,
      highlight.end_col
    )
  end
end

---@param view louiselm.ui.ChatBuffer
---@param win integer
---@param folds louiselm.ui.ContextFold[]
---@param counts table<integer, integer>
local function apply_incremental_folds(view, win, folds, counts)
  if not nvim.api.nvim_win_is_valid(win) or nvim.api.nvim_win_get_buf(win) ~= view.buffer then
    return
  end
  nvim.api.nvim_set_option_value("foldmethod", "manual", { win = win })
  nvim.api.nvim_set_option_value("foldenable", true, { win = win })
  local applied = counts[win] or 0
  nvim.api.nvim_win_call(win, function()
    for index = applied + 1, #folds do
      local fold = folds[index]
      nvim.api.nvim_cmd({ cmd = "fold", range = { fold.first + 1, fold.last + 1 } }, {})
    end
  end)
  counts[win] = #folds
end

---@param view louiselm.ui.ChatBuffer
local function close_tool_fold_run(view)
  local run = view.tool_fold_run
  if run == nil then
    return
  end
  local fold_first
  local added = false
  for line = run.first, run.last + 1 do
    local id = view.tool_ids[line]
    if id ~= nil and view.tool_statuses[id] == "completed" then
      fold_first = fold_first or line
    elseif fold_first ~= nil then
      if line - fold_first > 1 then
        view.tool_folds[#view.tool_folds + 1] = { first = fold_first, last = line - 1 }
        added = true
      end
      fold_first = nil
    end
  end
  if added then
    apply_incremental_folds(view, view.window, view.tool_folds, view.tool_fold_counts)
  end
  view.tool_fold_run = nil
end

---Reinstall every recorded reasoning fold that is not currently a real
---fold in `win`. Unlike the incremental context/tool fold appliers, this
---rescans the full `thought_folds` history on every call instead of trusting
---a "folds already installed" counter: something outside this module's
---control can silently drop a manual fold (observed for replayed reasoning
---paragraphs during `session/load`, louiselm-9wjm), and a counter that only
---ever advances has no way to notice or recover from that. Checking
---`foldlevel` first preserves manually opened folds and keeps repeated calls
---idempotent: re-issuing `:fold` nests another fold even when the existing
---fold is open.
---@param view louiselm.ui.ChatBuffer
---@param win integer
local function apply_thought_folds(view, win)
  if not nvim.api.nvim_win_is_valid(win) or nvim.api.nvim_win_get_buf(win) ~= view.buffer then
    return
  end
  nvim.api.nvim_set_option_value("foldmethod", "manual", { win = win })
  nvim.api.nvim_set_option_value("foldenable", true, { win = win })
  nvim.api.nvim_win_call(win, function()
    for _, fold in ipairs(view.thought_folds) do
      if nvim.fn.foldlevel(fold.first + 1) == 0 then
        nvim.api.nvim_cmd({ cmd = "fold", range = { fold.first + 1, fold.last + 1 } }, {})
      end
    end
  end)
end

---Close the current reasoning paragraph: fold the `[thinking]` header together
---with its content lines. Neovim cannot close a fold spanning a single line, so
---the header is folded along with the content rather than left outside it;
---Neovim's default foldtext then renders the fold's first line, `[thinking]`,
---as the closed summary. A paragraph with no content lines yet has nothing to
---fold.
---@param view louiselm.ui.ChatBuffer
local function close_thought_fold_run(view)
  local run = view.thought_run
  if run == nil then
    return
  end
  if run.last > run.first then
    view.thought_folds[#view.thought_folds + 1] = { first = run.first, last = run.last }
    apply_thought_folds(view, view.window)
  end
  view.thought_run = nil
end

---@param view louiselm.ui.ChatBuffer
local function toggle_chat_fold(view)
  local line = nvim.api.nvim_win_get_cursor(view.window)[1] - 1
  local run = view.thought_run
  if run ~= nil and line == run.first and nvim.fn.foldlevel(line + 1) == 0 then
    return
  end
  nvim.cmd("normal! za")
end

---@param view louiselm.ui.ChatBuffer
---@param line integer Zero-based rendered tool line.
local function record_tool_line(view, line)
  local run = view.tool_fold_run
  if run ~= nil and line ~= run.last + 1 then
    close_tool_fold_run(view)
    run = nil
  end
  if run == nil then
    view.tool_fold_run = { first = line, last = line }
  else
    run.last = line
  end
end

---@param view louiselm.ui.ChatBuffer
---@param first_line integer Inclusive zero-based start.
---@param end_line integer Exclusive zero-based end.
---@param id? integer Existing range to extend.
---@return integer mark_id
local function mark_submitted_prompt(view, first_line, end_line, id)
  return nvim.api.nvim_buf_set_extmark(view.buffer, view.prompt_namespace, first_line, 0, {
    id = id,
    end_row = end_line,
    end_col = 0,
    right_gravity = false,
    end_right_gravity = false,
    invalidate = true,
  })
end

---@param view louiselm.ui.ChatBuffer
---@param text string
---@param contexts louiselm.ui.ContextItem[]
---@return integer line_count
local function replace_submitted_prompt(view, text, contexts)
  local lines = {}
  if #contexts > 0 then
    local labels = {}
    for index, item in ipairs(contexts) do
      labels[index] = single_line(item.label)
    end
    lines[1] = "> [contexts: " .. table.concat(labels, " · ") .. "]"
    for _, item in ipairs(contexts) do
      lines[#lines + 1] = "[context: " .. single_line(item.label) .. "]"
      if item.uri == "louiselm://skills/index" and item.text ~= nil then
        lines[#lines + 1] = "type: resource"
        lines[#lines + 1] = "uri: " .. item.uri
        lines[#lines + 1] = "mimeType: text/plain"
        local body_first = view.prompt_line + #lines
        lines[#lines + 1] = "[body: " .. single_line(item.label) .. "]"
        for _, line in ipairs(nvim.split(item.text, "\n", { plain = true })) do
          lines[#lines + 1] = "  " .. line
        end
        view.context_folds[#view.context_folds + 1] = {
          first = body_first,
          last = view.prompt_line + #lines - 1,
        }
      elseif item.text ~= nil then
        nvim.list_extend(lines, nvim.split(item.text, "\n", { plain = true }))
      else
        lines[#lines + 1] = "type: resource_link"
        lines[#lines + 1] = "name: " .. item.label
        lines[#lines + 1] = "uri: " .. item.uri
      end
    end
    view.context_folds[#view.context_folds + 1] = {
      first = view.prompt_line,
      last = view.prompt_line + #lines - 1,
    }
  end
  local first_prompt_line = view.prompt_line + #lines
  for _, line in ipairs(nvim.split(text, "\n", { plain = true })) do
    lines[#lines + 1] = "> " .. line
  end
  nvim.api.nvim_buf_set_lines(view.buffer, view.prompt_line, -1, false, lines)
  mark_submitted_prompt(view, first_prompt_line, view.prompt_line + #lines)
  apply_incremental_folds(view, view.window, view.context_folds, view.fold_counts)
  return #lines
end

---@param view louiselm.ui.ChatBuffer
---@param line integer
local function mark_prompt(view, line)
  view.prompt_line = line
  view.prompt_mark = nvim.api.nvim_buf_set_extmark(view.buffer, view.prompt_namespace, line, 0, {
    id = view.prompt_mark,
    right_gravity = false,
  })
end

---@param view louiselm.ui.ChatBuffer
---@return integer line
local function current_prompt_line(view)
  local position = nvim.api.nvim_buf_get_extmark_by_id(view.buffer, view.prompt_namespace, view.prompt_mark, {})
  if #position == 2 then
    view.prompt_line = position[1]
  end
  return view.prompt_line
end

---Reconcile Lua-side indexes after undo moved the prompt marker.
---@param self louiselm.ui.ChatBuffer
function Buffer:reconcile()
  -- Undo restores the extmark but not these Lua-side indexes.
  if self.prompt_line < nvim.api.nvim_buf_line_count(self.buffer) then
    return
  end
  local prompt_line = current_prompt_line(self)
  self.transcript_tail = prompt_line - 1
  self.response_tail = nil
  self.response_started = false
  self.last_block_kind = nil
  self.trailing_blank = false
  self.thought_run = nil
  self.tool_fold_run = nil
end

---Read the editable prompt after resolving its undo-restored extmark.
---@param self louiselm.ui.ChatBuffer
---@return string text Without prompt chevrons.
function Buffer:prompt_text()
  local lines = nvim.api.nvim_buf_get_lines(self.buffer, current_prompt_line(self), -1, false)
  for index, line in ipairs(lines) do
    lines[index] = line:sub(1, 2) == "> " and line:sub(3) or line
  end
  return table.concat(lines, "\n")
end

---@param view louiselm.ui.ChatBuffer
---@param text string
---@return integer line_count
local function replace_prompt(view, text)
  local prompt_line = current_prompt_line(view)
  local lines = nvim.split(text, "\n", { plain = true })
  for index, line in ipairs(lines) do
    lines[index] = "> " .. line
  end
  nvim.api.nvim_buf_set_lines(view.buffer, prompt_line, -1, false, lines)
  mark_prompt(view, prompt_line)
  return #lines
end

---@param value unknown
---@return string? text Text carried by an ACP chunk.
local function chunk_text(value)
  if type(value) ~= "table" then
    return nil
  end
  if type(value.text) == "string" then
    return value.text
  end
  if type(value.content) == "table" and type(value.content.text) == "string" then
    return value.content.text
  end
  return nil
end

---@param value unknown
---@return string
local function tool_id(value)
  if type(value) == "table" then
    if type(value.toolCallId) == "string" and value.toolCallId ~= "" then
      return value.toolCallId
    end
    if type(value.tool_call_id) == "string" and value.tool_call_id ~= "" then
      return value.tool_call_id
    end
  end
  return "unknown"
end

---@param value unknown
---@return boolean has_image Whether an ACP tool payload contains an image block.
local function tool_has_image(value)
  if type(value) ~= "table" then
    return false
  end
  if value.type == "image" then
    return true
  end
  for _, nested in pairs(value) do
    if tool_has_image(nested) then
      return true
    end
  end
  return false
end

---@param value unknown
---@param title string? Tool title from this update or its preceding start event.
---@return string? text
local function terminal_completion_text(value, title)
  if type(value) ~= "table" or title ~= "task_complete" then
    return nil
  end
  local raw_output = value.rawOutput
  if type(raw_output) ~= "table" or type(raw_output.content) ~= "string" or raw_output.content == "" then
    return nil
  end
  return raw_output.content
end

---@param view louiselm.ui.ChatBuffer
---@param lines string[]
---@return integer insertion_line
local function insert_transcript(view, lines)
  local replacement = {}
  for _, line in ipairs(lines) do
    for _, part in ipairs(nvim.split(line, "\n", { plain = true })) do
      replacement[#replacement + 1] = part
    end
  end
  local insertion_line = view.transcript_tail == nil and view.prompt_line or view.transcript_tail + 1
  nvim.api.nvim_buf_set_lines(view.buffer, insertion_line, insertion_line, false, replacement)
  view.transcript_tail = insertion_line + #replacement - 1
  mark_prompt(view, view.prompt_line + #replacement)
  return insertion_line
end

---Render a terminal-completion tool's retained text inline unless that exact
---text was already streamed during the current turn.
---@param view louiselm.ui.ChatBuffer
---@param text string
local function flush_terminal_completion(view, text)
  if view.turn_prose:find(text, 1, true) ~= nil then
    return
  end
  local lines = {}
  if view.last_block_kind == "tool" or view.last_block_kind == "reasoning" then
    lines[#lines + 1] = ""
  end
  nvim.list_extend(lines, nvim.split(text, "\n", { plain = true }))
  lines[#lines + 1] = ""
  insert_transcript(view, lines)
  view.last_block_kind = "prose"
  view.trailing_blank = true
end

---Insert a usage row with its surrounding separators.
---@param self louiselm.ui.ChatBuffer
---@param line string
function Buffer:usage(line)
  local insertion_line = self.transcript_tail == nil and self.prompt_line or self.transcript_tail + 1
  local before = insertion_line > 0
      and nvim.api.nvim_buf_get_lines(self.buffer, insertion_line - 1, insertion_line, false)[1]
    or nil
  local after = nvim.api.nvim_buf_get_lines(self.buffer, insertion_line, insertion_line + 1, false)[1]
  local lines = {}
  if before ~= nil and before ~= "" then
    lines[#lines + 1] = ""
  end
  lines[#lines + 1] = line
  if after ~= nil and after ~= "" then
    lines[#lines + 1] = ""
  end
  insert_transcript(self, lines)
end

---Collect the Normal-mode navigation targets in the transcript history above
---the live prompt, in document order. A `prompt` target is the first line of
---each range recorded at a trusted user-prompt render boundary. A `reply`
---target is the first prose line of a turn's response: thinking content is
---excluded via the recorded thought-fold runs (text alone cannot distinguish
---it from prose), and so are blank lines, `[…` marker lines, and
---`Error:`/`Warning:` lines. A tool-only turn contributes no reply target.
---Usage/diagnostic rows exclude submitted context, prompts and thinking.
---Activity targets are thinking headers and the first row of each tool run,
---including single tools and reasoning that has not become a fold yet.
---@param view louiselm.ui.ChatBuffer
---@param kind string "prompt", "reply", "usage", "diagnostic" or "activity"
---@return integer[] targets Zero-based lines in document order.
local function navigation_targets(view, kind)
  local lines = nvim.api.nvim_buf_get_lines(view.buffer, 0, current_prompt_line(view), false)
  local prompt_lines = {}
  local prompt_starts = {}
  for _, mark in ipairs(nvim.api.nvim_buf_get_extmarks(view.buffer, view.prompt_namespace, 0, -1, { details = true })) do
    local details = mark[4]
    if details.end_row ~= nil and details.invalid ~= true then
      prompt_starts[mark[2]] = true
      for line = mark[2], details.end_row - 1 do
        prompt_lines[line] = true
      end
    end
  end
  local thinking_lines = {}
  local thinking_starts = {}
  for _, fold in ipairs(view.thought_folds) do
    thinking_starts[fold.first] = true
    for line = fold.first, fold.last do
      thinking_lines[line] = true
    end
  end
  local run = view.thought_run
  if run ~= nil then
    thinking_starts[run.first] = true
    for line = run.first, run.last do
      thinking_lines[line] = true
    end
  end
  local targets = {}
  local in_context_block = false
  local after_prompt = false
  local seen_reply = false
  for index, line in ipairs(lines) do
    local zero_based = index - 1
    if line:sub(1, #CONTEXTS_HEADER_PREFIX) == CONTEXTS_HEADER_PREFIX then
      in_context_block = true
    elseif prompt_lines[zero_based] then
      in_context_block = false
      if prompt_starts[zero_based] and kind == "prompt" then
        targets[#targets + 1] = zero_based
      end
      after_prompt = true
      seen_reply = false
    else
      local text = line:match("^%s*(.*)$")
      if not in_context_block then
        if
          kind == "activity"
          and (thinking_starts[zero_based] or (view.tool_ids[zero_based] and not view.tool_ids[zero_based - 1]))
        then
          targets[#targets + 1] = zero_based
        elseif
          not thinking_lines[zero_based]
          and (
            (kind == "usage" and text:sub(1, 8) == "[usage] ")
            or (kind == "diagnostic" and (text:sub(1, 6) == "Error:" or text:sub(1, 8) == "Warning:"))
          )
        then
          targets[#targets + 1] = zero_based
        end
      end
      if
        kind == "reply"
        and after_prompt
        and not seen_reply
        and not in_context_block
        and line ~= ""
        and line:sub(1, 1) ~= "["
        and not thinking_lines[zero_based]
        and line:sub(1, 6) ~= "Error:"
        and line:sub(1, 8) ~= "Warning:"
      then
        targets[#targets + 1] = zero_based
        seen_reply = true
      end
    end
  end
  return targets
end

---Move the cursor to the count-th target of `kind` in `direction` from the
---cursor. A motion with no further target is a silent no-op; landing is on
---the target's first non-blank column.
---@param view louiselm.ui.ChatBuffer
---@param kind string "prompt", "reply", "usage", "diagnostic" or "activity"
---@param direction integer 1 forward, -1 backward
---@param count integer
local function navigate_transcript(view, kind, direction, count)
  local targets = navigation_targets(view, kind)
  local cursor_line = nvim.api.nvim_win_get_cursor(view.window)[1] - 1
  local remaining = count
  local selected
  if direction > 0 then
    for _, target in ipairs(targets) do
      if target > cursor_line then
        remaining = remaining - 1
        if remaining == 0 then
          selected = target
          break
        end
      end
    end
  else
    for index = #targets, 1, -1 do
      local target = targets[index]
      if target < cursor_line then
        remaining = remaining - 1
        if remaining == 0 then
          selected = target
          break
        end
      end
    end
  end
  if selected == nil then
    return
  end
  local line = nvim.api.nvim_buf_get_lines(view.buffer, selected, selected + 1, false)[1] or ""
  nvim.api.nvim_win_set_cursor(view.window, { selected + 1, #(line:match("^%s*")) })
end

---Append diagnostic or transcript rows, preserving the editable prompt.
---@param self louiselm.ui.ChatBuffer
---@param lines string[]
function Buffer:append(lines)
  insert_transcript(self, lines)
end

---Replace the editable prompt and retain its boundary marker.
---@param self louiselm.ui.ChatBuffer
---@param text string Including any caller-owned context prefix.
function Buffer:replace_prompt(text)
  replace_prompt(self, text)
end

---Finish the current reasoning paragraph and restore its fold.
---@param self louiselm.ui.ChatBuffer
function Buffer:close_reasoning()
  close_thought_fold_run(self)
end

---Clear the queued-prompt indicator without changing semantic draft content.
---@param self louiselm.ui.ChatBuffer
function Buffer:clear_queue_indicator()
  if self.queue_mark ~= nil and nvim.api.nvim_buf_is_valid(self.buffer) then
    nvim.api.nvim_buf_del_extmark(self.buffer, self.queue_namespace, self.queue_mark)
  end
  self.queue_mark = nil
end

---Mark the editable prompt as queued.
---@param self louiselm.ui.ChatBuffer
function Buffer:queue_indicator()
  self.queue_mark = nvim.api.nvim_buf_set_extmark(self.buffer, self.queue_namespace, self.prompt_line, -1, {
    virt_text = { { "Queued for next turn", "Comment" } },
    virt_text_pos = "eol",
  })
end

---Render a changed context prefix without changing the caller's draft content.
---@param self louiselm.ui.ChatBuffer
---@param prefix string New visible prefix.
---@param text string Reconciled user text without context chips.
function Buffer:set_prefix(prefix, text)
  replace_prompt(self, prefix .. text)
  if nvim.api.nvim_get_current_buf() == self.buffer then
    nvim.api.nvim_win_set_cursor(0, { self.prompt_line + 1, 2 + #prefix })
  end
end

---Render an accepted prompt, its submitted contexts and the next editable prompt.
---@param self louiselm.ui.ChatBuffer
---@param text string
---@param contexts louiselm.ui.ContextItem[]
---@param next_prefix string Context prefix retained for slash prompts.
---@param focus boolean Whether to move the cursor when this buffer is current.
function Buffer:accept_prompt(text, contexts, next_prefix, focus)
  local prompt_line_count = replace_submitted_prompt(self, text, contexts)
  local response_line = self.prompt_line + prompt_line_count
  nvim.api.nvim_buf_set_lines(self.buffer, response_line, response_line, false, { "", "> " .. next_prefix })
  self.response_tail = response_line
  self.response_started = false
  self.pending_terminal_completion = nil
  self.last_block_kind = nil
  self.transcript_tail = response_line
  mark_prompt(self, response_line + 1)
  if focus and nvim.api.nvim_get_current_buf() == self.buffer then
    nvim.api.nvim_win_set_cursor(0, { self.prompt_line + 1, 2 + #next_prefix })
  end
end

---Insert or patch a compaction row without disturbing later streamed prose.
---@param self louiselm.ui.ChatBuffer
---@param entity louiselm.session.Compaction Validated Session snapshot.
---@return boolean inserted Whether this is a new timeline boundary.
function Buffer:compaction(entity)
  local view = self
  local text = "[compaction] "
    .. single_line(entity.id)
    .. " ("
    .. single_line(entity.status)
    .. ") — :LouiselmInspectTool"
  local line = view.compaction_lines[entity.id]
  if line ~= nil then
    set_line(view.buffer, line, text)
    return false
  else
    close_thought_fold_run(view)
    close_tool_fold_run(view)
    line = insert_transcript(view, { text })
    view.compaction_lines[entity.id] = line
    view.compaction_ids[line] = entity.id
    view.response_tail = nil
    view.response_started = false
    view.last_block_kind = nil
    return true
  end
end

---Apply one already-scheduled streaming event; never records or queues Session events.
---@param self louiselm.ui.ChatBuffer
---@param event louiselm.session.GenericEvent User/prose/reasoning chunk or tool update.
---@param replay_active? boolean Whether user chunks belong to loaded history.
---@param continuing_prompt? boolean Whether this user chunk extends the current replayed prompt.
function Buffer:render(event, replay_active, continuing_prompt)
  local view = self
  if event.type == "user_chunk" then
    local text = chunk_text(event.data)
    if text == nil then
      return
    end
    local lines = nvim.split(text, "\n", { plain = true })
    for index, line in ipairs(lines) do
      lines[index] = "> " .. line
    end
    local prompt_line_count = #lines
    lines[#lines + 1] = ""
    local insertion_line = insert_transcript(view, lines)
    local first_prompt_line = insertion_line
    local prompt_mark
    if continuing_prompt and view.replay_prompt_mark ~= nil then
      local position = nvim.api.nvim_buf_get_extmark_by_id(
        view.buffer,
        view.prompt_namespace,
        view.replay_prompt_mark,
        { details = true }
      )
      local details = position[3]
      if #position == 3 and type(details) == "table" and details.invalid ~= true then
        first_prompt_line = position[1]
        prompt_mark = view.replay_prompt_mark
      end
    end
    local mark = mark_submitted_prompt(view, first_prompt_line, insertion_line + prompt_line_count, prompt_mark)
    if replay_active then
      view.replay_prompt_mark = mark
    end
    view.response_tail = nil
    view.response_started = false
    view.turn_prose = ""
    view.turn_done_fired = false
    view.last_block_kind = nil
  elseif event.type == "chunk" then
    local text = chunk_text(event.data)
    if text == nil then
      return
    end
    view.turn_prose = view.turn_prose .. text
    close_tool_fold_run(view)
    close_thought_fold_run(view)
    if not view.response_started then
      local lines = nvim.split(text, "\n", { plain = true })
      local insertion_line = view.response_tail
        or (view.transcript_tail == nil and view.prompt_line or view.transcript_tail + 1)
      if view.response_tail ~= nil then
        insertion_line = insertion_line + 1
      end
      local separator = 0
      if view.last_block_kind == "tool" or view.last_block_kind == "reasoning" then
        nvim.api.nvim_buf_set_lines(view.buffer, insertion_line, insertion_line, false, { "" })
        insertion_line = insertion_line + 1
        separator = 1
      end
      local response_line_count = #lines
      lines[#lines + 1] = ""
      nvim.api.nvim_buf_set_lines(view.buffer, insertion_line, insertion_line, false, lines)
      view.response_tail = insertion_line + response_line_count - 1
      view.transcript_tail = view.response_tail + 1
      mark_prompt(view, view.prompt_line + separator + #lines)
      view.response_started = true
      view.last_block_kind = "prose"
      view.trailing_blank = true
      return
    end
    local current = nvim.api.nvim_buf_get_lines(view.buffer, view.response_tail, view.response_tail + 1, false)[1] or ""
    local lines = nvim.split(current .. text, "\n", { plain = true })
    nvim.api.nvim_buf_set_lines(view.buffer, view.response_tail, view.response_tail + 1, false, lines)
    local added = #lines - 1
    view.response_tail = view.response_tail + added
    view.transcript_tail = view.response_tail + 1
    mark_prompt(view, view.prompt_line + added)
  elseif event.type == "thought_chunk" then
    local text = chunk_text(event.data)
    if text == nil or text == "" then
      return
    end
    view.pending_terminal_completion = nil
    close_tool_fold_run(view)
    local run = view.thought_run
    if run == nil then
      -- A reasoning paragraph starts with a `[thinking]` header; its content lines
      -- are folded beneath the header once the paragraph ends (the assistant's
      -- answer, a tool call, or the turn boundary). Until then it streams like a
      -- response: later chunks append to the paragraph's last content line.
      local lines = { "[thinking]" }
      if view.last_block_kind == "prose" and not view.trailing_blank then
        table.insert(lines, 1, "")
      end
      insert_transcript(view, lines)
      run = { first = view.transcript_tail, last = view.transcript_tail }
      view.thought_run = run
      -- Inserting below the submitted prompt invalidates its response bookkeeping,
      -- exactly like a tool paragraph does.
      view.response_tail = nil
      view.response_started = false
      view.last_block_kind = "reasoning"
    end
    if run.last == run.first then
      local lines = nvim.split(text, "\n", { plain = true })
      nvim.api.nvim_buf_set_lines(view.buffer, run.first + 1, run.first + 1, false, lines)
      run.last = run.first + #lines
      view.transcript_tail = run.last
      mark_prompt(view, view.prompt_line + #lines)
    else
      local current = nvim.api.nvim_buf_get_lines(view.buffer, run.last, run.last + 1, false)[1] or ""
      local lines = nvim.split(current .. text, "\n", { plain = true })
      nvim.api.nvim_buf_set_lines(view.buffer, run.last, run.last + 1, false, lines)
      local added = #lines - 1
      run.last = run.last + added
      view.transcript_tail = run.last
      mark_prompt(view, view.prompt_line + added)
    end
  elseif event.type == "tool_call_started" or event.type == "tool_call_finished" then
    close_thought_fold_run(view)
    local status = field(event.data, "status")
    local id = tool_id(event.data)
    local title = field(event.data, "title")
    if title ~= nil then
      title = single_line(title)
    end
    if title ~= nil then
      view.tool_titles[id] = title
    else
      title = view.tool_titles[id]
    end
    local completion_text
    if event.type == "tool_call_started" then
      view.tool_statuses[id] = status or "started"
      local detail = id
      if title ~= nil then
        detail = detail .. ": " .. title
      end
      local line_text = "[tool] " .. detail .. " (started)"
      local existing_line = view.tool_lines[id]
      if existing_line ~= nil and existing_line < nvim.api.nvim_buf_line_count(view.buffer) then
        set_line(view.buffer, existing_line, line_text)
      else
        local lines = { line_text }
        if view.last_block_kind == "prose" and not view.trailing_blank then
          table.insert(lines, 1, "")
        end
        insert_transcript(view, lines)
        view.tool_lines[id] = view.transcript_tail
        view.tool_ids[view.transcript_tail] = id
        record_tool_line(view, view.transcript_tail)
      end
      view.last_block_kind = "tool"
    else
      local detail = id
      if title ~= nil then
        detail = detail .. ": " .. title
      end
      detail = detail .. " (" .. (status or "finished") .. ")"
      if status == "completed" then
        completion_text = terminal_completion_text(event.data, title)
        if tool_has_image(event.data) then
          detail = detail .. " · image result — use :LouiselmInspectTool"
        end
      end
      local line = view.tool_lines[id]
      local rendered_line
      if line ~= nil and line < nvim.api.nvim_buf_line_count(view.buffer) then
        set_line(view.buffer, line, "[tool] " .. detail)
        rendered_line = line
      else
        local lines = { "[tool] " .. detail }
        if view.last_block_kind == "prose" and not view.trailing_blank then
          table.insert(lines, 1, "")
        end
        insert_transcript(view, lines)
        local inserted_line = view.transcript_tail
        if inserted_line ~= nil then
          rendered_line = inserted_line
          view.tool_ids[inserted_line] = id
          record_tool_line(view, inserted_line)
        end
        view.last_block_kind = "tool"
      end
      -- Terminal replay updates may follow an already-completed tool_call.
      -- Retain its row and title so late details update the same tool.
      view.tool_lines[id] = rendered_line
      view.tool_statuses[id] = status or "finished"
      if rendered_line ~= nil then
        view.tool_ids[rendered_line] = id
      end
    end
    if completion_text ~= nil then
      -- Copilot in Autopilot can emit the completed task_complete update after
      -- turn_done already ran (louiselm-zufj): that flush point will not fire
      -- again for this turn, so a late completion must flush immediately here.
      if view.turn_done_fired then
        flush_terminal_completion(view, completion_text)
      else
        view.pending_terminal_completion = completion_text
      end
    end
    view.response_tail = nil
    view.response_started = false
  end
end

---Close folds and flush the retained terminal completion at the turn boundary.
---@param self louiselm.ui.ChatBuffer
function Buffer:finish_turn()
  close_tool_fold_run(self)
  close_thought_fold_run(self)
  local terminal_completion = self.pending_terminal_completion
  self.pending_terminal_completion = nil
  if terminal_completion ~= nil then
    flush_terminal_completion(self, terminal_completion)
  end
  self.turn_done_fired = true
end

---Forget active response coordinates after completion or error.
---@param self louiselm.ui.ChatBuffer
function Buffer:reset_response()
  self.response_tail = nil
  self.response_started = false
  self.last_block_kind = nil
end

---Render a Session error and stop the current streaming paragraph.
---@param self louiselm.ui.ChatBuffer
---@param message string
function Buffer:error(message)
  self.pending_terminal_completion = nil
  close_thought_fold_run(self)
  insert_transcript(self, { "Error: " .. message })
  self:reset_response()
end

---Return the tool ID under the current cursor, or nil for non-tool rows.
---@param self louiselm.ui.ChatBuffer
---@return string? id
function Buffer:tool_at_cursor()
  local cursor = nvim.api.nvim_win_get_cursor(0)
  return self.tool_ids[cursor[1] - 1]
end

---Return the compaction ID under the cursor, or nil on another row.
---@param self louiselm.ui.ChatBuffer
---@return string? id
function Buffer:compaction_at_cursor()
  local cursor = nvim.api.nvim_win_get_cursor(self.window)
  return self.compaction_ids[cursor[1] - 1]
end

---Show this buffer in the selected window and install its context/tool folds.
---@param self louiselm.ui.ChatBuffer
---@param window integer Normal window chosen by Chat.
---@param start_insert boolean Whether to enter Insert mode when a UI is attached.
function Buffer:show(window, start_insert)
  self.window = window
  nvim.api.nvim_set_current_win(window)
  nvim.api.nvim_win_set_buf(window, self.buffer)
  nvim.api.nvim_win_set_cursor(window, { self.prompt_line + 1, 2 })
  if start_insert and #nvim.api.nvim_list_uis() > 0 then
    nvim.cmd.startinsert()
    nvim.api.nvim_win_set_cursor(window, { self.prompt_line + 1, 2 })
  end
  apply_incremental_folds(self, self.window, self.context_folds, self.fold_counts)
  apply_incremental_folds(self, self.window, self.tool_folds, self.tool_fold_counts)
  apply_thought_folds(self, self.window)
end

---Delete the owned buffer and its buffer-local handlers; safe to repeat.
---@param self louiselm.ui.ChatBuffer
function Buffer:dispose()
  if self.tables ~= nil then
    self.tables:dispose()
  end
  self:clear_queue_indicator()
  if nvim.api.nvim_buf_is_valid(self.buffer) then
    nvim.api.nvim_buf_delete(self.buffer, { force = true })
  end
end

---Create the scratch buffer, coordinates, buffer-local handlers and navigation keys.
---@param state louiselm.session.State Initial Session snapshot.
---@param options louiselm.ui.ChatBufferOptions Host callbacks and display settings.
---@return louiselm.ui.ChatBuffer buffer
function M.new(state, options)
  local window = nvim.api.nvim_get_current_win()
  local buffer = nvim.api.nvim_create_buf(false, true)
  nvim.api.nvim_buf_set_name(buffer, "louiselm://" .. state.id)
  nvim.api.nvim_set_option_value("buftype", "nofile", { buf = buffer })
  nvim.api.nvim_set_option_value("bufhidden", "hide", { buf = buffer })
  nvim.api.nvim_set_option_value("swapfile", false, { buf = buffer })
  -- Not the literal "markdown": that filetype is what third-party
  -- filetype-keyed integrations (image.nvim's markdown integration, at
  -- least) key off of, and they cannot tell this live, ever-growing
  -- transcript apart from a real markdown file a user is editing -- causing
  -- e.g. a full buffer re-parse on every keystroke looking for images that
  -- will never exist here. Highlighting is attached explicitly below,
  -- decoupled from `filetype`, so this buffer opts back into only what it
  -- actually wants.
  nvim.api.nvim_set_option_value("filetype", "louiselm-session", { buf = buffer })
  nvim.treesitter.language.register("markdown", "louiselm-session")
  if options.markdown_highlighting then
    nvim.treesitter.start(buffer, "markdown")
  end
  local header = Status.session_header(state)
  local initial_lines = nvim.list_extend(header, { "", "> " })
  nvim.api.nvim_buf_set_lines(buffer, 0, -1, false, initial_lines)

  local view = setmetatable({
    buffer = buffer,
    window = window,
    prompt_line = HEADER_LINE_COUNT + 1,
    transcript_tail = nil,
    response_tail = nil,
    response_started = false,
    turn_prose = "",
    turn_done_fired = false,
    last_block_kind = nil,
    trailing_blank = false,
    tool_lines = {},
    tool_ids = {},
    compaction_lines = {},
    compaction_ids = {},
    tool_statuses = {},
    tool_titles = {},
    context_folds = {},
    fold_counts = {},
    tool_folds = {},
    tool_fold_counts = {},
    tool_fold_run = nil,
    thought_folds = {},
    thought_run = nil,
    queue_mark = nil,
    queue_namespace = nvim.api.nvim_create_namespace("louiselm.chat.queued_prompt"),
    replay_prompt_mark = nil,
    prompt_namespace = nvim.api.nvim_create_namespace("louiselm.chat.prompt"),
    header_namespace = nvim.api.nvim_create_namespace("louiselm.chat.header"),
  }, Buffer)
  mark_prompt(view, view.prompt_line)
  if options.markdown_highlighting then
    view.tables = Tables.new(buffer, function()
      -- Observation must not refresh the cached line used to detect undo recovery.
      local position = nvim.api.nvim_buf_get_extmark_by_id(buffer, view.prompt_namespace, view.prompt_mark, {})
      return position[1] or view.prompt_line
    end)
  end
  nvim.api.nvim_buf_attach(buffer, false, {
    on_lines = function(_, _, _, first_line, last_line)
      if first_line <= view.prompt_line and last_line > view.prompt_line then
        options.on_prompt_edit()
      end
    end,
  })
  nvim.keymap.set("n", "za", function()
    toggle_chat_fold(view)
  end, { buffer = buffer, silent = true, desc = "Toggle chat fold" })
  nvim.keymap.set("n", "]u", function()
    navigate_transcript(view, "prompt", 1, nvim.v.count1)
  end, { buffer = buffer, silent = true, desc = "Next submitted prompt" })
  nvim.keymap.set("n", "[u", function()
    navigate_transcript(view, "prompt", -1, nvim.v.count1)
  end, { buffer = buffer, silent = true, desc = "Previous submitted prompt" })
  nvim.keymap.set("n", "]r", function()
    navigate_transcript(view, "reply", 1, nvim.v.count1)
  end, { buffer = buffer, silent = true, desc = "Next assistant reply" })
  nvim.keymap.set("n", "[r", function()
    navigate_transcript(view, "reply", -1, nvim.v.count1)
  end, { buffer = buffer, silent = true, desc = "Previous assistant reply" })
  nvim.keymap.set("n", "]e", function()
    navigate_transcript(view, "usage", 1, nvim.v.count1)
  end, { buffer = buffer, silent = true, desc = "Next usage entry" })
  nvim.keymap.set("n", "[e", function()
    navigate_transcript(view, "usage", -1, nvim.v.count1)
  end, { buffer = buffer, silent = true, desc = "Previous usage entry" })
  nvim.keymap.set("n", "]E", function()
    navigate_transcript(view, "diagnostic", 1, nvim.v.count1)
  end, { buffer = buffer, silent = true, desc = "Next error or warning" })
  nvim.keymap.set("n", "[E", function()
    navigate_transcript(view, "diagnostic", -1, nvim.v.count1)
  end, { buffer = buffer, silent = true, desc = "Previous error or warning" })
  nvim.keymap.set("n", "]x", function()
    navigate_transcript(view, "activity", 1, nvim.v.count1)
  end, { buffer = buffer, silent = true, desc = "Next tool run or thinking block" })
  nvim.keymap.set("n", "[x", function()
    navigate_transcript(view, "activity", -1, nvim.v.count1)
  end, { buffer = buffer, silent = true, desc = "Previous tool run or thinking block" })
  nvim.keymap.set("n", "<CR>", function()
    local prompt_line = current_prompt_line(view)
    if #nvim.api.nvim_list_uis() > 0 then
      nvim.cmd.startinsert()
    end
    -- Insert mode permits the position just past an empty prompt's prefix.
    nvim.api.nvim_win_set_cursor(view.window, { prompt_line + 1, 2 + #options.prompt_prefix() })
  end, { buffer = buffer, silent = true, desc = "Jump to the louiselm prompt" })
  nvim.api.nvim_create_autocmd({ "BufEnter", "WinEnter" }, {
    buffer = buffer,
    callback = options.on_enter,
    desc = "Mark viewed LouiseLM Session Attention as seen",
  })
  nvim.keymap.set("i", "<CR>", options.submit, { buffer = buffer, silent = true, desc = "Submit louiselm prompt" })
  return view
end

return M
