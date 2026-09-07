-- Linux acceptance for louiselm-xq6c, run outside the editor being killed:
-- nvim --headless --noplugin -u NONE -l tests/acp/orphan_acceptance.lua /path/to/acp-proxy
-- Uses the real Session API, mock ACP Agent, and exit hooks; no credentials.
---@diagnostic disable-next-line: undefined-global -- Neovim owns this test process.
local nvim = vim
local root = nvim.fn.getcwd()
local script = root .. "/tests/acp/orphan_acceptance.lua"
nvim.opt.rtp:prepend(root)

local function exists(path)
  return nvim.uv.fs_stat(path) ~= nil
end

local function child_command(mode, directory, proxy)
  local command = { nvim.v.progpath, "--headless", "--noplugin", "-u", "NONE", "-l", script, mode, directory }
  if proxy then
    command[#command + 1] = proxy
  end
  return command
end

local mode = arg[1]
local directory = arg[2]
if mode == "worker" then
  local leaf = nvim.system({ "sleep", "120" })
  nvim.fn.writefile({ tostring(nvim.fn.getpid()), tostring(leaf.pid) }, directory .. "/worker-ready")
  nvim.wait(120000, function()
    return false
  end)
  return
elseif mode == "mock" then
  local command = child_command("worker", directory)
  table.insert(command, 1, "setsid")
  nvim.system(command)
  require("louiselm.dev.mock_agent").run({ mode = "static", response = "lifecycle fixture" })
  return
elseif mode == "editor" then
  nvim.env.XDG_STATE_HOME = directory .. "/state"
  local command = child_command("mock", directory)
  table.insert(command, 1, "--")
  table.insert(command, 1, directory .. "/logs")
  table.insert(command, 1, "--log-root")
  local agents = { mock = { command = arg[3], args = command } }
  assert(require("louiselm").setup({ agents = agents }))
  local api = assert(require("louiselm.session").new(agents))
  local ready, ready_error
  local session = assert(api:create_session("mock", { cwd = root }, function(value, err)
    ready, ready_error = value, err
  end))
  assert(
    nvim.wait(5000, function()
      return ready ~= nil or ready_error ~= nil
    end, 10),
    "Session did not initialize"
  )
  assert(ready, ready_error)
  local completed, prompt_error
  assert(session:prompt({ { type = "text", text = "lifecycle fixture" } }, function(result, err)
    completed, prompt_error = result, err
  end))
  assert(
    nvim.wait(5000, function()
      return completed ~= nil or prompt_error ~= nil
    end, 10),
    "Session prompt did not complete"
  )
  assert(completed, prompt_error)
  nvim.fn.writefile({ "ready" }, directory .. "/ready")
  assert(
    nvim.wait(30000, function()
      return exists(directory .. "/quit")
    end, 10),
    "acceptance controller did not stop the editor"
  )
  nvim.cmd("qa!")
  return
end

assert(nvim.uv.os_uname().sysname == "Linux", "this acceptance requires Linux")
local proxy = assert(mode, "supply the acp-proxy binary to test")
assert(nvim.fn.executable(proxy) == 1, "acp-proxy is not executable")

local function descendants(pid)
  local result = nvim.system({ "ps", "-eo", "pid=,ppid=" }, { text = true }):wait()
  assert(result.code == 0, result.stderr)
  local parents = {}
  for child, parent in result.stdout:gmatch("(%d+)%s+(%d+)") do
    parents[tonumber(child)] = tonumber(parent)
  end
  local found, changed = { [pid] = true }, true
  while changed do
    changed = false
    for child, parent in pairs(parents) do
      if found[parent] and not found[child] then
        found[child], changed = true, true
      end
    end
  end
  return found
end

local function check(clean)
  local created = nvim.system({ "mktemp", "-d", "-t", "louiselm-orphans.XXXXXX" }, { text = true }):wait()
  assert(created.code == 0, created.stderr)
  local temporary = nvim.trim(created.stdout)
  local editor = nvim.system(child_command("editor", temporary, proxy), {
    text = true,
    env = { XDG_STATE_HOME = temporary .. "/state", XDG_CACHE_HOME = temporary .. "/cache" },
  })
  local pids = { [editor.pid] = true }
  local ok, err = pcall(function()
    assert(
      nvim.wait(10000, function()
        return exists(temporary .. "/ready") and exists(temporary .. "/worker-ready")
      end, 10),
      "fixture did not become ready"
    )
    pids = descendants(editor.pid)
    assert(nvim.tbl_count(pids) >= 5, "expected editor, proxy, mock Agent, detached worker, and leaf")
    if clean then
      nvim.fn.writefile({ "quit" }, temporary .. "/quit")
    else
      editor:kill(9)
    end
    local result = editor:wait(5000)
    if clean then
      assert(result.code == 0, result.stderr)
      local breadcrumb = temporary .. "/state/nvim/louiselm/abandoned.json"
      local record = nvim.json.decode(table.concat(nvim.fn.readfile(breadcrumb), "\n"))
      assert(#record.sessions == 1 and record.sessions[1].agent == "mock", "quit lost the abandonment record")
    end
    local survivors = {}
    assert(
      nvim.wait(5000, function()
        survivors = {}
        for pid in pairs(pids) do
          if exists("/proc/" .. pid) then
            survivors[#survivors + 1] = pid
          end
        end
        return #survivors == 0
      end, 10),
      "surviving fixture PIDs: " .. nvim.inspect(survivors)
    )
  end)
  -- On failure, stop only this fixture's recorded processes. Preserve logs
  -- for diagnosis; a successful run can remove its private temporary state.
  if not ok then
    for pid in pairs(descendants(editor.pid)) do
      pids[pid] = true
    end
    for pid in pairs(pids) do
      if exists("/proc/" .. pid) then
        nvim.uv.kill(pid, 9)
      end
    end
    local result = editor:wait(1000)
    error(tostring(err) .. "\nfixture: " .. temporary .. "\n" .. (result.stderr or ""))
  end
  nvim.fn.delete(temporary, "rf")
  print((clean and "normal quit" or "editor SIGKILL") .. ": all fixture PIDs gone")
end

check(false)
check(true)
