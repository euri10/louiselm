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

# Captured native record from the first Publisher ceremony, 2026-11-08
# (louiselm-vhqzk). This gate scans privacy, not trust or the record's expiry.
endorsement_fixture=$(<"$script_dir/../tests/fixtures/endorsement_hardware_record.json")
endorsement_name=sha256-130ee13ba25efed10c675db0c1d4bb821e51e81814b4b7d9672a044a462ac4ea.json

write_endorsement() {
	clean
	mkdir -p "$work_dir/skills/endorsements"
	printf '%s' "$1" >"$work_dir/skills/endorsements/$endorsement_name"
}

write_endorsement "$endorsement_fixture"
printf '%s  %s\n' 130ee13ba25efed10c675db0c1d4bb821e51e81814b4b7d9672a044a462ac4ea \
	"$work_dir/skills/endorsements/$endorsement_name" | sha256sum --check --status || {
	echo "FAIL: captured Endorsement bytes changed" >&2; exit 1;
}
"$check" "$work_dir/skills" >/dev/null || { echo "FAIL: native hardware Endorsement refused" >&2; exit 1; }

expect_endorsement_refusal() {
	local rule=$1 text=$2 record output
	# Neither a known free-text field nor an unknown field may be skipped.
	# Both share a single compact JSON line with the valid public key.
	for record in \
		"${endorsement_fixture/'"assumptions":[]'/"\"assumptions\":[\"$text\"]"}" \
		"${endorsement_fixture%?},\"extra\":\"$text\"}"; do
		write_endorsement "$record"
		if output=$("$check" "$work_dir/skills" 2>&1); then
			echo "FAIL: Endorsement $rule not refused" >&2
			exit 1
		fi
		grep -q "$rule" <<<"$output" || { echo "FAIL: expected $rule, got: $output" >&2; exit 1; }
	done
}

expect_endorsement_refusal email 'person@private.example.net'
expect_endorsement_refusal email 'person@openssh.com'
expect_endorsement_refusal email 'owner+sk-ssh-ed25519@openssh.com'
expect_endorsement_refusal email 'sk-ssh-ed25519@openssh.com.private.example.net'
expect_endorsement_refusal email 'sk-ssh-ed25519@openssh.com, person@github.com'
expect_endorsement_refusal unlisted_host 'private.example.net'
expect_endorsement_refusal unlisted_host 'openssh.com'
expect_endorsement_refusal unlisted_host 'https://openssh.com'
expect_endorsement_refusal home_path '/home/someone/private/project/'
expect_endorsement_refusal ssh_remote 'git@private.example.net:team/repo.git'
expect_endorsement_refusal session_id '12345678-1234-1234-1234-123456789abc'

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
