---@alias louiselm.session.TranscriptLayout "claude"|"codex"|"openai-compatible"|"copilot"

---@class louiselm.session.TranscriptDefinition
---@field transcript_layout? string On-disk transcript layout for this Agent.

---@class louiselm.session.LocatorOptions
---@field roots? table<string, string> Absolute root overrides keyed by transcript layout.

local M = {}

---@return table
local function nvim()
  ---@diagnostic disable-next-line: undefined-global -- `vim` provides filesystem APIs in Neovim.
  return vim
end

---@param path string
---@return boolean
local function is_file(path)
  local stat = nvim().uv.fs_stat(path)
  return stat ~= nil and stat.type == "file"
end

---@param path string
---@return boolean
local function is_absolute(path)
  return nvim().fs.abspath(path) == nvim().fs.normalize(path)
end

---@param root string
---@param name string|fun(name: string, path: string): boolean
---@return string?
local function find_file(root, name)
  if nvim().uv.fs_stat(root) == nil then
    return nil
  end
  return nvim().fs.find(name, { path = root, type = "file", limit = 1 })[1]
end

---@param root string
---@param session_id string
---@return string?
local function claude_path(root, session_id)
  return find_file(nvim().fs.joinpath(root, "projects"), session_id .. ".jsonl")
end

---@param root string
---@param session_id string
---@return string?
local function codex_path(root, session_id)
  local suffix = "-" .. session_id .. ".jsonl"
  return find_file(nvim().fs.joinpath(root, "sessions"), function(name)
    return name:sub(1, 8) == "rollout-" and name:sub(-#suffix) == suffix
  end)
end

---@param root string
---@param session_id string
---@return string?
local function openai_compatible_path(root, session_id)
  local path = nvim().fs.joinpath(root, "sessions", session_id, "history.jsonl")
  return is_file(path) and path or nil
end

---@param root string
---@param session_id string
---@return string?
local function copilot_path(root, session_id)
  local path = nvim().fs.joinpath(root, "session-state", session_id, "events.jsonl")
  return is_file(path) and path or nil
end

---@type table<louiselm.session.TranscriptLayout, fun(root: string, session_id: string): string?>
local resolvers = {
  claude = claude_path,
  codex = codex_path,
  ["openai-compatible"] = openai_compatible_path,
  copilot = copilot_path,
}

---@return table<string, string>? roots
---@return string? error_message
local function default_roots()
  local home = os.getenv("HOME")
  if home == nil or home == "" then
    return nil, "cannot resolve transcript roots because HOME is unset"
  end
  local state_home = os.getenv("XDG_STATE_HOME")
  if state_home == nil or state_home == "" then
    state_home = nvim().fs.joinpath(home, ".local", "state")
  end
  local codex_home = os.getenv("CODEX_HOME")
  if codex_home == nil or codex_home == "" then
    codex_home = nvim().fs.joinpath(home, ".codex")
  end
  return {
    claude = nvim().fs.joinpath(home, ".claude"),
    codex = codex_home,
    ["openai-compatible"] = nvim().fs.joinpath(state_home, "acp-llm-adapter"),
    copilot = nvim().fs.joinpath(home, ".copilot"),
  },
    nil
end

---@param definitions table<string, louiselm.session.TranscriptDefinition>
---@return string[]? layouts
---@return boolean? automatic Whether all supported layouts should be searched.
---@return string? error_message
local function configured_layouts(definitions)
  local seen = {}
  local layouts = {}
  local invalid = false
  for _, definition in pairs(definitions) do
    local layout = type(definition) == "table" and definition.transcript_layout or nil
    if layout ~= nil then
      if type(layout) ~= "string" or layout == "" then
        invalid = true
      elseif not seen[layout] then
        seen[layout] = true
        layouts[#layouts + 1] = layout
      end
    end
  end
  if invalid then
    return nil, nil, "configured transcript layouts must be non-empty strings"
  end
  table.sort(layouts)
  if #layouts == 0 then
    for layout in pairs(resolvers) do
      layouts[#layouts + 1] = layout
    end
    table.sort(layouts)
    return layouts, true, nil
  end
  for _, layout in ipairs(layouts) do
    if resolvers[layout] == nil then
      return nil, nil, "unknown transcript layout '" .. layout .. "'"
    end
  end
  return layouts, false, nil
end

---Resolve an attributed Session id to an existing absolute transcript path.
---@param attributed_session_id string Session id shaped as `<agent>/<session-id>`; the Agent prefix is attribution only.
---@param definitions table<string, louiselm.session.TranscriptDefinition> Current Agent definitions; explicit layouts are searched, or all supported layouts when none are configured.
---@param options? louiselm.session.LocatorOptions Test or host-specific root overrides.
---@return string? path Absolute transcript path, or nil when validation or lookup fails.
---@return string? error_message Actionable malformed-id, layout, root, or missing-transcript failure.
function M.resolve(attributed_session_id, definitions, options)
  if type(attributed_session_id) ~= "string" then
    return nil, "invalid Session id: expected <agent>/<session-id>"
  end
  local agent_name, session_id = attributed_session_id:match("^([^/]+)/([^/]+)$")
  if agent_name == nil or session_id == nil or session_id == "." or session_id == ".." then
    return nil, "invalid Session id: expected <agent>/<session-id>"
  end
  if session_id:find("\\", 1, true) or session_id:find("%z") then
    return nil, "invalid Session id: expected <agent>/<session-id>"
  end
  if type(definitions) ~= "table" then
    return nil, "transcript resolution requires configured Agent definitions"
  end
  local layouts, automatic, layout_error = configured_layouts(definitions)
  if layouts == nil then
    return nil, layout_error
  end
  if options ~= nil and type(options) ~= "table" then
    return nil, "transcript locator options must be a table"
  end
  ---@type table<string, string>?
  local roots
  if options ~= nil and options.roots ~= nil then
    if type(options.roots) ~= "table" then
      return nil, "transcript locator roots must be a table"
    end
    roots = options.roots
  else
    local roots_error
    roots, roots_error = default_roots()
    if roots == nil then
      return nil, roots_error
    end
  end
  if roots == nil then
    return nil, "transcript roots are unavailable"
  end

  local matches = {}
  for _, layout in ipairs(layouts) do
    local root = roots[layout]
    if type(root) ~= "string" or root == "" or not is_absolute(root) then
      return nil, "transcript root for layout '" .. layout .. "' must be an absolute path"
    end
    local path = resolvers[layout](root, session_id)
    if path ~= nil then
      matches[#matches + 1] = { layout = layout, path = path }
    end
  end

  if #matches == 1 then
    return matches[1].path, nil
  end
  if #matches > 1 then
    local matching_layouts = {}
    for _, match in ipairs(matches) do
      matching_layouts[#matching_layouts + 1] = match.layout
    end
    return nil,
      "ambiguous transcript for Session '" .. attributed_session_id .. "' found in layouts: " .. table.concat(
        matching_layouts,
        ", "
      )
  end

  local qualifier = #layouts == 1 and "layout: " or "layouts: "
  local scope = automatic and "supported " or "configured "
  return nil,
    "transcript not found for Session '" .. attributed_session_id .. "' in " .. scope .. qualifier .. table.concat(
      layouts,
      ", "
    )
end

return M
