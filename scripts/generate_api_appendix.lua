-- `vim` is Neovim's injected runtime API, available under `-l` scripts.
---@diagnostic disable-next-line: undefined-global
local nvim = vim

local root = nvim.fn.getcwd()
local check = nvim.tbl_contains(nvim.v.argv, "--check")
local doc_path = root .. "/doc/api.md"

nvim.opt.rtp:prepend(root)

local ApiAppendix = require("louiselm.docs.api_appendix")

-- LuaLS 3.19.1 `--doc` races its own config load and exits 0 either way, so
-- an export is only trusted after `verify_export`. A refused export is
-- regenerated; the comparison below is unchanged (louiselm-qbr.9.9.7.1).
local ATTEMPTS = 3

---@return table[]? entries
---@return string? err
local function export()
  local tmp_directory = nvim.fn.tempname()
  assert(nvim.fn.mkdir(tmp_directory, "p") == 1)

  local result = nvim
    .system({
      "lua-language-server",
      "--doc=" .. root,
      "--doc_out_path=" .. tmp_directory,
      "--logpath=" .. tmp_directory .. "/log",
    }, { text = true })
    :wait()

  if result.code ~= 0 then
    return nil, "lua-language-server --doc failed:\n" .. (result.stderr or "")
  end

  local doc_json_path = tmp_directory .. "/doc.json"
  if nvim.fn.filereadable(doc_json_path) == 0 then
    return nil, "lua-language-server did not produce doc.json at " .. doc_json_path
  end

  local entries = nvim.json.decode(table.concat(nvim.fn.readfile(doc_json_path), "\n"))
  local sound, export_error = ApiAppendix.verify_export(entries)
  if not sound then
    return nil, "lua-language-server --doc export rejected: " .. (export_error or "")
  end
  return entries
end

local entries, export_error
for attempt = 1, ATTEMPTS do
  entries, export_error = export()
  if entries then
    break
  end
  io.stderr:write(string.format("export attempt %d/%d: %s\n", attempt, ATTEMPTS, export_error))
end

if not entries then
  io.stderr:write("doc/api.md was not compared or rewritten; re-run the generator.\n")
  os.exit(1)
end

local output = ApiAppendix.generate(entries)

---@param path string
---@return string?
local function read(path)
  if nvim.fn.filereadable(path) == 0 then
    return nil
  end
  return table.concat(nvim.fn.readfile(path), "\n") .. "\n"
end

---@param path string
---@param value string
local function write(path, value)
  nvim.fn.writefile(nvim.split(value:sub(1, -2), "\n", { plain = true }), path)
end

if check then
  local committed = read(doc_path)
  if committed ~= output then
    io.stderr:write("doc/api.md is stale; run ./scripts/generate-api-appendix\n")
    io.stderr:write(nvim.text.diff(committed or "", output, { ctxlen = 3 }))
    os.exit(1)
  end
  os.exit(0)
end

write(doc_path, output)
