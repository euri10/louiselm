-- `vim` is Neovim's injected runtime API, available under `-l` scripts.
---@diagnostic disable-next-line: undefined-global
local nvim = vim

local root = nvim.fn.getcwd()
local check = nvim.tbl_contains(nvim.v.argv, "--check")
local doc_path = root .. "/doc/api.md"

nvim.opt.rtp:prepend(root)

local ApiAppendix = require("louiselm.docs.api_appendix")

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
  io.stderr:write("lua-language-server --doc failed:\n" .. (result.stderr or "") .. "\n")
  os.exit(1)
end

local doc_json_path = tmp_directory .. "/doc.json"
if nvim.fn.filereadable(doc_json_path) == 0 then
  io.stderr:write("lua-language-server did not produce doc.json at " .. doc_json_path .. "\n")
  os.exit(1)
end

local raw = table.concat(nvim.fn.readfile(doc_json_path), "\n")
local entries = nvim.json.decode(raw)
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
  if read(doc_path) ~= output then
    io.stderr:write("doc/api.md is stale; run ./scripts/generate-api-appendix\n")
    os.exit(1)
  end
  os.exit(0)
end

write(doc_path, output)
