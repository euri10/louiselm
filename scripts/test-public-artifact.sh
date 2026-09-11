#!/usr/bin/env bash
set -euo pipefail

script_dir=$(CDPATH= cd -- "$(dirname -- "$0")" && pwd)
check_artifact="$script_dir/check-public-artifact"

work_dir=$(mktemp -d)
cleanup() { rm -rf "$work_dir"; }
trap cleanup EXIT

rendered_page() {
	# The generator marker is what tells the checker this page came from MyST
	# and therefore owes a description.
	printf '<meta name="generator" content="mystmd"/><meta name="description" content="%s"/>\n' "$1"
}

make_safe_artifact() {
	local root=$1
	local source=$2
	mkdir -p "$root/docs" "$root/blog" "$root/build" "$root/demo/runtime"
	mkdir -p "$source/docs" "$source/assets"
	printf 'icon bytes\n' >"$source/assets/louiselm-favicon.ico"
	rendered_page "Chat with ACP Agents inside Neovim." >"$root/index.html"
	rendered_page "The LouiseLM Tutor." >"$root/docs/index.html"
	rendered_page "The louiselm blog." >"$root/blog/index.html"
	# Built separately from MyST, so it carries no generator marker and owes
	# no description.
	: >"$root/demo/index.html"
	: >"$root/demo/runtime/nvim.data"
	: >"$root/demo/runtime/nvim.wasm"
	: >"$root/build/site.js"
	: >"$root/build/tutorial-0123456789abcdef0123456789abcdef.md"
	: >"$root/build/LICENSE-0123456789abcdef0123456789abcdef"
	: >"$source/docs/tutorial.md"
	: >"$source/LICENSE"
	: >"$root/config.json"
	cp "$source/assets/louiselm-favicon.ico" "$root/favicon.ico"
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
	"build/acp-log-backups-0123456789abcdef0123456789abcdef.md" \
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

# A relative link from README.md to a non-curated doc (e.g. skills-core/README.md)
# makes MyST export that doc under README's own basename, distinguished only by
# hash. The checker must reject it even though the basename matches a curated
# source name (louiselm-ejbu).
readme_collision="$work_dir/readme-collision"
readme_collision_source="$work_dir/readme-collision-source"
make_safe_artifact "$readme_collision" "$readme_collision_source"
printf '# LouiseLM skills core\n' >"$readme_collision/build/README-0123456789abcdef0123456789abcdef.md"
if "$check_artifact" "$readme_collision" "$readme_collision_source" >"$work_dir/check.log" 2>&1; then
	echo "expected rejection for a linked doc exported under README's basename" >&2
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

theme_favicon="$work_dir/theme-favicon"
theme_favicon_source="$work_dir/theme-favicon-source"
make_safe_artifact "$theme_favicon" "$theme_favicon_source"
printf 'someone else icon bytes\n' >"$theme_favicon/favicon.ico"
if "$check_artifact" "$theme_favicon" "$theme_favicon_source" >"$work_dir/check.log" 2>&1; then
	echo "expected rejection for a favicon that is not the project asset" >&2
	exit 1
fi

missing_favicon="$work_dir/missing-favicon"
missing_favicon_source="$work_dir/missing-favicon-source"
make_safe_artifact "$missing_favicon" "$missing_favicon_source"
rm "$missing_favicon/favicon.ico"
if "$check_artifact" "$missing_favicon" "$missing_favicon_source" >"$work_dir/check.log" 2>&1; then
	echo "expected rejection when the favicon is missing" >&2
	exit 1
fi

relative_image="$work_dir/relative-image"
relative_image_source="$work_dir/relative-image-source"
make_safe_artifact "$relative_image" "$relative_image_source"
printf '<meta property="og:image" content="/build/card.webp"/>\n' >>"$relative_image/index.html"
if "$check_artifact" "$relative_image" "$relative_image_source" >"$work_dir/check.log" 2>&1; then
	echo "expected rejection for a site-relative preview image" >&2
	exit 1
fi

absolute_image="$work_dir/absolute-image"
absolute_image_source="$work_dir/absolute-image-source"
make_safe_artifact "$absolute_image" "$absolute_image_source"
printf '<meta property="og:image" content="https://louiselm.com/build/card.webp"/>\n' >>"$absolute_image/index.html"
"$check_artifact" "$absolute_image" "$absolute_image_source"

no_description="$work_dir/no-description"
no_description_source="$work_dir/no-description-source"
make_safe_artifact "$no_description" "$no_description_source"
printf '<meta name="generator" content="mystmd"/>\n' >"$no_description/docs/index.html"
if "$check_artifact" "$no_description" "$no_description_source" >"$work_dir/check.log" 2>&1; then
	echo "expected rejection for a rendered page with no description" >&2
	exit 1
fi

empty_description="$work_dir/empty-description"
empty_description_source="$work_dir/empty-description-source"
make_safe_artifact "$empty_description" "$empty_description_source"
rendered_page "" >"$empty_description/docs/index.html"
if "$check_artifact" "$empty_description" "$empty_description_source" >"$work_dir/check.log" 2>&1; then
	echo "expected rejection for a rendered page with an empty description" >&2
	exit 1
fi

echo "public artifact policy tests passed"
