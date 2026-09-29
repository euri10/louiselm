-- Invoked only by the private-mount disposable-VM Rust fixture. Authorization,
-- launching, ACP permission replies and supervisor cleanup are production paths.
-- Capture admission/attachment is doubled here; its real CLI has separate gates.
---@diagnostic disable-next-line: undefined-global -- Neovim standalone fixture.
local nvim = vim
nvim.opt.rtp:prepend(nvim.fn.getcwd())
local Workflow = require("louiselm.workflow")
local input = nvim.json.decode(table.concat(nvim.fn.readfile(nvim.env.LOUISELM_BEADS_FIXTURE), "\n"))
local completed, failure, count = false, nil, 0
local controller
local function failed(err)
  failure = failure or tostring(err)
  completed = true
end
controller = assert(Workflow.new_bead_executor({
  agent_id = "agent",
  envelope = input.envelope,
  bead_ids = { "fixture-1", "fixture-2", "fixture-3" },
  system = function(argv, options, done)
    if argv[1] == "louiselm-capture" then
      nvim.schedule(function()
        done({ code = 0, stdout = '{"token":"fixture-capture-only"}', stderr = "" })
      end)
      return nil
    end
    return nvim.system(argv, options, function(result)
      if result.code ~= 0 then
        nvim.schedule(function()
          failed("installed " .. nvim.json.decode(options.stdin).kind .. " authorization refused: " .. result.stderr)
        end)
      end
      done(result)
    end)
  end,
  prepare = function(id, done)
    print("installed Lua worker: " .. id)
    local index = assert(tonumber(id:match("%d+$")))
    assert(count == index - 1, "preparation overtook verification")
    nvim.schedule(function()
      done({
        grant = input.grants[index],
        prompt = index == 3 and "unapprovable" or "Complete this offline fixture turn.",
      })
    end)
    return true
  end,
  on_worker = function(result, continue)
    if result.bead_id == "fixture-3" then
      assert(
        result.error and result.error:find("exited", 1, true),
        "unapprovable request must end without a human decision"
      )
    elseif result.error then
      return failed(result.error)
    else
      assert(result.session and result.session:inspect().status == "ready")
    end
    assert(result.envelope_digest ~= result.request_digest)
    assert(result.session:dispose())
    count = count + 1
    continue(true)
  end,
}))
assert(controller:start(function(ok, err)
  if not ok then
    return failed(err)
  end
  assert(count == 3)
  completed = true
end))
local finished = nvim.wait(90000, function()
  return completed
end, 10)
local disposed, dispose_error = controller:dispose()
assert(disposed, dispose_error)
assert(finished, "installed Lua controller timed out")
assert(not failure, failure)
print("installed Lua Bead executor: three authorized Sessions, automatic permission/refusal, sequential handoff")
