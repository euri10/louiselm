-- tohtml-driver.lua
--
-- Headless batch driver invoked by render-post.mjs, inside ONE shared
-- Neovim instance for the whole post (not one process per excerpt).
--
-- Reads a JSON manifest of {file, first, last, out} entries from the path in
-- $LOUISELM_TOHTML_MANIFEST. `first`/`last` are 1-based, inclusive Neovim
-- buffer line numbers (already translated from MyST's 1-based-inclusive
-- `:lines:` range by render-post.mjs, so they pass straight through). For
-- each entry this opens `file` in the current window, renders that line
-- range through :TOhtml's Lua API using whatever colorscheme/Tree-sitter
-- setup the caller's real init.lua sets up, and writes the FULL generated
-- HTML document verbatim to `out`.
--
-- This file does no HTML/CSS post-processing (extracting <pre>/<style>,
-- namespacing selectors, deduplicating) — that happens back in
-- render-post.mjs once every entry has been rendered, so this driver's only
-- job is turning (file, range) pairs into raw TOhtml output.
--
-- tohtml ships as an "opt" runtime plugin, so `require("tohtml")` is not
-- available until it is explicitly loaded, even with a full user config:
--   :help tohtml -> "The plugin is not loaded by default; use :packadd to
--   activate it: :packadd nvim.tohtml"
-- (verified on the installed Neovim 0.12.2 — requiring it beforehand raises
-- "module 'tohtml' not found").
vim.cmd('packadd nvim.tohtml')

-- Never block on a swap-file prompt in headless mode -- e.g. if one of the
-- referenced conversation files happens to already be open in the author's
-- interactive Neovim session while this batch script runs.
vim.o.swapfile = false
vim.opt.shortmess:append('A')

--- Reports a fatal error and exits with a non-zero status via :cquit, the
--- idiomatic way for a headless Neovim script to signal failure to its
--- caller (used the same way e.g. by `git commit`'s editor invocation).
--- @param msg string
local function fail(msg)
	io.stderr:write('tohtml-driver: ' .. msg .. '\n')
	vim.cmd('cquit! 1')
end

local manifest_path = os.getenv('LOUISELM_TOHTML_MANIFEST')
if not manifest_path then
	fail('LOUISELM_TOHTML_MANIFEST is not set')
	return
end

local fd, open_err = io.open(manifest_path, 'r')
if not fd then
	fail('could not open manifest ' .. manifest_path .. ': ' .. tostring(open_err))
	return
end
local raw = fd:read('*a')
fd:close()

local decode_ok, entries = pcall(vim.json.decode, raw)
if not decode_ok then
	fail('could not parse manifest JSON at ' .. manifest_path .. ': ' .. tostring(entries))
	return
end

for _, entry in ipairs(entries) do
	local edit_ok, edit_err = pcall(vim.cmd.edit, vim.fn.fnameescape(entry.file))
	if not edit_ok then
		fail(('could not open %s: %s'):format(entry.file, tostring(edit_err)))
		return
	end

	-- number_lines = false: TOhtml's own :help documents this as its default,
	-- but pass it explicitly since fragments never carry line numbers.
	--
	-- title = false: intended to suppress the <title> tag, but note this is
	-- discarded either way -- render-post.mjs only ever extracts the <pre>
	-- and <style> regions, never <head>. (Verified on the installed
	-- tohtml.lua: `state.title = opt.title or title or false` means
	-- `title = false` does NOT actually suppress it, since `false or x`
	-- evaluates to the buffer-name fallback `x` in Lua -- a real quirk in
	-- the installed 0.12.2 tohtml, not something to rely on. Irrelevant
	-- here because <head> is never read.)
	local render_ok, html = pcall(require('tohtml').tohtml, 0, {
		range = { entry.first, entry.last },
		number_lines = false,
		title = false,
	})
	if not render_ok then
		fail(
			('TOhtml failed for %s [%d-%d]: %s'):format(entry.file, entry.first, entry.last, tostring(html))
		)
		return
	end

	vim.fn.writefile(html, entry.out)
end

vim.cmd('cquit! 0')
