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

local function valid_skill_policy(value)
  local valid = value == "native" or value == "inject" or value == "off"
  return valid, "must be one of: native, inject, off"
end

local function valid_capability(value)
  return value ~= "", "must be a non-empty string"
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
        capabilities = {
          type = "array-of",
          items = { type = "string", validator = valid_capability },
          default = {},
          description = 'Capability tags this agent declares support for (e.g. "image-generation"), matched against `needs-capability:*` beads labels by the agent selecting work; louiselm does not read beads or route work itself.',
        },
        transcript_layout = {
          type = "string",
          default = "",
          description = "Optional Provenance layout override for this Agent's historical transcripts: claude, codex, openai-compatible, or copilot; live chat transcripts need no configuration; omission searches all supported layouts.",
        },
        skills = {
          type = "table",
          default = {},
          description = "Agent-specific Agent Skills policy override; paths remain global and the effective value is fixed when a session is created.",
          fields = {
            policy = {
              type = "string",
              validator = valid_skill_policy,
              description = "Override the global Agent Skills policy: native delegates to the adapter, inject uses LouiseLM discovery, and off disables automation; omission inherits the global policy.",
            },
          },
        },
        latest = {
          type = "table",
          default = {},
          description = "Optional command that resolves the agent's latest available version, e.g. `npm view <pkg> version`; omission disables the staleness check.",
          fields = {
            command = {
              type = "string",
              description = "Executable that prints the latest available version.",
            },
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
          },
        },
        version = {
          type = "table",
          default = {},
          description = "Optional override for querying the installed version, verbatim (nothing is auto-appended). Use when `command args... --version` is not the right invocation, e.g. a subcommand-based CLI wrapped by a debug script.",
          fields = {
            command = {
              type = "string",
              description = "Executable that prints the installed version.",
            },
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
        description = "Global directories searched for Agent Skills; relative paths resolve against each session workspace.",
      },
      policy = {
        type = "string",
        default = "native",
        validator = valid_skill_policy,
        description = "Default Agent Skills policy: native delegates to the adapter, inject uses LouiseLM discovery, and off disables automation.",
      },
      full_content = {
        type = "boolean",
        default = false,
        validator = removed_full_content,
        description = 'Removed legacy full-content injection switch; migrate to skills.policy = "inject".',
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
  keymaps = {
    type = "boolean",
    default = true,
    description = "Install LouiseLM's global default keymaps; false disables them.",
  },
  beads = {
    type = "table",
    default = {},
    description = "Beads issue-inspector settings.",
    fields = {
      sibling_roots = {
        type = "array-of",
        items = "string",
        default = {},
        description = "Directories globbed one level for a sibling `.beads/beads.db`, tried in order when the cursor is on a full issue ID belonging to a different workspace's prefix; empty disables cross-workspace lookup.",
      },
    },
  },
  capture = {
    type = "table",
    default = {},
    description = "Durable speech-capture command settings.",
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
    },
  },
}))

return M
