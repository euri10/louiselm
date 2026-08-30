local MiniTest = require("mini.test")
local Generator = require("louiselm.schema.gen_vimdoc")
local Schema = require("louiselm.schema")

local T = MiniTest.new_set()

T["generate"] = MiniTest.new_set()

T["generate"]["emits standard help structure and all fields"] = function()
  local schema = assert(Schema.define({
    agent = {
      type = "table",
      description = "Agent settings.",
      fields = {
        command = { type = "string", description = "Executable command." },
      },
    },
    name = { type = "string", default = "louiselm", description = "Display name." },
  }))

  local output = Generator.generate(schema)

  MiniTest.expect.equality(output:sub(1, #"*louiselm.txt*"), "*louiselm.txt*")
  MiniTest.expect.equality(output:find("*louiselm*", 1, true) ~= nil, true)
  MiniTest.expect.equality(output:find("*louiselm-config-agent*", 1, true) ~= nil, true)
  MiniTest.expect.equality(output:find("*louiselm-config-agent-command*", 1, true) ~= nil, true)
  MiniTest.expect.equality(output:find("Type: table", 1, true) ~= nil, true)
  MiniTest.expect.equality(output:find('Default: "louiselm"', 1, true) ~= nil, true)
  MiniTest.expect.equality(output:find("Display name.", 1, true) ~= nil, true)
  MiniTest.expect.equality(output:sub(-1), "\n")
end

T["generate"]["wraps long descriptions to the help width"] = function()
  local schema = assert(Schema.define({
    description = {
      type = "string",
      description = "This description contains enough words to require wrapping across multiple help lines.",
    },
  }))

  local output = Generator.generate(schema)
  local wrapped = false
  for line in output:gmatch("([^\n]*)\n") do
    if line:find("description contains enough words", 1, true) ~= nil then
      wrapped = true
    end
    if line:find("require wrapping across multiple help lines.", 1, true) ~= nil then
      wrapped = true
    end
    if #line > 78 and line:find("*", 1, true) == nil then
      error("vimdoc line exceeds 78 columns: " .. line)
    end
  end
  MiniTest.expect.equality(wrapped, true)
end

T["generate"]["documents fields inside array item tables"] = function()
  local schema = assert(Schema.define({
    agents = {
      type = "array-of",
      items = {
        type = "table",
        fields = {
          command = { type = "string", description = "Executable command." },
        },
      },
    },
  }))

  local output = Generator.generate(schema)

  MiniTest.expect.equality(output:find("*louiselm-config-agents-command*", 1, true) ~= nil, true)
  MiniTest.expect.equality(output:find("Executable command.", 1, true) ~= nil, true)
end

return T
