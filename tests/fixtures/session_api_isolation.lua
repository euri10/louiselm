-- Run in a child editor: the regression deliberately fails a collected case.
local MiniTest = require("mini.test")
local Session = require("louiselm.session")
---@diagnostic disable-next-line: undefined-global -- Neovim test runtime.
local nvim = vim

return function(missing_sqlite)
  local original_system = nvim.system
  local cases = MiniTest.collect({
    find_files = function()
      return { "tests/session/api_spec.lua", "tests/session/recording_spec.lua" }
    end,
    filter_cases = function(case)
      local name = case.desc[#case.desc]
      return name == "rejects option changes while prompting or waiting for permission"
        or name == "tracks scheduled work separately from the completed client prompt"
        or name == "headless unmeasured turns survive reopen and resumed local ordinals"
    end,
  })
  assert(#cases == 3)
  local first = cases[1].test
  local cleanup_observed = false
  cases[1].test = function()
    if missing_sqlite then
      local path = nvim.env.PATH
      MiniTest.finally(function()
        nvim.env.PATH = path
      end)
      nvim.env.PATH = ""
    else
      -- A prior case's asynchronous recorder must not contend with this case.
      local directory = require("louiselm.paths").state() .. "/usage"
      local writer = assert(require("louiselm.session.recording").new(directory, function() end))
      local ready = false
      writer:flush(function(err)
        assert(err == nil)
        ready = true
      end)
      assert(nvim.wait(6000, function()
        return ready
      end, 10))
      local held = false
      local locker = original_system({ "sqlite3", directory .. "/turns.sqlite3" }, {
        stdin = true,
        stdout = function(_, data)
          if data and data:find("held", 1, true) then
            held = true
          end
        end,
      })
      MiniTest.finally(function()
        locker:write("ROLLBACK;\n.quit\n")
        assert(locker:wait(6000).code == 0)
      end)
      locker:write("BEGIN IMMEDIATE;\nSELECT 'held';\n")
      assert(nvim.wait(6000, function()
        return held
      end, 10))
    end
    first()
  end
  table.insert(cases[1].hooks.post, function()
    MiniTest.expect.equality(nvim.system, original_system)
    MiniTest.expect.equality(Session.exit_verdict(), {})
    cleanup_observed = true
  end)
  table.insert(cases[1].hooks.post_source, "case")

  MiniTest.execute(cases, {
    reporter = {
      finish = function()
        local failures = cases[1].exec.fails
        local expected_failure = not missing_sqlite and #failures == 0
          or missing_sqlite and #failures == 1 and failures[1]:find("turn recording requires sqlite3", 1, true) ~= nil
        if not expected_failure or not cleanup_observed or #cases[2].exec.fails > 0 or #cases[3].exec.fails > 0 then
          for _, case in ipairs(cases) do
            for _, failure in ipairs(case.exec.fails) do
              print(failure)
            end
          end
          nvim.cmd("cquit 1")
        end
        nvim.cmd("qa!")
      end,
    },
  })
end
