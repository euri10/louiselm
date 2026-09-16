local SchemaVimdoc = require("louiselm.schema.gen_vimdoc")
local Text = require("louiselm.docs.vimdoc_text")

local M = {}

---Tags owned by Neovim's own help files, linkable but never emitted here.
---
---Exported so the suite can hold each one against `$VIMRUNTIME/doc/tags`: an
---allowlist nobody checks is how a dangling link gets waved through.
---@type table<string, true>
M.EXTERNAL_TAGS = {
  [":checkhealth"] = true,
  ["mapleader"] = true,
}

local MODELINE = " vim:tw=78:ts=8:noet:ft=help:norl:"

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

---@param value unknown
---@return string
local function format_scalar(value)
  if type(value) == "string" then
    return string.format("%q", value)
  end
  if type(value) == "table" then
    local items = {}
    for _, item in ipairs(value) do
      items[#items + 1] = format_scalar(item)
    end
    return "{ " .. table.concat(items, ", ") .. " }"
  end
  return tostring(value)
end

---@param value table
---@return string[]
local function sorted_keys(value)
  local keys = {}
  for key in pairs(value) do
    keys[#keys + 1] = key
  end
  table.sort(keys)
  return keys
end

---Render a configuration table as the Lua source a reader would type.
---@param lines string[] Accumulator appended in place.
---@param value table Table to render.
---@param indent string Indentation of the closing brace.
---@param prefix string Text preceding the opening brace on its line.
---@param suffix string Text following the closing brace.
local function append_lua_table(lines, value, indent, prefix, suffix)
  lines[#lines + 1] = indent .. prefix .. "{"
  for _, key in ipairs(sorted_keys(value)) do
    local child = value[key]
    local entry = key .. " = "
    if type(child) == "table" and #child == 0 and next(child) ~= nil then
      append_lua_table(lines, child, indent .. "  ", entry, ",")
    else
      lines[#lines + 1] = indent .. "  " .. entry .. format_scalar(child) .. ","
    end
  end
  lines[#lines + 1] = indent .. "}" .. suffix
end

---Configuration the Overview example documents.
---
---Returned as a table rather than rendered source so the suite can hold the
---documented example to the same schema `setup()` enforces: a help file that
---advertises a configuration `setup()` rejects is worse than no example.
---@return table example Fresh copy of the documented configuration.
function M.setup_example()
  return {
    agents = {
      claude = { command = "claude-code-acp", provider = "Anthropic" },
      codex = { command = "codex-acp", provider = "OpenAI" },
    },
    context = { instructions_file = "AGENTS.md" },
  }
end

---@param lines string[]
---@param code string[]
local function append_code(lines, code)
  lines[#lines + 1] = ">lua"
  for _, line in ipairs(code) do
    lines[#lines + 1] = line == "" and "" or "  " .. line
  end
  lines[#lines + 1] = "<"
end

---@param lines string[]
---@param text string
---@param context louiselm.docs.Context
local function append_prose(lines, text, context)
  lines[#lines + 1] = ""
  Text.append_wrapped(lines, Text.link_commands(text, context.command_tags), "  ", "  ")
end

---@param lines string[]
---@param context louiselm.docs.Context
local function emit_overview(lines, context)
  append_prose(
    lines,
    "LouiseLM is an ACP-first Neovim client with an interactive chat and a headless Session API.",
    context
  )
  append_prose(
    lines,
    "Sessions remain bound to their Agent. A Handoff creates a new Session with a configured Agent, including the current Agent, and seeds it with your takeover task and a compacted source transcript. The source Session remains attached.",
    context
  )
  lines[#lines + 1] = ""
  Text.append_heading(lines, "Setup ~", "louiselm.setup()")
  append_prose(
    lines,
    "Call setup() once with at least one Agent. Every Agent needs a command and the Provider supplying its access or quota; see |louiselm-configuration| for the complete schema.",
    context
  )
  lines[#lines + 1] = ""
  local code = {}
  append_lua_table(code, M.setup_example(), "", 'require("louiselm").setup(', ")")
  append_code(lines, code)
  append_prose(
    lines,
    "Then use :LouiselmChat to start a Session, or |louiselm-api| to drive an Agent without the chat UI. Use |:checkhealth| louiselm to discover optional capabilities and validate the prerequisites of explicitly enabled integrations. Ordinary chat requires an authenticated Agent and sqlite3 >= 3.38 with JSON support.",
    context
  )
end

---@param lines string[]
---@param context louiselm.docs.Context
local function emit_mappings(lines, context)
  append_prose(
    lines,
    "Installed by |louiselm.setup()| unless |louiselm-config-keymaps| is false. Capture, Beads, skill and Park mappings are installed only when their integration is enabled. Existing user mappings are preserved. Each mapping runs the command it links to; |mapleader| decides what <leader> expands to.",
    context
  )
  lines[#lines + 1] = ""
  for _, mapping in ipairs(context.mappings) do
    local command = mapping.rhs:match("^<[Cc][Mm][Dd]>(%a+)")
    local left = string.format("  %-2s  %-13s  %s", mapping.mode, mapping.lhs, mapping.desc)
    if command ~= nil and context.command_tags[":" .. command] then
      lines[#lines + 1] = Text.align_columns(left, "|:" .. command .. "|")
    else
      lines[#lines + 1] = left
    end
  end
end

---@param lines string[]
---@param context louiselm.docs.Context
local function emit_commands(lines, context)
  append_prose(lines, "Commands are registered by |louiselm.setup()|.", context)

  for _, command in ipairs(documented_commands(context.commands)) do
    lines[#lines + 1] = ""
    local invocation = ":" .. command.name .. (command.bang and "[!]" or "")
    Text.append_heading(lines, invocation, ":" .. command.name)
    Text.append_wrapped(lines, Text.link_commands(command.description, context.command_tags), "    ", "    ")
    lines[#lines + 1] = "    Arguments: " .. arguments_description(command.nargs)
  end
end

---@param lines string[]
---@param context louiselm.docs.Context
local function emit_configuration(lines, context)
  SchemaVimdoc.append_configuration_fields(lines, context.schema, function(text)
    return Text.link_commands(text, context.command_tags)
  end)
end

---@param lines string[]
---@param context louiselm.docs.Context
local function emit_api(lines, context)
  append_prose(
    lines,
    "The headless Session API drives Agents without the chat UI. It takes the same Agent definitions as |louiselm-config-agents|, returns the definitions it rejected instead of raising, and hands back Sessions the caller owns and disposes.",
    context
  )
  lines[#lines + 1] = ""
  append_code(lines, {
    'local Session = require("louiselm.session")',
    "local api, errors = Session.new({",
    '  codex = { command = "codex-acp", provider = "OpenAI" },',
    "})",
    "if not api then",
    "  return vim.notify(vim.inspect(errors), vim.log.levels.ERROR)",
    "end",
    "",
    'api:create_session("codex", { cwd = vim.uv.cwd() }, function(session, err)',
    "  if not session then",
    "    return vim.notify(err, vim.log.levels.ERROR)",
    "  end",
    '  session:prompt("Summarise AGENTS.md", function(_, prompt_error)',
    "    if prompt_error then",
    "      vim.notify(prompt_error, vim.log.levels.ERROR)",
    "    end",
    "    session:dispose()",
    "  end)",
    "end)",
  })
  append_prose(
    lines,
    "Prompt completion, permission requests, and Agent output arrive as typed events on the Session. doc/api.md in the repository is the complete typed surface, generated from the same annotations.",
    context
  )
end

---@class louiselm.docs.Section
---@field title string Heading shown in the body and the table of contents.
---@field tag string Help tag naming the section.
---@field emit fun(lines: string[], context: louiselm.docs.Context) Body writer.

---@class louiselm.docs.Context
---@field schema louiselm.schema.Schema Normalized configuration schema.
---@field commands table<string, table> Registered user commands.
---@field mappings louiselm.ui.KeymapDefault[] Default mappings installed by setup.
---@field command_tags table<string, true> Command tags this document emits.

---@type louiselm.docs.Section[]
local SECTIONS = {
  { title = "Overview", tag = "louiselm", emit = emit_overview },
  {
    title = "Sessions",
    tag = "louiselm-sessions",
    emit = function(lines, context)
      append_prose(
        lines,
        "A Session is bound to one Agent for its lifetime. Use :LouiselmSessionNew to start another Session without stopping the current one, and :LouiselmSessionSwitch to focus an attached Session.",
        context
      )
      append_prose(
        lines,
        "Prompt silence has no time limit. A quiet or reconnecting Agent keeps its Session active until it responds, reports an error, or you dispose it. Use :LouiselmCancel to request cancellation; dispose the Session if the Agent cannot respond. LouiseLM never resubmits an interrupted prompt automatically.",
        context
      )
      append_prose(
        lines,
        "Agents supporting the Session activity extension can run again after a prompt completes, for example when Claude wakes for a scheduled check. LouiseLM shows Model responding and queues input until the Agent reports idle. Each client prompt still completes once. Cancellation requests a stop; the Session remains active until the Agent acknowledges it. Agents without this extension retain their ordinary prompt lifecycle.",
        context
      )
      append_prose(
        lines,
        "A Handoff creates a new Session with a configured Agent after you fill in its takeover task and review the context. When available, it uses the latest usable completed ACP compaction summary plus recent conversation, including conservative overlap for the current instruction and open tools. Otherwise it uses the filtered full transcript. This is lossy context, not the Agent's complete replacement history. No new source turn is requested; the source Session and full transcript remain intact.",
        context
      )
      append_prose(
        lines,
        "Experimental ACP compaction updates appear as separate timeline rows. Use :LouiselmInspectTool on a compaction row to inspect its retained summary and status. Agents may omit summaries. LouiseLM consumes the shared protocol without adapter-specific hooks or transcript parsing; compaction does not replace usage telemetry or erase the source transcript.",
        context
      )
      append_prose(
        lines,
        "Cold Park requires an Agent that supports session/load and persisted conversation history. A fresh Session must send a prompt first; a successfully loaded, ready Session can Park immediately without another prompt. Cold resume restores recoverable history after editor exit, but staged context is lost.",
        context
      )
    end,
  },
  {
    title = "Workflow",
    tag = "louiselm-workflow",
    emit = function(lines, context)
      append_prose(
        lines,
        "Configure an Agent, start or resume a Session, queue optional context, then submit a prompt. Respond deliberately to permission requests and cancel a current turn when the intent changes.",
        context
      )
    end,
  },
  {
    title = "Permissions",
    tag = "louiselm-permissions",
    emit = function(lines, context)
      append_prose(
        lines,
        "Sessions ask a human to answer ACP permission requests by default. Persistent choices remain scoped to the configured Agent, exact adapter command and arguments, and Session workspace.",
        context
      )
      append_prose(lines, "Use :LouiselmPermissions to inspect and revoke remembered decisions.", context)
    end,
  },
  {
    title = "Context and skills",
    tag = "louiselm-context",
    emit = function(lines, context)
      append_prose(
        lines,
        "Use the context commands to queue a buffer, visual selection, file, or skill for the next prompt. Queued context is sent only when you submit that prompt.",
        context
      )
      append_prose(
        lines,
        "To remove a staged skill, erase its [context: skill: NAME] chip from the prompt. Before submitting or staging another item, LouiseLM keeps only skills with intact leading chips. Edited chip fragments remain ordinary text; other queued context and the hidden skill catalog are retained.",
        context
      )
      append_prose(
        lines,
        "|louiselm-config-skills| decides where skills are discovered and who executes them.",
        context
      )
    end,
  },
  { title = "Default mappings", tag = "louiselm-mappings", emit = emit_mappings },
  { title = "Commands", tag = "louiselm-commands", emit = emit_commands },
  { title = "Configuration", tag = "louiselm-configuration", emit = emit_configuration },
  {
    title = "Optional capabilities",
    tag = "louiselm-optional-capabilities",
    emit = function(lines, context)
      append_prose(
        lines,
        "Attention, Beads, desktop capture, workflow Runs and trusted skill management default to disabled. skills.policy defaults to off; explicit native/inject choices and per-Agent overrides remain available. Installed tools, skill paths, tracker directories and credentials never enable integrations. Agent permission and YOLO choices are independent.",
        context
      )
      append_code(lines, {
        "-- Explicit opt-ins for an operator who wants every delivered integration:",
        "attention = { enabled = true },",
        "beads = { enabled = true },",
        "capture = { enabled = true },",
        "workflows = { enabled = true },",
        'skills = { policy = "native", paths = {}, management = { enabled = true } },',
      })
      append_prose(
        lines,
        "These fields belong inside setup({...}). Run |:checkhealth| louiselm for setup guidance and readiness. It never installs dependencies, starts services, initializes Beads, pairs devices, enrolls trust or admits skills. Disabled commands explain the required setting. Close live Sessions and finish recording/ingestion before reconfiguring; retained data is never deleted by a toggle.",
        context
      )
      append_prose(
        lines,
        "Service choices belong in ~/.config/louiselm/capture.env: LOUISELM_ATTENTION_ENABLED, LOUISELM_RUNS_ENABLED, LOUISELM_RECEIVER_ENABLED, LOUISELM_TRANSCRIPTION_ENABLED and LOUISELM_PUSH_ENABLED each require true. Omission means false. Attention-only startup needs no microphone, receiver or cloud credentials. Runs require LOUISELM_BEADS_WORKSPACE and LOUISELM_REAL_BR for cleanup. Cold Park also requires explicit editor Attention and Beads opt-ins, a ready service, and resumable Agent history. Keep Run support enabled while retained Runs have obligations.",
        context
      )
      append_prose(
        lines,
        "Manual installation and service setup are documented in capture-service/README.md in the plugin checkout. Desktop capture retains audio locally; transcription explicitly sends audio to the configured OpenAI API, and push uses separately configured Google credentials. The client cannot reconfigure a daemon shared by another editor.",
        context
      )
      append_prose(
        lines,
        "Trusted skill management currently exposes :LouiselmPreflight for selected artifacts. Packaging, Admission, and trust operations remain explicit installed CLI operations documented in skills-core/README.md. Enabling management never grants Verified posture or changes an Agent launch command.",
        context
      )
    end,
  },
  { title = "Session API", tag = "louiselm-api", emit = emit_api },
  {
    title = "Troubleshooting",
    tag = "louiselm-troubleshooting",
    emit = function(lines, context)
      append_prose(
        lines,
        "Run |:checkhealth| louiselm to validate configuration and local tool availability. For Session-specific reports, :LouiselmSessionId copies the Agent-scoped ACP Session identifier.",
        context
      )
      append_prose(
        lines,
        "Direct vendor commands, including wrappers, have no LouiseLM Verified posture. With skills.management.enabled = true and louiselm-skills on PATH, :LouiselmPreflight {request-file} [{manifest-file} [{prior-request-file} {prior-manifest-file}]] asynchronously inspects canonical launch request/2 and Session input-manifest/1 artifacts and opens health. Prior files are explicitly selected, never inferred from history.",
        context
      )
      append_prose(
        lines,
        "The selected prospective snapshot separates proposed identities from independently checked supply/runtime artifacts. Native loading, isolation, network enforcement and disclosure remain unproven; network scope is unresolved without revision-bound rules. The snapshot is not approval, launch authority or live Session status. Refresh after input changes; future launch integration must bind the exact displayed request digest. Setup/reset forgets the selection and cancels pending reads.",
        context
      )
    end,
  },
}

---@param lines string[]
local function emit_contents(lines)
  lines[#lines + 1] = ""
  lines[#lines + 1] = Text.SECTION_RULE
  Text.append_heading(lines, "Contents", "louiselm-contents")
  lines[#lines + 1] = ""
  for index, section in ipairs(SECTIONS) do
    local left = string.format("  %d. %s ", index, section.title)
    lines[#lines + 1] = Text.align_columns(left, " |" .. section.tag .. "|", ".")
  end
end

---@param commands table<string, table>
---@return table<string, true>
local function command_tags(commands)
  local tags = {}
  for _, command in ipairs(documented_commands(commands)) do
    tags[":" .. command.name] = true
  end
  return tags
end

---Generate LouiseLM's human Vimdoc reference.
---
---Every `|link|` the document emits is checked against the tags it emits, so a
---cross-reference that would fail with `E149` is a generation error rather than
---something a reader discovers.
---@param schema louiselm.schema.Schema Normalized configuration schema.
---@param commands table<string, table> Registered user commands from `nvim_get_commands`.
---@param mappings louiselm.ui.KeymapDefault[] Default mappings from `louiselm.ui.keymaps`.
---@return string vimdoc Deterministic content for `doc/louiselm.txt`.
function M.generate(schema, commands, mappings)
  ---@type louiselm.docs.Context
  local context = {
    schema = schema,
    commands = commands,
    mappings = mappings,
    command_tags = command_tags(commands),
  }

  local lines = { "*louiselm.txt*  LouiseLM human reference", "" }
  emit_contents(lines)
  for _, section in ipairs(SECTIONS) do
    lines[#lines + 1] = ""
    lines[#lines + 1] = Text.SECTION_RULE
    Text.append_heading(lines, section.title, section.tag)
    section.emit(lines, context)
  end
  lines[#lines + 1] = ""
  lines[#lines + 1] = MODELINE

  local document = table.concat(lines, "\n") .. "\n"
  local tags = {}
  for tag in document:gmatch("%*(%S-)%*") do
    tags[tag] = true
  end
  local dangling = Text.dangling_links(document, tags, M.EXTERNAL_TAGS)
  if #dangling > 0 then
    error("vimdoc links resolve to no tag: |" .. table.concat(dangling, "|, |") .. "|")
  end
  return document
end

return M
