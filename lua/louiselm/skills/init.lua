local Discovery = require("louiselm.skills.discovery")
local Inject = require("louiselm.skills.inject")
local NativeCommand = require("louiselm.skills.native_command")
local Overlap = require("louiselm.skills.overlap")
local Policy = require("louiselm.skills.policy")

---@class louiselm.skills.Module
---@field discover fun(paths: unknown, cwd?: string): louiselm.skills.Skill[], louiselm.skills.DiscoveryDiagnostic[] Discover and validate skill metadata.
---@field read fun(path: unknown): string?, string? Read the current contents of one discovered skill.
---@field inject fun(skills: unknown): louiselm.skills.Catalog?, string? Build a bounded hidden skill catalog.
---@field policy fun(value?: unknown): louiselm.skills.Policy?, string? Normalize one agent policy.
---@field overlap fun(native_path: unknown, configured_paths: unknown): boolean?, string?, string? Detect path overlap.
---@field resolve_command fun(skill: unknown, commands: unknown): string?, string? Resolve a selected skill against advertised native commands.

local M = {}

M.discover = Discovery.discover
M.read = Discovery.read
M.inject = Inject.index
M.policy = Policy.normalize
M.overlap = Overlap.detect
M.resolve_command = NativeCommand.resolve

return M
