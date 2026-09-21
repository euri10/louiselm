#!/usr/bin/env bash
# Run inside the disposable guest, from the campaign checkout's skills-core.
set -euo pipefail
library_test="$(cargo test --lib --all-features --locked --no-run --message-format=json |
  jq -r 'select(.reason == "compiler-artifact" and .target.name == "louiselm_skills" and .profile.test) | .executable // empty' | tail -n 1)"
test -n "$library_test"
test_name=launch_supervisor::system::installed_tests::daemon::state::privileged_activated_daemon_upgrades_and_adopts_state
date -u +%FT%TZ
for sample in warmup 1 2 3; do
  printf 'sample=%s\n' "$sample"
  TIMEFORMAT='wall=%R user=%U sys=%S'
  time sudo env LOUISELM_REQUIRE_CONTROL_DAEMON=1 timeout 240 \
    unshare --mount --propagation private -- /bin/bash -c \
    'umask 022; exec "$1" "$2" --exact --nocapture' bash "$library_test" "$test_name"
done
../digest-benchmark "$CARGO_TARGET_DIR/debug/louiselm-launch" \
  "$CARGO_TARGET_DIR/debug/louiselm-control" \
  "$CARGO_TARGET_DIR/debug/louiselm-tool-test-agent" \
  "$CARGO_TARGET_DIR/debug/louiselm-tool-test-helper"
date -u +%FT%TZ
