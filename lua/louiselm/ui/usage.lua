---@diagnostic disable-next-line: undefined-global -- This module owns the explorer buffer and scheduling.
local nvim = vim
local Recording = require("louiselm.session.recording")
local Paths = require("louiselm.paths")
local M = {}
local View = {}
View.__index = View

---@class louiselm.ui.UsageOptions
---@field query? fun(query: louiselm.session.UsageQuery, callback: louiselm.session.UsageQueryCallback): fun()? Asynchronous reader returning optional cancellation; defaults to the shared recording store.
---@class louiselm.ui.UsageView
---@field buffer integer
---@field disposed boolean
---@field generation integer
---@field query louiselm.session.UsageQuery
---@field reader fun(query: louiselm.session.UsageQuery, callback: louiselm.session.UsageQueryCallback): fun()?
---@field cancel? fun()
---@field page? louiselm.session.UsagePage
---@field error? string
---@field selected? louiselm.session.UsageRow Starting turn shown above its observations.
---@field history {query: louiselm.session.UsageQuery, selected?: louiselm.session.UsageRow}[]
---@field row_lines table<integer, integer>
---@field refresh fun(self: louiselm.ui.UsageView)
---@field set_query fun(self: louiselm.ui.UsageView, query: louiselm.session.UsageQuery)
---@field next_page fun(self: louiselm.ui.UsageView)
---@field previous_page fun(self: louiselm.ui.UsageView)
---@field enter fun(self: louiselm.ui.UsageView, index?: integer)
---@field back fun(self: louiselm.ui.UsageView)
---@field dispose fun(self: louiselm.ui.UsageView)

local function encoded(value)
  return nvim.json.encode(value)
end

local function sorted_keys(value)
  local keys = nvim.tbl_keys(value)
  table.sort(keys)
  return keys
end

---@param lines string[]
---@param summary louiselm.session.UsageTotals
local function metrics(lines, summary)
  lines[#lines + 1] = string.format("%d turns | outcomes %s", summary.turns, encoded(summary.outcomes))
  for _, field in ipairs(sorted_keys(summary.tokens)) do
    local value = summary.tokens[field]
    lines[#lines + 1] = string.format(
      "  %s: total %g | mean %g | coverage %d/%d",
      field,
      value.total,
      value.average,
      value.samples,
      summary.turns
    )
  end
  for _, value in ipairs(summary.costs) do
    lines[#lines + 1] = string.format(
      "  Cost %s: total %g | mean %g | coverage %d/%d",
      value.currency,
      value.total,
      value.average,
      value.samples,
      summary.turns
    )
  end
  if next(summary.tokens) == nil then
    lines[#lines + 1] = "  Tokens: not reported"
  end
  if #summary.costs == 0 then
    lines[#lines + 1] = "  Cost: no complete reported delta"
  end
end

---@param self louiselm.ui.UsageView
local function render(self)
  if self.disposed then
    return
  end
  local query = self.query
  local lines = {
    "LouiseLM usage | " .. (query.view or "summary") .. " | UTC",
    "<Enter> inspect/apply  <BS> back  n/p page  r refresh  q close",
    "f filters  g grouping  t time range  b bucket  d dimensions  v view  m mixed  s Session timeline",
    "Time: [" .. (query.from and encoded(query.from) or "beginning") .. ", " .. (query.until_time and encoded(
      query.until_time
    ) or "latest") .. ") UTC | buckets: " .. (query.bucket or "none"),
    "Filters: " .. encoded(query.filters or nvim.empty_dict()) .. " | grouping: " .. encoded(query.group_by or {}),
    "Turn filters use immutable starting values; events use observed timestamps.",
    "Mixed: " .. (query.mixed or "include") .. "; configuration summaries exclude changed-during-turn records.",
    "",
  }
  self.row_lines = {}
  local turn = self.selected
  if turn then
    lines[#lines + 1] = "Session: "
      .. encoded(turn.agent)
      .. "/"
      .. encoded(turn.acp_session_id)
      .. " | turn "
      .. encoded(turn.id)
    lines[#lines + 1] = "Started: "
      .. encoded(turn.prepared_at)
      .. " | Provider: "
      .. encoded(turn.provider)
      .. " | Model: "
      .. encoded(turn.model)
    lines[#lines + 1] = "Starting options: " .. encoded(turn.options)
    lines[#lines + 1] = "Starting cost: "
      .. encoded(turn.cost_baseline)
      .. " | changed during turn: "
      .. tostring(turn.mixed)
    if turn.summary then
      metrics(lines, turn.summary)
    end
    lines[#lines + 1] = ""
  end
  local page = self.page
  if self.error then
    lines[#lines + 1] = "Query failed: " .. self.error
    lines[#lines + 1] = "Edit filters/range or press r to retry."
  elseif not page then
    lines[#lines + 1] = "Loading recorded history..."
  else
    if query.view ~= "events" then
      metrics(lines, page.summary)
    end
    lines[#lines + 1] =
      string.format("Mixed turns: %d | excluded from this summary: %d", page.mixed_turns, page.excluded_mixed)
    lines[#lines + 1] = string.format(
      "Rows %d-%d of %d%s",
      #page.rows == 0 and 0 or (query.offset or 0) + 1,
      (query.offset or 0) + #page.rows,
      page.total,
      page.next_offset and " | n: next page" or ""
    )
    lines[#lines + 1] = ""
    if #page.rows == 0 then
      lines[#lines + 1] = "No matching history on this page."
    end
    for index, row in ipairs(page.rows) do
      local first = #lines + 1
      if query.view == "events" then
        lines[#lines + 1] = string.format(
          "%s | %s | sequence %d | turn %s | observer %s",
          encoded(row.observed_at),
          row.kind or "",
          row.sequence or 0,
          encoded(row.turn_id),
          encoded(row.observer_id)
        )
        lines[#lines + 1] = "  " .. encoded(row.data)
      elseif query.view == "dimensions" then
        lines[#lines + 1] = encoded(row.dimension) .. " = " .. encoded(row.value)
      elseif query.view == "turns" then
        lines[#lines + 1] = string.format(
          "%s | %s/%s | %s%s",
          encoded(row.prepared_at),
          encoded(row.agent),
          encoded(row.acp_session_id),
          encoded(row.id),
          row.mixed and " | MIXED: excluded from fixed comparisons" or ""
        )
        lines[#lines + 1] = "  " .. encoded(row.provider) .. " | " .. encoded(row.options)
        if row.summary then
          metrics(lines, row.summary)
        end
      else
        lines[#lines + 1] = encoded(row.dimensions or nvim.empty_dict())
          .. (row.bucket_start and " | [" .. row.bucket_start .. ", " .. row.bucket_end .. ") UTC" or "")
        if row.summary then
          metrics(lines, row.summary)
        end
      end
      for line = first, #lines do
        self.row_lines[line] = index
      end
      lines[#lines + 1] = ""
    end
  end
  nvim.bo[self.buffer].modifiable = true
  nvim.api.nvim_buf_set_lines(self.buffer, 0, -1, false, lines)
  nvim.bo[self.buffer].modifiable = false
end

---Recompute this view; older responses cannot overwrite a newer query or revive a closed buffer.
---@param self louiselm.ui.UsageView
function View:refresh()
  if self.disposed then
    return
  end
  self.generation = self.generation + 1
  if self.cancel then
    self.cancel()
    self.cancel = nil
  end
  local generation = self.generation
  self.page, self.error = nil, nil
  render(self)
  local completed = false
  self.cancel = self.reader(nvim.deepcopy(self.query), function(page, err)
    if completed then
      return
    end
    completed = true
    nvim.schedule(function()
      if self.disposed or self.generation ~= generation then
        return
      end
      self.cancel = nil
      self.page, self.error = page, err and err.message or nil
      if page == nil and err == nil then
        self.error = "usage reader returned no result"
      end
      render(self)
    end)
  end)
end

---Replace filters/grouping with caller-owned query data, reset paging and asynchronously recompute.
---@param self louiselm.ui.UsageView
---@param query louiselm.session.UsageQuery Invalid input is displayed through the query callback.
function View:set_query(query)
  if self.disposed then
    return
  end
  self.query = nvim.deepcopy(query)
  self.query.offset = 0
  self:refresh()
end

---Read the next page when the current snapshot advertises one.
---@param self louiselm.ui.UsageView
function View:next_page()
  if self.disposed or not self.page or not self.page.next_offset then
    return
  end
  self.query.offset = self.page.next_offset
  self:refresh()
end

---Read the previous page, preserving the complete query.
---@param self louiselm.ui.UsageView
function View:previous_page()
  if self.disposed or not self.page or (self.query.offset or 0) == 0 then
    return
  end
  self.query.offset = math.max(0, (self.query.offset or 0) - (self.query.limit or 25))
  self:refresh()
end

---@param self louiselm.ui.UsageView
local function remember(self)
  self.history[#self.history + 1] = { query = nvim.deepcopy(self.query), selected = self.selected }
end

---Inspect a group/turn, or apply a recorded dimension value. Missing rows are a no-op.
---@param self louiselm.ui.UsageView
---@param index? integer Page row; defaults to the cursor's row.
function View:enter(index)
  if self.disposed or not self.page then
    return
  end
  index = index or self.row_lines[nvim.api.nvim_win_get_cursor(0)[1]]
  local row = index and self.page.rows[index]
  if not row or self.query.view == "events" then
    return
  end
  remember(self)
  local query = nvim.deepcopy(self.query)
  if query.view == "turns" then
    self.selected = row
    query = { view = "events", turn_id = row.id, limit = query.limit }
  elseif query.view == "dimensions" then
    query.filters = query.filters or {}
    query.filters[row.dimension] = row.value
    query.view = "summary"
  else
    query.view = "turns"
    query.filters = query.filters or {}
    for _, key in ipairs(query.group_by or {}) do
      local value = (row.dimensions or {})[key]
      query.filters[key] = value == nil and nvim.NIL or value
    end
    -- Carry all recorded group dimensions into the joint turn filter.
    for key, value in pairs(row.dimensions or {}) do
      query.filters[key] = value
    end
    if row.bucket_start then
      local function time_key(value)
        return value:sub(1, 19) .. ((value:match("%.(%d+)Z$") or "") .. "000"):sub(1, 3)
      end
      query.from = query.from and time_key(query.from) > time_key(row.bucket_start) and query.from or row.bucket_start
      query.until_time = query.until_time and time_key(query.until_time) < time_key(row.bucket_end) and query.until_time
        or row.bucket_end
    end
    if self.page.excluded_mixed > 0 then
      query.mixed = "exclude"
    end
  end
  self:set_query(query)
end

---Return to the previous query and page; refresh committed facts.
---@param self louiselm.ui.UsageView
function View:back()
  if self.disposed then
    return
  end
  local previous = table.remove(self.history)
  if not previous then
    return
  end
  self.query, self.selected = previous.query, previous.selected
  self:refresh()
end

---Close the buffer and invalidate all pending query/input callbacks. Idempotent.
---@param self louiselm.ui.UsageView
function View:dispose()
  if self.disposed then
    return
  end
  self.disposed = true
  self.generation = self.generation + 1
  if self.cancel then
    self.cancel()
    self.cancel = nil
  end
  if nvim.api.nvim_buf_is_valid(self.buffer) then
    nvim.api.nvim_buf_delete(self.buffer, { force = true })
  end
end

---@param self louiselm.ui.UsageView
---@param prompt string
---@param value table
---@param apply fun(value: table, query: louiselm.session.UsageQuery)
local function edit(self, prompt, value, apply)
  local generation = self.generation
  nvim.ui.input({ prompt = prompt, default = encoded(value) }, function(input)
    nvim.schedule(function()
      if self.disposed or generation ~= self.generation or input == nil then
        return
      end
      local ok, decoded = pcall(nvim.json.decode, input)
      if not ok or type(decoded) ~= "table" then
        self.error = "expected a JSON object or array"
        render(self)
        return
      end
      local query = nvim.deepcopy(self.query)
      apply(decoded, query)
      self:set_query(query)
    end)
  end)
end

---Open a disposable history buffer and start a paged asynchronous read.
---Requires no live Session or Agent configuration. Returns a typed construction error if unavailable.
---@param options? louiselm.ui.UsageOptions
---@return louiselm.ui.UsageView? view
---@return louiselm.session.RecordingError? error
function M.open(options)
  local reader = options and options.query
  if reader == nil then
    local store, err = Recording.new(nvim.fs.joinpath(Paths.state(), "usage"), function() end)
    if store == nil then
      return nil, err
    end
    reader = function(query, callback)
      return store:usage_query(query, callback)
    end
  end
  local self = setmetatable({
    buffer = nvim.api.nvim_create_buf(false, true),
    disposed = false,
    generation = 0,
    query = {},
    reader = reader,
    history = {},
    row_lines = {},
  }, View)
  nvim.api.nvim_buf_set_name(self.buffer, "louiselm://usage/" .. self.buffer)
  nvim.bo[self.buffer].bufhidden = "wipe"
  nvim.bo[self.buffer].swapfile = false
  nvim.bo[self.buffer].filetype = "louiselm_usage"
  nvim.api.nvim_create_autocmd("BufWipeout", {
    buffer = self.buffer,
    once = true,
    callback = function()
      self.disposed = true
      self.generation = self.generation + 1
      if self.cancel then
        self.cancel()
        self.cancel = nil
      end
    end,
  })
  local function map(key, callback, description)
    nvim.keymap.set("n", key, callback, { buffer = self.buffer, nowait = true, silent = true, desc = description })
  end
  map("q", function()
    self:dispose()
  end, "Close usage history")
  map("r", function()
    self:refresh()
  end, "Refresh usage history")
  map("n", function()
    self:next_page()
  end, "Next usage page")
  map("p", function()
    self:previous_page()
  end, "Previous usage page")
  map("<CR>", function()
    self:enter()
  end, "Inspect usage row")
  map("<BS>", function()
    self:back()
  end, "Previous usage query")
  map("f", function()
    edit(
      self,
      "Filters JSON (agent/provider/model/session/option:<ID>, null = absent): ",
      self.query.filters or nvim.empty_dict(),
      function(value, query)
        query.filters = value
      end
    )
  end, "Edit joint filters")
  map("g", function()
    edit(
      self,
      'Group dimensions JSON (e.g. ["agent","model","option:reasoning"]): ',
      self.query.group_by or {},
      function(value, query)
        query.group_by = value
        query.view = "summary"
      end
    )
  end, "Edit usage grouping")
  map("t", function()
    edit(
      self,
      'UTC range JSON {"from":"...Z","until_time":"...Z"} (inclusive/exclusive): ',
      { from = self.query.from, until_time = self.query.until_time },
      function(value, query)
        query.from, query.until_time = value.from, value.until_time
      end
    )
  end, "Edit UTC time range")
  map("b", function()
    local query = nvim.deepcopy(self.query)
    query.bucket = ({ none = "hour", hour = "day", day = "none" })[query.bucket or "none"]
    query.view = "summary"
    self:set_query(query)
  end, "Cycle UTC hour/day buckets")
  map("d", function()
    remember(self)
    local query = nvim.deepcopy(self.query)
    query.view = "dimensions"
    self:set_query(query)
  end, "Browse recorded dimensions")
  map("v", function()
    remember(self)
    local query = nvim.deepcopy(self.query)
    query.view = ({ summary = "turns", turns = "events", events = "summary", dimensions = "summary" })[query.view or "summary"]
    self.selected = nil
    self:set_query(query)
  end, "Cycle summaries/turns/events")
  map("m", function()
    local query = nvim.deepcopy(self.query)
    query.mixed = ({ include = "exclude", exclude = "only", only = "include" })[query.mixed or "include"]
    self:set_query(query)
  end, "Cycle mixed-turn filter")
  map("s", function()
    local row = self.selected or (self.page and self.page.rows[self.row_lines[nvim.api.nvim_win_get_cursor(0)[1]] or 0])
    if not row or not row.agent or not row.acp_session_id then
      return
    end
    remember(self)
    self.selected = nil
    self:set_query({ view = "events", filters = { agent = row.agent, session = row.acp_session_id } })
  end, "Inspect Session option transitions")
  nvim.api.nvim_set_current_buf(self.buffer)
  self:refresh()
  return self, nil
end

return M
