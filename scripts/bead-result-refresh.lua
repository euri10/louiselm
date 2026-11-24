-- Explicit private-TTY refresh entrypoint; selection is JSON, never executable Lua.
---@diagnostic disable-next-line: undefined-global -- Neovim operator script.
local nvim = vim
local function refuse(message)
  nvim.notify(message, nvim.log.levels.ERROR)
end
local path = nvim.g.louiselm_saved_refresh_selection
local metadata = type(path) == "string" and nvim.uv.fs_lstat(path) or nil
if
  not metadata
  or metadata.type ~= "file"
  or metadata.size > 65536
  or metadata.uid ~= nvim.uv.getuid()
  or metadata.mode % 512 ~= 384
then
  return refuse("Refresh requires an operator-owned mode-0600 JSON selection, at most 64KiB")
end
local read, lines = pcall(nvim.fn.readfile, path)
if not read then
  return refuse("Refresh selection is unavailable")
end
local decoded, selection = pcall(nvim.json.decode, table.concat(lines, "\n"))
if not decoded or type(selection) ~= "table" or selection.system ~= nil or selection.session_api ~= nil then
  return refuse("Refresh selection is invalid JSON or contains test seams")
end
local function command(verb, payload, callback)
  local started = pcall(
    nvim.system,
    { "louiselm-control", "promotion", verb, "--json" },
    { stdin = nvim.json.encode(payload), text = true, cwd = "/" },
    function(output)
      nvim.schedule(function()
        if output.code ~= 0 then
          return refuse("Saved result operation refused or uncertain; inspect before retry")
        end
        local parsed, response = pcall(nvim.json.decode, output.stdout)
        if not parsed or type(response) ~= "table" then
          return refuse("Saved result response unavailable")
        end
        callback(response)
      end)
    end
  )
  if not started then
    refuse("Installed saved result control unavailable")
  end
end
nvim.ui.select(
  { "Refresh with this fresh one-verifier approval", "Cancel" },
  { prompt = "Refresh saved work? No paid worker or automatic acceptance." },
  function(choice)
    if choice ~= "Refresh with this fresh one-verifier approval" then
      return
    end
    local cancel, err = require("louiselm.workflow.saved_result").refresh(selection, function(result, refresh_error)
      if not result then
        return refuse(refresh_error or "Refresh did not complete; original remains pending")
      end
      command("preview", { result_id = result.request.request_id }, function(preview)
        require("louiselm.ui.run_promotion").prompt(preview, function(accepted)
          if accepted == nil then
            return
          end
          command(accepted and "commit" or "discard", {
            result_id = result.request.request_id,
            approval_digest = accepted and preview.approval_digest or nil,
          }, function()
            nvim.notify(accepted and "Refreshed result accepted; no successor started" or "Saved result discarded")
          end)
        end)
      end)
    end)
    if not cancel then
      return refuse(err or "Refresh selection refused")
    end
  end
)
