local SchemaVimdoc = require("louiselm.schema.gen_vimdoc")

local append_wrapped = SchemaVimdoc.append_wrapped

local M = {}

---@param title string
---@param tag string
---@return string
local function heading(title, tag)
  local padding = math.max(1, 62 - #title)
  return title .. string.rep(" ", padding) .. "*" .. tag .. "*"
end

---@param value string
---@return string
local function arguments_description(value)
  if value == "0" then
    return "none"
  end
  if value == "?" then
    return "optional"
  end
  if value == "1" then
    return "one required"
  end
  if value == "*" then
    return "any number"
  end
  if value == "+" then
    return "one or more"
  end
  return value
end

---@param commands table<string, table>
---@return { name: string, description: string, nargs: string, bang: boolean }[]
local function documented_commands(commands)
  local result = {}
  for name, command in pairs(commands) do
    local description = command.desc
    if type(description) ~= "string" or description == "" then
      description = command.definition
    end
    if
      name:match("^Louiselm")
      and type(description) == "string"
      and type(command.nargs) == "string"
      and type(command.bang) == "boolean"
    then
      result[#result + 1] = {
        name = name,
        description = description,
        nargs = command.nargs,
        bang = command.bang,
      }
    end
  end
  table.sort(result, function(left, right)
    return left.name < right.name
  end)
  return result
end

---@param lines string[]
---@param commands table<string, table>
local function emit_commands(lines, commands)
  lines[#lines + 1] = ""
  lines[#lines + 1] = "=============================================================================="
  lines[#lines + 1] = heading("Commands", "louiselm-commands")
  lines[#lines + 1] = ""
  lines[#lines + 1] = "  Commands are registered by |require('louiselm').setup()|."

  for _, command in ipairs(documented_commands(commands)) do
    lines[#lines + 1] = ""
    local invocation = ":" .. command.name .. (command.bang and "[!]" or "")
    lines[#lines + 1] = invocation .. string.rep(" ", math.max(1, 62 - #invocation)) .. "*:" .. command.name .. "*"
    append_wrapped(lines, command.description, "    ", "    ")
    lines[#lines + 1] = "    Arguments: " .. arguments_description(command.nargs)
  end
end

---@param lines string[]
local function emit_overview(lines)
  lines[#lines + 1] = ""
  lines[#lines + 1] = "=============================================================================="
  lines[#lines + 1] = heading("Overview", "louiselm")
  lines[#lines + 1] = ""
  append_wrapped(
    lines,
    "LouiseLM is an ACP-first Neovim client with an interactive chat and a headless Session API.",
    "  ",
    "  "
  )
  lines[#lines + 1] = ""
  append_wrapped(
    lines,
    "Configure at least one Agent with require('louiselm').setup(), then use :LouiselmChat to start a Session.",
    "  ",
    "  "
  )
  lines[#lines + 1] = ""
  append_wrapped(
    lines,
    "Use :checkhealth louiselm to validate configured Agents, local skills, and capture tools. Health output is operational evidence, not a substitute for this reference.",
    "  ",
    "  "
  )
  lines[#lines + 1] = ""
  append_wrapped(
    lines,
    "Sessions remain bound to their Agent. A Handoff creates a new Session with a configured Agent, including the current Agent, and seeds it with your takeover task and a compacted source transcript. The source Session remains attached.",
    "  ",
    "  "
  )
end

---@param lines string[]
---@param title string
---@param tag string
---@param paragraphs string[]
local function emit_section(lines, title, tag, paragraphs)
  lines[#lines + 1] = ""
  lines[#lines + 1] = "=============================================================================="
  lines[#lines + 1] = heading(title, tag)
  for _, paragraph in ipairs(paragraphs) do
    lines[#lines + 1] = ""
    append_wrapped(lines, paragraph, "  ", "  ")
  end
end

---Generate LouiseLM's human Vimdoc reference.
---@param schema louiselm.schema.Schema Normalized configuration schema.
---@param commands table<string, table> Registered user commands from `nvim_get_commands`.
---@return string vimdoc Deterministic content for `doc/louiselm.txt`.
function M.generate(schema, commands)
  local lines = {
    "*louiselm.txt*  LouiseLM human reference",
    "",
  }
  emit_overview(lines)
  emit_section(lines, "Sessions", "louiselm-sessions", {
    "A Session is bound to one Agent for its lifetime. Use :LouiselmSessionNew to start another Session without stopping the current one, and :LouiselmSessionSwitch to focus an attached Session.",
    "Prompt silence has no time limit. A quiet or reconnecting Agent keeps its Session active until it responds, reports an error, or you dispose it. Use :LouiselmCancel to request cancellation; dispose the Session if the Agent cannot respond. LouiseLM never resubmits an interrupted prompt automatically.",
    "A Handoff creates a new Session with a configured Agent after you fill in its takeover task and review the compacted source transcript. The source Session remains attached.",
  })
  emit_section(lines, "Workflow", "louiselm-workflow", {
    "Configure an Agent, start or resume a Session, queue optional context, then submit a prompt. Respond deliberately to permission requests and cancel a current turn when the intent changes.",
  })
  emit_section(lines, "Permissions", "louiselm-permissions", {
    "Sessions ask a human to answer ACP permission requests by default. Persistent choices remain scoped to the configured Agent, exact adapter command and arguments, and Session workspace.",
  })
  emit_section(lines, "Context and skills", "louiselm-context", {
    "Use the context commands to queue a buffer, visual selection, file, or skill for the next prompt. Queued context is sent only when you submit that prompt.",
  })
  emit_section(lines, "Troubleshooting", "louiselm-troubleshooting", {
    "Run :checkhealth louiselm to validate configuration and local tool availability. For Session-specific reports, :LouiselmSessionId copies the Agent-scoped ACP Session identifier.",
  })
  emit_commands(lines, commands)
  lines[#lines + 1] = ""
  local configuration = SchemaVimdoc.generate_configuration(schema)
  for line in configuration:gmatch("([^\n]*)\n") do
    lines[#lines + 1] = line
  end
  return table.concat(lines, "\n") .. "\n"
end

return M
