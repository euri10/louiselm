---@class louiselm.Config
---@field agents? table<string, louiselm.ConfigAgentsValue> Named ACP agent processes.
---@field beads? louiselm.ConfigBeads Beads issue-inspector settings.
---@field capture? louiselm.ConfigCapture Durable speech-capture command settings.
---@field context? louiselm.ConfigContext Project-context injection settings.
---@field keymaps? boolean Install LouiseLM's global default keymaps; false disables them.
---@field skills? louiselm.ConfigSkills Agent Skill discovery and injection settings.

---@class louiselm.ConfigAgentsValue
---@field args? string[] Arguments passed after the executable.
---@field capabilities? string[] Capability tags this agent declares support for (e.g. "image-generation"), matched against `needs-capability:*` beads labels by the agent selecting work; louiselm does not read beads or route work itself.
---@field command string Executable to start.
---@field env? table<string, string> Environment variables passed to the process.
---@field latest? louiselm.ConfigAgentsValueLatest Optional command that resolves the agent's latest available version, e.g. `npm view <pkg> version`; omission disables the staleness check.
---@field skills? louiselm.ConfigAgentsValueSkills Agent-specific Agent Skills policy override; paths remain global and the effective value is fixed when a session is created.
---@field transcript_layout? string Optional Provenance layout override for this Agent's historical transcripts: claude, codex, openai-compatible, or copilot; live chat transcripts need no configuration; omission searches all supported layouts.
---@field version? louiselm.ConfigAgentsValueVersion Optional override for querying the installed version, verbatim (nothing is auto-appended). Use when `command args... --version` is not the right invocation, e.g. a subcommand-based CLI wrapped by a debug script.

---@class louiselm.ConfigAgentsValueLatest
---@field args? string[] Arguments passed after the executable.
---@field command string Executable that prints the latest available version.
---@field env? table<string, string> Environment variables passed to the process.

---@class louiselm.ConfigAgentsValueSkills
---@field policy string Override the global Agent Skills policy: native delegates to the adapter, inject uses LouiseLM discovery, and off disables automation; omission inherits the global policy.

---@class louiselm.ConfigAgentsValueVersion
---@field args? string[] Arguments passed after the executable.
---@field command string Executable that prints the installed version.
---@field env? table<string, string> Environment variables passed to the process.

---@class louiselm.ConfigBeads
---@field sibling_roots? string[] Directories globbed one level for a sibling `.beads/beads.db`, tried in order when the cursor is on a full issue ID belonging to a different workspace's prefix; empty disables cross-workspace lookup.

---@class louiselm.ConfigCapture
---@field recorder? string[] Recorder argv with one {output} placeholder.
---@field service? string[] Capture-service argv prefix.

---@class louiselm.ConfigContext
---@field instructions_file? string Project-root filename attached as a resource_link on new sessions; empty disables.

---@class louiselm.ConfigSkills
---@field full_content? boolean Removed legacy full-content injection switch; migrate to skills.policy = "inject".
---@field paths? string[] Global directories searched for Agent Skills; relative paths resolve against each session workspace.
---@field policy? string Default Agent Skills policy: native delegates to the adapter, inject uses LouiseLM discovery, and off disables automation.
