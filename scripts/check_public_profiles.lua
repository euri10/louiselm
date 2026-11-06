-- Public trees must not contain private profiles, including encoded YAML types.
---@diagnostic disable-next-line: undefined-global -- Neovim injects the script runtime.
local nvim = vim
local yaml = require("lyaml")
local root = arg[1]
local failed = false

local function reject(rule, path)
  io.stderr:write("public skills rejected: " .. rule .. ": " .. path .. "\n")
  failed = true
end

for path, kind in nvim.fs.dir(root, { depth = math.huge }) do
  if kind == "file" then
    local lines = nvim.fn.readfile(root .. "/" .. path, "b")
    local text = table.concat(lines, "\n")
    if nvim.fs.basename(path) == "PROFILE.md" or text:find("louiselm/operator-profile", 1, true) then
      reject("operator_profile", path)
    elseif lines[1] and lines[1]:gsub("\r$", "") == "---" then
      for index = 2, #lines do
        if lines[index]:gsub("\r$", "") == "---" then
          local ok, metadata = pcall(yaml.load, table.concat(lines, "\n", 2, index - 1))
          if not ok then
            -- A parse failure cannot establish that a declaration is safe to publish.
            reject("invalid_frontmatter", path)
          elseif type(metadata) == "table" and metadata.type == "louiselm/operator-profile" then
            reject("operator_profile", path)
          end
          break
        end
      end
    end
  end
end

os.exit(failed and 1 or 0)
