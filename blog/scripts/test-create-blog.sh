#!/usr/bin/env bash
#
# test-create-blog.sh
#
# Lightweight, dependency-free tests for create-blog.sh, matching the pattern
# render-post.sh's own test-render-post.sh already established: plain shell
# assertions against real output (there is no existing test framework for
# blog/scripts/, which sits outside AGENTS.md's lua/-scoped mini.test
# contract).
#
# The session-id mode tests drive create-blog.sh's real
# `louiselm.session.transcript_export.run` path end to end -- no real agent
# process, but a real headless Neovim loading a real session -- by pointing
# it at `louiselm.dev.mock_agent` through the
# LOUISELM_CREATE_BLOG_AGENT_DEFINITION_MOCK override documented in
# create-blog.sh's header, the same way
# tests/session/transcript_export_spec.lua drives it inside the plugin's own
# suite (nvim.v.progpath + tests/mock/init.lua +
# `lua require('louiselm.dev.mock_agent').run()`).
#
# Requires the real Neovim config (with louiselm installed, per README's
# `vim.pack.add`) to be present and working, same as render-post.sh's tests.
set -euo pipefail

script_dir=$(CDPATH= cd -- "$(dirname -- "$0")" && pwd)
repo_root=$(CDPATH= cd -- "$script_dir/../.." && pwd)
create_blog_sh="$script_dir/create-blog.sh"

pass_count=0
fail_count=0

ok() {
	pass_count=$((pass_count + 1))
	echo "ok - $1"
}

fail() {
	fail_count=$((fail_count + 1))
	echo "not ok - $1"
}

work_dir=$(mktemp -d)
cleanup() { rm -rf "$work_dir"; }
trap cleanup EXIT

blog_root="$work_dir/blog"
mkdir -p "$blog_root"
export LOUISELM_CREATE_BLOG_ROOT="$blog_root"

nvim_bin=$(command -v nvim || true)
mock_definition=""
if [ -n "$nvim_bin" ]; then
	mock_definition=$(
		cat <<EOF
{ command = "$nvim_bin", args = { "--headless", "--noplugin", "-u", "$repo_root/tests/mock/init.lua", "-c", "lua require('louiselm.dev.mock_agent').run()" }, env = { LOUISELM_MOCK_REPLAY_USER = "what did we decide last time" } }
EOF
	)
fi

echo "== part 1: bare mode =="

if "$create_blog_sh" bare-post >"$work_dir/bare.log" 2>&1; then
	ok "create-blog.sh <name> with no session args exits 0"
else
	fail "create-blog.sh <name> with no session args exits 0"
	cat "$work_dir/bare.log" >&2 || true
fi

post_dir="$blog_root/post-bare-post"
if [ -f "$post_dir/blog.md" ]; then
	ok "blog.md was created"
else
	fail "blog.md was created"
fi

if [ -d "$post_dir/conversations" ] && [ -z "$(ls -A "$post_dir/conversations" 2>/dev/null)" ]; then
	ok "conversations/ was created and is empty"
else
	fail "conversations/ was created and is empty"
fi

if grep -qx -- "---" "$post_dir/blog.md" &&
	grep -q "^title: bare-post$" "$post_dir/blog.md"; then
	ok "blog.md has the documented MyST frontmatter stub (matches blog/post-example/blog.md's shape)"
else
	fail "blog.md has the documented MyST frontmatter stub (matches blog/post-example/blog.md's shape)"
	cat "$post_dir/blog.md" >&2 || true
fi

echo
echo "== part 2: clobber protection =="

if "$create_blog_sh" bare-post >"$work_dir/clobber.log" 2>&1; then
	fail "running create-blog.sh twice with the same name is rejected"
else
	ok "running create-blog.sh twice with the same name is rejected"
fi

if grep -q "already exists" "$work_dir/clobber.log"; then
	ok "clobber rejection reports a clear 'already exists' error"
else
	fail "clobber rejection reports a clear 'already exists' error"
	cat "$work_dir/clobber.log" >&2 || true
fi

echo
echo "== part 3: unknown agent name is rejected without guessing a binary =="

if "$create_blog_sh" unknown-agent-post fooagent/some-session >"$work_dir/unknown.log" 2>&1; then
	fail "an agent name with no known launch definition is rejected"
else
	ok "an agent name with no known launch definition is rejected"
fi

if grep -q "no known launch definition for agent 'fooagent'" "$work_dir/unknown.log" &&
	grep -q "LOUISELM_CREATE_BLOG_AGENT_DEFINITION_FOOAGENT" "$work_dir/unknown.log"; then
	ok "the error names the missing agent and the override env var to set"
else
	fail "the error names the missing agent and the override env var to set"
	cat "$work_dir/unknown.log" >&2 || true
fi

echo
echo "== part 4: malformed agent/session-id argument is rejected =="

if "$create_blog_sh" malformed-post not-a-slash-arg >"$work_dir/malformed.log" 2>&1; then
	fail "an argument without an 'agent/session-id' slash is rejected"
else
	ok "an argument without an 'agent/session-id' slash is rejected"
fi

echo
echo "== part 5: session-id mode against the real transcript_export.run path (via mock_agent) =="

if [ -z "$mock_definition" ]; then
	fail "nvim is on PATH (required for session-id mode tests)"
else
	ok "nvim is on PATH (required for session-id mode tests)"

	export LOUISELM_CREATE_BLOG_AGENT_DEFINITION_MOCK="$mock_definition"

	session_post_dir="$blog_root/post-session-post"
	if "$create_blog_sh" session-post "mock/prior-acp-session-1" >"$work_dir/session.log" 2>&1; then
		ok "create-blog.sh <name> <agent>/<session-id> exits 0"
	else
		fail "create-blog.sh <name> <agent>/<session-id> exits 0"
		cat "$work_dir/session.log" >&2 || true
	fi

	exported_file="$session_post_dir/conversations/mock-prior-acp-session-1.md"
	if [ -s "$exported_file" ]; then
		ok "the session was exported to conversations/<agent>-<session-id>.md and is non-empty"
	else
		fail "the session was exported to conversations/<agent>-<session-id>.md and is non-empty"
	fi

	if grep -q -- "- agent: mock" "$exported_file" &&
		grep -q -- "- acp session: prior-acp-session-1" "$exported_file" &&
		grep -q -- "## User" "$exported_file" &&
		grep -q -- "what did we decide last time" "$exported_file"; then
		ok "exported transcript matches transcript_export.run's real output for the mock agent's replayed session"
	else
		fail "exported transcript matches transcript_export.run's real output for the mock agent's replayed session"
		cat "$exported_file" >&2 || true
	fi

	blog_md="$session_post_dir/blog.md"
	expected_block=$(
		cat <<'EOF'
```{literalinclude} ./conversations/mock-prior-acp-session-1.md
:lines: 1
:class: nvim-transcript
```
EOF
	)
	# grep -F with an embedded-newline pattern matches each line as its own
	# alternative (like -e per line), not a contiguous block -- so pull the
	# three lines starting at the opening fence and compare them for an exact,
	# contiguous, in-order match instead.
	actual_block=$(grep -A3 -F -- '```{literalinclude} ./conversations/mock-prior-acp-session-1.md' "$blog_md" || true)
	if [ -f "$blog_md" ] && [ "$actual_block" = "$expected_block" ]; then
		ok "blog.md got the documented literalinclude stub, with the ':lines: 1' placeholder untouched"
	else
		fail "blog.md got the documented literalinclude stub, with the ':lines: 1' placeholder untouched"
		cat "$blog_md" >&2 || true
	fi

	echo
	echo "== part 6: an agent that crashes mid-export fails the whole script with the agent's own error =="

	export LOUISELM_CREATE_BLOG_AGENT_DEFINITION_MOCK=$(
		cat <<EOF
{ command = "$nvim_bin", args = { "--headless", "--noplugin", "-u", "$repo_root/tests/mock/init.lua", "-c", "lua require('louiselm.dev.mock_agent').run()" }, env = { LOUISELM_MOCK_CRASH_ON = "session/load" } }
EOF
	)
	if "$create_blog_sh" crash-post "mock/session-x" >"$work_dir/crash.log" 2>&1; then
		fail "a crashing export fails create-blog.sh"
	else
		ok "a crashing export fails create-blog.sh"
	fi
	if grep -q "louiselm: agent process exited with code 23" "$work_dir/crash.log"; then
		ok "the agent's own 'louiselm: '-prefixed stderr line is surfaced"
	else
		fail "the agent's own 'louiselm: '-prefixed stderr line is surfaced"
		cat "$work_dir/crash.log" >&2 || true
	fi

	echo
	echo "== part 7: two arguments whose sanitized filenames collide are rejected, not silently overwritten =="

	export LOUISELM_CREATE_BLOG_AGENT_DEFINITION_MOCK="$mock_definition"
	if "$create_blog_sh" collide-post "mock/s:1" "mock/s_1" >"$work_dir/collide.log" 2>&1; then
		fail "colliding sanitized filenames are rejected"
	else
		ok "colliding sanitized filenames are rejected"
	fi
	if grep -q "both sanitize to conversations/mock-s_1.md" "$work_dir/collide.log"; then
		ok "the collision error names both conflicting arguments and the shared target"
	else
		fail "the collision error names both conflicting arguments and the shared target"
		cat "$work_dir/collide.log" >&2 || true
	fi
fi

echo
echo "$pass_count passed, $fail_count failed"
[ "$fail_count" -eq 0 ]
