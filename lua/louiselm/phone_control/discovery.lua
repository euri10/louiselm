---Bounded Session discovery from the live registries owned by a controller.
---@diagnostic disable-next-line: undefined-global -- Neovim owns the event loop and filesystem.
local nvim = vim
local Scope = require("louiselm.phone_control.scope")
local M = {}
local ZERO = string.rep("0", 32)
local LABEL_LIMIT = 512
local statuses = {
  starting = true,
  ready = true,
  admitting = true,
  configuring = true,
  preparing = true,
  prompting = true,
  running = true,
  waiting_permission = true,
  cancelling = true,
  error = true,
}

local function label(value)
  if type(value) ~= "string" then
    return nil
  end
  local index = 1
  while index <= #value and index <= LABEL_LIMIT do
    local first = value:byte(index)
    local width = first < 128 and 1
      or (first >= 194 and first <= 223 and 2)
      or (first >= 224 and first <= 239 and 3)
      or (first >= 240 and first <= 244 and 4)
    if first == 0 or not width or index + width - 1 > #value then
      return nil
    end
    if index + width - 1 > LABEL_LIMIT then
      return value:sub(1, index - 1), true
    end
    for offset = 1, width - 1 do
      local byte = value:byte(index + offset)
      if
        byte < 128
        or byte > 191
        or (offset == 1 and ((first == 224 and byte < 160) or (first == 237 and byte > 159)))
        or (offset == 1 and ((first == 240 and byte < 144) or (first == 244 and byte > 143)))
      then
        return nil
      end
    end
    index = index + width
  end
  return value:sub(1, index - 1), index <= #value
end

---List scoped live Sessions; filesystem resolution and completion are asynchronous.
---@param controller table Owning controller and its tracked Session incarnations.
---@param grant table Verified local grant.
---@param request table Verified list request.
---@param callback fun(result: table) Closed response or refusal.
function M.list(controller, grant, request, callback)
  local after = request.payload:sub(1, 32)
  local limit = tonumber(request.payload:sub(33, 34), 16)
  local owners, ids = {}, {}
  for session in pairs(controller.sessions) do
    owners[session.owner] = true
  end
  for owner in pairs(owners) do
    for _, id in ipairs(owner:list_sessions()) do
      local session = owner:get_session(id)
      local subject = controller.sessions[session]
      if subject ~= nil and subject.id ~= nil and subject.id > after then
        ids[#ids + 1] = subject.id
      end
    end
  end
  table.sort(ids)
  local rows, selected, index = {}, {}, 0
  local function fail(code)
    callback({ v = 1, type = "error", code = code })
  end
  local function next_session()
    index = index + 1
    local id = ids[index]
    if id == nil or #rows > limit then
      for _, saved in ipairs(selected) do
        local state = saved.session:inspect()
        if
          saved.session.owner:get_session(state.id) ~= saved.session
          or state.status == "disposed"
          or state.agent ~= saved.agent
          or state.working_dir ~= saved.working_dir
          or controller.subjects[saved.id] ~= saved.subject
        then
          fail("stale_subject")
          return
        end
        local name, shortened = label(state.name)
        if name == nil or not statuses[state.status] then
          fail("unavailable")
          return
        end
        saved.row.name = name
        saved.row.status = state.status
        saved.row.truncated = saved.truncated or shortened
      end
      local next_after = ""
      if #rows > limit then
        rows[#rows] = nil
        next_after = rows[#rows].subject_id
      end
      callback({
        v = 1,
        type = "session_list",
        controller_id = controller.controller_id,
        operation_id = request.operation_id,
        sessions = rows,
        next_after = next_after,
      })
      return
    end
    local subject = controller.subjects[id]
    if subject == nil then
      next_session()
      return
    end
    local session = subject.session
    local state = session:inspect()
    if
      state.status == "disposed"
      or session.owner:get_session(state.id) ~= session
      or not Scope.contains(grant.agents, state.agent)
    then
      next_session()
      return
    end
    if type(state.working_dir) ~= "string" or state.working_dir == "" then
      fail("unavailable")
      return
    end
    nvim.uv.fs_realpath(state.working_dir, function(err, canonical)
      nvim.schedule(function()
        if err ~= nil or type(canonical) ~= "string" then
          fail("unavailable")
        elseif not Scope.within_project(grant.projects, canonical) then
          next_session()
        else
          local name, name_short = label(state.name)
          local agent, agent_short = label(state.agent)
          local project, project_short = label(canonical)
          if name == nil or agent == nil or project == nil or not statuses[state.status] then
            fail("unavailable")
            return
          end
          local row = {
            subject_id = id,
            name = name,
            agent = agent,
            project = project,
            status = state.status,
            truncated = name_short or agent_short or project_short,
          }
          rows[#rows + 1] = row
          selected[#selected + 1] = {
            id = id,
            subject = subject,
            session = session,
            agent = state.agent,
            working_dir = state.working_dir,
            row = row,
            truncated = agent_short or project_short,
          }
          next_session()
        end
      end)
    end)
  end
  next_session()
end

return M
