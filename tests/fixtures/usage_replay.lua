-- Structural ordering from louiselm-hxvc comments 247-252: DeepSeek
-- session-5b4fb715-7267-4f09-848b-54e84fca2dfc, connection
-- ~/.local/state/acp-llm-adapter/connections/57902114-99cf-42b6-ae4c-d88b46a85b04.jsonl
-- lines 4-170: user/Agent/tool updates precede the load response; tool updates
-- cause intermediate state_changed(starting). Text and turn count are synthetic.
-- Consecutive split user chunks exercise grouping; that split was not observed.
---@diagnostic disable-next-line: undefined-global -- Neovim test runtime.
local nvim = vim
local updates = {}
for turn = 1, tonumber(nvim.env.LOUISELM_TEST_REPLAY_TURNS) or 0 do
  for _, text in ipairs({ "prompt " .. turn, "continued " .. turn }) do
    updates[#updates + 1] = { sessionUpdate = "user_message_chunk", content = { type = "text", text = text } }
  end
  updates[#updates + 1] = {
    sessionUpdate = "agent_message_chunk",
    content = { type = "text", text = "answer " .. turn },
  }
  updates[#updates + 1] = { sessionUpdate = "tool_call", toolCallId = "tool-" .. turn, status = "completed" }
end
require("louiselm.dev.mock_agent").run({
  replay_updates = updates,
  -- Normalized fixture values; field shape from hxvc's prompt response at
  -- connections/3b75a6dd-f56b-407c-84ad-f9194c74e416.jsonl:2086.
  usage = {
    totalTokens = 30,
    inputTokens = 20,
    outputTokens = 10,
    thoughtTokens = 0,
    cachedReadTokens = 7,
    cachedWriteTokens = 2,
  },
})
