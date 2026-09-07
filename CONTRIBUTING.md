# Contributing to LouiseLM

LouiseLM welcomes bug reports, workflow descriptions, configuration examples,
design criticism, reproducible experiments, and forks. External pull requests
are also welcome as temporary, reviewable proposals, but should not be opened
with an expectation that their commits will be merged directly.

The maintainer or an agent may study a PR, reproduce its evidence, and then
independently decide whether and how to implement the idea. A PR may therefore
be closed even when its diagnosis or design is adopted. Material ideas will be
credited.

This is unusual for an open-source project, so the constraint is explicit to
avoid wasted effort and hurt feelings. LouiseLM is still an unstable,
dogfooded expression of one person's workflow. Directly merging outside code
creates an asymmetric long-term review and maintenance obligation for the
person whose name and attention remain responsible for it. At this stage,
independent reimplementation preserves a coherent product direction and the
velocity needed to discover the right abstractions.

Copyright in LouiseLM is held solely by the maintainer. Because outside code is
not merged into the project, no contributor licence agreement is required and
none will be requested under this policy. The maintainer retains the right to
license future versions under different terms; versions already distributed
keep the rights granted by the licence under which they were received.

This policy is not a rejection of community. A useful LouiseLM community can
form around usage, issues, workflows, provider implementations, forks, and
clear evidence about what should change. If the project stabilizes and gains
the review capacity and governance needed to share maintenance responsibly,
the merge policy can change too.

## Helpful submissions

- Describe the workflow goal and the human attention it should preserve.
- Include minimal reproduction steps for bugs, with expected and actual
  behavior. Never include prompts, credentials, audio, or private idea data.
- For code proposals, explain the invariant, add the smallest failing test, and
  show the documented quality gates passing.
- Keep a proposal narrow. Compatibility layers and speculative extension
  points are especially unlikely to survive independent implementation while
  LouiseLM has no external users.

The repository's internal development contract and required checks live in
`AGENTS.md`; that operator-facing file is intentionally not part of the public
site.

For Linux `acp-proxy` lifecycle acceptance, run from this checkout:

```sh
nvim --headless --noplugin -u NONE -l tests/acp/orphan_acceptance.lua /path/to/acp-proxy
```

This creates disposable Neovim Sessions against the mock Agent, kills one editor,
and quits another normally. It checks that detached descendants disappear and
normal quit preserves the abandonment record. It does not stop your live editor.
