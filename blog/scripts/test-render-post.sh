#!/usr/bin/env bash
#
# test-render-post.sh
#
# Lightweight, dependency-free tests for render-post.sh. There is no
# existing test framework for shell/Node tooling in this repo (mini.test,
# per AGENTS.md, governs lua/, not this private blog/ tooling), so this is
# plain shell + node assertions against real output rather than a framework.
#
# Two things are tested, matching louiselm-nsj's testing notes -- the
# CSS-namespacing step is called out there as the highest-risk part of the
# whole feature:
#
#   1. CSS namespacing, against a REAL :TOhtml capture (this actually runs
#      `nvim --headless -u ~/.config/nvim/init.lua` against a small fixture
#      file to get real TOhtml output -- not hand-written fake TOhtml CSS)
#      -- every generated selector must come out prefixed under
#      `.nvim-transcript`, and nothing must escape that namespace.
#   2. End-to-end idempotency: running render-post.sh twice against an
#      unchanged fixture post produces byte-identical output, and re-running
#      after removing a block cleans up that block's stale fragment.
#
# Requires the real Neovim config (Catppuccin + Tree-sitter, resolved via
# ~/.config/nvim) to be present and working, same as render-post.sh itself.
set -euo pipefail

script_dir=$(CDPATH= cd -- "$(dirname -- "$0")" && pwd)
render_post_sh="$script_dir/render-post.sh"

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

echo "== part 1: CSS namespacing against a real TOhtml capture =="

css_fixture_dir="$work_dir/css-fixture"
mkdir -p "$css_fixture_dir"
cat >"$css_fixture_dir/sample.lua" <<'EOF'
-- a fixture file with enough syntax variety to exercise real TOhtml classes
local function greet(name)
  -- says hello
  print("Hello, " .. name .. "!")
end

greet("world")
EOF

raw_html="$work_dir/css-fixture-raw.html"
cat >"$work_dir/capture.lua" <<EOF
vim.cmd('packadd nvim.tohtml')
vim.cmd('edit $css_fixture_dir/sample.lua')
local html = require('tohtml').tohtml(0, { range = { 1, 7 }, number_lines = false, title = false })
vim.fn.writefile(html, '$raw_html')
EOF

if nvim --headless -u "$nvim_init" -c "luafile $work_dir/capture.lua" -c quit >"$work_dir/nvim-capture.log" 2>&1 && [ -s "$raw_html" ]; then
	ok "real 'nvim --headless -u \$nvim_init' TOhtml capture produced output"
else
	fail "real 'nvim --headless -u \$nvim_init' TOhtml capture produced output"
	cat "$work_dir/nvim-capture.log" >&2 || true
fi

css_test_js="$work_dir/css-namespace-test.mjs"
cat >"$css_test_js" <<EOF
import { readFileSync } from 'node:fs';
import { collectCss } from '$script_dir/render-post.mjs';

const raw = readFileSync('$raw_html', 'utf8').split('\n');
const openIdx = raw.indexOf('<style>');
const closeIdx = raw.indexOf('</style>');
if (openIdx === -1 || closeIdx === -1) {
  console.error('no <style> block found in captured TOhtml output');
  process.exit(1);
}
const styleBlock = raw.slice(openIdx, closeIdx + 1);

const originalSelectors = styleBlock
  .slice(1, -1)
  .filter((l) => l.trim() !== '')
  .map((l) => l.match(/^(.*?)\s*\{/)[1].trim());

if (originalSelectors.length === 0) {
  console.error('captured TOhtml CSS had no rules to test against');
  process.exit(1);
}
// A real capture over syntax-highlighted code must contain at least one bare
// highlight-group class selector (not just \`*\`/\`body\`), else this test
// would pass vacuously without exercising the interesting case.
if (!originalSelectors.some((s) => s.startsWith('.'))) {
  console.error('captured TOhtml CSS had no .ClassName selectors, fixture is not representative');
  process.exit(1);
}

const cssRules = new Map();
collectCss(styleBlock, cssRules);

for (const original of originalSelectors) {
  const expected =
    original === '*' ? '.nvim-transcript *' : original === 'body' ? '.nvim-transcript' : \`.nvim-transcript \${original}\`;
  if (!cssRules.has(expected)) {
    console.error(\`selector \${JSON.stringify(original)} did not come out as \${JSON.stringify(expected)}\`);
    process.exit(1);
  }
}

for (const selector of cssRules.keys()) {
  if (selector !== '.nvim-transcript' && !selector.startsWith('.nvim-transcript ')) {
    console.error(\`selector \${JSON.stringify(selector)} escaped the .nvim-transcript namespace\`);
    process.exit(1);
  }
}

console.log(\`namespaced \${cssRules.size} selector(s) correctly, from \${originalSelectors.length} original rule(s): \${[...cssRules.keys()].join(', ')}\`);
EOF

if node "$css_test_js"; then
	ok "every real TOhtml selector is correctly namespaced under .nvim-transcript, and none escape it"
else
	fail "every real TOhtml selector is correctly namespaced under .nvim-transcript, and none escape it"
fi

echo
echo "== part 2: end-to-end idempotency =="

post_dir="$work_dir/post-idempotency"
mkdir -p "$post_dir/conversations"

cat >"$post_dir/conversations/fixture-session.md" <<'EOF'
# Session transcript

## User

Please list the files in this repo.

## Assistant

```bash
ls -la
git status
```

Here is the output above. Let me also show a Lua snippet:

```lua
local function greet(name)
  print("hello " .. name)
end
```

That's it for this excerpt.
EOF

cat >"$post_dir/blog.md" <<'EOF'
---
title: Fixture Post
---

# Fixture Post

Excerpt A:

```{literalinclude} ./conversations/fixture-session.md
:lines: 9-12
:class: nvim-transcript
```

Excerpt B:

```{literalinclude} ./conversations/fixture-session.md
:lines: 16-20
:class: nvim-transcript
```
EOF

collect_outputs() {
	# $1 = destination dir
	mkdir -p "$1"
	find "$post_dir" -maxdepth 1 -type f \( -name '*.nvim-transcript.html' -o -name 'nvim-transcript.css' \) -exec cp {} "$1" \;
}

run1_dir="$work_dir/run1"
run2_dir="$work_dir/run2"

if "$render_post_sh" "$post_dir" >"$work_dir/run1.log" 2>&1; then
	collect_outputs "$run1_dir"
	ok "render-post.sh run 1 exits 0"
else
	fail "render-post.sh run 1 exits 0"
	cat "$work_dir/run1.log" >&2 || true
fi

if "$render_post_sh" "$post_dir" >"$work_dir/run2.log" 2>&1; then
	collect_outputs "$run2_dir"
	ok "render-post.sh run 2 (unchanged input) exits 0"
else
	fail "render-post.sh run 2 (unchanged input) exits 0"
	cat "$work_dir/run2.log" >&2 || true
fi

expected_files=(
	"conversations-fixture-session.L9-12.nvim-transcript.html"
	"conversations-fixture-session.L16-20.nvim-transcript.html"
	"nvim-transcript.css"
)
missing=0
for f in "${expected_files[@]}"; do
	if [ ! -f "$run1_dir/$f" ]; then
		missing=1
		echo "  missing expected output file: $f" >&2
	fi
done
if [ "$missing" -eq 0 ]; then
	ok "expected fragment/css filenames were produced (documented naming scheme)"
else
	fail "expected fragment/css filenames were produced (documented naming scheme)"
fi

if diff -rq "$run1_dir" "$run2_dir" >"$work_dir/diff.log" 2>&1; then
	ok "running render-post.sh twice against unchanged input is byte-identical"
else
	fail "running render-post.sh twice against unchanged input is byte-identical"
	cat "$work_dir/diff.log" >&2 || true
fi

frag="$run1_dir/conversations-fixture-session.L9-12.nvim-transcript.html"
if [ -f "$frag" ] &&
	[ "$(head -n1 "$frag")" = '<div class="nvim-transcript">' ] &&
	[ "$(tail -n1 "$frag")" = '</div>' ] &&
	grep -qx '<pre>' "$frag" &&
	grep -qx '</pre>' "$frag"; then
	ok "fragment file has the documented <div class=nvim-transcript><pre>...</pre></div> shape"
else
	fail "fragment file has the documented <div class=nvim-transcript><pre>...</pre></div> shape"
fi

css_file="$run1_dir/nvim-transcript.css"
if [ -f "$css_file" ] &&
	grep -qE '^\.nvim-transcript( \*)? ?\{?' "$css_file" &&
	! grep -qE '^(body|\*|html)\s*\{' "$css_file"; then
	ok "shared CSS file has no bare body/*/html selector outside the namespace"
else
	fail "shared CSS file has no bare body/*/html selector outside the namespace"
fi

echo
echo "== part 2b: re-running after removing a block cleans up its stale fragment =="

# Drop excerpt B from blog.md, keep A's range unchanged.
cat >"$post_dir/blog.md" <<'EOF'
---
title: Fixture Post
---

# Fixture Post

Excerpt A only now:

```{literalinclude} ./conversations/fixture-session.md
:lines: 9-12
:class: nvim-transcript
```
EOF

if "$render_post_sh" "$post_dir" >"$work_dir/run3.log" 2>&1; then
	ok "render-post.sh run 3 (block removed) exits 0"
else
	fail "render-post.sh run 3 (block removed) exits 0"
	cat "$work_dir/run3.log" >&2 || true
fi

if [ -f "$post_dir/conversations-fixture-session.L9-12.nvim-transcript.html" ] &&
	[ ! -f "$post_dir/conversations-fixture-session.L16-20.nvim-transcript.html" ]; then
	ok "removing a block deletes its stale fragment and keeps the remaining one"
else
	fail "removing a block deletes its stale fragment and keeps the remaining one"
fi

if diff -q "$run1_dir/conversations-fixture-session.L9-12.nvim-transcript.html" \
	"$post_dir/conversations-fixture-session.L9-12.nvim-transcript.html" >/dev/null 2>&1; then
	ok "excerpt A's fragment is unchanged by editing an unrelated block"
else
	fail "excerpt A's fragment is unchanged by editing an unrelated block"
fi

echo
echo "$pass_count passed, $fail_count failed"
[ "$fail_count" -eq 0 ]
