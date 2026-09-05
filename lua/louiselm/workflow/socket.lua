---@diagnostic disable-next-line: undefined-global -- `vim` is Neovim's injected runtime API.
local nvim = vim
local M = {}

---Stop reads and close an open pipe; an absent or closing pipe is a no-op.
---@param pipe? table Libuv-compatible pipe handle.
function M.close_pipe(pipe)
  if pipe ~= nil and not pipe:is_closing() then
    pipe:read_stop()
    pipe:close()
  end
end

---Drain pending requests before notifying them, including on reentrant disposal.
---@param client {pending: table<string, fun(value: nil, error_message: string)>} Mutated in place.
---@param message string Failure delivered to each pending callback.
function M.fail_pending(client, message)
  local pending = client.pending
  client.pending = {}
  for _, callback in pairs(pending) do
    callback(nil, message)
  end
end

---Buffer partial frames and schedule complete lines in order on the main loop.
---The protocol handler owns validation and must ignore disposed clients.
---@param client {buffer: string} Mutated in place.
---@param chunk string Received bytes.
---@param handle_line fun(client: table, line: string)
function M.consume(client, chunk, handle_line)
  client.buffer = client.buffer .. chunk
  while true do
    local newline = client.buffer:find("\n", 1, true)
    if newline == nil then
      return
    end
    local line = client.buffer:sub(1, newline - 1)
    client.buffer = client.buffer:sub(newline + 1)
    nvim.schedule(function()
      handle_line(client, line)
    end)
  end
end

return M
