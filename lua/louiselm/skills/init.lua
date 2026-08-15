local Discovery = require("louiselm.skills.discovery")
local Inject = require("louiselm.skills.inject")
local Overlap = require("louiselm.skills.overlap")
local Policy = require("louiselm.skills.policy")

---@class louiselm.skills.Module
---@field discover fun(paths: unknown): louiselm.skills.Skill[], louiselm.skills.DiscoveryError[] Discover skill metadata.
---@field inject fun(skills: unknown): string?, string? Build a skill prompt index.
---@field policy fun(value?: unknown): louiselm.skills.Policy?, string? Normalize one agent policy.
---@field overlap fun(native_path: unknown, configured_paths: unknown): boolean?, string?, string? Detect path overlap.

local M = {}

M.discover = Discovery.discover
M.inject = Inject.index
M.policy = Policy.normalize
M.overlap = Overlap.detect

return M
