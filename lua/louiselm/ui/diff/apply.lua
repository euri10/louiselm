local M = {}

---@diagnostic disable-next-line: undefined-global -- `vim` is Neovim's injected runtime API.
local nvim = vim

---@class louiselm.ui.DiffEdit
---@field path string File to inspect.
---@field diff? string Unified diff for one file.
---@field content? string Replacement file content.

---@class louiselm.ui.DiffPreview
---@field path string File being previewed.
---@field original string Content read before the preview.
---@field proposed string Content proposed by the agent.
---@field diff string Unified diff between original and proposed content.

local function split_lines(value)
  local lines = {}
  local start = 1
  while start <= #value do
    local newline = value:find("\n", start, true)
    if newline == nil then
      lines[#lines + 1] = value:sub(start)
      return lines, false
    end
    lines[#lines + 1] = value:sub(start, newline - 1)
    start = newline + 1
  end
  return lines, true
end

local function join_lines(lines, trailing_newline)
  local value = table.concat(lines, "\n")
  if trailing_newline and #lines > 0 then
    return value .. "\n"
  end
  return value
end

---@param path unknown
---@return string? normalized_path
---@return string? error_message
local function normalize_path(path)
  if type(path) ~= "string" or path == "" then
    return nil, "diff path must be a non-empty string"
  end
  return nvim.fs.normalize(nvim.fn.fnamemodify(path, ":p"))
end

---@param path string
---@return string? content
---@return string? error_message
function M.read(path)
  local normalized, path_error = normalize_path(path)
  if normalized == nil then
    return nil, path_error
  end
  local stat = nvim.uv.fs_stat(normalized)
  if stat == nil then
    return ""
  end
  if stat.type ~= "file" then
    return nil, "diff path is not a regular file"
  end
  local fd, open_error = nvim.uv.fs_open(normalized, "r", 420)
  if fd == nil then
    return nil, "could not read diff file: " .. tostring(open_error)
  end
  local content, read_error = nvim.uv.fs_read(fd, stat.size, 0)
  nvim.uv.fs_close(fd)
  if content == nil then
    return nil, "could not read diff file: " .. tostring(read_error)
  end
  return content
end

---@param value string
---@return string? proposed
---@return string? error_message
local function apply_unified_diff(original, value)
  local old_lines, old_trailing = split_lines(original)
  local patch_lines = split_lines(value)
  local result = {}
  local old_cursor = 1
  local found_hunk = false
  local proposed_trailing = old_trailing
  local index = 1

  while index <= #patch_lines do
    local line = patch_lines[index]
    local old_start, old_count, new_count = line:match("^@@ %-(%d+),?(%d*) %+%d+,?(%d*) @@")
    if old_start ~= nil then
      found_hunk = true
      old_start = tonumber(old_start)
      old_count = tonumber(old_count) or 1
      new_count = tonumber(new_count) or 1
      if old_start == 0 then
        old_start = 1
      end
      if old_start < old_cursor or old_start > #old_lines + 1 then
        return nil, "diff hunk starts outside the original file"
      end
      while old_cursor < old_start do
        result[#result + 1] = old_lines[old_cursor]
        old_cursor = old_cursor + 1
      end

      local consumed_old = 0
      local produced_new = 0
      index = index + 1
      while index <= #patch_lines and patch_lines[index]:sub(1, 2) ~= "@@" do
        local patch_line = patch_lines[index]
        local prefix = patch_line:sub(1, 1)
        local text = patch_line:sub(2)
        if prefix == " " then
          if old_lines[old_cursor] ~= text then
            return nil, "diff context does not match the original file"
          end
          result[#result + 1] = text
          old_cursor = old_cursor + 1
          consumed_old = consumed_old + 1
          produced_new = produced_new + 1
        elseif prefix == "-" then
          if old_lines[old_cursor] ~= text then
            return nil, "diff deletion does not match the original file"
          end
          old_cursor = old_cursor + 1
          consumed_old = consumed_old + 1
        elseif prefix == "+" then
          result[#result + 1] = text
          produced_new = produced_new + 1
        elseif patch_line == "\\ No newline at end of file" then
          proposed_trailing = false
        else
          return nil, "diff contains an unsupported line"
        end
        index = index + 1
      end
      if consumed_old ~= old_count or produced_new ~= new_count then
        return nil, "diff hunk line counts do not match"
      end
    elseif found_hunk and (line:match("^diff ") ~= nil or line:match("^%-%-%- ") ~= nil) then
      return nil, "diff must contain one file"
    else
      index = index + 1
    end
  end

  if not found_hunk then
    return nil, "diff must contain a unified hunk"
  end
  while old_cursor <= #old_lines do
    result[#result + 1] = old_lines[old_cursor]
    old_cursor = old_cursor + 1
  end
  return join_lines(result, proposed_trailing)
end

---@param edit unknown Agent file-edit payload.
---@return louiselm.ui.DiffEdit? normalized_edit
---@return string? error_message
function M.normalize(edit)
  if type(edit) ~= "table" then
    return nil, "diff edit must be a table"
  end
  local path, path_error = normalize_path(edit.path or edit.filePath or edit.file_path)
  if path == nil then
    return nil, path_error
  end
  local diff = edit.diff
  local content = edit.content or edit.newText or edit.new_text
  if diff ~= nil and type(diff) ~= "string" then
    return nil, "diff edit diff must be a string"
  end
  if content ~= nil and type(content) ~= "string" then
    return nil, "diff edit content must be a string"
  end
  if diff ~= nil and content ~= nil then
    return nil, "diff edit cannot contain both diff and content"
  end
  if diff == nil and content == nil then
    return nil, "diff edit must contain diff or content"
  end
  return { path = path, diff = diff, content = content }
end

---Build a file preview from an ACP edit payload without changing the file.
---@param edit louiselm.ui.DiffEdit Agent file-edit payload.
---@return louiselm.ui.DiffPreview? preview Preview, or nil on invalid input or filesystem error.
---@return string? error_message Validation or filesystem error.
function M.preview(edit)
  local normalized, normalize_error = M.normalize(edit)
  if normalized == nil then
    return nil, normalize_error
  end
  local original, read_error = M.read(normalized.path)
  if original == nil then
    return nil, read_error
  end
  local proposed = normalized.content
  if proposed == nil then
    proposed, read_error = apply_unified_diff(original, normalized.diff)
    if proposed == nil then
      return nil, read_error
    end
  end
  return {
    path = normalized.path,
    original = original,
    proposed = proposed,
    diff = nvim.diff(original, proposed, { result_type = "unified", ctxlen = 3 }),
  }
end

---@param preview unknown Preview returned by `preview`.
---@return boolean applied True when the proposed content was written.
---@return string? error_message Validation, stale-file, or filesystem error.
function M.apply(preview)
  if type(preview) ~= "table" then
    return false, "diff preview must be a table"
  end
  local path, path_error = normalize_path(preview.path)
  if path == nil then
    return false, path_error
  end
  if type(preview.original) ~= "string" or type(preview.proposed) ~= "string" then
    return false, "diff preview must contain original and proposed content"
  end
  local current, read_error = M.read(path)
  if current == nil then
    return false, read_error
  end
  if current ~= preview.original then
    return false, "file changed since diff preview"
  end
  local fd, open_error = nvim.uv.fs_open(path, "w", 420)
  if fd == nil then
    return false, "could not write diff file: " .. tostring(open_error)
  end
  local written, write_error = nvim.uv.fs_write(fd, preview.proposed, 0)
  nvim.uv.fs_close(fd)
  if written ~= #preview.proposed then
    return false, "could not write diff file: " .. tostring(write_error or "short write")
  end
  return true
end

return M
