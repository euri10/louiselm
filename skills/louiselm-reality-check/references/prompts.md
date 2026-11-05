# Prompts and phrases

Public wording by Jeffrey Emanuel (@doodlestein): from his posts, or quoted
verbatim in his public run reports. Borrow the wording; do not add steps. His
`bv`/`beads-*`/`ntm` names map to `bvr`, the `louiselm-to-beads` skills, and the
project's own swarm tooling.

References below to a "plan document" are historical wording, not instructions
to create a file. Apply the rounds to the working plan in chat and the relevant
Beads. A separate report remains opt-in under the skill's destination rule.

## Quick check (origin, 2026-03-09)

> Where are we on this project? Do we have a working <core promise>? If not,
> what is blocking us? If we were to intelligently implement all open and
> in-progress beads, would we close that gap completely? Why or why not?

His reports turn it into the five fixed Phase 1 questions:

1. What specifically IS working right now?
2. What is NOT working or not yet implemented?
3. What is blocking us from getting there?
4. If we were to implement all open and in-progress beads, would we close the
   gap completely? Why or why not?
5. What goals from the vision are NOT covered by ANY existing bead?

## Full run (2026-04-06)

> First read ALL of the AGENTS.md file and README.md file super carefully and
> understand ALL of both! Then use your code investigation agent mode to fully
> understand the code and technical architecture and purpose of the project.
> THEN apply /reality-check-for-project here in an exhaustive way.

## Close the gaps (after the report)

> I need you to help me fix this. That is, making all the things that are
> unimplemented but which SHOULD have been implemented according to the beads
> and markdown plan. Figure out exactly what needs to be done to get us over
> the goal line with a finished, polished, reliable, performant project in
> line with the vision described earlier.

His August form adds: close the gaps with the most "bang for the buck",
"things like wiring in finished code that just isn't properly connected yet",
and "Keep the lessons of /just-say-no-to-process-porn-and-ceremony top of mind".

Either message, or any request to fix, bridge, or file the gaps, authorizes
Phase 2. Implementing the Beads is a separate request.

## Ambition rounds (2, at most 3)

Round 1, as quoted in his public reports:

> That's a decent start but it barely scratches the surface and is light years
> away from being OPTIMAL. Please try again and revise your existing plan
> document in-place to make it MUCH, MUCH, MUCH better in EVERY WAY.

Round 2:

> That's a lot better than before but STILL is a far cry from being OPTIMAL.
> Please try yet again and revise your existing plan document in-place to make
> it MUCH, MUCH, MUCH better in EVERY WAY. I believe in you, you can do this!!!
> Show me how brilliant you really are.

Round 3, when used, asks for depth: better mathematics, algorithms, or domain
technique. His later runs rejected round-3 additions with no measured or
user-visible benefit; do the same, and log the rejection.

Guard: a bolder approach is yours to propose. A change to what the project
promises belongs to the maintainer: present it, or file a `needs-design`
question for `louiselm-grill-me`.

## Create the Beads (frozen "Phase 3a" prompt)

> OK so please take ALL of that and elaborate on it and use it to create a
> comprehensive and granular set of beads for all this with tasks, subtasks,
> and dependency structure overlaid, with detailed comments so that the whole
> thing is totally self-contained and self-documenting (including relevant
> background, reasoning/justification, considerations, etc.-- anything we'd
> want our "future self" to know about the goals and intentions and thought
> process and how it serves the overarching goals of the project.). The beads
> should be so detailed that we never need to consult back to the original
> markdown plan document. Remember to ONLY use the `br` tool to create and
> modify the beads and add the dependencies.

## Refine the Beads (frozen "Phase 5" prompt; repeat until a round finds nothing)

> Check over each bead super carefully-- are you sure it makes sense? Is it
> optimal? Could we change anything to make the system work better for users?
> If so, revise the beads. It's a lot easier and faster to operate in "plan
> space" before we start implementing these things! DO NOT OVERSIMPLIFY THINGS!
> DO NOT LOSE ANY FEATURES OR FUNCTIONALITY! Also make sure that as part of the
> beads we include comprehensive unit tests and e2e test scripts with great,
> detailed logging so we can be sure that everything is working perfectly after
> implementation. Make sure to ONLY use the `br` cli tool for all changes, and
> you can and should also use the `bv` tool to help diagnose potential problems
> with the beads.

## Wire for parallel work (2026-08-21)

> Is there a better way to wire the beads dependencies [...] to implement the
> same work, but with greater opportunity for agent concurrency so as to enable
> faster overall development velocity using a larger swarm size?

Here: `bvr --robot-plan` for parallel tracks, `bvr --robot-insights` for
bottlenecks and cycles.

## Fresh-eyes review of the report (2026-08-27)

> I want you to meticulously and comprehensively study <the report>; do you see
> anything you disagree with? Anything that is obviously contradictory?
> Foolishly inconsistent? Misguided and unlikely to work in practice? [...]
> Important: don't disagree just to be disagreeable, it's OK to just agree if
> you think it's right!
