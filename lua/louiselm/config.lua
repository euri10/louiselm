local Schema = require("louiselm.schema")

local M = {}

local function valid_recorder(value)
  if #value == 0 or value[1] == "" then
    return false, "must start with a recorder executable"
  end
  local placeholders = 0
  for _, argument in ipairs(value) do
    if argument == "{output}" then
      placeholders = placeholders + 1
    end
  end
  return placeholders == 1, "must contain {output} exactly once"
end

local function valid_service(value)
  return #value > 0 and value[1] ~= "", "must start with the louiselm-capture executable"
end

local function valid_receiver_url(value)
  if value == "" then
    return true
  end
  local authority = value:match("^https://(.+)$")
  local valid = authority ~= nil and authority ~= "" and authority:find("[%s/@%?#]") == nil
  return valid, "must be empty or a clean HTTPS base URL"
end

local function valid_skill_policy(value)
  local valid = value == "native" or value == "inject" or value == "off"
  return valid, "must be one of: native, inject, off"
end

local function removed_full_content()
  return false, 'was removed; use skills.policy = "inject" for LouiseLM-managed skills'
end

M.schema = assert(Schema.define({
  agents = {
    type = "map-of",
    default = {},
    description = "Named ACP agent processes.",
    items = {
      type = "table",
      fields = {
        command = { type = "string", description = "Executable to start." },
        args = {
          type = "array-of",
          items = "string",
          default = {},
          description = "Arguments passed after the executable.",
        },
        env = {
          type = "map-of",
          items = "string",
          default = {},
          description = "Environment variables passed to the process.",
        },
        skills = {
          type = "table",
          default = {},
          description = "Agent-specific Agent Skills policy override.",
          fields = {
            policy = {
              type = "string",
              validator = valid_skill_policy,
              description = "Override the global Agent Skills policy for this agent; omission inherits the global policy.",
            },
          },
        },
      },
    },
  },
  skills = {
    type = "table",
    default = {},
    description = "Agent Skill discovery and injection settings.",
    fields = {
      paths = {
        type = "array-of",
        items = "string",
        default = {},
        description = "Directories searched for Agent Skills.",
      },
      policy = {
        type = "string",
        default = "native",
        validator = valid_skill_policy,
        description = "Whether agents load skills natively, by injection, or not at all.",
      },
      full_content = {
        type = "boolean",
        default = false,
        validator = removed_full_content,
        description = "Removed legacy full-content injection switch.",
      },
    },
  },
  context = {
    type = "table",
    default = {},
    description = "Project-context injection settings.",
    fields = {
      instructions_file = {
        type = "string",
        default = "",
        description = "Project-root filename attached as a resource_link on new sessions; empty disables.",
      },
    },
  },
  capture = {
    type = "table",
    default = {},
    description = "Durable speech-capture commands and receiver settings.",
    fields = {
      recorder = {
        type = "array-of",
        items = "string",
        default = { "pw-record", "{output}" },
        validator = valid_recorder,
        description = "Recorder argv with one {output} placeholder.",
      },
      service = {
        type = "array-of",
        items = "string",
        default = { "louiselm-capture" },
        validator = valid_service,
        description = "Capture-service argv prefix.",
      },
      receiver_url = {
        type = "string",
        default = "",
        validator = valid_receiver_url,
        description = "HTTPS URL placed in Android pairing offers.",
      },
    },
  },
}))

return M
