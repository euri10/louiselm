---@diagnostic disable-next-line: undefined-global -- Neovim test runtime.
local nvim = vim

-- Controller tests use the same asynchronous callback and cancellation shape as the Session owner.
return function(session, controller)
  session.history = { revision = 1 }
  function session:read_history(request, callback)
    local cancelled = false
    nvim.schedule(function()
      if cancelled then
        return
      end
      local projection = controller.sessions[session].projection
      local page, err = projection:page(request.before, request.limit, request.revision)
      if page ~= nil then
        while #nvim.json.encode(page.entries) > 245760 and #page.entries > 1 do
          table.remove(page.entries, 1)
          page.next_before = page.entries[1].seq
        end
        page.subject = "fixture-subject"
        page.source = {
          replay = "not_applicable",
          archive = "unavailable",
          redaction = "unknown",
          missing = false,
          truncated = false,
        }
      end
      callback(page, err)
    end)
    return function()
      cancelled = true
    end
  end
end
