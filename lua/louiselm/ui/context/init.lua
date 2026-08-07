local Buffer = require("louiselm.ui.context.buffer")
local Files = require("louiselm.ui.context.files")
local Selection = require("louiselm.ui.context.selection")
local Skills = require("louiselm.ui.context.skill-picker")

---@class louiselm.ui.ContextModule
---@field buffer fun(buffer?: integer): louiselm.ui.ContextItem Buffer context.
---@field selection fun(buffer?: integer): louiselm.ui.ContextItem?, string? Visual selection context.
---@field files table File listing, file context, and picker functions.
---@field skills table Skill context and picker functions.

local M = {
  buffer = Buffer.current,
  selection = Selection.current,
  files = Files,
  skills = Skills,
}

return M
