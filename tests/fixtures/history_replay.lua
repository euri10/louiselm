-- Synthetic source gaps and redactions; this ordering was not observed in a live log.
require("louiselm.dev.mock_agent").run({
  replay_updates = {
    {
      sessionUpdate = "user_message_chunk",
      content = { type = "text", text = "[resource] <file:///secret>\nPRIVATE" },
    },
    { sessionUpdate = "agent_message_chunk", content = { type = "text", text = "Visible answer" } },
    { sessionUpdate = "user_message_chunk", content = { type = "image", data = "not retained" } },
    {
      sessionUpdate = "tool_call",
      toolCallId = "tool",
      status = "completed",
      rawInput = { command = "ls", credentials = "PRIVATE" },
      rawOutput = { stdout = string.rep("x", 9000) },
    },
  },
})
