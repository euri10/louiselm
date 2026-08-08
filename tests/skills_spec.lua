local MiniTest = require("mini.test")
local Skills = require("louiselm.skills")

---@diagnostic disable-next-line: undefined-global -- `vim` is Neovim's injected runtime API.
local nvim = vim
local temp_dir
local T = MiniTest.new_set({
  hooks = {
    pre_case = function()
      temp_dir = nvim.fn.tempname()
      assert(nvim.fn.mkdir(temp_dir, "p") == 1)
    end,
    post_case = function()
      nvim.fn.delete(temp_dir, "rf")
      temp_dir = nil
    end,
  },
})

local function write_skill(directory, name, description, body)
  assert(nvim.fn.mkdir(directory, "p") == 1 or nvim.fn.isdirectory(directory) == 1)
  local path = nvim.fs.joinpath(directory, "SKILL.md")
  nvim.fn.writefile({
    "---",
    "name: " .. name,
    "description: " .. description,
    "---",
    body or ("# " .. name),
  }, path)
  return path
end

T["discover"] = MiniTest.new_set()

T["discover"]["reads skill metadata and ignores skill content"] = function()
  local first_path = write_skill(nvim.fs.joinpath(temp_dir, "z-last"), "z-last", "Last skill", "secret body")
  local second_path = write_skill(nvim.fs.joinpath(temp_dir, "a-first"), "a-first", "First skill", "secret body")

  local skills, errors = Skills.discover({ temp_dir })

  MiniTest.expect.equality(errors, {})
  MiniTest.expect.equality(skills, {
    { name = "a-first", description = "First skill", path = second_path },
    { name = "z-last", description = "Last skill", path = first_path },
  })
end

T["discover"]["collects malformed metadata without stopping other skills"] = function()
  write_skill(nvim.fs.joinpath(temp_dir, "valid"), "valid", "Valid skill")
  local invalid_dir = nvim.fs.joinpath(temp_dir, "invalid")
  assert(nvim.fn.mkdir(invalid_dir, "p") == 1)
  nvim.fn.writefile({ "# no frontmatter" }, nvim.fs.joinpath(invalid_dir, "SKILL.md"))

  local skills, errors = Skills.discover({ temp_dir })

  MiniTest.expect.equality(#skills, 1)
  MiniTest.expect.equality(skills[1].name, "valid")
  MiniTest.expect.equality(#errors, 1)
  MiniTest.expect.equality(errors[1].path, nvim.fs.joinpath(invalid_dir, "SKILL.md"))
end

T["discover"]["accepts standard nested metadata"] = function()
  local skill_dir = nvim.fs.joinpath(temp_dir, "nested-metadata")
  assert(nvim.fn.mkdir(skill_dir, "p") == 1)
  local path = nvim.fs.joinpath(skill_dir, "SKILL.md")
  nvim.fn.writefile({
    "---",
    "name: nested-metadata",
    "description: Top-level description remains authoritative",
    "metadata:",
    "  short-description: Short description for a picker",
    "---",
    "# Nested metadata",
  }, path)

  local skills, errors = Skills.discover({ temp_dir })

  MiniTest.expect.equality(errors, {})
  MiniTest.expect.equality(skills, {
    {
      name = "nested-metadata",
      description = "Top-level description remains authoritative",
      path = path,
    },
  })
end

T["discover"]["rejects malformed nested metadata indentation"] = function()
  local skill_dir = nvim.fs.joinpath(temp_dir, "bad-metadata")
  assert(nvim.fn.mkdir(skill_dir, "p") == 1)
  local path = nvim.fs.joinpath(skill_dir, "SKILL.md")
  nvim.fn.writefile({
    "---",
    "name: bad-metadata",
    "description: Invalid indentation",
    "metadata:",
    "    short-description: Too deeply indented",
    "---",
  }, path)

  local skills, errors = Skills.discover({ temp_dir })

  MiniTest.expect.equality(skills, {})
  MiniTest.expect.equality(errors, { { path = path, message = "malformed YAML frontmatter" } })
end

T["policy"] = MiniTest.new_set()

T["policy"]["defaults to native and accepts the three policies"] = function()
  MiniTest.expect.equality(Skills.policy(), "native")
  MiniTest.expect.equality(Skills.policy(nil, true), "native")
  MiniTest.expect.equality(Skills.policy(nil, false), "inject")
  MiniTest.expect.equality(Skills.policy("native"), "native")
  MiniTest.expect.equality(Skills.policy("inject"), "inject")
  MiniTest.expect.equality(Skills.policy("off"), "off")
end

T["policy"]["rejects unknown policy values"] = function()
  local policy, err = Skills.policy("always")

  MiniTest.expect.equality(policy, nil)
  MiniTest.expect.equality(err, "skills policy must be one of: inject, native, off")
end

T["inject"] = MiniTest.new_set()

T["inject"]["includes only skill index metadata"] = function()
  local index = assert(Skills.inject({
    { name = "grill-me", description = "Stress test an idea", path = "/skills/grill-me/SKILL.md", content = "secret" },
  }))

  MiniTest.expect.equality(index:find("grill%-me", 1, false) ~= nil, true)
  MiniTest.expect.equality(index:find("Stress test an idea", 1, true) ~= nil, true)
  MiniTest.expect.equality(index:find("/skills/grill%-me/SKILL.md", 1, false) ~= nil, true)
  MiniTest.expect.equality(index:find("secret", 1, true), nil)
end

T["overlap"] = MiniTest.new_set()

T["overlap"]["resolves symlinks before checking native skill directories"] = function()
  local configured = nvim.fs.joinpath(temp_dir, "configured")
  local native_target = nvim.fs.joinpath(configured, "native")
  local native_link = nvim.fs.joinpath(temp_dir, "native-link")
  assert(nvim.fn.mkdir(native_target, "p") == 1)
  assert(nvim.uv.fs_symlink(native_target, native_link))

  local overlaps, overlap_path = Skills.overlap(native_link, { configured })

  MiniTest.expect.equality(overlaps, true)
  MiniTest.expect.equality(overlap_path, configured)
end

T["overlap"]["does not confuse path prefixes"] = function()
  local native = nvim.fs.joinpath(temp_dir, "skills")
  assert(nvim.fn.mkdir(native, "p") == 1)

  local overlaps, overlap_path = Skills.overlap(native, { nvim.fs.joinpath(temp_dir, "skills-other") })

  MiniTest.expect.equality(overlaps, false)
  MiniTest.expect.equality(overlap_path, nil)
end

return T
