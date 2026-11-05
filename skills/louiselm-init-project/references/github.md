# Requested GitHub hosting

Read this only when the session selects GitHub hosting or remote setup.

Use `github.com` unless another GitHub host is confirmed. The owner may be a
personal account or organization; confirm `OWNER/REPO` and visibility from the
session. Neither the signed-in account nor GitLab's `oss-public` namespace is
an implicit choice of GitHub owner. Hosting remains optional.

Use existing authenticated `gh` and Git access; inspect installed help as needed
and keep credentials out of output. Target the chosen host explicitly with
`gh api --hostname <host> --method GET repos/<owner>/<slug>` and use the full
repository URL with `gh repo view`. Verify the exact owner, repository and
available access before creating anything. An authentication/network failure
or ambiguous not-found response is not proof that a private repository is absent.

Reuse a matching repository, checking its visibility, permissions and branches
before pushing to a nonempty remote. Do not change existing visibility, replace
a different remote, force-push or merge unrelated histories implicitly. If a
destination or permission is missing, finish the independent local result and
ask only for that missing decision; do not create under a guessed account.

For an authorized new repository, use non-interactive `gh repo create` with
explicit `OWNER/REPO` and the confirmed `--private` or `--public` flag
(`--internal` only where supported and requested). Set command-scoped
`GH_HOST=<host>` for the selected host. Create an empty remote without
`--add-readme`, `--license`, `--gitignore` or `--template`. Keep creation separate
from pushing; omit `--push`, `--clone` and `--source` so reviewed local content
and remote configuration remain under explicit control.

Verify the returned destination and clone URL, then add `origin` only when absent
and matching the requested repository. Preserve a matching existing remote.
Complete an already authorized commit and push without asking again, flushing
Beads and staging only named task-owned paths and relevant issue export changes.

After pushing, compare the remote branch to the local commit and use
`gh repo view <repository-url> --json nameWithOwner,url,visibility,defaultBranchRef`
to verify the owner, visibility and default branch. Report any unmet hosting
requirement and keep initialization open until resolved. If closure follows the
push verification, include its new export in the authorized Git handoff when
covered, or report that local change as pending rather than already published.

Command references: [create](https://cli.github.com/manual/gh_repo_create),
[view](https://cli.github.com/manual/gh_repo_view),
[API](https://cli.github.com/manual/gh_api).
