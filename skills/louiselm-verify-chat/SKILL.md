---
name: louiselm-verify-chat
description: Verifies a LouiseLM chat turn against the real Neovim UI coordination and a local mock ACP Agent. Use when checking chat or Session changes end to end, or when asked for a reproducible LouiseLM smoke check.
---

# Verify LouiseLM

Run from the repository root. Follow `AGENTS.md` and `docs/agent-testing.md` for the affected test gates; this skill adds one observable chat journey. Do not substitute this smoke check for the full affected suite.

## Feature map: first chat turn

- **User goal:** open a chat, submit a prompt, and see the Agent's reply.
- **Drive:** `scripts/first-chat.lua` opens `:LouiselmChat` with the repository's mock ACP Agent, edits the chat prompt, and invokes the buffer's insert-mode Enter handler.
- **Proof:** the Session completes turn 1 and the chat buffer contains both the submitted prompt and echoed reply. The script writes a JSON receipt before disposing the chat and Session.
- **Coverage limit:** this invokes the actual Enter handler but does not exercise physical key dispatch, the skill picker, a stock Agent, or Provider access. Add another mapped journey only when it has its own runnable proof.

## Run

Check that `nvim` is Neovim 0.12 or newer and `sqlite3` is at least 3.38, then run:

```sh
verify_dir=$(mktemp -d /tmp/louiselm-verify.XXXXXX)
XDG_STATE_HOME="$verify_dir/state" \
  LOUISELM_VERIFY_EVIDENCE="$verify_dir/evidence.json" \
  nvim --headless --noplugin -u NONE -l \
    ~/.config/agentskills/generated/louiselm-verify-chat/scripts/first-chat.lua
```

Read `$verify_dir/evidence.json` after a zero exit. A missing receipt or nonzero exit is failure; report the command output and the exact phase that failed. The script uses no credentials or network and disposes its mock Agent and Session even on assertion failure. Keep the JSON receipt for the review; remove only the scratch `state` directory after inspecting it. Never target the repository, a broad temp directory, or another Neovim instance for cleanup.
