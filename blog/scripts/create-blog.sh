#!/usr/bin/env bash
#
# create-blog.sh <name> [agent/session-id ...]
#
# Scaffolds a new blog post directory and, when given session references,
# auto-exports each one and stubs a literalinclude block for it. Private
# tooling for the blog-publishing pipeline; NOT a Neovim command (see
# render-post.sh's header for why this class of tooling lives outside
# lua/louiselm/).
#
# Bare mode
# ---------
#   create-blog.sh <name>
#
# creates:
#   blog/post-<name>/blog.md            -- minimal MyST frontmatter stub,
#                                           matching blog/post-example/blog.md's
#                                           shape (`---\ntitle: ...\n---\n`)
#   blog/post-<name>/conversations/     -- empty directory
#
# Never clobbers an existing blog/post-<name>/: the whole directory must not
# already exist, or this exits 1 without touching anything.
#
# Session-id mode
# ----------------
#   create-blog.sh <name> claude/71c82ed5-9db8-44a3-9e64-e610a51b6cea codex/abc123
#
# does the same scaffold, plus for each `<agent>/<session-id>` argument:
#
#   1. Exports that session (via `louiselm.session.transcript_export`, driven
#      through one headless Neovim invocation per session -- see that
#      module's doc comment for the exact stdout/stderr/exit-code contract)
#      to:
#        blog/post-<name>/conversations/<agent>-<sanitized-session-id>.md
#      where "sanitized" replaces every character outside [A-Za-z0-9_.-] with
#      "_". This keeps the filename collision-safe against session ids that
#      contain characters the filesystem or MyST would choke on, while still
#      passing the exact, unsanitized session id to the export call itself.
#      If two arguments would sanitize to the same output filename, this
#      script fails loudly rather than silently overwriting one export with
#      another.
#   2. Appends a literalinclude block to blog.md referencing it:
#
#        ```{literalinclude} ./conversations/claude-71c82ed5-9db8-44a3-9e64-e610a51b6cea.md
#        :lines: 1
#        :class: nvim-transcript
#        ```
#
#      `:lines: 1` is a deliberate placeholder -- the author picks real
#      ranges by hand afterward. Do not try to be smarter about it here.
#
# Exports run one at a time, each in its own headless Neovim process (matching
# transcript_export.lua's documented one-shot `-c "lua ... run(...)" -c "qa!"`
# invocation exactly). If an export fails, this script stops immediately
# (previous successful exports and the bare scaffold are left in place --
# there is no automatic rollback) and reports the agent's own
# "louiselm: "-prefixed stderr line.
#
# Agent launch definitions
# -------------------------
# A standalone shell script has no access to the user's `louiselm.setup()`
# config, so it cannot resolve `<agent>` names to launch commands the way the
# plugin normally does. Instead this script hardcodes the small set of agents
# this repo actually documents a launch command for today (see README.md):
#
#   claude -> { command = "claude-agent-acp", args = {} }
#   codex  -> { command = "codex-acp",         args = {} }
#
# For any other agent name, set an override environment variable before
# running this script:
#
#   LOUISELM_CREATE_BLOG_AGENT_DEFINITION_<NAME>='{ command = "...", args = { ... } }'
#
# where <NAME> is the agent name uppercased with every character outside
# [A-Za-z0-9_] replaced by "_" (e.g. agent "my-agent" -> env var suffix
# "MY_AGENT"). The value is spliced verbatim as a Lua table literal -- get the
# Lua syntax right, it is not shell-escaped for you. This is also how this
# script's own tests exercise the export path against
# `louiselm.dev.mock_agent` without a real agent process, the same way the
# plugin's own test suite does (see tests/session/transcript_export_spec.lua).
set -euo pipefail

usage() {
	echo "usage: $(basename "$0") <name> [agent/session-id ...]" >&2
}

if [ "$#" -lt 1 ]; then
	usage
	exit 1
fi

name=$1
shift

if [ -z "$name" ]; then
	echo "error: <name> must not be empty" >&2
	exit 1
fi
case "$name" in
*/*)
	echo "error: <name> must not contain '/'" >&2
	exit 1
	;;
esac

script_dir=$(CDPATH= cd -- "$(dirname -- "$0")" && pwd)
repo_root=$(CDPATH= cd -- "$script_dir/../.." && pwd)
# Overridable so this script's own tests can scaffold into a throwaway
# directory instead of the real blog/ tree.
blog_root=${LOUISELM_CREATE_BLOG_ROOT:-"$repo_root/blog"}

post_dir="$blog_root/post-$name"

if [ -e "$post_dir" ]; then
	echo "error: $post_dir already exists" >&2
	exit 1
fi

conversations_dir="$post_dir/conversations"
mkdir -p "$conversations_dir"

blog_md="$post_dir/blog.md"
cat >"$blog_md" <<EOF
---
title: $name
---
EOF

if [ "$#" -eq 0 ]; then
	exit 0
fi

command -v nvim >/dev/null 2>&1 || {
	echo "error: nvim not found on PATH" >&2
	exit 1
}

nvim_config_dir=${XDG_CONFIG_HOME:-"$HOME/.config"}/nvim
if [ ! -d "$nvim_config_dir" ]; then
	echo "error: nvim config directory not found: $nvim_config_dir" >&2
	exit 1
fi
# Resolve through the ~/.config/nvim -> dotfiles symlink at run time (never
# hardcode the dotfiles target), same as render-post.sh.
nvim_config_dir=$(CDPATH= cd -- "$nvim_config_dir" && pwd)
nvim_init="$nvim_config_dir/init.lua"
if [ ! -f "$nvim_init" ]; then
	echo "error: $nvim_init not found" >&2
	exit 1
fi

work_dir=$(mktemp -d)
cleanup() { rm -rf "$work_dir"; }
trap cleanup EXIT

# Embed an arbitrary shell string as a double-quoted Lua string literal.
lua_quote() {
	local s=$1
	s=${s//\\/\\\\}
	s=${s//\"/\\\"}
	s=${s//$'\n'/\\n}
	printf '"%s"' "$s"
}

# Known launch definitions for the agents this repo documents today (see
# README.md's `claude-agent-acp` / `codex-acp` usage). Prints a Lua table
# literal for the given agent name on stdout, or fails with no output.
known_agent_definition() {
	case "$1" in
	claude)
		printf '{ command = %s, args = {} }' "$(lua_quote "claude-agent-acp")"
		;;
	codex)
		printf '{ command = %s, args = {} }' "$(lua_quote "codex-acp")"
		;;
	*)
		return 1
		;;
	esac
}

# Env var suffix for an agent's override, e.g. "claude" -> "CLAUDE",
# "my-agent" -> "MY_AGENT".
agent_override_var() {
	local suffix
	suffix=$(printf '%s' "$1" | tr '[:lower:]' '[:upper:]' | sed 's/[^A-Za-z0-9_]/_/g')
	printf 'LOUISELM_CREATE_BLOG_AGENT_DEFINITION_%s' "$suffix"
}

# Prints a Lua table literal for the given agent name on stdout: the known
# mapping first, falling back to LOUISELM_CREATE_BLOG_AGENT_DEFINITION_<NAME>.
# Fails with no output when neither is available.
resolve_agent_definition() {
	local agent=$1 definition var override
	if definition=$(known_agent_definition "$agent"); then
		printf '%s' "$definition"
		return 0
	fi
	var=$(agent_override_var "$agent")
	override=${!var:-}
	if [ -n "$override" ]; then
		printf '%s' "$override"
		return 0
	fi
	return 1
}

# Replace every character outside [A-Za-z0-9_.-] with "_" for safe use in a
# filename.
sanitize_for_filename() {
	printf '%s' "$1" | sed 's/[^A-Za-z0-9_.-]/_/g'
}

declare -A used_targets

for arg in "$@"; do
	case "$arg" in
	*/*) ;;
	*)
		echo "error: expected <agent>/<session-id>, got '$arg'" >&2
		exit 1
		;;
	esac
	agent=${arg%%/*}
	session_id=${arg#*/}
	if [ -z "$agent" ] || [ -z "$session_id" ]; then
		echo "error: expected <agent>/<session-id>, got '$arg'" >&2
		exit 1
	fi

	if ! definition_lua=$(resolve_agent_definition "$agent"); then
		echo "error: no known launch definition for agent '$agent'; supported agents: claude, codex" \
			"(set $(agent_override_var "$agent") to override, see this script's header comment)" >&2
		exit 1
	fi

	filename="${agent}-$(sanitize_for_filename "$session_id").md"
	if [ -n "${used_targets[$filename]:-}" ]; then
		echo "error: '$arg' and '${used_targets[$filename]}' both sanitize to conversations/$filename" >&2
		exit 1
	fi
	used_targets[$filename]=$arg

	dest_path="$conversations_dir/$filename"

	export_lua="$work_dir/export-$(sanitize_for_filename "$arg").lua"
	{
		# Prepend this live checkout to rtp before requiring anything under
		# louiselm.*: the user's real Neovim config installs louiselm through
		# its plugin manager's own clone (see README's "installs this checkout
		# with vim.pack.add"), which tracks committed history and can lag
		# behind this working tree. Prepending guarantees the module actually
		# being developed here is what gets required, not a stale copy.
		printf 'vim.opt.rtp:prepend(%s)\n' "$(lua_quote "$repo_root")"
		printf 'local definitions = { [%s] = %s }\n' "$(lua_quote "$agent")" "$definition_lua"
		printf "require('louiselm.session.transcript_export').run(definitions, %s, %s, %s)\n" \
			"$(lua_quote "$agent")" "$(lua_quote "$session_id")" "$(lua_quote "$dest_path")"
	} >"$export_lua"

	export_log="$work_dir/export-$(sanitize_for_filename "$arg").log"
	# Run with cwd pinned to the repo root: definitions using this repo's own
	# tests/mock/init.lua (as this script's own tests do) rely on the spawned
	# agent process inheriting a cwd it can prepend to its runtimepath.
	if ! (cd "$repo_root" && nvim --headless -u "$nvim_init" -c "luafile $export_lua" -c "qa!") \
		>"$export_log" 2>&1; then
		echo "error: exporting '$arg' failed:" >&2
		cat "$export_log" >&2
		exit 1
	fi

	cat >>"$blog_md" <<EOF

\`\`\`{literalinclude} ./conversations/$filename
:lines: 1
:class: nvim-transcript
\`\`\`
EOF

	echo "exported $arg -> conversations/$filename"
done

echo "created $post_dir"
