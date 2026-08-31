---@diagnostic disable-next-line: undefined-global -- `vim` is Neovim's injected runtime API.
local nvim = vim

local root = nvim.fn.getcwd()
local check = nvim.tbl_contains(nvim.v.argv, "--check")
local types_path = root .. "/lua/louiselm/types.lua"

nvim.opt.rtp:prepend(root)

local output = require("louiselm.schema").generate_luacats(require("louiselm.config").schema)

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
  if read(types_path) ~= output then
    io.stderr:write("lua/louiselm/types.lua is stale; run ./scripts/generate-luacats\n")
    os.exit(1)
  end
  os.exit(0)
end

write(types_path, output)
