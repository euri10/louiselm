---Run-owned generated-work ledger reached through the capture service.

local M = {}
local Compatibility = require("louiselm.capture.compatibility")

---@diagnostic disable-next-line: undefined-global -- `vim` is Neovim's injected runtime API.
local nvim = vim

---@class louiselm.workflow.LedgerRecord
---@field id string Run UUID.
---@field token string Generate capability.

---@class louiselm.workflow.LedgerOptions
---@field capture? string Path to the `louiselm-capture` executable.
---@field system? fun(command: string[], options: table, callback: fun(result: table)): unknown

---@class louiselm.workflow.LedgerContext
---@field id string
---@field token string
---@field capture string
---@field system fun(command: string[], options: table, callback: fun(result: table)): unknown

---Report a ledger failure without leaking the Run's generate capability.
---@param result table Completed `vim.system` result.
---@return string message
local function failure(result)
  local stderr = type(result.stderr) == "string" and nvim.trim(result.stderr) or ""
  if stderr ~= "" then
    return Compatibility.command_error(stderr)
  end
  return string.format("Run ledger command failed with status %s", tostring(result.code))
end

---Run one `louiselm-capture run` ledger subcommand.
---
---The Run id and token travel in the environment rather than in the argument
---vector, because argv is readable from the process table by every other user
---on the host.
---@param context louiselm.workflow.LedgerContext
---@param arguments string[]
---@param callback fun(state: string?, error_message?: string)
local function invoke(context, arguments, callback)
  local command = { context.capture, "--require-interface=1", "run" }
  nvim.list_extend(command, arguments)
  local options = {
    text = true,
    env = { LOUISELM_RUN_ID = context.id, LOUISELM_RUN_TOKEN = context.token },
  }
  local started = pcall(context.system, command, options, function(result)
    nvim.schedule(function()
      if result.code ~= 0 then
        return callback(nil, failure(result))
      end
      local decoded, response = pcall(nvim.json.decode, result.stdout)
      if not decoded or type(response) ~= "table" or type(response.state) ~= "string" then
        return callback(nil, "Run ledger returned invalid data")
      end
      return callback(response.state)
    end)
  end)
  if not started then
    nvim.schedule(function()
      callback(nil, "could not start the Run ledger command")
    end)
  end
end

---Create a ledger bound to one admitted Run.
---@param record louiselm.workflow.LedgerRecord
---@param options? louiselm.workflow.LedgerOptions
---@return louiselm.workflow.ExecutorLedger? ledger
---@return string? error_message
function M.new(record, options)
  if type(record) ~= "table" or type(record.id) ~= "string" or record.id == "" then
    return nil, "Run ledger id must be a non-empty string"
  end
  if type(record.token) ~= "string" or record.token == "" then
    return nil, "Run ledger token must be a non-empty string"
  end
  options = options or {}
  local capture = options.capture or nvim.fn.exepath("louiselm-capture")
  if type(capture) ~= "string" or capture == "" then
    return nil, "Run ledger requires the louiselm-capture executable"
  end
  ---@type louiselm.workflow.LedgerContext
  local context = {
    id = record.id,
    token = record.token,
    capture = capture,
    system = options.system or nvim.system,
  }

  ---@param mutation_id string
  ---@param kind string
  ---@param units integer
  ---@param callback fun(state: string?, error_message?: string)
  local function reserve(mutation_id, kind, units, callback)
    invoke(context, {
      "reserve",
      "--mutation-id",
      mutation_id,
      "--kind",
      kind,
      "--units",
      tostring(units),
    }, callback)
  end

  ---@param mutation_id string
  ---@param output_id string
  ---@param callback fun(confirmed: boolean, error_message?: string)
  local function confirm(mutation_id, output_id, callback)
    invoke(context, {
      "confirm",
      "--mutation-id",
      mutation_id,
      "--issue-id",
      output_id,
    }, function(state, error_message)
      return callback(state == "confirmed", error_message)
    end)
  end

  return {
    reserve = reserve,
    confirm = confirm,
    ---Charge one unit for an automatic back-edge traversal.
    ---
    ---A back-edge produces no Beads issue, so there is no external id to confirm
    ---against. The traversal's own mutation id is used: it is unique per
    ---traversal and non-empty, it makes the recorded output row self-describing
    ---as `kind=back_edge, external_id=<mutation id>`, and replaying it hits the
    ---store's existing same-identity short circuit rather than double-charging.
    ---@param mutation_id string
    ---@param kind string
    ---@param units integer
    ---@param callback fun(state: string?, error_message?: string)
    consume = function(mutation_id, kind, units, callback)
      reserve(mutation_id, kind, units, function(state, reserve_error)
        if state == "consumed" then
          return callback("consumed")
        end
        if state ~= "reserved" and state ~= "pending" then
          return callback(state, reserve_error)
        end
        confirm(mutation_id, mutation_id, function(confirmed, confirm_error)
          if not confirmed then
            return callback(nil, confirm_error)
          end
          return callback("consumed")
        end)
      end)
    end,
    ---@param mutation_id string
    ---@param callback fun(released: boolean, error_message?: string)
    release = function(mutation_id, callback)
      invoke(context, { "release", "--mutation-id", mutation_id }, function(state, error_message)
        return callback(state == "released", error_message)
      end)
    end,
  }
end

return M
