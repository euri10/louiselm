local Record = require("louiselm.forensics.record")

---@class louiselm.forensics.Store
---@field directory string Private record directory.
---@field write fun(self: louiselm.forensics.Store, record: louiselm.forensics.Record): string?, string? Write one immutable record.
---@field read fun(self: louiselm.forensics.Store, path: string): louiselm.forensics.Record?, string? Read and validate one record.

local M = {}
local Store = {}
Store.__index = Store

---@return table
local function nvim()
  ---@diagnostic disable-next-line: undefined-global -- `vim` is Neovim's injected runtime API.
  return vim
end

---@param value unknown
---@return string
local function record_name(value)
  return string.format("%d-%s.json", os.time(), value)
end

---@param directory string
---@return louiselm.forensics.Store? store
---@return string? error_message
function M.new(directory)
  if type(directory) ~= "string" or directory == "" then
    return nil, "forensics directory must be a non-empty path"
  end
  return setmetatable({ directory = directory }, Store), nil
end

---@param self louiselm.forensics.Store
---@param record louiselm.forensics.Record
---@return string? path
---@return string? error_message
function Store:write(record)
  local checked, validation_error = Record.build(record)
  if checked == nil then
    return nil, validation_error
  end
  local editor = nvim()
  if editor.fn.mkdir(self.directory, "p", 448) == 0 and editor.fn.isdirectory(self.directory) ~= 1 then
    return nil, "could not create forensics directory"
  end
  local encoded_ok, content = pcall(editor.json.encode, checked)
  if not encoded_ok then
    return nil, "could not encode forensics record"
  end
  local path = editor.fs.joinpath(self.directory, record_name(checked.id))
  local file, temporary_or_error = editor.uv.fs_mkstemp(path .. ".tmp-XXXXXX")
  if file == nil then
    return nil, "could not create temporary forensics record: " .. tostring(temporary_or_error)
  end
  local temporary = temporary_or_error
  local written, write_error = editor.uv.fs_write(file, content, 0)
  if written ~= #content then
    editor.uv.fs_close(file)
    editor.uv.fs_unlink(temporary)
    return nil, "could not write forensics record: " .. tostring(write_error or "short write")
  end
  local synced, sync_error = editor.uv.fs_fsync(file)
  if not synced then
    editor.uv.fs_close(file)
    editor.uv.fs_unlink(temporary)
    return nil, "could not sync forensics record: " .. tostring(sync_error)
  end
  local closed, close_error = editor.uv.fs_close(file)
  if not closed then
    editor.uv.fs_unlink(temporary)
    return nil, "could not close forensics record: " .. tostring(close_error)
  end
  local renamed, rename_error = editor.uv.fs_rename(temporary, path)
  if not renamed then
    editor.uv.fs_unlink(temporary)
    return nil, "could not publish forensics record: " .. tostring(rename_error)
  end
  editor.uv.fs_chmod(path, 384)
  return path, nil
end

---@param self louiselm.forensics.Store
---@param path string
---@return louiselm.forensics.Record? record
---@return string? error_message
function Store:read(path)
  if type(path) ~= "string" or path == "" then
    return nil, "forensics record path must be a non-empty string"
  end
  local editor = nvim()
  local stat = editor.uv.fs_stat(path)
  if stat == nil or stat.type ~= "file" then
    return nil, "forensics record is not a regular file"
  end
  local file, open_error = editor.uv.fs_open(path, "r", 384)
  if file == nil then
    return nil, "could not read forensics record: " .. tostring(open_error)
  end
  local content, read_error = editor.uv.fs_read(file, stat.size, 0)
  editor.uv.fs_close(file)
  if content == nil then
    return nil, "could not read forensics record: " .. tostring(read_error)
  end
  local decoded_ok, decoded = pcall(editor.json.decode, content)
  if not decoded_ok then
    return nil, "forensics record is not valid JSON"
  end
  return Record.build(decoded)
end

return M
