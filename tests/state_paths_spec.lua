local MiniTest = require("mini.test")
local Session = require("louiselm.session")
local Permission = require("louiselm.permission")
local Usage = require("louiselm.routing.usage")

---@diagnostic disable-next-line: undefined-global -- Neovim test runtime.
local nvim = vim
local state_home, appname, directory
local T = MiniTest.new_set({
  hooks = {
    pre_case = function()
      state_home, appname = nvim.env.XDG_STATE_HOME, nvim.env.NVIM_APPNAME
      directory = nvim.fn.tempname()
      nvim.env.XDG_STATE_HOME = directory
    end,
    post_case = function()
      nvim.env.XDG_STATE_HOME, nvim.env.NVIM_APPNAME = state_home, appname
      nvim.fn.delete(directory, "rf")
    end,
  },
})

local function registry(options)
  local api = assert(Session.new({ test = { provider = "test", command = "unused" } }, nil, options))
  ---@cast api louiselm.session.Registry
  assert(api:dispose())
  return api
end

T["shared stores are independent of the Neovim profile"] = function()
  for _, profile in ipairs({ "nvim", "other-profile", "nested/profile" }) do
    nvim.env.NVIM_APPNAME = profile
    local root = nvim.fs.joinpath(directory, "louiselm")
    local api = registry()
    MiniTest.expect.equality(api.recording.path, root .. "/usage/turns.sqlite3")
    MiniTest.expect.equality(api.forensics_store.directory, root .. "/forensics")
    MiniTest.expect.equality(assert(Permission.store()).path, root .. "/permissions.json")
    MiniTest.expect.equality(assert(Usage.new()).path, root .. "/usage.json")
    MiniTest.expect.equality(nvim.uv.fs_stat(root), nil)
  end
end

T["unset and empty XDG state use the user state root"] = function()
  for _, value in ipairs({ false, "" }) do
    if value == false then
      nvim.env.XDG_STATE_HOME = nil
    else
      nvim.env.XDG_STATE_HOME = value
    end
    local root = nvim.fs.joinpath(nvim.fn.expand("~"), ".local", "state", "louiselm")
    MiniTest.expect.equality(assert(Usage.new()).path, root .. "/usage.json")
    MiniTest.expect.equality(assert(Permission.store()).path, root .. "/permissions.json")
  end
end

T["explicit headless and store paths retain precedence"] = function()
  local api = registry({
    usage_directory = directory .. "/private-usage",
    forensics_directory = directory .. "/private-forensics",
    permission_store = assert(Permission.store(directory .. "/private-permissions.json")),
  })
  MiniTest.expect.equality(api.recording.path, directory .. "/private-usage/turns.sqlite3")
  MiniTest.expect.equality(api.forensics_store.directory, directory .. "/private-forensics")
  MiniTest.expect.equality(api.permission_store.path, directory .. "/private-permissions.json")
  MiniTest.expect.equality(assert(Usage.new(directory .. "/legacy.json")).path, directory .. "/legacy.json")
end

T["legacy replay reads the shared ledger without using the old Neovim directory"] = function()
  local root = directory .. "/louiselm"
  assert(nvim.fn.mkdir(root, "p", 448) == 1)
  local turn = { agent = "test", session_id = "old-session", turn = 1, usage = { total_tokens = 37 } }
  assert(
    nvim.fn.writefile({ nvim.json.encode({ version = 1, records = {}, turns = { turn } }) }, root .. "/usage.json") == 0
  )
  MiniTest.expect.equality(assert(assert(Usage.new()):turns("test", "old-session")), { turn })
  MiniTest.expect.equality(nvim.uv.fs_stat(directory .. "/nvim/louiselm"), nil)
end

return T
