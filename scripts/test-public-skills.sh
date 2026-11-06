#!/usr/bin/env bash
# Behavioral test for check-public-skills: each private marker is refused by
# name, and a clean tree passes.
set -euo pipefail

script_dir=$(CDPATH= cd -- "$(dirname -- "$0")" && pwd)
check="$script_dir/check-public-skills"
work_dir=$(mktemp -d)
trap 'rm -rf "$work_dir"' EXIT

clean() {
	rm -rf "$work_dir/skills"
	mkdir -p "$work_dir/skills/demo"
	printf -- '---\nname: demo\n---\nSee https://github.com/org/repo and git@<host>:<owner>/<slug>.git.\n' \
		>"$work_dir/skills/demo/SKILL.md"
}

clean
"$check" "$work_dir/skills" >/dev/null || { echo "FAIL: clean tree refused" >&2; exit 1; }

expect_refusal() {
	local rule=$1 text=$2 output
	clean
	printf '%s\n' "$text" >>"$work_dir/skills/demo/SKILL.md"
	if output=$("$check" "$work_dir/skills" 2>&1); then
		echo "FAIL: $rule not refused" >&2
		exit 1
	fi
	grep -q "$rule" <<<"$output" || { echo "FAIL: expected rule $rule, got: $output" >&2; exit 1; }
}

expect_refusal home_path 'Run it from /home/someone/code/project.'
expect_refusal home_path 'Or /Users/someone/project.'
expect_refusal email 'Contact person@private.example.net.'
expect_refusal ssh_remote 'Push to git@git.private.example:team/repo.git.'
expect_refusal session_id 'Refs claude/12345678-1234-1234-1234-123456789abc'
expect_refusal unlisted_host 'Docs at https://intranet.example.net/page.'
expect_refusal unlisted_host 'The usual host is code.private.example.fr, namespace team.'
expect_refusal operator_profile 'type: louiselm/operator-profile'
expect_refusal operator_profile '"type": "louiselm/operator-profile"'
expect_refusal operator_profile "type: 'louiselm/operator-profile'"

clean
printf '%s\n' '---' 'type: "louiselm\u002foperator-profile"' '---' 'Private defaults.' \
	>"$work_dir/skills/demo/renamed.md"
if output=$("$check" "$work_dir/skills" 2>&1); then
	echo "FAIL: encoded profile declaration not refused" >&2
	exit 1
fi
grep -q operator_profile <<<"$output" || { echo "FAIL: expected operator_profile, got: $output" >&2; exit 1; }
if grep -q 'Private defaults' <<<"$output"; then
	echo "FAIL: profile diagnostic exposed content" >&2
	exit 1
fi

clean
printf '%s\n' '---' 'type: [' '---' >"$work_dir/skills/demo/invalid.md"
if output=$("$check" "$work_dir/skills" 2>&1); then
	echo "FAIL: malformed declaration not refused" >&2
	exit 1
fi
grep -q invalid_frontmatter <<<"$output" || { echo "FAIL: expected invalid_frontmatter, got: $output" >&2; exit 1; }

clean
printf 'anything\n' >"$work_dir/skills/demo/PROFILE.md"
if output=$("$check" "$work_dir/skills" 2>&1); then
	echo "FAIL: PROFILE.md not refused" >&2
	exit 1
fi
grep -q operator_profile <<<"$output" || { echo "FAIL: expected operator_profile, got: $output" >&2; exit 1; }

clean
ln -s /etc/hostname "$work_dir/skills/demo/link"
if "$check" "$work_dir/skills" >/dev/null 2>&1; then
	echo "FAIL: symlink not refused" >&2
	exit 1
fi

echo "check-public-skills: all cases pass"
