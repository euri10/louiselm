local M = {}

---Publish bytes through a private temporary file; retain the destination on failure.
---The caller owns directory creation, encoding, and any post-publication work.
---@param uv table Libuv filesystem operations.
---@param path string Destination path.
---@param content string Encoded file bytes.
---@param name string File description used in returned errors.
---@param rename_action "replace"|"publish" Caller-specific publication error wording.
---@return boolean written
---@return string? error_message Failed filesystem operation and its detail.
function M.write(uv, path, content, name, rename_action)
  local file, temporary_or_error = uv.fs_mkstemp(path .. ".tmp-XXXXXX")
  if file == nil then
    return false, "could not create temporary " .. name .. ": " .. tostring(temporary_or_error)
  end
  local temporary = temporary_or_error
  local written, write_error = uv.fs_write(file, content, 0)
  if written ~= #content then
    uv.fs_close(file)
    uv.fs_unlink(temporary)
    return false, "could not write " .. name .. ": " .. tostring(write_error or "short write")
  end
  local synced, sync_error = uv.fs_fsync(file)
  if not synced then
    uv.fs_close(file)
    uv.fs_unlink(temporary)
    return false, "could not sync " .. name .. ": " .. tostring(sync_error)
  end
  local closed, close_error = uv.fs_close(file)
  if not closed then
    uv.fs_unlink(temporary)
    return false, "could not close " .. name .. ": " .. tostring(close_error)
  end
  local renamed, rename_error = uv.fs_rename(temporary, path)
  if not renamed then
    uv.fs_unlink(temporary)
    return false, "could not " .. rename_action .. " " .. name .. ": " .. tostring(rename_error)
  end
  return true, nil
end

return M
