-- A separate editor exits without Disposal after a committed waiting entry.
---@diagnostic disable-next-line: undefined-global -- Neovim test runtime.
local nvim = vim
local root = nvim.fn.getcwd()
local directory = assert(nvim.env.LOUISELM_QUEUE_TEST_DIR)
nvim.fn.mkdir(directory, "p", 448)
local settled_mode = nvim.env.LOUISELM_QUEUE_TEST_SETTLED == "1"
local replacement_phase = nvim.env.LOUISELM_QUEUE_TEST_REPLACEMENT
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
    env = { LOUISELM_MOCK_MODE = replacement_phase ~= nil and replacement_phase ~= "" and "hold" or "permission" },
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
  if
    replacement_phase ~= nil
    and replacement_phase ~= ""
    and event.type == "prompt_dispatched"
    and event.data.display_text == "held before restart"
  then
    assert(session:enqueue("saved for review", function(id, err)
      assert(id, err)
      if replacement_phase == "cancelling" then
        session.client.cancel = function()
          return true
        end
      elseif replacement_phase == "admitting" then
        local store = session.owner.queue_store
        local save = store.save
        store.save = function(owner, queue_id, revision, snapshot, callback)
          if snapshot.state.replacement ~= nil and snapshot.state.replacement.status == "admitting" then
            save(owner, queue_id, revision, snapshot, function(save_error)
              assert(save_error == nil)
              publish_ready()
            end)
          else
            save(owner, queue_id, revision, snapshot, callback)
          end
        end
      end
      assert(session:stop_and_send(event.data.turn_id, "saved replacement", function(replacement_id, replacement_error)
        assert(replacement_id, replacement_error)
        if replacement_phase == "cancelling" then
          publish_ready()
        end
      end))
    end))
  elseif
    replacement_phase == "dispatched"
    and event.type == "queue_changed"
    and event.data.replacement ~= nil
    and event.data.replacement.status == "dispatched"
    and not event.data.pending
  then
    publish_ready()
  elseif event.type == "permission_requested" then
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
