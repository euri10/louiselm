---@class louiselm.provenance.Node
---@field kind "commit"|"issue"|"session" Node kind.
---@field id string Stable identifier for the node.

---@class louiselm.provenance.Commit
---@field id string Full commit object id.
---@field message string Commit message, including trailers.

---@class louiselm.provenance.Edge
---@field source louiselm.provenance.Node Commit that recorded the reference.
---@field target louiselm.provenance.Node Issue or Session named by the trailer.
---@field relation "refs" The recorded relationship.

---@class louiselm.provenance.Error
---@field code string Stable machine-readable error code.
---@field message string Human-readable error description.
---@field index? integer Input index associated with the error.

local M = {}

---@param code string
---@param message string
---@param index? integer
---@return louiselm.provenance.Error
local function make_error(code, message, index)
  return { code = code, message = message, index = index }
end

---@param value unknown
---@param index integer
---@return louiselm.provenance.Commit? commit
---@return louiselm.provenance.Error? error_value
local function validate_commit(value, index)
  if type(value) ~= "table" then
    return nil, make_error("invalid_commit", "commit must be a table", index)
  end
  if type(value.id) ~= "string" or value.id == "" then
    return nil, make_error("invalid_commit", "commit id must be a non-empty string", index)
  end
  if type(value.message) ~= "string" then
    return nil, make_error("invalid_commit", "commit message must be a string", index)
  end
  return { id = value.id, message = value.message }, nil
end

---@param reference string
---@return "issue"|"session"?
local function reference_kind(reference)
  if reference:find("/", 1, true) ~= nil then
    return "session"
  end
  if reference:match("^[%w][%w%._/-]*$") ~= nil then
    return "issue"
  end
  return nil
end

---@param message string
---@return string[]
local function references(message)
  local result = {}
  local seen = {}
  for line in message:gmatch("[^\r\n]+") do
    local values = line:match("^%s*Refs%s+(.+)%s*$")
    if values ~= nil then
      for reference in values:gmatch("%S+") do
        reference = reference:gsub("[,;]$", "")
        if reference_kind(reference) ~= nil and not seen[reference] then
          seen[reference] = true
          result[#result + 1] = reference
        end
      end
    end
  end
  return result
end

---Build explicit Provenance edges from already-collected git commits.
---This function performs no filesystem, process, or editor I/O.
---@param commits louiselm.provenance.Commit[] Collected commits in git order.
---@return louiselm.provenance.Edge[]? edges Edges in commit and trailer order.
---@return louiselm.provenance.Error? error_value Malformed input, if any.
function M.commits(commits)
  if type(commits) ~= "table" then
    return nil, make_error("invalid_commits", "commits must be an array")
  end

  local edges = {}
  for index, value in ipairs(commits) do
    local commit, validation_error = validate_commit(value, index)
    if commit == nil then
      return nil, validation_error
    end
    for _, reference in ipairs(references(commit.message)) do
      edges[#edges + 1] = {
        source = { kind = "commit", id = commit.id },
        target = { kind = reference_kind(reference), id = reference },
        relation = "refs",
      }
    end
  end
  return edges, nil
end

return M
