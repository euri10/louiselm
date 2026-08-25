---@diagnostic disable-next-line: undefined-global -- `vim` is Neovim's injected runtime API.
local nvim = vim

local root = nvim.fn.getcwd()
local check = nvim.tbl_contains(nvim.v.argv, "--check")
local doc_directory = root .. "/doc"
local doc_path = doc_directory .. "/louiselm.txt"
local tags_path = doc_directory .. "/tags"

nvim.opt.rtp:prepend(root)
require("louiselm.ui.chat.command").register()
require("louiselm.capture.command").register()

local output = require("louiselm.docs.vimdoc").generate(
  require("louiselm.config").schema,
  nvim.api.nvim_get_commands({ builtin = false })
)

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

---@return string
local function generated_tags()
  local directory = nvim.fn.tempname()
  assert(nvim.fn.mkdir(directory, "p") == 1)
  write(directory .. "/louiselm.txt", output)
  nvim.cmd.helptags(directory)
  local tags = assert(read(directory .. "/tags"))
  nvim.fn.delete(directory, "rf")
  return tags
end

local tags = generated_tags()
if check then
  if read(doc_path) ~= output or read(tags_path) ~= tags then
    nvim.api.nvim_err_writeln("louiselm Vimdoc is stale; run ./scripts/generate-vimdoc")
    nvim.cmd.cquit({ args = { "1" }, bang = true })
    return
  end
  nvim.cmd.quit({ bang = true })
  return
end

assert(nvim.fn.mkdir(doc_directory, "p") == 1)
write(doc_path, output)
write(tags_path, tags)
nvim.cmd.quit({ bang = true })
