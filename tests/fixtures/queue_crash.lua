-- A separate editor exits without Disposal after a committed waiting entry.
---@diagnostic disable-next-line: undefined-global -- Neovim test runtime.
local nvim = vim
local root = nvim.fn.getcwd()
local directory = assert(nvim.env.LOUISELM_QUEUE_TEST_DIR)
nvim.fn.mkdir(directory, "p", 448)
local settled_mode = nvim.env.LOUISELM_QUEUE_TEST_SETTLED == "1"
local api = assert(require("louiselm.session").new({
  mock = {
    provider = "test",
    command = nvim.v.progpath,
    args = {
      "--headless",
      "--noplugin",
      "-u",
      root .. "/tests/mock/init.lua",
      "-c",
      "lua require('louiselm.dev.mock_agent').run()",
    },
    env = { LOUISELM_MOCK_MODE = "permission" },
  },
}, nil, { usage_directory = directory }))
local session = assert(api:create_session("mock", { cwd = root }, function(value, err)
  assert(value, err)
  assert(value:prompt("held before restart"))
end))
local saved = false
local function publish_ready()
  if saved then
    return
  end
  if
    settled_mode
    and (
      session:inspect().status ~= "ready"
      or session.queue == nil
      or session.queue.busy
      or not session:queue_inspect().paused
    )
  then
    return
  end
  saved = true
  api:flush_recording(function(recording_error)
    assert(recording_error == nil)
    nvim.fn.writefile({ session:inspect().acp_session_id }, directory .. "/ready")
    if nvim.env.LOUISELM_QUEUE_TEST_GRACEFUL == "1" then
      nvim.schedule(function()
        nvim.cmd("qa!")
      end)
    end
  end)
end
session:on(function(event)
  if event.type == "permission_requested" then
    assert(session:enqueue("saved for review", function(id, err)
      assert(id, err)
      if settled_mode then
        assert(session:cancel())
      else
        publish_ready()
      end
    end))
  elseif settled_mode and (event.type == "queue_changed" or event.type == "state_changed") then
    nvim.defer_fn(publish_ready, 10)
  end
end)
