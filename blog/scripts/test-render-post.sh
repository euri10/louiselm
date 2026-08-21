#!/usr/bin/env bash
#
# test-render-post.sh
#
# Lightweight, dependency-free tests for render-post.sh. There is no
# existing test framework for shell/Node tooling in this repo (mini.test,
# per AGENTS.md, governs lua/, not this private blog/ tooling), so this is
# plain shell + node assertions against real output rather than a framework.
#
# Four things are tested:
#
#   1. A fragment's embedded <style>/<pre> content matches an independent
#      real TOhtml capture of the same excerpt, byte-for-byte -- this is the
#      pipeline's core correctness property now that no CSS rewriting
#      happens (delivery is through an isolated <iframe>, see render-post.sh's
#      header for why).
#   2. End-to-end idempotency across fragment files, blog.md, and myst.yml:
#      running render-post.sh twice against unchanged input produces
#      byte-identical output in all three, and removing an excerpt cleans up
#      its fragment, its blog.md iframe block, and its myst.yml
#      static_files entry.
#   3. A real `myst build --html --strict` actually wires the fragment into
#      the deployed page -- the exact gap louiselm-xrf identified the rest
#      of this suite structurally could not reach, now closed.
#
# Requires the real Neovim config (Catppuccin + Tree-sitter, resolved via
# ~/.config/nvim) and the `myst` CLI to be present and working, same as
# render-post.sh itself.
set -euo pipefail

script_dir=$(CDPATH= cd -- "$(dirname -- "$0")" && pwd)
render_post_sh="$script_dir/render-post.sh"
render_post_mjs="$script_dir/render-post.mjs"

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

nvim_config_dir=$(CDPATH= cd -- "${XDG_CONFIG_HOME:-$HOME/.config}/nvim" && pwd)
nvim_init="$nvim_config_dir/init.lua"

# work_dir is the fixture MyST project root for every part below: render-post.mjs
# now requires <post-dir>/../myst.yml to exist.
cat >"$work_dir/myst.yml" <<'EOF'
version: 1
project:
  title: fixture
  toc:
    - file: index.md
    - pattern: 'post-*/blog.md'
site:
  template: book-theme
  options:
    folders: true
    style: ./blog.css
EOF
touch "$work_dir/blog.css"
# A real, crawlable link to the post is required for part 3: the book theme's
# server only writes a page's static HTML to _build/html on first request, and
# its own startup crawl only follows links reachable from index.md.
cat >"$work_dir/index.md" <<'EOF'
---
title: fixture
---

# fixture

- [Fixture Post](post-fixture/blog.md)
EOF

fixture_post="$work_dir/post-fixture"
mkdir -p "$fixture_post/conversations"

# The excerpted ranges below must stay rich enough to expose CSS ordering
# non-determinism (louiselm-qcq): TOhtml emits one CSS rule per highlight
# group it encountered, and with only a handful of groups the varying
# iteration order does not actually change the emitted order often enough to
# fail. Prose emphasis, inline code, a link, a blockquote, a list and two
# different fenced languages together produce enough groups that ten runs
# reliably disagree without normalisation. Line numbers are load-bearing --
# excerpt A is 9-20 and excerpt B is 22-26, counting the `# Session
# transcript` line as 1; the ruler comments are inside the heredoc's content
# only as a reminder, so keep them out of it.
cat >"$fixture_post/conversations/fixture-session.md" <<'EOF'
# Session transcript

## User

Please list the files in this repo.

## Assistant

Here is **bold text**, *italic text*, `inline code`, and a
[link](http://example.com) all in one paragraph.

> A blockquote containing **emphasis** and `code`.

- first list item
- second list item

```bash
ls -la
git status
```

```lua
local function greet(name)
  print("hello " .. name)
end
```

That's it for this excerpt.
EOF

cat >"$fixture_post/blog.md" <<'EOF'
---
title: Fixture Post
---

# Fixture Post

Excerpt A:

% nvim-transcript: ./conversations/fixture-session.md :lines: 9-20

Excerpt B:

% nvim-transcript: ./conversations/fixture-session.md :lines: 22-26
EOF

echo "== part 1: fragment content matches an independent real TOhtml capture =="

capture_lua="$work_dir/capture.lua"
raw_capture="$work_dir/capture-A.raw.html"
cat >"$capture_lua" <<EOF
vim.cmd('packadd nvim.tohtml')
vim.cmd('edit $fixture_post/conversations/fixture-session.md')
local html = require('tohtml').tohtml(0, { range = { 9, 20 }, number_lines = false, title = false })
vim.fn.writefile(html, '$raw_capture')
EOF

if nvim --headless -u "$nvim_init" -c "luafile $capture_lua" -c quit >"$work_dir/capture.log" 2>&1 && [ -s "$raw_capture" ]; then
	ok "independent 'nvim --headless -u \$nvim_init' TOhtml capture produced output"
else
	fail "independent 'nvim --headless -u \$nvim_init' TOhtml capture produced output"
	cat "$work_dir/capture.log" >&2 || true
fi

if "$render_post_sh" "$fixture_post" >"$work_dir/run1.log" 2>&1; then
	ok "render-post.sh run 1 exits 0"
else
	fail "render-post.sh run 1 exits 0"
	cat "$work_dir/run1.log" >&2 || true
fi

fragment_a="$fixture_post/post-fixture--conversations-fixture-session.L9-20.nvim-transcript.html"

expected_fragment_js="$work_dir/expected-fragment.mjs"
cat >"$expected_fragment_js" <<EOF
import { readFileSync, writeFileSync } from 'node:fs';
import { buildFragmentDoc } from '$render_post_mjs';

const raw = readFileSync('$raw_capture', 'utf8').split('\n');
const styleOpen = raw.indexOf('<style>');
const styleClose = raw.indexOf('</style>');
const preOpen = raw.indexOf('<pre>');
const preClose = raw.indexOf('</pre>');
if (styleOpen === -1 || styleClose === -1 || preOpen === -1 || preClose === -1) {
  console.error('independent capture is missing <style> or <pre> tags');
  process.exit(1);
}
const styleBlock = raw.slice(styleOpen, styleClose + 1);
const preBlock = raw.slice(preOpen, preClose + 1);
writeFileSync('$work_dir/expected-fragment.html', buildFragmentDoc(styleBlock, preBlock));
EOF

if node "$expected_fragment_js" >"$work_dir/expected-fragment.log" 2>&1; then
	ok "built expected fragment content from the independent capture"
else
	fail "built expected fragment content from the independent capture"
	cat "$work_dir/expected-fragment.log" >&2 || true
fi

if [ -f "$fragment_a" ] && diff -q "$work_dir/expected-fragment.html" "$fragment_a" >"$work_dir/fragment-diff.log" 2>&1; then
	ok "generated fragment matches the independent TOhtml capture byte-for-byte"
else
	fail "generated fragment matches the independent TOhtml capture byte-for-byte"
	cat "$work_dir/fragment-diff.log" >&2 || true
fi

if grep -qx '<!DOCTYPE html>' "$fragment_a" 2>/dev/null &&
	grep -qx '<html>' "$fragment_a" &&
	grep -qx '</html>' "$fragment_a"; then
	ok "fragment is a standalone HTML document (doctype + html tags)"
else
	fail "fragment is a standalone HTML document (doctype + html tags)"
fi

echo
echo "== part 2: end-to-end idempotency across fragments, blog.md, and myst.yml =="

collect_outputs() {
	# $1 = destination dir
	mkdir -p "$1"
	find "$fixture_post" -maxdepth 1 -type f -name '*.nvim-transcript.html' -exec cp {} "$1" \;
	cp "$fixture_post/blog.md" "$1/blog.md"
	cp "$work_dir/myst.yml" "$1/myst.yml"
}

run1_dir="$work_dir/run1"
collect_outputs "$run1_dir"

if "$render_post_sh" "$fixture_post" >"$work_dir/run2.log" 2>&1; then
	ok "render-post.sh run 2 (unchanged input) exits 0"
else
	fail "render-post.sh run 2 (unchanged input) exits 0"
	cat "$work_dir/run2.log" >&2 || true
fi

run2_dir="$work_dir/run2"
collect_outputs "$run2_dir"

expected_files=(
	"post-fixture--conversations-fixture-session.L9-20.nvim-transcript.html"
	"post-fixture--conversations-fixture-session.L22-26.nvim-transcript.html"
)
missing=0
for f in "${expected_files[@]}"; do
	if [ ! -f "$run1_dir/$f" ]; then
		missing=1
		echo "  missing expected output file: $f" >&2
	fi
done
if [ "$missing" -eq 0 ]; then
	ok "expected fragment filenames were produced (post-name-prefixed naming scheme)"
else
	fail "expected fragment filenames were produced (post-name-prefixed naming scheme)"
fi

if diff -rq "$run1_dir" "$run2_dir" >"$work_dir/diff.log" 2>&1; then
	ok "running render-post.sh twice against unchanged input is byte-identical (fragments, blog.md, myst.yml)"
else
	fail "running render-post.sh twice against unchanged input is byte-identical (fragments, blog.md, myst.yml)"
	cat "$work_dir/diff.log" >&2 || true
fi

# Two runs is not enough evidence for the "pure function of the current
# input" claim in render-post.sh's header. TOhtml's CSS rule order varies
# between Neovim processes (louiselm-qcq), and each render-post.sh
# invocation is a fresh process, so a two-run check passes whenever the two
# happen to agree. Ten runs makes agreement-by-luck vanishingly unlikely.
determinism_runs=10
determinism_failed=0
for i in $(seq 2 "$determinism_runs"); do
	if ! "$render_post_sh" "$fixture_post" >"$work_dir/run-det-$i.log" 2>&1; then
		determinism_failed=1
		echo "  render-post.sh run $i exited non-zero" >&2
		cat "$work_dir/run-det-$i.log" >&2 || true
		break
	fi
	det_dir="$work_dir/run-det-$i"
	collect_outputs "$det_dir"
	if ! diff -rq "$run1_dir" "$det_dir" >"$work_dir/diff-det-$i.log" 2>&1; then
		determinism_failed=1
		echo "  run $i differs from run 1:" >&2
		cat "$work_dir/diff-det-$i.log" >&2 || true
		break
	fi
done
if [ "$determinism_failed" -eq 0 ]; then
	ok "$determinism_runs consecutive runs against unchanged input are all byte-identical"
else
	fail "$determinism_runs consecutive runs against unchanged input are all byte-identical"
fi

if grep -qF '```{iframe} /post-fixture--conversations-fixture-session.L9-20.nvim-transcript.html' "$fixture_post/blog.md" &&
	grep -qF '```{iframe} /post-fixture--conversations-fixture-session.L22-26.nvim-transcript.html' "$fixture_post/blog.md" &&
	grep -qF '% nvim-transcript: ./conversations/fixture-session.md :lines: 9-20' "$fixture_post/blog.md"; then
	ok "blog.md has an iframe block per excerpt, and marker comments are untouched"
else
	fail "blog.md has an iframe block per excerpt, and marker comments are untouched"
	cat "$fixture_post/blog.md" >&2 || true
fi

if grep -qF -- "- 'post-fixture/post-fixture--conversations-fixture-session.L9-20.nvim-transcript.html'" "$work_dir/myst.yml" &&
	grep -qF -- "- 'post-fixture/post-fixture--conversations-fixture-session.L22-26.nvim-transcript.html'" "$work_dir/myst.yml"; then
	ok "myst.yml static_files lists both generated fragments"
else
	fail "myst.yml static_files lists both generated fragments"
	cat "$work_dir/myst.yml" >&2 || true
fi

echo
echo "== part 2b: re-running after removing a block cleans up its fragment, blog.md block, and static_files entry =="

cat >"$fixture_post/blog.md" <<'EOF'
---
title: Fixture Post
---

# Fixture Post

Excerpt A only now:

% nvim-transcript: ./conversations/fixture-session.md :lines: 9-20
EOF

if "$render_post_sh" "$fixture_post" >"$work_dir/run3.log" 2>&1; then
	ok "render-post.sh run 3 (block removed) exits 0"
else
	fail "render-post.sh run 3 (block removed) exits 0"
	cat "$work_dir/run3.log" >&2 || true
fi

fragment_b="$fixture_post/post-fixture--conversations-fixture-session.L22-26.nvim-transcript.html"
if [ -f "$fragment_a" ] && [ ! -f "$fragment_b" ]; then
	ok "removing a block deletes its stale fragment and keeps the remaining one"
else
	fail "removing a block deletes its stale fragment and keeps the remaining one"
fi

if diff -q "$run1_dir/post-fixture--conversations-fixture-session.L9-20.nvim-transcript.html" "$fragment_a" >/dev/null 2>&1; then
	ok "excerpt A's fragment is unchanged by removing an unrelated block"
else
	fail "excerpt A's fragment is unchanged by removing an unrelated block"
fi

if ! grep -qF 'L22-26' "$fixture_post/blog.md"; then
	ok "removed block's iframe reference is gone from blog.md"
else
	fail "removed block's iframe reference is gone from blog.md"
	cat "$fixture_post/blog.md" >&2 || true
fi

if ! grep -qF 'L22-26' "$work_dir/myst.yml"; then
	ok "removed block's entry is gone from myst.yml's static_files"
else
	fail "removed block's entry is gone from myst.yml's static_files"
	cat "$work_dir/myst.yml" >&2 || true
fi

echo
echo "== part 3: a real 'myst build --html --strict' wires the fragment into the deployed page =="

myst_bin=$(command -v myst || true)
if [ -z "$myst_bin" ]; then
	fail "myst CLI is on PATH (required for the end-to-end build check)"
else
	ok "myst CLI is on PATH (required for the end-to-end build check)"

	build_log="$work_dir/myst-build.log"
	target_page="$work_dir/_build/html/post-fixture/blog/index.html"

	# `myst build --html` fetches every site route through a throwaway internal
	# server, writes each page as its fetch resolves, copies static_files only
	# after every route is in, then exits on its own -- no server is left
	# running, so this just needs to run to completion (bounded by `timeout` as
	# a safety net, not as the normal exit path).
	if (cd "$work_dir" && timeout 90 "$myst_bin" build --html --strict --ci >"$build_log" 2>&1); then
		ok "myst build exits 0"
	else
		fail "myst build exits 0"
		cat "$build_log" >&2 || true
	fi

	if [ -f "$target_page" ]; then
		ok "myst build produced the fixture post's static page"
	else
		fail "myst build produced the fixture post's static page"
		cat "$build_log" >&2 || true
	fi

	expected_src="/post-fixture--conversations-fixture-session.L9-20.nvim-transcript.html"
	if [ -f "$target_page" ] && grep -qF "src=\"$expected_src\"" "$target_page"; then
		ok "deployed page embeds the fragment through an iframe with the expected absolute src"
	else
		fail "deployed page embeds the fragment through an iframe with the expected absolute src"
	fi

	deployed_fragment="$work_dir/_build/html/post-fixture--conversations-fixture-session.L9-20.nvim-transcript.html"
	if [ -f "$deployed_fragment" ] && diff -q "$fragment_a" "$deployed_fragment" >/dev/null 2>&1; then
		ok "the deployed fragment file is byte-identical to the one render-post.sh generated"
	else
		fail "the deployed fragment file is byte-identical to the one render-post.sh generated"
	fi
fi

echo
echo "$pass_count passed, $fail_count failed"
[ "$fail_count" -eq 0 ]
