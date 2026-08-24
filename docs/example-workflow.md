# An Example Workflow

This describes one person's working loop — the maintainer's. It is an example,
not a contract. Nothing here binds anyone, no part of it is required to
contribute, and other loops are expected. `AGENTS.md` holds the rules; this
document holds a habit.

It is written down for two reasons. First, because a loop that exists only as
muscle memory cannot be examined, argued with, or improved. Second, because
louiselm intends to make workflows configurable rather than conventional, and
when that lands this document is the thing it should be able to express. Treat
it as a description that wants to become a specification.

## The loop

Five stages. Each one hands something concrete to the next, which is what makes
it a loop rather than five habits practised near each other.

### 1. Grill

An idea arrives — a hunch, a complaint about friction, a design question with
more than one defensible answer — and instead of being implemented, it gets
interrogated.

The agent asks one question at a time and waits. Multiple questions at once
produce agreeable mush rather than decisions. Each question carries the agent's
own recommended answer, so the human is reacting to a concrete proposal rather
than generating one from nothing; disagreement is faster and more informative
than invention. Anything discoverable by looking — a file's contents, a tool's
flags, what a command actually outputs — is looked up rather than asked. The
decisions, and only the decisions, belong to the human.

The stage ends when both sides can state the same plan. Not before.

**Hands off:** an agreed plan, with the reasoning that produced it and the
alternatives that were rejected.

### 2. Route to beads

The agreed plan becomes work items, at whatever altitude fits: an epic for a
multi-phase initiative, a feature for one coherent capability, tasks for
implementation slices, a bug for a defect with evidence.

The important property is that the *reasoning* survives, not just the
instruction. A task that says what to do but not why it beat the alternative
will be re-litigated by whoever picks it up, or worse, quietly done differently.
Rejected options belong in the issue as explicitly rejected.

**Hands off:** issues in the graph, with dependencies wired so that order is a
property of the data rather than something to remember.

### 3. Execute

Tasks get picked up and done, in sessions that may be interactive, automated, or
some mix. `br ready` and `bvr --robot-*` decide what is actionable; the graph,
not a memory of the planning conversation, is the source of truth.

This is the stage most likely to be handled by an agent working alone, which is
exactly why stages 1 and 2 put so much weight on writing the reasoning down.

**Hands off:** commits, and closed issues that reference them.

### 4. QA review

The maintainer tests by hand, at the keyboard, against what was just committed.
Not automated tests — those live in the ordinary development cycle and are
covered by `AGENTS.md`. This is a human driving the actual application looking
for things that are wrong in ways a test suite was never going to notice.

The plan for a round lives in a working file whose path the tester is told, and
is edited as findings come in — so the round is a document that evolves rather
than a checklist that is consumed. A round spans hours and several rebuilds;
steps buried in chat history are unusable by someone who has scrolled away from
them. A defect found here is filed with the evidence that proves it, before it
is fixed, because the artifact that proves it — a log line, a payload, a hung
process — disappears the moment it is fixed.

The tester's reading of a symptom is a lead, not a verdict. The logs decide, and
pulling them is the writer's job, not the tester's.

Three things the format has had to learn:

- **Mark which sub-points share state.** A multi-part case run against one open
  session, with no reset between parts, can send the tester down a false trail
  when an earlier failure taints a later one. That is invisible from the
  outside unless the plan says so, so each case states which parts are
  order-dependent and which are independent. A failure report then arrives with
  "this may just be fallout from 2.2" already attached, instead of reading as a
  fresh mystery.
- **Embed prompts as fenced code blocks, never blockquotes.** A `>` blockquote
  copied out of a rendered markdown pane brings the `>` with it, forcing an
  edit before it can be pasted into the application's input line.
- **Beware cumulative logs when reviewing from inside the session under test.**
  When the round runs in louiselm's own chat, talking to the assistant also
  writing the review, the ACP JSON-RPC log covers every turn of the whole
  conversation rather than the turn being examined. Diffing an export straight
  against the full log reports later turns' tool calls as "missing". Restrict
  the log's tool-call ids to the prefix ending at the last id that also appears
  in the export before diffing.

**Hands off:** bugs in the graph, and confidence that the committed thing works.

### 5. Retrospect

After the exact live reproduction passes, the work asks why the defect was
expensive to understand. This is not a second code review and it does not turn
every focused bug into a full grill. It separates what belongs only to this
issue from what should improve the loop the next time a similar bug appears.

For a focused defect, a one-sentence acceptance lock is often enough design:
state what the maintainer must see and distinguish it from nearby signals that
could look like success. Then inspect the live view first, trace one affected
value through every boundary to its presentation, and build the regression from
the real structural ordering rather than convenient invented state. If the
exact reproduction remains available, automated gates support the fix but do
not replace live confirmation.

The retrospective has three destinations:

- Issue-specific evidence, false starts, and engineering invariants stay in the
  Beads issue comment thread.
- A repeatable rule that should bind future work is promoted to `AGENTS.md`.
- A habit worth inspecting but not enforcing stays in this document. An
  unresolved design question becomes a `needs-design` question and gets its own
  grill.

Tracking- or diagnostic-only commits are named as such and are not presented as
new builds to retest. That keeps movement in the record from being mistaken for
movement in behavior.

**Hands off:** a closed evidence trail for the issue, any reusable rule in its
binding home, and any unresolved process question routed to a grill.

## Skill chaining

The loop holds together because each stage is designed to trigger the next
rather than merely precede it. A grill session ends by routing into beads. A
defect found during review routes into a bug. A confirmed fix routes into a
retrospective, which either records a local lesson, promotes a standing rule, or
opens a design question for the next grill. The handoff is part of each stage's
definition, not something remembered separately.

This is the property most worth preserving if any of it ever becomes
configuration. A workflow is not a list of stages; it is a set of stages plus
the rules about what each one hands to the next, and the second half is where
all the value is.

## Process findings

A QA round sometimes surfaces something that is not a code defect at all, but a
question about the process itself — how rounds should be archived, whether a
working file is the right medium, whether a stage is doing what it claims.

The retrospective routes that finding by scope. A local observation stays with
the issue that exposed it; a settled standing convention moves to `AGENTS.md`;
an unresolved question worth designing becomes a Beads `question` labeled
`needs-design` and gets a dedicated grill. Process findings therefore survive
by an explicit handoff rather than the maintainer remembering to revisit them.

## What this is not

It is not a requirement, a recommendation, or a claim that this loop is good.
It is one loop that one person runs, written down so that it can be inspected
and eventually configured. If you are looking for the rules, they are in
`AGENTS.md`.
