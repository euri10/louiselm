local MiniTest = require("mini.test")
local Draft = require("louiselm.ui.chat.draft")

local T = MiniTest.new_set()

T["assembles resolved input in order without consuming it or requiring a buffer or Session"] = function()
  local draft = Draft.new()
  local item = { label = "notes", text = "original notes" }
  draft:add_context(item)
  item.text = "changed outside the draft"
  draft:select_skill({
    name = "review",
    description = "Review code",
    explicit_only = false,
    path = "/not-read/SKILL.md",
    content = "skill body",
  }, false)
  draft:add_context({ label = "AGENTS.md", uri = "file:///repo/AGENTS.md" })
  draft:set_catalog("hidden catalog")
  local text = assert(draft:prompt_text(draft.context_prefix .. "first line\nsecond line"))
  draft:queue(text)

  MiniTest.expect.equality({ draft:content("/compact", "$review", true) }, { "/compact", {} })
  local content, contexts = draft:content(text, "$review", true)
  MiniTest.expect.equality(content, {
    {
      type = "resource",
      resource = { uri = "louiselm://skills/index", mimeType = "text/plain", text = "hidden catalog" },
    },
    { type = "text", text = "original notes" },
    { type = "text", text = "skill body" },
    { type = "resource_link", uri = "file:///repo/AGENTS.md", name = "AGENTS.md" },
    { type = "text", text = "/$review first line\nsecond line" },
  })
  MiniTest.expect.equality(contexts, {
    { label = "skill-index", text = "hidden catalog" },
    { label = "notes", text = "original notes" },
    { label = "skill: review", text = "skill body", skill_path = "/not-read/SKILL.md" },
    { label = "AGENTS.md", uri = "file:///repo/AGENTS.md" },
  })
  local retry, retry_contexts = draft:content(text, "$review", false)
  content[1] = { type = "text", text = "hidden catalog" }
  MiniTest.expect.equality(retry, content)
  MiniTest.expect.equality(retry_contexts, contexts)
  MiniTest.expect.equality(draft:staged_context(), { contexts = 3, pending_skill = false, queued_prompt = true })
end

T["queue cancellation retains staged material until accepted context is consumed"] = function()
  local draft = Draft.new()
  local other = Draft.new()
  draft:select_skill({
    name = "review",
    description = "Review code",
    explicit_only = false,
    path = "/not-read/SKILL.md",
    content = "body",
  }, true)
  draft:set_catalog("catalog")
  draft:queue("next turn")
  draft:queue(nil)
  MiniTest.expect.equality(draft:staged_context(), { contexts = 0, pending_skill = true, queued_prompt = false })
  MiniTest.expect.equality(draft:prompt_text(draft.context_prefix), "")
  MiniTest.expect.equality(draft:content("", "$review"), {
    { type = "text", text = "catalog" },
    { type = "text", text = "/$review" },
  })
  draft:clear_context()
  MiniTest.expect.equality({ draft:prompt_text("") }, { nil, "prompt must be a non-empty string" })
  MiniTest.expect.equality(draft:content("follow-up"), "follow-up")
  MiniTest.expect.equality(draft:staged_context(), other:staged_context())
  MiniTest.expect.equality(other:content("independent"), "independent")
end

T["skill reconciliation requires an intact leading chip and preserves literal edits"] = function()
  for _, case in ipairs({
    { visible = "ordinary task", task = "ordinary task", pending = false },
    { visible = "[context: skill: review", task = "[context: skill: review", pending = false },
    { visible = "ordinary [context: skill: review]", task = "ordinary [context: skill: review]", pending = false },
    { visible = "[context: skill: review]task", task = "task", pending = true },
    { visible = "[context: skill: review]", task = "", pending = true },
  }) do
    local draft = Draft.new()
    draft:select_skill({
      name = "review",
      description = "Review code",
      explicit_only = false,
      path = "/not-read/SKILL.md",
      content = "body",
    }, true)
    MiniTest.expect.equality(draft:reconcile_skills(case.visible), case.task)
    MiniTest.expect.equality(draft:staged_context().pending_skill, case.pending)
  end
end

T["prepends staged context to typed Handoff blocks without mutating or consuming inputs"] = function()
  local draft = Draft.new()
  draft:set_catalog("catalog")
  local body = {
    { type = "text", text = "take over" },
    { type = "resource", resource = { uri = "louiselm://handoff/source", mimeType = "text/markdown", text = "brief" } },
  }
  local content, contexts = draft:with_context(body, true)
  MiniTest.expect.equality(#body, 2)
  MiniTest.expect.equality(#content, 3)
  MiniTest.expect.equality(content[1].resource.uri, "louiselm://skills/index")
  MiniTest.expect.equality({ content[2], content[3] }, body)
  MiniTest.expect.equality(contexts, { { label = "skill-index", text = "catalog" } })
  MiniTest.expect.equality(draft:with_context(body, true), content)
  draft:clear_context()
  MiniTest.expect.equality({ draft:with_context(body, true) }, { body, {} })
end

return T
