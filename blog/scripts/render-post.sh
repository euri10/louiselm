#!/usr/bin/env bash
#
# render-post.sh <post-dir>
#
# Private, standalone batch renderer for finished blog posts. NOT a Neovim
# command and NOT wired into `myst build`/`myst start` — this is an explicit,
# manual, pre-build step the author runs once a post is finished, so that
# publishing (`myst build`) never needs Neovim available (e.g. in CI).
#
# What it does
# ------------
# Given <post-dir>/blog.md, finds every MyST literalinclude code block tagged
# `:class: nvim-transcript`, e.g.:
#
#   ```{literalinclude} ./conversations/claude-xxx.md
#   :lines: 87-112
#   :class: nvim-transcript
#   ```
#
# (`:lines:` is 1-based and inclusive on both ends, per MyST's literalinclude
# docs — https://mystmd.org/guide/directives — line 87 through line 112,
# not a 0-based/exclusive range.)
#
# For the whole batch, it opens the referenced source files in ONE shared
# headless Neovim instance (see tohtml-driver.lua), using the author's real
# Neovim config (`~/.config/nvim/init.lua`, resolved fresh every run so it
# keeps working if the dotfiles symlink target moves) — not `nvim --clean` —
# so the rendered excerpt has the real Catppuccin colorscheme and Tree-sitter
# highlighting. Each excerpt is rendered via `:TOhtml`'s Lua API
# (`require("tohtml").tohtml(0, {range = {first, last}, ...})`).
#
# Only the `<pre>...</pre>` fragment and the generated `<style>` CSS are ever
# extracted from TOhtml's output — the full HTML document TOhtml produces is
# discarded. Every generated CSS selector is namespaced under `.nvim-transcript`
# (e.g. `.Comment` becomes `.nvim-transcript .Comment`, `body {...}` becomes
# `.nvim-transcript {...}`, and the bare `*` TOhtml emits becomes
# `.nvim-transcript *`) so this CSS can never leak into or collide with the
# surrounding blog theme. Selectors are deduplicated across every excerpt in
# the post into one shared stylesheet.
#
# Output naming (written directly into <post-dir>, nothing else in that
# directory is touched)
# ------------------------------------------------------------------------
#   <slug>.nvim-transcript.html   -- one fragment per excerpt, where <slug> is
#                                    the excerpt's source path (relative to
#                                    <post-dir>, `/` -> `-`, extension
#                                    dropped) plus `.L<first>-<last>`, e.g.
#                                    conversations-claude-xxx.L87-112.nvim-transcript.html
#                                    A numeric `-2`, `-3`, ... suffix is added
#                                    (in document order) if the same
#                                    (source, range) pair appears twice.
#                                    Each fragment's body is exactly:
#                                      <div class="nvim-transcript">
#                                      <pre>...</pre>
#                                      </div>
#   nvim-transcript.css           -- one shared, namespaced stylesheet for
#                                    every excerpt in the post.
#
# Every run first removes any *.nvim-transcript.html / nvim-transcript.css
# already in <post-dir>, then regenerates from the current blog.md, so output
# is a pure function of the current input (never accumulated state from a
# previous run) and running twice against unchanged input is byte-identical.
#
# The markdown parsing and CSS-rewriting live in render-post.mjs (Node) since
# that is far cleaner than hand-rolled sed/awk; this file just validates
# arguments, resolves the real Neovim config, and hands off.
set -euo pipefail

usage() {
	echo "usage: $(basename "$0") <post-dir>" >&2
}

if [ "$#" -ne 1 ]; then
	usage
	exit 1
fi

post_dir=$1
script_dir=$(CDPATH= cd -- "$(dirname -- "$0")" && pwd)

if [ ! -d "$post_dir" ]; then
	echo "error: post directory not found: $post_dir" >&2
	exit 1
fi

if [ ! -f "$post_dir/blog.md" ]; then
	echo "error: $post_dir/blog.md not found" >&2
	exit 1
fi

command -v nvim >/dev/null 2>&1 || {
	echo "error: nvim not found on PATH" >&2
	exit 1
}
command -v node >/dev/null 2>&1 || {
	echo "error: node not found on PATH" >&2
	exit 1
}

nvim_config_dir=${XDG_CONFIG_HOME:-"$HOME/.config"}/nvim
if [ ! -d "$nvim_config_dir" ]; then
	echo "error: nvim config directory not found: $nvim_config_dir" >&2
	exit 1
fi
# Resolve through the ~/.config/nvim -> ~/dotfiles/... symlink at run time
# (never hardcode the dotfiles target) so this keeps working if the user
# moves their dotfiles checkout.
nvim_config_dir=$(CDPATH= cd -- "$nvim_config_dir" && pwd)
nvim_init="$nvim_config_dir/init.lua"
if [ ! -f "$nvim_init" ]; then
	echo "error: $nvim_init not found" >&2
	exit 1
fi

exec node "$script_dir/render-post.mjs" "$post_dir" "$nvim_init" "$script_dir/tohtml-driver.lua"
