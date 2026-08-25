local MiniTest = require("mini.test")

local ApiAppendix = require("louiselm.docs.api_appendix")

local T = MiniTest.new_set()

local SECTIONS = {
  { title = "Widgets", files = { "lua/widgets/init.lua" } },
  { title = "Gadgets", files = { "lua/gadgets/init.lua" } },
}

T["api_appendix"] = MiniTest.new_set()

T["api_appendix"]["renders class fields grouped by section, excluding unlisted files"] = function()
  local entries = {
    {
      name = "widgets.Widget",
      type = "type",
      defines = { { file = "lua/widgets/init.lua", start = { 3 }, type = "doc.class" } },
      fields = {
        { name = "spin", view = "fun(self: widgets.Widget): boolean", desc = "Spin the widget." },
        { name = "size", view = "integer" },
      },
    },
    {
      name = "gadgets.Gadget",
      type = "type",
      defines = { { file = "lua/gadgets/init.lua", start = { 1 }, type = "doc.class" } },
      fields = {},
    },
    {
      name = "internal.Secret",
      type = "type",
      defines = { { file = "lua/internal/secret.lua", start = { 1 }, type = "doc.class" } },
      fields = { { name = "leak", view = "string" } },
    },
  }

  local output = ApiAppendix.generate(entries, SECTIONS)

  MiniTest.expect.equality(output:find("internal.Secret", 1, true), nil)
  MiniTest.expect.equality(
    output:find(
      "## Widgets\n\n### widgets.Widget\n\n"
        .. "- `spin: fun(self: widgets.Widget): boolean` -- Spin the widget.\n"
        .. "- `size: integer`",
      1,
      true
    ) ~= nil,
    true
  )
  MiniTest.expect.equality(output:find("## Gadgets", 1, true) ~= nil, true)
  MiniTest.expect.equality(output:find("## Widgets", 1, true) < output:find("## Gadgets", 1, true), true)
end

T["api_appendix"]["falls back to the alias union description when there are no fields"] = function()
  local entries = {
    {
      name = "widgets.Mode",
      type = "type",
      defines = {
        {
          file = "lua/widgets/init.lua",
          start = { 1 },
          type = "doc.alias",
          desc = '```lua\nwidgets.Mode:\n    | "fast"\n    | "slow"\n```',
        },
      },
      fields = {},
    },
  }

  local output = ApiAppendix.generate(entries, SECTIONS)

  MiniTest.expect.equality(
    output:find('### widgets.Mode\n\n```lua\nwidgets.Mode:\n    | "fast"\n    | "slow"\n```', 1, true) ~= nil,
    true
  )
end

T["api_appendix"]["orders files by their declared position, not alphabetically"] = function()
  local sections = {
    { title = "Widgets", files = { "lua/widgets/zeta.lua", "lua/widgets/alpha.lua" } },
  }
  local entries = {
    {
      name = "widgets.FromAlpha",
      type = "type",
      defines = { { file = "lua/widgets/alpha.lua", start = { 1 }, type = "doc.class" } },
      fields = {},
    },
    {
      name = "widgets.FromZeta",
      type = "type",
      defines = { { file = "lua/widgets/zeta.lua", start = { 1 }, type = "doc.class" } },
      fields = {},
    },
  }

  local output = ApiAppendix.generate(entries, sections)

  MiniTest.expect.equality(output:find("widgets.FromZeta", 1, true) < output:find("widgets.FromAlpha", 1, true), true)
end

T["api_appendix"]["falls back to the definition's own view when there is no desc"] = function()
  local entries = {
    {
      name = "widgets.Callback",
      type = "type",
      defines = {
        {
          file = "lua/widgets/init.lua",
          start = { 1 },
          type = "doc.alias",
          view = "fun(path?: string)",
        },
      },
      fields = {},
    },
  }

  local output = ApiAppendix.generate(entries, SECTIONS)

  MiniTest.expect.equality(output:find("### widgets.Callback\n\n```lua\nfun(path?: string)\n```", 1, true) ~= nil, true)
end

T["api_appendix"]["orders entries from the same section by file then definition line"] = function()
  local entries = {
    {
      name = "widgets.Second",
      type = "type",
      defines = { { file = "lua/widgets/init.lua", start = { 20 }, type = "doc.class" } },
      fields = {},
    },
    {
      name = "widgets.First",
      type = "type",
      defines = { { file = "lua/widgets/init.lua", start = { 5 }, type = "doc.class" } },
      fields = {},
    },
  }

  local output = ApiAppendix.generate(entries, SECTIONS)

  MiniTest.expect.equality(output:find("widgets.First", 1, true) < output:find("widgets.Second", 1, true), true)
end

T["api_appendix"]["ends with exactly one trailing newline"] = function()
  local output = ApiAppendix.generate({}, SECTIONS)

  MiniTest.expect.equality(output:sub(-1), "\n")
  MiniTest.expect.equality(output:sub(-2, -2) ~= "\n", true)
end

return T
