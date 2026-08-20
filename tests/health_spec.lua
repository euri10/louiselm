local MiniTest = require("mini.test")
local Health = require("louiselm.health")
local Louiselm = require("louiselm")
local Skills = require("louiselm.skills")

---@diagnostic disable-next-line: undefined-global -- `vim` is Neovim's injected runtime API.
local nvim = vim
local T = MiniTest.new_set()

---@param callback fun(calls: table)
---@param system? function Override for `vim.system`, for scenarios beyond a single "agent 1.2.3" reply.
local function with_health_stubs(callback, system)
  local original_health = nvim.health
  local original_executable = nvim.fn.executable
  local original_system = nvim.system
  local original_in_fast_event = nvim.in_fast_event
  local calls = { ok = {}, error = {}, info = {}, warn = {} }
  nvim.health = {
    start = function(message)
      calls.start = message
    end,
    ok = function(message)
      calls.ok[#calls.ok + 1] = message
    end,
    error = function(message)
      calls.error[#calls.error + 1] = message
    end,
    info = function(message)
      calls.info[#calls.info + 1] = message
    end,
    warn = function(message)
      calls.warn[#calls.warn + 1] = message
    end,
  }
  nvim.fn.executable = function()
    return 1
  end
  nvim.in_fast_event = function()
    return false
  end
  nvim.system = system
    or function(command, options, callback)
      callback({ code = 0, signal = 0, stdout = "agent 1.2.3\n", stderr = "" })
      return {}
    end

  local ok, err = pcall(function()
    callback(calls)
  end)

  nvim.health = original_health
  nvim.fn.executable = original_executable
  nvim.system = original_system
  nvim.in_fast_event = original_in_fast_event
  if not ok then
    error(err)
  end
end

T["check"] = MiniTest.new_set()

T["check"]["reports setup validation and agent version"] = function()
  local skill_path = nvim.fn.tempname()
  assert(nvim.fn.mkdir(skill_path, "p") == 1)
  assert(Louiselm.setup({ agents = { agent = { command = "agent" } }, skills = { paths = { skill_path } } }))

  with_health_stubs(function(calls)
    Health.check()
    MiniTest.expect.equality(calls.start, "louiselm")
    MiniTest.expect.equality(calls.error, {})
    MiniTest.expect.equality(calls.ok[1], "configuration is valid")
    MiniTest.expect.equality(calls.ok[2], "agent — agent 1.2.3")
    MiniTest.expect.equality(calls.info[2], "agent agent skills policy: native")
    MiniTest.expect.equality(calls.warn, {})
    MiniTest.expect.equality(calls.ok[3], "discovered 0 skills")
    MiniTest.expect.equality(calls.ok[4], "capture recorder is executable: pw-record")
    MiniTest.expect.equality(calls.ok[5], "capture service is executable: louiselm-capture")
  end)

  Health.reset()
  nvim.fn.delete(skill_path, "rf")
end

T["check"]["warns instead of failing when the installed agent trails the latest check"] = function()
  assert(Louiselm.setup({
    agents = {
      agent = {
        command = "agent",
        latest = { command = "npm", args = { "view", "agent", "version" } },
      },
    },
  }))

  with_health_stubs(function(calls)
    Health.check()
    MiniTest.expect.equality(calls.error, {})
    MiniTest.expect.equality(nvim.tbl_contains(calls.ok, "agent — agent 1.2.3"), false)
    MiniTest.expect.equality(calls.warn, { "agent — agent 1.2.3 (latest 9.9.9 available)" })
  end, function(command, options, callback)
    if command[1] == "npm" then
      callback({ code = 0, signal = 0, stdout = "9.9.9\n", stderr = "" })
    else
      callback({ code = 0, signal = 0, stdout = "agent 1.2.3\n", stderr = "" })
    end
    return {}
  end)

  Health.reset()
end

T["check"]["reports discovered skills alongside invalid siblings"] = function()
  local root = nvim.fn.tempname()
  local source_dir = nvim.fs.joinpath(root, "source")
  local skill_path = nvim.fs.joinpath(root, "generated")
  local valid_dir = nvim.fs.joinpath(skill_path, "valid")
  local invalid_dir = nvim.fs.joinpath(skill_path, "invalid")
  assert(nvim.fn.mkdir(source_dir, "p") == 1)
  assert(nvim.fn.mkdir(valid_dir, "p") == 1)
  assert(nvim.fn.mkdir(invalid_dir, "p") == 1)
  local source = nvim.fs.joinpath(source_dir, "SKILL.md")
  assert(nvim.fn.writefile({ "---", "name: valid", "description: Valid skill", "---" }, source) == 0)
  assert(nvim.uv.fs_symlink(source, nvim.fs.joinpath(valid_dir, "SKILL.md")))
  local invalid = nvim.fs.joinpath(invalid_dir, "SKILL.md")
  assert(nvim.fn.writefile({ "# no frontmatter" }, invalid) == 0)
  assert(Louiselm.setup({ agents = { agent = { command = "agent" } }, skills = { paths = { skill_path } } }))

  with_health_stubs(function(calls)
    Health.check()
    MiniTest.expect.equality(calls.error, { invalid .. ": missing YAML frontmatter" })
    MiniTest.expect.equality(calls.ok[3], "discovered 1 skill")
  end)

  Health.reset()
  nvim.fn.delete(root, "rf")
end

T["check"]["reports injected catalog budget diagnostics"] = function()
  assert(Louiselm.setup({
    agents = { agent = { command = "agent", skills = { policy = "inject" } } },
    skills = { paths = { "/tmp/skills" } },
  }))
  local original_discover = Skills.discover
  local original_inject = Skills.inject
  Skills.discover = function()
    return {
      { name = "alpha", description = "Alpha", path = "/tmp/skills/alpha/SKILL.md" },
      { name = "omega", description = "Omega", path = "/tmp/skills/omega/SKILL.md" },
    }, {}
  end
  Skills.inject = function()
    return { text = string.rep("x", 8000), truncated = { "alpha" }, omitted = { "omega" } }
  end

  local call_ok, call_error = pcall(function()
    with_health_stubs(function(calls)
      Health.check()
      MiniTest.expect.equality(nvim.tbl_contains(calls.info, "injected skill catalog: 8000/8000 bytes"), true)
      MiniTest.expect.equality(calls.warn, {
        "injected skill catalog shortened descriptions: alpha",
        "injected skill catalog omitted skills: omega",
      })
    end)
  end)

  Skills.discover = original_discover
  Skills.inject = original_inject
  Health.reset()
  if not call_ok then
    error(call_error)
  end
end

T["check"]["reports the Neovim working directory used for relative skill roots"] = function()
  local original_cwd = nvim.fn.getcwd()
  local workspace = nvim.fn.tempname()
  local skill_path = nvim.fs.joinpath(workspace, "skills")
  assert(nvim.fn.mkdir(skill_path, "p") == 1)
  nvim.api.nvim_set_current_dir(workspace)
  assert(Louiselm.setup({ agents = { agent = { command = "agent" } }, skills = { paths = { "skills" } } }))

  local call_ok, call_error = pcall(function()
    with_health_stubs(function(calls)
      Health.check()
      MiniTest.expect.equality(
        nvim.tbl_contains(
          calls.info,
          "relative skill paths resolve against Neovim's current working directory: " .. workspace
        ),
        true
      )
      MiniTest.expect.equality(calls.ok[3], "discovered 0 skills")
    end)
  end)

  Health.reset()
  nvim.api.nvim_set_current_dir(original_cwd)
  nvim.fn.delete(workspace, "rf")
  if not call_ok then
    error(call_error)
  end
end

T["check"]["reports persistent LuaRocks path guidance"] = function()
  local skill_path = nvim.fn.tempname()
  local skill_dir = nvim.fs.joinpath(skill_path, "local-skill")
  assert(nvim.fn.mkdir(skill_dir, "p") == 1)
  assert(
    nvim.fn.writefile(
      { "---", "name: local-skill", "description: Local skill", "---" },
      nvim.fs.joinpath(skill_dir, "SKILL.md")
    ) == 0
  )
  assert(Louiselm.setup({ agents = { agent = { command = "agent" } }, skills = { paths = { skill_path } } }))
  local loaded = package.loaded.lyaml
  local preload = package.preload.lyaml
  package.loaded.lyaml = nil
  rawset(package.preload, "lyaml", function()
    error("module 'lyaml' not found", 0)
  end)

  local call_ok, call_error = pcall(function()
    with_health_stubs(function(calls)
      Health.check()
      MiniTest.expect.equality(calls.error, {
        'skills: Neovim cannot find lyaml in package.path or package.cpath; install it with `luarocks --lua-version 5.1 install lyaml` or, if LuaRocks already reports it installed, add `eval "$(luarocks path --lua-version 5.1 --no-bin)"` to the shell startup file that launches Neovim',
      })
    end)
  end)

  package.loaded.lyaml = loaded
  rawset(package.preload, "lyaml", preload)
  Health.reset()
  nvim.fn.delete(skill_path, "rf")
  if not call_ok then
    error(call_error)
  end
end

T["check"]["reports missing setup as a warning"] = function()
  Health.reset()
  with_health_stubs(function(calls)
    MiniTest.expect.equality(Health.check(), false)
    MiniTest.expect.equality(
      calls.warn,
      { "LouiseLM has not been configured; run setup() before checking configuration" }
    )
  end)
end

return T
