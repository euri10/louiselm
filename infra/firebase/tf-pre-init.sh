#!/bin/sh
set -eu

# The pinned GitLab component runs this from TF_PROJECT_DIR before every init.
for config in ./*.tf; do
  if grep -Eq '^[[:space:]]*backend[[:space:]]+"http"[[:space:]]*\{' "$config"; then
    exit 0
  fi
done

echo 'Firebase CI requires an HTTP backend declaration for GitLab-managed state.' >&2
exit 1
