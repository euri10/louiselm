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
# Given <post-dir>/blog.md, finds every `% nvim-transcript: <source> :lines:
# N-M` marker comment, e.g.:
#
#   % nvim-transcript: ./conversations/claude-xxx.md :lines: 87-112
#
# This is a MyST `%` line comment (https://mystmd.org/guide, "comments"), so
# it is never rendered by `myst build` — it exists purely as input to this
# script, which rewrites the line(s) after it in place on every run. Because
# the comment itself is never touched, the author can keep editing `:lines:`
# and re-run this indefinitely; nothing here is a one-shot destructive
# rewrite. (`:lines:` is 1-based and inclusive on both ends — line 87 through
# line 112, not a 0-based/exclusive range.)
#
# For the whole batch, it opens the referenced source files in ONE shared
# headless Neovim instance (see tohtml-driver.lua), using the author's real
# Neovim config (`~/.config/nvim/init.lua`, resolved fresh every run so it
# keeps working if the dotfiles symlink target moves) — not `nvim --clean` —
# so the rendered excerpt has the real Catppuccin colorscheme and Tree-sitter
# highlighting. Each excerpt is rendered via `:TOhtml`'s Lua API
# (`require("tohtml").tohtml(0, {range = {first, last}, ...})`).
#
# The `<pre>...</pre>` fragment and the generated `<style>` CSS are extracted
# from TOhtml's output and wrapped into a standalone HTML document per
# excerpt. Neither is rewritten (no selector namespacing, no property
# translation); the only transformation is that the `<style>` block's rules
# and declarations are sorted into a canonical order, because TOhtml emits
# them in an unspecified `pairs()` order that varies between processes and
# would otherwise make every re-render dirty the committed fragment
# (louiselm-qcq). Delivery is through MyST's `{iframe}`
# directive rather than same-page embedding: mystmd's HTML build target does
# not pass raw HTML through from markdown (neither a literal `<style>`/`<pre>`
# written in blog.md nor the `{raw}` directive survive to the built page —
# both were tried and confirmed broken, see louiselm-xrf), so an iframe's own
# isolated document is the only way to ship real TOhtml markup byte-for-byte.
# That isolation also means no CSS-selector namespacing or shared stylesheet
# is needed: each fragment's <style> block only ever applies inside its own
# iframe.
#
# Output naming (written directly into <post-dir>, nothing else in that
# directory is touched) and wiring
# ------------------------------------------------------------------------
#   <post-dir-name>--<slug>.nvim-transcript.html
#                                  -- one standalone document per excerpt,
#                                     where <slug> is the excerpt's source
#                                     path (relative to <post-dir>, `/` -> `-`,
#                                     extension dropped) plus `.L<first>-<last>`,
#                                     e.g. post-foo--conversations-claude-xxx.L87-112.nvim-transcript.html.
#                                     The post directory's own name is folded
#                                     into the filename because MyST's
#                                     `static_files` copy step (see below)
#                                     flattens every declared file into one
#                                     shared directory by basename alone — two
#                                     posts' same-slug excerpts would
#                                     otherwise collide. A numeric `-2`, `-3`,
#                                     ... suffix is added (in document order)
#                                     if the same (source, range) pair appears
#                                     twice within one post.
#
# Every run first removes any *.nvim-transcript.html already in <post-dir>,
# then regenerates from the current blog.md, so fragment output is a pure
# function of the current input (never accumulated state from a previous
# run) and repeated runs against unchanged input are byte-identical. That
# last property depends on the CSS canonicalisation described above and is
# checked over ten consecutive runs by test-render-post.sh part 2 -- two
# runs used to pass by luck.
#
# Two more files are rewritten in place, both idempotently:
#   - blog.md: any previously generated `{iframe}` block (matched by its
#     `/<post-dir-name>--...` src, regardless of whether its marker comment
#     still exists) is stripped, then a fresh one is inserted directly after
#     each current marker comment, referencing the fragment by its absolute
#     site-root path (`/<basename>` — relative paths break under the book
#     theme's client-side routing). MyST's `{iframe}` directive has no
#     height option, and the theme sizes the box to a fixed width-relative
#     aspect ratio it does not expose for us to configure — the fragment
#     document itself scrolls instead, so a tall excerpt stays reachable
#     rather than silently clipped.
#   - <project-root>/myst.yml: this post's entries in `project.static_files`
#     (the list of files `myst build` copies verbatim into the deployed
#     site) are replaced with whatever fragments this run produced; every
#     other post's entries are left untouched.
#
# The markdown parsing, fragment generation, and blog.md/myst.yml rewriting
# live in render-post.mjs (Node) since that is far cleaner than hand-rolled
# sed/awk; this file just validates arguments, resolves the real Neovim
# config, and hands off.
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
