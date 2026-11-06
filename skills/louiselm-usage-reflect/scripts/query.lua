-- Standalone headless bridge to the existing read-only query API.
-- Usage: nvim --headless --noplugin -u NONE -l query.lua REPO DIRECTORY|- QUERY_JSON
---@diagnostic disable-next-line: undefined-global -- Standalone Neovim entrypoint, as in the repository's query harness.
local nvim = vim
local function fail(code, message)
  io.stdout:write(nvim.json.encode({ error = { code = code, message = message } }) .. "\n")
  nvim.cmd("cquit 1")
end

if #arg ~= 3 or arg[1]:sub(1, 1) ~= "/" then
  fail("invalid", "expected absolute repository path, usage directory or -, and query JSON")
  return
end
local decoded, query = pcall(nvim.json.decode, arg[3])
if not decoded then
  fail("invalid", "query must be valid JSON")
  return
end
nvim.opt.rtp:prepend(arg[1])
local loaded, recording = pcall(require, "louiselm.session.recording")
if not loaded then
  fail("prerequisite", "cannot load LouiseLM recording API from the supplied repository")
  return
end
local directory = arg[2] == "-" and nvim.fs.joinpath(require("louiselm.paths").state(), "usage") or arg[2]
local store, err = recording.new(directory, function() end)
if not store then
  if err then
    fail(err.code, err.message)
  else
    fail("unavailable", "recording API returned no store or error")
  end
  return
end
local present = nvim.uv.fs_stat(store.path) ~= nil
local done, page, failure
local cancel = store:usage_query(query, function(result, query_error)
  done, page, failure = true, result, query_error
end)
-- Only this standalone child waits; never run this entrypoint in a live editor.
if not nvim.wait(10000, function()
  return done
end, 10) then
  cancel()
  fail("timeout", "usage query did not complete; narrow the scope before retrying")
  return
end
if failure then
  fail(failure.code, failure.message)
  return
end
io.stdout:write(nvim.json.encode({
  query = query,
  directory = directory,
  store_present = present,
  read_at = os.date("!%Y-%m-%dT%H:%M:%SZ"),
  page = page,
}) .. "\n")
