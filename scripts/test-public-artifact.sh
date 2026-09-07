#!/usr/bin/env bash
set -euo pipefail

script_dir=$(CDPATH= cd -- "$(dirname -- "$0")" && pwd)
check_artifact="$script_dir/check-public-artifact"

work_dir=$(mktemp -d)
cleanup() { rm -rf "$work_dir"; }
trap cleanup EXIT

make_safe_artifact() {
	local root=$1
	local source=$2
	mkdir -p "$root/docs" "$root/blog" "$root/build" "$root/demo/runtime"
	mkdir -p "$source/docs"
	: >"$root/index.html"
	: >"$root/docs/index.html"
	: >"$root/blog/index.html"
	: >"$root/demo/index.html"
	: >"$root/demo/runtime/nvim.data"
	: >"$root/demo/runtime/nvim.wasm"
	: >"$root/build/site.js"
	: >"$root/build/tutorial-0123456789abcdef0123456789abcdef.md"
	: >"$root/build/LICENSE-0123456789abcdef0123456789abcdef"
	: >"$source/docs/tutorial.md"
	: >"$source/LICENSE"
	: >"$root/config.json"
	: >"$root/favicon.ico"
	: >"$root/objects.inv"
}

safe="$work_dir/safe"
source_root="$work_dir/source"
make_safe_artifact "$safe" "$source_root"
"$check_artifact" "$safe" "$source_root"

# README links this guide; MyST exports it alongside the rendered public page.
recording_export="$safe/build/turn-recording-0123456789abcdef0123456789abcdef.md"
printf '# Durable turn recording\n' >"$source_root/docs/turn-recording.md"
cp "$source_root/docs/turn-recording.md" "$recording_export"
"$check_artifact" "$safe" "$source_root"
printf 'private material\n' >"$recording_export"
if "$check_artifact" "$safe" "$source_root" >"$work_dir/check.log" 2>&1; then
	echo "expected rejection for a non-curated turn recording export" >&2
	exit 1
fi

for forbidden in \
	"conversations/session.html" \
	"events.jsonl" \
	"terraform.tfstate" \
	"production.plan" \
	"production.tfvars" \
	".env" \
	"source.md" \
	"build/AGENTS-0123456789abcdef0123456789abcdef.md" \
	"build/private-0123456789abcdef0123456789abcdef.md" \
	"build/nested/source.md" \
	"demo/runtime/private.data" \
	"demo/runtime/private.wasm" \
	"outside-demo.data" \
	"outside-demo.wasm" \
	"unexpected.exe"; do
	unsafe="$work_dir/unsafe"
	unsafe_source="$work_dir/unsafe-source"
	rm -rf "$unsafe"
	rm -rf "$unsafe_source"
	make_safe_artifact "$unsafe" "$unsafe_source"
	mkdir -p "$(dirname -- "$unsafe/$forbidden")"
	: >"$unsafe/$forbidden"
	if "$check_artifact" "$unsafe" "$unsafe_source" >"$work_dir/check.log" 2>&1; then
		echo "expected rejection for $forbidden" >&2
		exit 1
	fi
done

mismatched="$work_dir/mismatched"
mismatched_source="$work_dir/mismatched-source"
make_safe_artifact "$mismatched" "$mismatched_source"
printf 'private material\n' >"$mismatched/build/tutorial-0123456789abcdef0123456789abcdef.md"
if "$check_artifact" "$mismatched" "$mismatched_source" >"$work_dir/check.log" 2>&1; then
	echo "expected rejection for a named but non-curated Markdown export" >&2
	exit 1
fi

empty_conversations="$work_dir/empty-conversations"
empty_conversations_source="$work_dir/empty-conversations-source"
make_safe_artifact "$empty_conversations" "$empty_conversations_source"
mkdir "$empty_conversations/conversations"
if "$check_artifact" "$empty_conversations" "$empty_conversations_source" >"$work_dir/check.log" 2>&1; then
	echo "expected rejection for an empty conversations directory" >&2
	exit 1
fi

missing_route="$work_dir/missing-route"
missing_route_source="$work_dir/missing-route-source"
make_safe_artifact "$missing_route" "$missing_route_source"
rm "$missing_route/docs/index.html"
if "$check_artifact" "$missing_route" "$missing_route_source" >"$work_dir/check.log" 2>&1; then
	echo "expected rejection when /docs is missing" >&2
	exit 1
fi

local_origin="$work_dir/local-origin"
local_origin_source="$work_dir/local-origin-source"
make_safe_artifact "$local_origin" "$local_origin_source"
printf 'Sitemap: http://localhost:3000/sitemap.xml\n' >"$local_origin/robots.txt"
if "$check_artifact" "$local_origin" "$local_origin_source" >"$work_dir/check.log" 2>&1; then
	echo "expected rejection for MyST's local build-server origin" >&2
	exit 1
fi

echo "public artifact policy tests passed"
