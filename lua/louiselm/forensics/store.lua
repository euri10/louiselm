local PrivateFile = require("louiselm.private_file")
local Record = require("louiselm.forensics.record")

---@class louiselm.forensics.Store
---@field directory string Private record directory.
---@field write fun(self: louiselm.forensics.Store, record: louiselm.forensics.Record): string?, string? Write one immutable record.
---@field read fun(self: louiselm.forensics.Store, path: string): louiselm.forensics.Record?, string? Read and validate one record.
---@field inspect fun(self: louiselm.forensics.Store, path: string): louiselm.forensics.Inspection?, string? Read a record and report current evidence availability without mutating it.

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

local RECORDED_AVAILABILITY = {
  present = "available",
  absent = "missing",
  inaccessible = "unreadable",
  unsupported = "missing",
  omitted = "missing",
}

---@param code string?
---@return boolean
local function is_missing(code)
  return code == "ENOENT" or code == "ENOTDIR"
end

---@param editor table
---@param source louiselm.forensics.EvidenceSource
---@return louiselm.forensics.AvailabilityState
local function current_availability(editor, source)
  if source.path == nil then
    if source.state == "present" and source.kind ~= "git" then
      return "missing"
    end
    return RECORDED_AVAILABILITY[source.state]
  end
  local read_flags = editor.uv.constants.O_RDONLY + editor.uv.constants.O_NONBLOCK
  local file, _, open_code = editor.uv.fs_open(source.path, read_flags, 384)
  if file == nil then
    return is_missing(open_code) and "missing" or "unreadable"
  end
  local opened_stat = editor.uv.fs_fstat(file)
  local closed = editor.uv.fs_close(file)
  if opened_stat == nil or opened_stat.type ~= "file" or not closed then
    return "unreadable"
  end
  return "available"
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
  local written, write_error = PrivateFile.write(editor.uv, path, content, "forensics record", "publish")
  if not written then
    return nil, write_error
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
  local read_flags = editor.uv.constants.O_RDONLY + editor.uv.constants.O_NONBLOCK
  local file, open_error, open_code = editor.uv.fs_open(path, read_flags, 384)
  if file == nil then
    if is_missing(open_code) then
      return nil, "forensics record is not a regular file"
    end
    return nil, "could not read forensics record: " .. tostring(open_error)
  end
  local stat, stat_error = editor.uv.fs_fstat(file)
  if stat == nil then
    editor.uv.fs_close(file)
    return nil, "could not inspect forensics record: " .. tostring(stat_error)
  end
  if stat.type ~= "file" then
    editor.uv.fs_close(file)
    return nil, "forensics record is not a regular file"
  end
  local content, read_error = editor.uv.fs_read(file, stat.size, 0)
  local closed, close_error = editor.uv.fs_close(file)
  if content == nil then
    return nil, "could not read forensics record: " .. tostring(read_error)
  end
  if not closed then
    return nil, "could not close forensics record: " .. tostring(close_error)
  end
  local decoded_ok, decoded = pcall(editor.json.decode, content)
  if not decoded_ok then
    return nil, "forensics record is not valid JSON"
  end
  return Record.build(decoded)
end

---Read a Forensics record and derive current evidence availability without changing it.
---@param self louiselm.forensics.Store
---@param path string
---@return louiselm.forensics.Inspection? inspection
---@return string? error_message
function Store:inspect(path)
  local record, read_error = self:read(path)
  if record == nil then
    return nil, read_error
  end
  local editor = nvim()
  local source_availability = {}
  for index, source in ipairs(record.evidence_sources) do
    source_availability[index] = current_availability(editor, source)
  end
  return Record.with_availability(record, source_availability), nil
end

return M
