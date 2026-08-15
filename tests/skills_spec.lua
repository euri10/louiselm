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

local function write_lines(directory, lines)
  assert(nvim.fn.mkdir(directory, "p") == 1 or nvim.fn.isdirectory(directory) == 1)
  local path = nvim.fs.joinpath(directory, "SKILL.md")
  assert(nvim.fn.writefile(lines, path) == 0)
  return path
end

local function write_openai_metadata(directory, lines)
  local agents = nvim.fs.joinpath(directory, "agents")
  assert(nvim.fn.mkdir(agents, "p") == 1 or nvim.fn.isdirectory(agents) == 1)
  local path = nvim.fs.joinpath(agents, "openai.yaml")
  assert(nvim.fn.writefile(lines, path) == 0)
  return path
end

T["discover"] = MiniTest.new_set()

T["discover"]["reads skill metadata and full skill content"] = function()
  local first_path = write_skill(nvim.fs.joinpath(temp_dir, "z-last"), "z-last", "Last skill", "secret body")
  local second_path = write_skill(nvim.fs.joinpath(temp_dir, "a-first"), "a-first", "First skill", "secret body")

  local skills, errors = Skills.discover({ temp_dir })

  MiniTest.expect.equality(errors, {})
  MiniTest.expect.equality(skills, {
    {
      name = "a-first",
      description = "First skill",
      path = second_path,
      content = "---\nname: a-first\ndescription: First skill\n---\nsecret body\n",
      explicit_only = false,
    },
    {
      name = "z-last",
      description = "Last skill",
      path = first_path,
      content = "---\nname: z-last\ndescription: Last skill\n---\nsecret body\n",
      explicit_only = false,
    },
  })
end

T["discover"]["uses root order before name order and reports shadowed names"] = function()
  local first_root = nvim.fs.joinpath(temp_dir, "z-first-root")
  local second_root = nvim.fs.joinpath(temp_dir, "a-second-root")
  local zulu_path = write_skill(nvim.fs.joinpath(first_root, "zulu"), "zulu", "First root")
  local first_shared = write_skill(nvim.fs.joinpath(first_root, "shared"), "shared", "First wins")
  local alpha_path = write_skill(nvim.fs.joinpath(second_root, "alpha"), "alpha", "Second root")
  local shadowed = write_skill(nvim.fs.joinpath(second_root, "shared"), "shared", "Second loses")

  local skills, diagnostics = Skills.discover({ first_root, second_root })

  MiniTest.expect.equality(
    nvim.tbl_map(function(skill)
      return { skill.name, skill.path }
    end, skills),
    {
      { "shared", first_shared },
      { "zulu", zulu_path },
      { "alpha", alpha_path },
    }
  )
  MiniTest.expect.equality(diagnostics, {
    {
      path = shadowed,
      message = "skill 'shared' is shadowed by " .. first_shared,
      severity = "warning",
    },
  })
  local repeated_skills, repeated_diagnostics = Skills.discover({ first_root, second_root })
  MiniTest.expect.equality(
    nvim.tbl_map(function(skill)
      return skill.path
    end, repeated_skills),
    nvim.tbl_map(function(skill)
      return skill.path
    end, skills)
  )
  MiniTest.expect.equality(repeated_diagnostics, diagnostics)
end

T["discover"]["deduplicates canonical roots while preserving the first alias"] = function()
  local target = nvim.fs.joinpath(temp_dir, "target")
  write_skill(nvim.fs.joinpath(target, "linked"), "linked", "Linked skill")
  local first_alias = nvim.fs.joinpath(temp_dir, "z-first-alias")
  local second_alias = nvim.fs.joinpath(temp_dir, "a-second-alias")
  assert(nvim.uv.fs_symlink(target, first_alias))
  assert(nvim.uv.fs_symlink(target, second_alias))

  local skills, diagnostics = Skills.discover({ first_alias, second_alias })

  MiniTest.expect.equality(#skills, 1)
  MiniTest.expect.equality(skills[1].path, nvim.fs.joinpath(first_alias, "linked", "SKILL.md"))
  MiniTest.expect.equality(diagnostics, {
    {
      path = second_alias,
      message = "configured root resolves to the already scanned root " .. first_alias,
      severity = "warning",
    },
  })
end

T["discover"]["deduplicates identical canonical skill files"] = function()
  local root = nvim.fs.joinpath(temp_dir, "root")
  local target_directory = nvim.fs.joinpath(root, "target")
  assert(nvim.fn.mkdir(target_directory, "p") == 1)
  local target = nvim.fs.joinpath(target_directory, "instructions.md")
  assert(nvim.fn.writefile({ "---", "name: linked", "description: Linked skill", "---" }, target) == 0)
  local first_directory = nvim.fs.joinpath(root, "a-first", "linked")
  local second_directory = nvim.fs.joinpath(root, "b-second", "linked")
  assert(nvim.fn.mkdir(first_directory, "p") == 1)
  assert(nvim.fn.mkdir(second_directory, "p") == 1)
  local first = nvim.fs.joinpath(first_directory, "SKILL.md")
  local second = nvim.fs.joinpath(second_directory, "SKILL.md")
  assert(nvim.uv.fs_symlink(target, first))
  assert(nvim.uv.fs_symlink(target, second))

  local skills, diagnostics = Skills.discover({ root })

  MiniTest.expect.equality(#skills, 1)
  MiniTest.expect.equality(skills[1].path, first)
  MiniTest.expect.equality(diagnostics, {
    {
      path = second,
      message = "canonical SKILL.md already discovered: " .. second .. " -> " .. first,
      severity = "warning",
    },
  })
end

T["discover"]["follows skill directory symlinks and warns when they leave the root"] = function()
  local root = nvim.fs.joinpath(temp_dir, "root")
  local outside = nvim.fs.joinpath(temp_dir, "outside", "external")
  local target = write_skill(outside, "external", "External skill")
  assert(nvim.fn.mkdir(root, "p") == 1)
  local alias = nvim.fs.joinpath(root, "external")
  assert(nvim.uv.fs_symlink(outside, alias))

  local skills, diagnostics = Skills.discover({ root })

  MiniTest.expect.equality(#skills, 1)
  MiniTest.expect.equality(skills[1].path, nvim.fs.joinpath(alias, "SKILL.md"))
  MiniTest.expect.equality(skills[1].content:find("External skill", 1, true) ~= nil, true)
  MiniTest.expect.equality(diagnostics, {
    {
      path = alias,
      message = "symlink resolves outside configured root: " .. alias .. " -> " .. nvim.fs.dirname(target),
      severity = "warning",
    },
  })
end

T["discover"]["detects symlink cycles without traversing them"] = function()
  local root = nvim.fs.joinpath(temp_dir, "root")
  local skill_path = write_skill(nvim.fs.joinpath(root, "safe"), "safe", "Safe skill")
  local loop = nvim.fs.joinpath(root, "loop")
  assert(nvim.uv.fs_symlink(root, loop))

  local skills, diagnostics = Skills.discover({ root })

  MiniTest.expect.equality(#skills, 1)
  MiniTest.expect.equality(skills[1].path, skill_path)
  MiniTest.expect.equality(diagnostics, {
    {
      path = loop,
      message = "canonical directory already scanned: " .. loop .. " -> " .. root,
      severity = "warning",
    },
  })
end

T["discover"]["resolves relative roots against an explicit session cwd"] = function()
  local cwd = nvim.fs.joinpath(temp_dir, "workspace")
  local path = write_skill(nvim.fs.joinpath(cwd, "relative-skills", "relative"), "relative", "Relative skill")

  local skills, diagnostics = Skills.discover({ "relative-skills" }, cwd)

  MiniTest.expect.equality(diagnostics, {})
  MiniTest.expect.equality(#skills, 1)
  MiniTest.expect.equality(skills[1].path, path)
end

T["discover"]["continues past unreadable skills with stable diagnostics"] = function()
  local readable = write_skill(nvim.fs.joinpath(temp_dir, "readable"), "readable", "Readable skill")
  local unreadable = write_skill(nvim.fs.joinpath(temp_dir, "unreadable"), "unreadable", "Unreadable skill")
  local original_readfile = nvim.fn.readfile
  nvim.fn.readfile = function(path, ...)
    if path == unreadable then
      error("permission denied")
    end
    return original_readfile(path, ...)
  end

  local call_ok, skills, diagnostics = pcall(Skills.discover, { temp_dir })

  nvim.fn.readfile = original_readfile
  assert(call_ok)
  MiniTest.expect.equality(#skills, 1)
  MiniTest.expect.equality(skills[1].path, readable)
  MiniTest.expect.equality(diagnostics, { { path = unreadable, message = "could not read SKILL.md" } })
  local repeated_skills, repeated_diagnostics = Skills.discover({ temp_dir })
  MiniTest.expect.equality(#repeated_skills, 2)
  MiniTest.expect.equality(repeated_diagnostics, {})
end

T["discover"]["reads SKILL.md symlinks whose targets are files"] = function()
  local source = nvim.fs.joinpath(temp_dir, "source")
  local target = write_skill(source, "linked-skill", "Linked skill")
  local skill_dir = nvim.fs.joinpath(temp_dir, "generated", "linked-skill")
  assert(nvim.fn.mkdir(skill_dir, "p") == 1)
  local link = nvim.fs.joinpath(skill_dir, "SKILL.md")
  assert(nvim.uv.fs_symlink(target, link))

  local skills, errors = Skills.discover({ nvim.fs.joinpath(temp_dir, "generated") })

  MiniTest.expect.equality(errors, {
    {
      path = link,
      message = "symlink resolves outside configured root: " .. link .. " -> " .. target,
      severity = "warning",
    },
  })
  MiniTest.expect.equality(skills, {
    {
      name = "linked-skill",
      description = "Linked skill",
      path = link,
      content = "---\nname: linked-skill\ndescription: Linked skill\n---\n# linked-skill\n",
      explicit_only = false,
    },
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
      content = table.concat({
        "---",
        "name: nested-metadata",
        "description: Top-level description remains authoritative",
        "metadata:",
        "  short-description: Short description for a picker",
        "---",
        "# Nested metadata",
      }, "\n") .. "\n",
      explicit_only = false,
    },
  })
end

T["discover"]["accepts LibYAML syntax and validates standard metadata"] = function()
  local directory = nvim.fs.joinpath(temp_dir, "yaml-features")
  local path = write_lines(directory, {
    "---",
    "name: yaml-features",
    "description: >-",
    "  Handles YAML anchors, folded scalars,",
    "  and quoted values.",
    "license: &license MIT",
    "compatibility: 'Requires git'",
    "metadata:",
    "  author: example-org",
    "  license: *license",
    'allowed-tools: "Bash(git:*) Read"',
    "x-extension:",
    "  arbitrary: true",
    "---",
    "# Instructions",
  })

  local skills, diagnostics = Skills.discover({ temp_dir })

  MiniTest.expect.equality(diagnostics, {})
  MiniTest.expect.equality(skills, {
    {
      name = "yaml-features",
      description = "Handles YAML anchors, folded scalars, and quoted values.",
      path = path,
      content = table.concat({
        "---",
        "name: yaml-features",
        "description: >-",
        "  Handles YAML anchors, folded scalars,",
        "  and quoted values.",
        "license: &license MIT",
        "compatibility: 'Requires git'",
        "metadata:",
        "  author: example-org",
        "  license: *license",
        'allowed-tools: "Bash(git:*) Read"',
        "x-extension:",
        "  arbitrary: true",
        "---",
        "# Instructions",
      }, "\n") .. "\n",
      explicit_only = false,
    },
  })
  MiniTest.expect.equality(rawget(skills[1], "allowed_tools"), nil)
end

T["discover"]["rejects invalid YAML and invalid standard field types"] = function()
  local yaml_path = write_lines(nvim.fs.joinpath(temp_dir, "invalid-yaml"), {
    "---",
    "name: invalid-yaml",
    "description: [unterminated",
    "---",
  })
  local metadata_path = write_lines(nvim.fs.joinpath(temp_dir, "invalid-metadata"), {
    "---",
    "name: invalid-metadata",
    "description: Invalid metadata",
    "metadata:",
    "  version: 1",
    "---",
  })
  local tools_path = write_lines(nvim.fs.joinpath(temp_dir, "invalid-tools"), {
    "---",
    "name: invalid-tools",
    "description: Invalid tools",
    "allowed-tools:",
    "  - Read",
    "---",
  })

  local skills, diagnostics = Skills.discover({ temp_dir })

  MiniTest.expect.equality(skills, {})
  MiniTest.expect.equality(diagnostics, {
    { path = metadata_path, message = "frontmatter metadata must map string keys to string values" },
    { path = tools_path, message = "frontmatter allowed-tools must be a string" },
    { path = yaml_path, message = "invalid YAML frontmatter" },
  })
end

T["discover"]["validates Agent Skills name and description constraints"] = function()
  local long_name = string.rep("a", 65)
  local name_path = write_lines(nvim.fs.joinpath(temp_dir, long_name), {
    "---",
    "name: " .. long_name,
    "description: Too long",
    "---",
  })
  local mismatch_path = write_skill(nvim.fs.joinpath(temp_dir, "directory-name"), "different-name", "Mismatch")
  local description_path = write_lines(nvim.fs.joinpath(temp_dir, "long-description"), {
    "---",
    "name: long-description",
    "description: " .. string.rep("x", 1025),
    "---",
  })

  local skills, diagnostics = Skills.discover({ temp_dir })

  MiniTest.expect.equality(skills, {})
  MiniTest.expect.equality(diagnostics, {
    { path = name_path, message = "frontmatter name must be at most 64 characters" },
    { path = mismatch_path, message = "skill name must match its parent directory 'directory-name'" },
    { path = description_path, message = "frontmatter description must be at most 1024 characters" },
  })
end

T["discover"]["honors both explicit-only controls and warns on conflicts"] = function()
  local claude_dir = nvim.fs.joinpath(temp_dir, "claude-only")
  local claude_path = write_lines(claude_dir, {
    "---",
    "name: claude-only",
    "description: Explicit through SKILL.md",
    "disable-model-invocation: true",
    "---",
  })
  local codex_dir = nvim.fs.joinpath(temp_dir, "codex-only")
  local codex_path = write_skill(codex_dir, "codex-only", "Explicit through OpenAI metadata")
  write_openai_metadata(codex_dir, { "policy:", "  allow_implicit_invocation: false" })
  local conflict_dir = nvim.fs.joinpath(temp_dir, "conflicting")
  local conflict_path = write_lines(conflict_dir, {
    "---",
    "name: conflicting",
    "description: Controls disagree",
    "disable-model-invocation: false",
    "---",
  })
  local conflict_metadata = write_openai_metadata(conflict_dir, {
    "policy:",
    "  allow_implicit_invocation: false",
  })

  local skills, diagnostics = Skills.discover({ temp_dir })

  MiniTest.expect.equality(skills, {
    {
      name = "claude-only",
      description = "Explicit through SKILL.md",
      path = claude_path,
      content = table.concat({
        "---",
        "name: claude-only",
        "description: Explicit through SKILL.md",
        "disable-model-invocation: true",
        "---",
      }, "\n") .. "\n",
      explicit_only = true,
    },
    {
      name = "codex-only",
      description = "Explicit through OpenAI metadata",
      path = codex_path,
      content = "---\nname: codex-only\ndescription: Explicit through OpenAI metadata\n---\n# codex-only\n",
      explicit_only = true,
    },
    {
      name = "conflicting",
      description = "Controls disagree",
      path = conflict_path,
      content = table.concat({
        "---",
        "name: conflicting",
        "description: Controls disagree",
        "disable-model-invocation: false",
        "---",
      }, "\n") .. "\n",
      explicit_only = true,
    },
  })
  MiniTest.expect.equality(diagnostics, {
    {
      path = conflict_metadata,
      message = "invocation controls disagree; treating skill as explicit-only",
      severity = "warning",
    },
  })
end

T["discover"]["retains skills with broken optional invocation metadata"] = function()
  local frontmatter_dir = nvim.fs.joinpath(temp_dir, "bad-frontmatter-control")
  local frontmatter_path = write_lines(frontmatter_dir, {
    "---",
    "name: bad-frontmatter-control",
    "description: Bad optional frontmatter control",
    "disable-model-invocation: sometimes",
    "---",
  })
  local openai_dir = nvim.fs.joinpath(temp_dir, "bad-openai-control")
  local openai_path = write_skill(openai_dir, "bad-openai-control", "Bad optional OpenAI control")
  local openai_metadata = write_openai_metadata(openai_dir, { "policy: [invalid" })
  local unreadable_dir = nvim.fs.joinpath(temp_dir, "unreadable-openai")
  local unreadable_path = write_skill(unreadable_dir, "unreadable-openai", "Unreadable OpenAI metadata")
  local unreadable_metadata = nvim.fs.joinpath(unreadable_dir, "agents", "openai.yaml")
  assert(nvim.fn.mkdir(unreadable_metadata, "p") == 1)

  local skills, diagnostics = Skills.discover({ temp_dir })

  MiniTest.expect.equality(#skills, 3)
  MiniTest.expect.equality(skills[1].path, frontmatter_path)
  MiniTest.expect.equality(skills[1].explicit_only, true)
  MiniTest.expect.equality(skills[2].path, openai_path)
  MiniTest.expect.equality(skills[2].explicit_only, true)
  MiniTest.expect.equality(skills[3].path, unreadable_path)
  MiniTest.expect.equality(skills[3].explicit_only, true)
  MiniTest.expect.equality(diagnostics, {
    {
      path = frontmatter_path,
      message = "frontmatter disable-model-invocation must be a boolean; treating skill as explicit-only",
      severity = "warning",
    },
    {
      path = openai_metadata,
      message = "invalid agents/openai.yaml; treating skill as explicit-only",
      severity = "warning",
    },
    {
      path = unreadable_metadata,
      message = "could not read agents/openai.yaml; treating skill as explicit-only",
      severity = "warning",
    },
  })
end

T["discover"]["warns when SKILL.md exceeds 500 lines"] = function()
  local lines = { "---", "name: long-skill", "description: Long skill", "---" }
  for index = 5, 501 do
    lines[index] = "body"
  end
  local path = write_lines(nvim.fs.joinpath(temp_dir, "long-skill"), lines)

  local skills, diagnostics = Skills.discover({ temp_dir })

  MiniTest.expect.equality(#skills, 1)
  MiniTest.expect.equality(diagnostics, {
    {
      path = path,
      message = "SKILL.md exceeds 500 lines; move detailed material into referenced files",
      severity = "warning",
    },
  })
end

T["discover"]["reports a structured missing lyaml dependency"] = function()
  write_skill(nvim.fs.joinpath(temp_dir, "valid"), "valid", "Valid skill")
  local loaded = package.loaded.lyaml
  local preload = package.preload.lyaml
  package.loaded.lyaml = nil
  rawset(package.preload, "lyaml", function()
    error("module 'lyaml' not found", 0)
  end)

  local call_ok, skills, diagnostics = pcall(Skills.discover, { temp_dir })

  package.loaded.lyaml = loaded
  rawset(package.preload, "lyaml", preload)
  assert(call_ok)
  MiniTest.expect.equality(skills, {})
  MiniTest.expect.equality(diagnostics, {
    {
      path = "skills",
      message = "Neovim cannot load lyaml; run :checkhealth louiselm",
      detail = 'Neovim cannot find lyaml in package.path or package.cpath; install it with `luarocks --lua-version 5.1 install lyaml` or, if LuaRocks already reports it installed, add `eval "$(luarocks path --lua-version 5.1 --no-bin)"` to the shell startup file that launches Neovim',
      code = "missing_dependency",
    },
  })
end

T["discover"]["distinguishes a lyaml native loader failure"] = function()
  write_skill(nvim.fs.joinpath(temp_dir, "valid"), "valid", "Valid skill")
  local loaded = package.loaded.lyaml
  local preload = package.preload.lyaml
  package.loaded.lyaml = nil
  rawset(package.preload, "lyaml", function()
    error("error loading module 'yaml': libyaml.so.0: cannot open shared object file", 0)
  end)

  local call_ok, skills, diagnostics = pcall(Skills.discover, { temp_dir })

  package.loaded.lyaml = loaded
  rawset(package.preload, "lyaml", preload)
  assert(call_ok)
  MiniTest.expect.equality(skills, {})
  MiniTest.expect.equality(diagnostics, {
    {
      path = "skills",
      message = "Neovim cannot load lyaml; run :checkhealth louiselm",
      detail = "Neovim found lyaml but could not load it; reinstall lyaml for Lua 5.1 and verify that LibYAML is available",
      code = "missing_dependency",
    },
  })
end

T["discover"]["ignores optional sequence metadata"] = function()
  local skill_dir = nvim.fs.joinpath(temp_dir, "sequence-metadata")
  local path = nvim.fs.joinpath(skill_dir, "SKILL.md")
  assert(nvim.fn.mkdir(skill_dir, "p") == 1)
  nvim.fn.writefile({
    "---",
    "name: sequence-metadata",
    "description: Optional metadata does not affect discovery",
    "triggers:",
    "  - first",
    "  - second",
    "x-sources:",
    "  sources:",
    "    - first source",
    "    - second source",
    "---",
  }, path)

  local skills, errors = Skills.discover({ temp_dir })

  MiniTest.expect.equality(errors, {})
  MiniTest.expect.equality(#skills, 1)
  MiniTest.expect.equality(skills[1].name, "sequence-metadata")
end

T["discover"]["rejects tab-indented optional metadata"] = function()
  local skill_dir = nvim.fs.joinpath(temp_dir, "bad-metadata")
  assert(nvim.fn.mkdir(skill_dir, "p") == 1)
  local path = nvim.fs.joinpath(skill_dir, "SKILL.md")
  nvim.fn.writefile({
    "---",
    "name: bad-metadata",
    "description: Invalid indentation",
    "metadata:",
    "\tshort-description: Tabs cannot indent YAML",
    "---",
  }, path)

  local skills, errors = Skills.discover({ temp_dir })

  MiniTest.expect.equality(skills, {})
  MiniTest.expect.equality(errors, { { path = path, message = "invalid YAML frontmatter" } })
end

T["policy"] = MiniTest.new_set()

T["policy"]["defaults to native and accepts the three policies"] = function()
  MiniTest.expect.equality(Skills.policy(), "native")
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

T["inject"]["does not restore removed full-content injection"] = function()
  local index, err = Skills.inject({
    {
      name = "grill-me",
      description = "Stress test an idea",
      path = "/skills/grill-me/SKILL.md",
      content = "secret body",
    },
  }, true)

  MiniTest.expect.equality(index, nil)
  MiniTest.expect.equality(err, 'full-content injection was removed; use skills.policy = "inject"')
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
