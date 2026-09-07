#!/usr/bin/env bash
set -euo pipefail

script_dir=$(CDPATH= cd -- "$(dirname -- "$0")" && pwd)
snapshot="$script_dir/agent-liveness-snapshot"

work_dir=$(mktemp -d)
cleanup() { rm -rf "$work_dir"; }
trap cleanup EXIT

failed=0
now=$(date +%s)

fail() {
	echo "FAIL: $*" >&2
	failed=1
}

# Build an isolated fake home so no assertion depends on the developer's real
# session history.
make_home() {
	local home=$1
	mkdir -p "$home/.local/state/acp-llm-adapter/proxy/sessions"
	mkdir -p "$home/.codex/sessions/2026/09/06"
	mkdir -p "$home/.claude/projects/-home-lotso-code-louiselm"
}

# age_minutes is how long ago the heartbeat last moved.
proxy_heartbeat() {
	local home=$1 id=$2 age_minutes=$3
	local dir="$home/.local/state/acp-llm-adapter/proxy/sessions/$id"
	mkdir -p "$dir"
	: >"$dir/log.jsonl"
	touch -d "@$((now - age_minutes * 60))" "$dir/log.jsonl"
}

codex_heartbeat() {
	local home=$1 id=$2 age_minutes=$3
	local file="$home/.codex/sessions/2026/09/06/rollout-2026-09-06T06-24-27-$id.jsonl"
	: >"$file"
	touch -d "@$((now - age_minutes * 60))" "$file"
}

claude_heartbeat() {
	local home=$1 id=$2 age_minutes=$3
	local file="$home/.claude/projects/-home-lotso-code-louiselm/$id.jsonl"
	: >"$file"
	touch -d "@$((now - age_minutes * 60))" "$file"
}

# Run the generator against a fake home, capturing stdout and stderr apart.
run_snapshot() {
	local home=$1
	shift
	local stdin_file=$1
	shift
	HOME="$home" "$snapshot" "$@" <"$stdin_file" >"$work_dir/out.jsonl" 2>"$work_dir/err.txt"
}

row_for() {
	jq -c --arg holder "$1" 'select(.holder == $holder)' "$work_dir/out.jsonl"
}

# --- a live session is reported with a reservation that has not expired -------

home="$work_dir/live"
make_home "$home"
proxy_heartbeat "$home" "820e1ab6-83e2-45fd-92f3-79263d4d7141" 3
printf '%s\n' "claude/820e1ab6-83e2-45fd-92f3-79263d4d7141" >"$work_dir/in.txt"
run_snapshot "$home" "$work_dir/in.txt"

row=$(row_for "claude/820e1ab6-83e2-45fd-92f3-79263d4d7141")
if [ -z "$row" ]; then
	fail "a 3-minute-old proxy heartbeat produced no reservation row"
else
	expires=$(printf '%s' "$row" | jq -r '.expires_ts')
	expires_epoch=$(date -u -d "$expires" +%s)
	if [ "$expires_epoch" -le "$now" ]; then
		fail "live session expires_ts $expires is not in the future"
	fi
	# br only treats a reservation as active when released_ts is absent.
	if [ "$(printf '%s' "$row" | jq -r 'has("released_ts")')" != "false" ]; then
		fail "live session row must not carry released_ts"
	fi
	if [ "$(printf '%s' "$row" | jq -r '.exclusive')" != "true" ]; then
		fail "reservation row must be exclusive"
	fi
	if [ "$(printf '%s' "$row" | jq -r 'has("path_pattern")')" != "true" ]; then
		fail "reservation row is missing the required path_pattern field"
	fi
fi

# --- a dead session is reported, but already expired -------------------------

home="$work_dir/dead"
make_home "$home"
proxy_heartbeat "$home" "01a056ef-03ae-7203-885d-c73b4ba00d1b" 9000
printf '%s\n' "codex/01a056ef-03ae-7203-885d-c73b4ba00d1b" >"$work_dir/in.txt"
run_snapshot "$home" "$work_dir/in.txt"

row=$(row_for "codex/01a056ef-03ae-7203-885d-c73b4ba00d1b")
if [ -z "$row" ]; then
	fail "a dead session must still be reported, so br records why it is reclaimable"
else
	expires=$(printf '%s' "$row" | jq -r '.expires_ts')
	expires_epoch=$(date -u -d "$expires" +%s)
	if [ "$expires_epoch" -ge "$now" ]; then
		fail "dead session expires_ts $expires must be in the past"
	fi
fi

# --- holder is the exact assignee string br will match against ---------------

if [ "$(printf '%s' "$row" | jq -r '.holder')" != "codex/01a056ef-03ae-7203-885d-c73b4ba00d1b" ]; then
	fail "holder must be the verbatim beads assignee, agent prefix included"
fi

# --- standalone adapter transcripts are found too ----------------------------

home="$work_dir/codex"
make_home "$home"
codex_heartbeat "$home" "01a074f5-e592-72d1-9ccf-69e162d8cdfe" 5
printf '%s\n' "codex/01a074f5-e592-72d1-9ccf-69e162d8cdfe" >"$work_dir/in.txt"
run_snapshot "$home" "$work_dir/in.txt"
if [ -z "$(row_for "codex/01a074f5-e592-72d1-9ccf-69e162d8cdfe")" ]; then
	fail "a standalone codex rollout transcript was not recognised as a heartbeat"
fi

home="$work_dir/claude"
make_home "$home"
claude_heartbeat "$home" "4e301c59-b296-4ef8-af9d-c13cd7d3e840" 5
printf '%s\n' "claude/4e301c59-b296-4ef8-af9d-c13cd7d3e840" >"$work_dir/in.txt"
run_snapshot "$home" "$work_dir/in.txt"
if [ -z "$(row_for "claude/4e301c59-b296-4ef8-af9d-c13cd7d3e840")" ]; then
	fail "a standalone Claude Code transcript was not recognised as a heartbeat"
fi

# --- the freshest source wins when a session has several ---------------------
# A session run through LouiseLM writes both a proxy log and an adapter
# transcript. Taking the older one would report a live agent as dead.

home="$work_dir/multi"
make_home "$home"
codex_heartbeat "$home" "01a07753-51de-7733-bfeb-8a3f26fa907e" 600
proxy_heartbeat "$home" "01a07753-51de-7733-bfeb-8a3f26fa907e" 2
printf '%s\n' "codex/01a07753-51de-7733-bfeb-8a3f26fa907e" >"$work_dir/in.txt"
run_snapshot "$home" "$work_dir/in.txt"

row=$(row_for "codex/01a07753-51de-7733-bfeb-8a3f26fa907e")
expires_epoch=$(date -u -d "$(printf '%s' "$row" | jq -r '.expires_ts')" +%s)
if [ "$expires_epoch" -le "$now" ]; then
	fail "a session with one stale and one fresh heartbeat must count as live"
fi

# --- an unknown assignee is announced, never silently dropped ----------------

home="$work_dir/unknown"
make_home "$home"
printf '%s\n' "opencode/ses_fad76691affe5is0XoeZAMvtXH" >"$work_dir/in.txt"
run_snapshot "$home" "$work_dir/in.txt"

if [ -s "$work_dir/out.jsonl" ]; then
	fail "an assignee with no heartbeat file must not get a reservation row"
fi
if ! grep -q "ses_fad76691affe5is0XoeZAMvtXH" "$work_dir/err.txt"; then
	fail "an assignee with no heartbeat file must be named on stderr"
fi

# --- the liveness window is configurable -------------------------------------

home="$work_dir/window"
make_home "$home"
proxy_heartbeat "$home" "820e1ab6-83e2-45fd-92f3-79263d4d7141" 30
printf '%s\n' "claude/820e1ab6-83e2-45fd-92f3-79263d4d7141" >"$work_dir/in.txt"

LOUISELM_LIVENESS_WINDOW_MINUTES=45 run_snapshot "$home" "$work_dir/in.txt"
expires_epoch=$(date -u -d "$(row_for "claude/820e1ab6-83e2-45fd-92f3-79263d4d7141" | jq -r '.expires_ts')" +%s)
if [ "$expires_epoch" -le "$now" ]; then
	fail "30 minutes idle inside a 45 minute window must still be live"
fi

LOUISELM_LIVENESS_WINDOW_MINUTES=10 run_snapshot "$home" "$work_dir/in.txt"
expires_epoch=$(date -u -d "$(row_for "claude/820e1ab6-83e2-45fd-92f3-79263d4d7141" | jq -r '.expires_ts')" +%s)
if [ "$expires_epoch" -ge "$now" ]; then
	fail "30 minutes idle inside a 10 minute window must count as dead"
fi

# --- input hygiene -----------------------------------------------------------

home="$work_dir/hygiene"
make_home "$home"
proxy_heartbeat "$home" "820e1ab6-83e2-45fd-92f3-79263d4d7141" 1
printf '%s\n\n   \n%s\n' "claude/820e1ab6-83e2-45fd-92f3-79263d4d7141" "claude/820e1ab6-83e2-45fd-92f3-79263d4d7141" >"$work_dir/in.txt"
run_snapshot "$home" "$work_dir/in.txt"

if [ "$(wc -l <"$work_dir/out.jsonl")" -ne 1 ]; then
	fail "blank lines and repeated assignees must collapse to one row per holder"
fi

# A bare session id with no agent prefix is a legacy assignee shape that still
# has to resolve, because those rows exist in the record.
home="$work_dir/bare"
make_home "$home"
proxy_heartbeat "$home" "ef5f2ba3-8f5c-4322-9c60-c49dcd1e48a2" 1
printf '%s\n' "ef5f2ba3-8f5c-4322-9c60-c49dcd1e48a2" >"$work_dir/in.txt"
run_snapshot "$home" "$work_dir/in.txt"
if [ -z "$(row_for "ef5f2ba3-8f5c-4322-9c60-c49dcd1e48a2")" ]; then
	fail "a bare session id assignee was not resolved"
fi

# --- every line is a standalone JSON object br can deserialise ---------------

home="$work_dir/shape"
make_home "$home"
proxy_heartbeat "$home" "820e1ab6-83e2-45fd-92f3-79263d4d7141" 1
proxy_heartbeat "$home" "01a056ef-03ae-7203-885d-c73b4ba00d1b" 9000
printf '%s\n%s\n' "claude/820e1ab6-83e2-45fd-92f3-79263d4d7141" "codex/01a056ef-03ae-7203-885d-c73b4ba00d1b" >"$work_dir/in.txt"
run_snapshot "$home" "$work_dir/in.txt"

while IFS= read -r line; do
	if ! printf '%s' "$line" | jq -e 'has("holder") and has("path_pattern") and has("exclusive") and has("expires_ts")' >/dev/null; then
		fail "row is missing a field br requires: $line"
	fi
	if ! printf '%s' "$line" | jq -e '.expires_ts | test("^[0-9]{4}-[0-9]{2}-[0-9]{2}T.*Z$")' >/dev/null; then
		fail "expires_ts is not the RFC 3339 UTC shape br parses: $line"
	fi
done <"$work_dir/out.jsonl"

# A reason field that happened to contain an issue id would make br correlate
# the row to an unrelated claim.
if grep -qE 'louiselm-[a-z0-9]' "$work_dir/out.jsonl"; then
	fail "rows must not mention issue ids, which br would treat as a match reason"
fi

# --- br itself accepts the generated file ------------------------------------

if command -v br >/dev/null 2>&1; then
	if ! br coordination status --reservations "$work_dir/out.jsonl" --json >/dev/null 2>"$work_dir/br-err.txt"; then
		fail "br rejected the generated snapshot: $(cat "$work_dir/br-err.txt")"
	fi
else
	echo "note: br not on PATH, skipped the snapshot acceptance check" >&2
fi

# --- runner stdin and the complete --status boundary -------------------------
# Substitute br so an empty or partial test snapshot never judges real claims.
mkdir -p "$work_dir/bin" "$work_dir/status tmp ' quoted"
cat >"$work_dir/bin/br" <<'MOCK'
#!/usr/bin/env bash
set -euo pipefail
case "$*" in
"list --status in_progress --json")
	printf 'list\n' >>"$LIVENESS_TEST_CALLS"
	if [ "${LIVENESS_TEST_LIST_FAIL:-0}" -ne 0 ]; then exit 42; fi
	cat "$LIVENESS_TEST_ISSUES"
	;;
"coordination status --reservations "*)
	printf 'status\n' >>"$LIVENESS_TEST_CALLS"
	[ "$#" -eq 5 ] && [ "$5" = --json ] || exit 64
	printf '%s' "$4" >"$LIVENESS_TEST_SNAPSHOT_PATH"
	cp "$4" "$LIVENESS_TEST_RESERVATIONS"
	[ -s "$4" ] || exit 65
	exit "${LIVENESS_TEST_STATUS_EXIT:-0}"
	;;
*) exit 64 ;;
esac
MOCK
chmod +x "$work_dir/bin/br"
export PATH="$work_dir/bin:$PATH" TMPDIR="$work_dir/status tmp ' quoted"
export LIVENESS_TEST_CALLS="$work_dir/calls.txt"
export LIVENESS_TEST_ISSUES="$work_dir/issues.json"
export LIVENESS_TEST_SNAPSHOT_PATH="$work_dir/snapshot-path.txt"
export LIVENESS_TEST_RESERVATIONS="$work_dir/reservations.jsonl"
printf '%s\n' '{"issues":[{"assignee":"claude/820e1ab6-83e2-45fd-92f3-79263d4d7141"},{"assignee":null}]}' >"$LIVENESS_TEST_ISSUES"

for input in empty whitespace; do
	printf '' >"$work_dir/in.txt"
	if [ "$input" = whitespace ]; then printf ' \n\t\n' >"$work_dir/in.txt"; fi
	for mode in emit status; do
		printf '' >"$LIVENESS_TEST_CALLS"
		args=()
		if [ "$mode" = status ]; then args=(--status --json); fi
		if ! run_snapshot "$work_dir/shape" "$work_dir/in.txt" "${args[@]}"; then
			fail "$mode with $input stdin failed: $(cat "$work_dir/err.txt")"
		fi
		if ! grep -qx list "$LIVENESS_TEST_CALLS"; then
			fail "$mode with $input stdin did not discover actual claims"
		fi
		if [ "$mode" = status ]; then
			cp "$LIVENESS_TEST_RESERVATIONS" "$work_dir/out.jsonl"
			if [ -e "$(cat "$LIVENESS_TEST_SNAPSHOT_PATH")" ]; then
				fail "successful status left its temporary snapshot behind"
			fi
		fi
		if [ -z "$(row_for "claude/820e1ab6-83e2-45fd-92f3-79263d4d7141")" ]; then
			fail "$mode with $input stdin lost the live holder"
		fi
	done
done

# Explicit assignee input keeps working and must not trigger discovery.
printf '%s\n' 'codex/01a056ef-03ae-7203-885d-c73b4ba00d1b' >"$work_dir/in.txt"
printf '' >"$LIVENESS_TEST_CALLS"
status_exit=0
LIVENESS_TEST_STATUS_EXIT=42 run_snapshot "$work_dir/shape" "$work_dir/in.txt" --status --json || status_exit=$?
if [ "$status_exit" -ne 42 ]; then fail "status lost br's exit code 42"; fi
if grep -qx list "$LIVENESS_TEST_CALLS"; then fail "explicit stdin unexpectedly queried br list"; fi
if [ -e "$(cat "$LIVENESS_TEST_SNAPSHOT_PATH")" ]; then fail "failed status left its snapshot behind"; fi
if [ ! -d "$TMPDIR" ]; then fail "cleanup removed the snapshot's parent directory"; fi
if ! jq -e '.holder == "codex/01a056ef-03ae-7203-885d-c73b4ba00d1b"' "$LIVENESS_TEST_RESERVATIONS" >/dev/null; then
	fail "status replaced explicit stdin with discovered holders"
fi

# No evidence must never become an assertion that every claim is abandoned.
for scenario in no_claims unresolved list_failure closed_stdin; do
	printf '' >"$LIVENESS_TEST_CALLS"
	printf '' >"$work_dir/in.txt"
	printf '%s\n' '{"issues":[]}' >"$LIVENESS_TEST_ISSUES"
	case "$scenario" in
	unresolved) printf '%s\n' 'opencode/unknown-session' >"$work_dir/in.txt" ;;
	list_failure) export LIVENESS_TEST_LIST_FAIL=1 ;;
	esac
	status_exit=0
	if [ "$scenario" = closed_stdin ]; then
		timeout 5 bash -c 'exec "$1" --status --json <&-' _ "$snapshot" >"$work_dir/out.jsonl" 2>"$work_dir/err.txt" || status_exit=$?
	else
		run_snapshot "$work_dir/shape" "$work_dir/in.txt" --status --json || status_exit=$?
	fi
	unset LIVENESS_TEST_LIST_FAIL
	if [ "$status_exit" -eq 0 ]; then fail "$scenario must refuse coordination without evidence"; fi
	if [ "$status_exit" -eq 124 ]; then fail "$scenario hung instead of refusing"; fi
	if grep -qx status "$LIVENESS_TEST_CALLS"; then fail "$scenario passed an empty snapshot to br"; fi
	if [ "$scenario" = unresolved ] && ! grep -q 'opencode/unknown-session' "$work_dir/err.txt"; then
		fail "unresolved holder was not named on stderr"
	fi
done

if [ "$failed" -ne 0 ]; then
	echo "agent-liveness-snapshot: FAILED" >&2
	exit 1
fi

echo "agent-liveness-snapshot: all checks passed"
