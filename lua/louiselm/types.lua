---@class louiselm.Config
---@field agents? table<string, louiselm.ConfigAgentsValue> Named ACP agent processes.
---@field attention? louiselm.ConfigAttention Durable Attention integration; local Session status remains available independently.
---@field beads? louiselm.ConfigBeads Beads issue-inspector settings.
---@field capture? louiselm.ConfigCapture Durable speech-capture command settings.
---@field context? louiselm.ConfigContext Project-context injection settings.
---@field keymaps? boolean Install LouiseLM's global default keymaps; false disables them.
---@field skills? louiselm.ConfigSkills Agent Skill discovery and injection settings.
---@field workflows? louiselm.ConfigWorkflows Workflow Run and Park integration; ordinary Handoff remains available independently.

---@class louiselm.ConfigAgentsValue
---@field args? string[] Arguments passed after the executable.
---@field capabilities? string[] Capability tags this agent declares support for (e.g. "image-generation"), matched against `needs-capability:*` beads labels by the agent selecting work; louiselm does not read beads or route work itself.
---@field command string Executable to start.
---@field env? table<string, string> Environment variables passed to the process.
---@field latest? louiselm.ConfigAgentsValueLatest Optional command that resolves the agent's latest available version, e.g. `npm view <pkg> version`; omission disables the staleness check.
---@field provider string|louiselm.ConfigAgentsValueProviderOption2Item[]|louiselm.ConfigAgentsValueProviderOption3 Required access/quota service: a fixed name, exact typed option routes, or { option, prefixes } mapping literal prefixes of an advertised string option to services. Exactly one route or prefix must match before each prompt; display names never determine Provider.
---@field skills? louiselm.ConfigAgentsValueSkills Agent-specific Agent Skills policy override; paths remain global and the effective value is fixed when a session is created.
---@field transcript_layout? string Optional Provenance layout override for this Agent's historical transcripts: claude, codex, openai-compatible, or copilot; live chat transcripts need no configuration; omission searches all supported layouts.
---@field upgrade? string[]|string Optional upgrade executable and arguments (e.g. { 'npm', 'install', '-g', 'my-agent@latest' }), or a nonblank string explaining a manual update (e.g. 'Update and rebuild /path/to/checkout; global npm upgrades do not affect this copy.'). Displayed in outdated-Agent warnings, never executed. Only argv commands are shell-escaped and joined with && when every outdated Agent has one; manual instructions are never included in that command.
---@field version? louiselm.ConfigAgentsValueVersion Optional override for querying the installed version, verbatim (nothing is auto-appended). Use when `command args... --version` is not the right invocation, e.g. a subcommand-based CLI wrapped by a debug script.

---@class louiselm.ConfigAgentsValueLatest
---@field args? string[] Arguments passed after the executable.
---@field command string Executable that prints the latest available version.
---@field env? table<string, string> Environment variables passed to the process.

---@class louiselm.ConfigAgentsValueProviderOption2Item
---@field options table<string, string|boolean>
---@field provider string

---@class louiselm.ConfigAgentsValueProviderOption3
---@field option string Advertised string-valued option ID to match, not its display name.
---@field prefixes table<string, string> Nonempty literal, case-sensitive prefixes mapped to access/quota services. No regex or longest-match precedence; multiple matches are ambiguous even for the same service.

---@class louiselm.ConfigAgentsValueSkills
---@field policy string Override the global Agent Skills policy: native delegates to the adapter, inject uses LouiseLM discovery, and off disables automation; omission inherits the global policy.

---@class louiselm.ConfigAgentsValueVersion
---@field args? string[] Arguments passed after the executable.
---@field command string Executable that prints the installed version.
---@field env? table<string, string> Environment variables passed to the process.

---@class louiselm.ConfigAttention
---@field enabled? boolean Connect to the explicitly configured local Attention service; does not enable capture or delivery.

---@class louiselm.ConfigBeads
---@field enabled? boolean Enable Beads inspection of existing workspaces; never initialize a tracker.
---@field sibling_roots? string[] Directories globbed one level for a sibling `.beads/beads.db`, tried in order when the cursor is on a full issue ID belonging to a different workspace's prefix; empty disables cross-workspace lookup.

---@class louiselm.ConfigCapture
---@field enabled? boolean Enable desktop capture commands. Receiver, transcription and push require separate service opt-ins.
---@field recorder? string[] Recorder argv with one {output} placeholder.
---@field service? string[] Capture-service argv prefix.

---@class louiselm.ConfigContext
---@field instructions_file? string Project-root filename attached as a resource_link on new sessions; empty disables.

---@class louiselm.ConfigSkills
---@field management? louiselm.ConfigSkillsManagement Trusted skill-management integration; independent of skill invocation and Admission.
---@field paths? string[] Global directories searched for Agent Skills; relative paths resolve against each session workspace.
---@field policy? string Default Agent Skills policy: native delegates to the adapter, inject uses LouiseLM discovery, and off disables automation.

---@class louiselm.ConfigSkillsManagement
---@field enabled? boolean Expose installed trusted skill-management operations; never install tools or admit skills automatically.

---@class louiselm.ConfigWorkflows
---@field enabled? boolean Enable workflow Runs and Park, subject to operation-specific prerequisites.
