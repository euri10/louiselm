# Requested GitLab hosting

Read this only when the session requests hosting or remote setup.

Use the host, namespace and SSH remote settled in the interview. If none was
given, use the Operator profile's default host when it names one, else ask. A
default does not authorize publication.

Use existing authenticated `glab`/SSH access. Inspect installed help and query
the exact group and URL-encoded project path before creating anything. Always
pass `--hostname` for self-hosted `glab api`. Preserve failures with
`set -o pipefail` when piping JSON through `jq`; do not assume `glab api`
supports `--jq`. Keep tokens out of output.

Resolve the namespace ID, project existence, visibility and authorization from
the session and read-only checks. A group name does not authorize changing an
existing project's visibility. Authentication/network failure is not proof of
absence. Reuse a matching project and inspect branches before pushing to a
nonempty repository. Never overwrite a different remote, force-push or merge
unrelated histories implicitly.

Create an authorized new project without a server-generated README so the local
initial commit supplies its history. Verify the returned namespace, visibility
and clone URL. Add `origin` only when absent and matching the destination.
Complete an authorized initial commit and push without asking again. If a
material publication choice remains unresolved, finish the reviewable local
result first, then ask only for that missing decision.

Before committing, flush Beads and stage only named task-owned paths and relevant
issue export changes. After pushing, verify the remote branch equals the local
commit and check GitLab's namespace, visibility and default branch.

If initialization closes after that verification, its final export is a new
local change. Include it in the authorized Git handoff when covered, or report
it as pending; never describe an uncommitted closure as already published.
