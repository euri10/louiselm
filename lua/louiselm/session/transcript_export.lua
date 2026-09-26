---Headless, script-driven export of one named agent's ACP session to a markdown
---transcript. Separate from `louiselm.ui.chat`'s interactive `LouiselmToMarkdown`
---command on purpose: that command can only export a session already open as a live
---chat view, which requires a human to have run `:LouiselmChat`/`:LouiselmResume`
---first. This module instead loads the session itself (via the same headless
---`louiselm.session` API `Api:create_session`/`Api:load_session` already use), waits
---for the load to finish, and renders whatever the agent replayed -- so a plain shell
---script can export an arbitrary session with no chat UI, buffer, or human involved.
---
---`M.export` is the pure, testable core: given an already-constructed
---`louiselm.session.Api`, it returns the written path or an error. `M.run` is the
---ready-to-call entry point for a `-c "lua ..."` command line; see its own doc
---comment for the exact stdout/stderr/exit-code contract a shell caller depends on.

local Session = require("louiselm.session")
local Transcript = require("louiselm.session.transcript")
local Provenance = require("louiselm.output_provenance")
local PrivateFile = require("louiselm.private_file")

---@diagnostic disable-next-line: undefined-global -- `vim` is Neovim's injected runtime API.
local nvim = vim

local M = {}

-- Generous but finite: a shell-driven export must eventually give up on an agent
-- that never answers session/load rather than hang a build pipeline forever.
local DEFAULT_TIMEOUT_MS = 60000

---@class louiselm.session.TranscriptExportIoHooks
---@field write_out? fun(text: string) Receives the written path (with a trailing newline) on success. Defaults to stdout.
---@field write_err? fun(text: string) Receives a `"louiselm: "`-prefixed error line on failure. Defaults to stderr.
---@field quit? fun() Force-quits Neovim with a non-zero exit code on failure. Defaults to `:cquit! 1`.

---@type louiselm.session.TranscriptExportIoHooks
local DEFAULT_IO_HOOKS = {
  write_out = function(text)
    io.stdout:write(text)
  end,
  write_err = function(text)
    io.stderr:write(text)
  end,
  quit = function()
    -- `:cquit` is the documented way for a headless Neovim script to signal failure
    -- with a specific exit code; unlike `os.exit()`, it still runs Neovim's normal
    -- shutdown instead of skipping it (the ACP session is already disposed by the
    -- time this runs regardless).
    nvim.cmd.cquit({ bang = true, args = { "1" } })
  end,
}

---The one `louiselm.session.Api` method this module actually calls; a full `Api`
---satisfies this structurally, and so does a lightweight test double. Kept as a
---plain alias (not a `@class`) so lua-language-server matches it structurally
---instead of requiring the exact `Api` class name.
---@alias louiselm.session.SessionLoader { load_session: fun(self: table, agent_name: string, acp_session_id: string, options?: louiselm.session.Options, ready_callback?: fun(session: louiselm.session.Session?, error?: string)): louiselm.session.Session?, string? }

---Load one ACP session and export its full, untruncated transcript to markdown.
---@param api louiselm.session.SessionLoader Headless session API already constructed for the target agent's definition.
---@param agent_name string Named agent to load the session from.
---@param acp_session_id string Agent-side ACP session identifier to load.
---@param path string Destination markdown file path.
---@param timeout_ms? integer Milliseconds to wait for the agent to finish loading. Defaults to 60000.
---@return string? path Markdown file written, on success.
---@return string? error_message Validation, load, or filesystem error.
function M.export(api, agent_name, acp_session_id, path, timeout_ms)
  if type(path) ~= "string" or path == "" then
    return nil, "output path must be a non-empty string"
  end

  local transcript = Transcript.new()
  local ready_error ---@type string?
  local ready = false
  local session, start_error = api:load_session(agent_name, acp_session_id, {
    on_event = function(event)
      transcript:record(event)
    end,
  }, function(_, err)
    ready = true
    ready_error = err
  end)
  if session == nil then
    return nil, start_error or "could not load session"
  end

  local completed = nvim.wait(timeout_ms or DEFAULT_TIMEOUT_MS, function()
    return ready
  end, 10)
  if not completed then
    session:dispose()
    return nil, "timed out waiting for agent '" .. agent_name .. "' to load session " .. acp_session_id
  end
  if ready_error ~= nil then
    session:dispose()
    return nil, ready_error
  end

  local state = session:inspect()
  -- This entry point creates an ordinary Agent Session with no Control broker
  -- binding. Its status is explicitly not_managed, not an inferred clean bill.
  local provenance = Provenance.initial({ kind = "not_managed" })
  local markdown = Provenance.markdown(Transcript.render(transcript:snapshot(), state), provenance)
  local written = PrivateFile.write(nvim.uv, path, markdown, "markdown transcript", "replace")
  session:dispose()
  if not written then
    return nil, "could not write markdown file: " .. path
  end
  return path
end

---Entry point for a single `-c "lua ..."` command run under:
---```
---nvim --headless -u <init.lua> \
---  -c "lua require('louiselm.session.transcript_export').run(<definitions>, '<agent>', '<session id>', '<path>')" \
---  -c "qa!"
---```
---Contract for the calling shell script:
---  - success: the written file's absolute path is printed to stdout, exactly one
---    line with a trailing newline, and this function returns normally so the
---    subsequent `-c "qa!"` exits with status 0.
---  - failure: one line prefixed `"louiselm: "` is printed to stderr and Neovim is
---    force-quit with `:cquit! 1` (status 1) before `-c "qa!"` ever runs.
---A caller should check the process exit status, not merely whether stdout is
---non-empty, to detect failure.
---@param definitions table<string, louiselm.agent.Definition> Named agent definitions, same shape `louiselm.session.Api` normally takes.
---@param agent_name string Named agent to load the session from.
---@param acp_session_id string Agent-side ACP session identifier to load.
---@param path string Destination markdown file path.
---@param io_hooks? louiselm.session.TranscriptExportIoHooks Injected for tests; defaults to real stdout/stderr/`:cquit`.
function M.run(definitions, agent_name, acp_session_id, path, io_hooks)
  local hooks = nvim.tbl_extend("force", DEFAULT_IO_HOOKS, io_hooks or {})
  local api, errors = Session.new(definitions)
  if api == nil then
    hooks.write_err("louiselm: invalid agent configuration (" .. #errors .. " errors)\n")
    hooks.quit()
    return
  end
  local written_path, export_error = M.export(api, agent_name, acp_session_id, path)
  api:dispose()
  if written_path == nil then
    hooks.write_err("louiselm: " .. (export_error or "export failed") .. "\n")
    hooks.quit()
    return
  end
  hooks.write_out(written_path .. "\n")
end

return M
