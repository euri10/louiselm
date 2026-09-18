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

local COVERAGE_SECTIONS = {
  {
    title = "Widgets",
    files = { "lua/widgets/init.lua", "lua/widgets/spin.lua" },
  },
  {
    title = "Gadgets",
    files = { "lua/gadgets/init.lua", "lua/gadgets/facade.lua" },
    -- The facade declares no LuaCATS type of its own, so a complete export
    -- documents nothing for it.
    expect_no_entries = { "lua/gadgets/facade.lua" },
  },
}

---@param name string
---@param file string
---@return table
local function class_entry(name, file)
  return {
    name = name,
    type = "type",
    defines = { { file = file, start = { 1 }, type = "doc.class" } },
    fields = {},
  }
end

T["api_appendix"]["verify_export accepts an export covering every section"] = function()
  local entries = {
    {
      name = "widgets.Widget",
      type = "type",
      defines = { { file = "lua/widgets/init.lua", start = { 3 }, type = "doc.class" } },
      fields = {},
    },
    {
      name = "gadgets.Gadget",
      type = "type",
      defines = { { file = "lua/gadgets/init.lua", start = { 1 }, type = "doc.class" } },
      fields = {},
    },
  }

  local ok, err = ApiAppendix.verify_export(entries, SECTIONS)

  MiniTest.expect.equality(ok, true)
  MiniTest.expect.equality(err, nil)
end

T["api_appendix"]["verify_export rejects an export missing a whole section"] = function()
  local entries = {
    {
      name = "widgets.Widget",
      type = "type",
      defines = { { file = "lua/widgets/init.lua", start = { 3 }, type = "doc.class" } },
      fields = {},
    },
  }

  local ok, err = ApiAppendix.verify_export(entries, SECTIONS)

  MiniTest.expect.equality(ok, false)
  MiniTest.expect.equality(type(err), "string")
  local message = err or ""
  MiniTest.expect.equality(message:find("Gadgets", 1, true) ~= nil, true)
  MiniTest.expect.equality(message:find("Widgets", 1, true), nil)
end

T["api_appendix"]["verify_export rejects an entirely empty export"] = function()
  local ok, err = ApiAppendix.verify_export({}, SECTIONS)

  MiniTest.expect.equality(ok, false)
  local message = err or ""
  MiniTest.expect.equality(message:find("Widgets", 1, true) ~= nil, true)
  MiniTest.expect.equality(message:find("Gadgets", 1, true) ~= nil, true)
end

T["api_appendix"]["verify_export rejects an export that covers every section but loses a file"] = function()
  -- `lua-language-server --doc` truncates per file, so an export can populate
  -- every section while dropping the files that loaded last.
  local entries = {
    class_entry("widgets.Widget", "lua/widgets/init.lua"),
    class_entry("gadgets.Gadget", "lua/gadgets/init.lua"),
  }

  local ok, err = ApiAppendix.verify_export(entries, COVERAGE_SECTIONS)

  MiniTest.expect.equality(ok, false)
  local message = err or ""
  MiniTest.expect.equality(message:find("lua/widgets/spin.lua", 1, true) ~= nil, true)
  MiniTest.expect.equality(message:find("lua/widgets/init.lua", 1, true), nil)
  MiniTest.expect.equality(message:find("lua/gadgets/facade.lua", 1, true), nil)
end

T["api_appendix"]["verify_export accepts a curated file declared to export no entries"] = function()
  local entries = {
    class_entry("widgets.Widget", "lua/widgets/init.lua"),
    class_entry("widgets.Spin", "lua/widgets/spin.lua"),
    class_entry("gadgets.Gadget", "lua/gadgets/init.lua"),
  }

  local ok, err = ApiAppendix.verify_export(entries, COVERAGE_SECTIONS)

  MiniTest.expect.equality(ok, true)
  MiniTest.expect.equality(err, nil)
end

T["api_appendix"]["verify_export rejects a stale expect_no_entries declaration"] = function()
  -- An exempt file that starts exporting is no longer covered by the gate;
  -- fail until its exemption is removed rather than leave it unguarded.
  local entries = {
    class_entry("widgets.Widget", "lua/widgets/init.lua"),
    class_entry("widgets.Spin", "lua/widgets/spin.lua"),
    class_entry("gadgets.Gadget", "lua/gadgets/init.lua"),
    class_entry("gadgets.Facade", "lua/gadgets/facade.lua"),
  }

  local ok, err = ApiAppendix.verify_export(entries, COVERAGE_SECTIONS)

  MiniTest.expect.equality(ok, false)
  local message = err or ""
  MiniTest.expect.equality(message:find("lua/gadgets/facade.lua", 1, true) ~= nil, true)
  MiniTest.expect.equality(message:find("expect_no_entries", 1, true) ~= nil, true)
end

return T
