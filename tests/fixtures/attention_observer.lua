-- Cross-crate gate, not mini.test: expected drafts come from actual trusted
-- broker outbox output in attention_delivery.rs, in observed producer order.
---@diagnostic disable-next-line: undefined-global -- Neovim fixture runtime.
local nvim = vim
nvim.opt.rtp:prepend(nvim.fn.getcwd())
local ok, err = pcall(function()
  local expected = nvim.json.decode(assert(nvim.env.LOUISELM_ATTENTION_EXPECTED))
  local view = assert(require("louiselm.ui.attention_view").open(assert(nvim.env.LOUISELM_ATTENTION_SOCKET)))
  local ready = nvim.wait(5000, function()
    local text = table.concat(nvim.api.nvim_buf_get_lines(view.buffer, 0, -1, false), "\n")
    if #expected == 0 then
      return text:find("No unresolved", 1, true) ~= nil
    end
    local _, operations = text:gsub("  operation:", "")
    if operations ~= #expected then
      return false
    end
    for _, item in ipairs(expected) do
      if
        not text:find(item.source_operation_id, 1, true)
        or not text:find(item.subject_id, 1, true)
        or (item.code and not text:find(item.code, 1, true))
      then
        return false
      end
    end
    return true
  end)
  assert(ready, "durable Attention view did not match trusted broker output")
  assert(not nvim.bo[view.buffer].modifiable)
  view:dispose()
end)
if not ok then
  io.stderr:write(tostring(err) .. "\n")
  nvim.cmd("cquit 1")
end
nvim.cmd("qa!")
