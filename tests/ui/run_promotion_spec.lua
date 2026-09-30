local MiniTest = require("mini.test")
local Promotion = require("louiselm.ui.run_promotion")
---@diagnostic disable-next-line: undefined-global -- Neovim runtime API.
local nvim = vim

local T = MiniTest.new_set()

T["promotion prompt shows paths and accepts only the exact choice"] = function()
  local original = nvim.ui.select
  MiniTest.finally(function()
    nvim.ui.select = original
  end)
  local prompt, choose, decision
  nvim.ui.select = function(_, options, callback)
    prompt, choose = options.prompt, callback
  end
  Promotion.prompt({
    selection = { bead_id = "bead-1", run_id = "run-1" },
    approval_digest = "sha256:approval",
    changes = { added = { "new.txt" }, modified = { "old.txt" }, deleted = {} },
    tainted_review_digest = nvim.NIL,
  }, function(accepted)
    decision = accepted
  end)
  MiniTest.expect.equality(prompt:find("+ new.txt", 1, true) ~= nil, true)
  MiniTest.expect.equality(prompt:find("~ old.txt", 1, true) ~= nil, true)
  choose("Accept exact preview")
  MiniTest.expect.equality(decision, true)
end

T["tainted promotion shows the exact-use review"] = function()
  local original = nvim.ui.select
  MiniTest.finally(function()
    nvim.ui.select = original
  end)
  local prompt
  nvim.ui.select = function(_, options, callback)
    prompt = options.prompt
    callback("Reject")
  end
  local rejected
  Promotion.prompt({
    selection = { bead_id = "bead-1", run_id = "run-1" },
    approval_digest = "sha256:approval",
    changes = { added = {}, modified = {}, deleted = {} },
    tainted_review = {
      taint_digest = "sha256:taint",
      output_digest = "sha256:output",
      request_digest = "sha256:request",
      action = "workspace_promotion",
      destination = { uid = 1000, device = 1, inode = 2 },
    },
    tainted_review_digest = "sha256:review",
  }, function(accepted)
    rejected = accepted
  end)
  MiniTest.expect.equality(rejected, false)
  for _, value in ipairs({ "sha256:taint", "sha256:output", "sha256:request", "sha256:review" }) do
    MiniTest.expect.equality(prompt:find(value, 1, true) ~= nil, true)
  end
end

return T
