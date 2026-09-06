# LouiseLM Tutor

Welcome. LouiseLM gives you a chat inside Neovim and lets an Agent work with
the files in your project. This short tour teaches the core loop.

Want to feel the interaction before configuring an Agent? [Try LouiseLM in
your browser](https://louiselm.com/demo/). It runs real Neovim and LouiseLM
against a clearly disclosed scripted demo; no Agent or Provider is connected.

## 1. Start here

You are reading the Tutor inside Neovim. The file is read-only so the shipped
lesson stays intact. To keep your own copy, use:

```vim
:saveas ~/.config/nvim/louiselm-tutorial.md
```

Commands are the reliable way to use LouiseLM. Default keymaps are listed as
shortcuts; your Neovim configuration may change or disable them.

## 2. Check your setup

Run:

```vim
:checkhealth louiselm
```

Health checks tell you whether LouiseLM can find your configured Agents and
their executables. If no Agent is configured, finish the setup instructions in
the README or [onboarding guide](onboarding.md), then run the health check again.

## 3. Try a first prompt

Open the chat:

```vim
:LouiselmChat
```

Type your prompt after the `>` marker and press `<Enter>` in Insert mode. Try
something small, such as:

```text
Explain what this project does in three sentences. Do not change any files.
```

The current Session stays attached to the Agent that started it. LouiseLM
shows the Agent's response as it arrives and asks you before an Agent performs
an operation that needs your permission.

Default shortcut: `<leader>lc`.

## 4. Permissions

When the Agent asks to run a command or change a file, read the request and
choose deliberately. A permission decision is not the same as accepting the
Agent's answer. You remain responsible for allowing effects in your project.

Inspect or revoke remembered choices with:

```vim
:LouiselmPermissions
```

Default shortcut: `<leader>lp`.

## 5. Give the Agent context

You can queue context for the next prompt. LouiseLM sends queued context when
you submit that prompt.

From a file you want the Agent to see:

```vim
:LouiselmMentionBuffer
```

Choose a file with:

```vim
:LouiselmPickFile
```

You can also select text in Visual mode and run:

```vim
:LouiselmSendSelection
```

Default shortcuts:

- `<leader>lb` queues the current buffer.
- `<leader>lf` picks a file.
- `<leader>ls` sends a Visual selection.

## 6. Stay in control

Stop the current turn when you change your mind:

```vim
:LouiselmCancel
```

Default shortcut: `<leader>lC`.

Create another Session without closing the current one:

```vim
:LouiselmSessionNew
```

Switch between attached Sessions:

```vim
:LouiselmSessionSwitch
```

Close the current Session when you are done:

```vim
:LouiselmSessionClose
```

Active Sessions use a native Close/Keep confirmation, even while a permission
picker is open. Press `c` to close; `k`, Enter, or Escape keeps the Session open.

Default shortcuts:

- `<leader>lsn` creates a Session.
- `<leader>lsw` switches Sessions.
- `<leader>lsx` closes a Session.

## 7. Continue learning

The human reference is available with:

```vim
:help louiselm
```

Useful next topics include skills, inline edits, session options, resuming a
previous Session, and exporting a transcript. LouiseLM also has headless APIs
for applications that do not use the chat buffer.
