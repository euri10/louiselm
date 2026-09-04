#!/usr/bin/env node
// render-post.mjs <post-dir> <nvim-init.lua> <tohtml-driver.lua>
//
// Implementation helper for render-post.sh (see that file's header for the
// full user-facing description, naming scheme, and idempotency contract).
// Invoked only by render-post.sh, not meant to be run directly.
//
// Responsibilities:
//   1. Parse <post-dir>/blog.md for `% nvim-transcript: <source> :lines: N-M`
//      marker comments (never rendered by MyST -- see the `%` comment
//      syntax).
//   2. Spawn ONE headless Neovim process (tohtml-driver.lua) that renders
//      every excerpt through :TOhtml using the real user config.
//   3. Extract the <pre>...</pre> fragment and <style> CSS from each raw
//      TOhtml document and wrap them into a standalone HTML document per
//      excerpt. The <pre> is verbatim; the <style> block is sorted into a
//      canonical order (see normalizeStyleBlock) so that re-rendering an
//      unchanged post does not rewrite its fragment with different bytes
//      (no shared stylesheet, no selector namespacing
//      -- the fragment is embedded through an <iframe>, which already
//      isolates it from the surrounding page and from every other excerpt).
//   4. Rewrite blog.md in place, inserting/replacing an `{iframe}` block
//      after each marker comment -- the comment itself is never touched, so
//      the author can keep editing `:lines:` and re-run this indefinitely.
//   5. Keep the project's `myst.yml` `static_files` list in sync with the
//      fragment files this post currently produces, so `myst build` copies
//      them into the deployed site.
import { existsSync, mkdtempSync, readdirSync, readFileSync, rmSync, unlinkSync, writeFileSync } from 'node:fs';
import { tmpdir } from 'node:os';
import { basename, dirname, join, relative, resolve } from 'node:path';
import { spawnSync } from 'node:child_process';

const FRAGMENT_SUFFIX = '.nvim-transcript.html';
const COMMENT_RE = /^%\s*nvim-transcript:\s*(\S+)\s*:lines:\s*(.+?)\s*$/;
const IFRAME_OPEN_RE = /^```\{iframe\}\s+(\S+)\s*$/;

function fail(message) {
	console.error(`render-post: ${message}`);
	process.exit(1);
}

function findProjectConfig(startDir) {
	let dir = startDir;
	while (true) {
		const candidate = join(dir, 'myst.yml');
		if (existsSync(candidate)) return candidate;
		const parent = dirname(dir);
		if (parent === dir) return undefined;
		dir = parent;
	}
}

/**
 * Scans blog.md for `% nvim-transcript: <source> :lines: N-M` marker
 * comments and returns their source path, 1-based inclusive line range, and
 * the 1-based line the comment itself was found on.
 *
 * Only a single contiguous "N-M" :lines: range is supported; anything else
 * (including the `:lines: 1` placeholder create-blog.sh stubs in) raises a
 * clear error instead of silently mis-rendering it.
 *
 * @param {string} text contents of blog.md
 * @returns {{sourcePath: string, first: number, last: number, declaredAtLine: number}[]}
 */
export function parseTranscriptBlocks(text) {
	const lines = text.split(/\r?\n/);
	const blocks = [];

	for (let i = 0; i < lines.length; i++) {
		const m = lines[i].match(COMMENT_RE);
		if (!m) continue;

		const sourcePath = m[1];
		const linesOpt = m[2];
		const declaredAtLine = i + 1;

		const range = linesOpt.match(/^(\d+)-(\d+)$/);
		if (!range) {
			throw new Error(
				`nvim-transcript comment at blog.md:${declaredAtLine} has an unsupported :lines: value ` +
					`${JSON.stringify(linesOpt)} (expected a single "N-M" range -- replace the placeholder before rendering)`,
			);
		}
		const first = Number(range[1]);
		const last = Number(range[2]);
		if (first < 1 || last < first) {
			throw new Error(`nvim-transcript comment at blog.md:${declaredAtLine} has an invalid :lines: range ${linesOpt}`);
		}

		blocks.push({ sourcePath, first, last, declaredAtLine });
	}

	return blocks;
}

/**
 * Deterministic slug for an excerpt: its source path (relative to the post
 * dir, extension dropped, `/` -> `-`) plus its line range, e.g.
 * "conversations/claude-xxx.md" lines 87-112 -> "conversations-claude-xxx.L87-112".
 * @param {string} sourcePath
 * @param {number} first
 * @param {number} last
 */
export function slugFor(sourcePath, first, last) {
	const noExt = sourcePath.replace(/\.[^./]+$/, '');
	const cleaned = noExt.replace(/^\.\//, '').replace(/\//g, '-');
	return `${cleaned}.L${first}-${last}`;
}

/**
 * A generated fragment's site-wide-unique basename: `static_files` copies
 * every declared file into one flat directory keyed by basename alone, so
 * the post directory name is folded into the filename itself rather than
 * relying on directory structure to keep two posts' excerpts apart.
 * @param {string} postName basename of the post directory, e.g. "post-foo"
 * @param {string} slug see slugFor
 */
export function fragmentBasename(postName, slug) {
	return `${postName}--${slug}${FRAGMENT_SUFFIX}`;
}

/**
 * Returns the inclusive slice of `lines` from the line exactly equal to
 * `openTag` through the line exactly equal to `closeTag`. Throws if either
 * tag is missing, duplicated, or out of order -- TOhtml emits each of
 * <pre>/</pre>/<style>/</style> as exactly one standalone line, so any other
 * shape means TOhtml's output format changed underneath us.
 * @param {string[]} lines
 * @param {string} openTag
 * @param {string} closeTag
 */
function extractTagBlock(lines, openTag, closeTag) {
	const openIdx = lines.indexOf(openTag);
	const closeIdx = lines.indexOf(closeTag);
	if (openIdx === -1 || closeIdx === -1 || closeIdx < openIdx) {
		throw new Error(`expected to find "${openTag}" ... "${closeTag}" in TOhtml output, did not`);
	}
	if (lines.indexOf(openTag, openIdx + 1) !== -1) {
		throw new Error(`found more than one "${openTag}" line in TOhtml output`);
	}
	return lines.slice(openIdx, closeIdx + 1);
}

/**
 * Sorts a TOhtml <style> block into a canonical form: rules ordered by
 * selector, and declarations ordered within each rule. The <style>/</style>
 * lines keep their positions.
 *
 * TOhtml builds this block by iterating the highlight groups it collected
 * with `pairs()`, whose order Lua leaves unspecified and which varies
 * between processes. Two runs over identical input therefore emit the same
 * rules in different orders, and rendering an unchanged post rewrote its
 * committed fragment with a semantically null diff (louiselm-qcq).
 *
 * Reordering is safe precisely *because* TOhtml's order is already
 * arbitrary: if the cascade were load-bearing here, TOhtml's own output
 * would already be nondeterministically wrong. Selector specificity, which
 * sorting cannot change, is what actually resolves these rules.
 *
 * Lines that do not parse as `selector {declarations}` are passed through
 * unchanged and sorted by their raw text, so an unexpected TOhtml output
 * shape degrades to "still deterministic" rather than being dropped.
 * @param {string[]} styleBlock inclusive <style>...</style> slice
 * @returns {string[]} the same slice, canonically ordered
 */
export function normalizeStyleBlock(styleBlock) {
	const open = styleBlock[0];
	const close = styleBlock[styleBlock.length - 1];
	const rules = styleBlock.slice(1, -1).map((line) => {
		const match = /^(.*?)\s*\{(.*)\}\s*$/.exec(line);
		if (!match) {
			return { key: line, text: line };
		}
		const selector = match[1];
		const declarations = match[2]
			.split(';')
			.map((d) => d.trim())
			.filter(Boolean)
			.sort();
		return { key: selector, text: `${selector} {${declarations.join('; ')}}` };
	});
	// Code-unit comparison, not localeCompare: the output must not depend on
	// the locale of whoever runs the pipeline.
	rules.sort((a, b) => (a.key < b.key ? -1 : a.key > b.key ? 1 : 0));
	return [open, ...rules.map((r) => r.text), close];
}

/**
 * Wraps a TOhtml capture's <style> and <pre> blocks (tags included) into a
 * standalone HTML document. The <pre> block is used verbatim; the <style>
 * block is canonically ordered by normalizeStyleBlock, which is what makes
 * the fragment a pure function of its input. No selector namespacing is
 * needed: this document is only ever loaded through an <iframe>, which
 * already isolates it from the surrounding blog page and from every other
 * excerpt's fragment.
 *
 * MyST's `{iframe}` directive has no height option (only width/align/title/
 * placeholder) -- the theme sizes the iframe itself via a fixed
 * width-relative aspect ratio it does not expose for us to configure. Rather
 * than fight that, the document scrolls: an excerpt taller than the iframe
 * box stays fully reachable instead of silently clipped.
 * @param {string[]} styleBlock
 * @param {string[]} preBlock
 */
export function buildFragmentDoc(styleBlock, preBlock) {
	return [
		'<!DOCTYPE html>',
		'<html>',
		'<head>',
		'<meta charset="utf-8">',
		'<style>html, body { margin: 0; height: 100%; overflow: auto; }</style>',
		...normalizeStyleBlock(styleBlock),
		'</head>',
		'<body>',
		...preBlock,
		'</body>',
		'</html>',
		'',
	].join('\n');
}

/**
 * Removes every previously generated `{iframe}` block for this post (any
 * fence whose src is `/<postName>--...<FRAGMENT_SUFFIX>`), along with the
 * one blank line this tool always inserts immediately before it. This runs
 * before regenerating blocks, so it is what makes regeneration a clean
 * fixed point regardless of whether a block's source comment still exists
 * (an author deleting a whole marker comment leaves no dangling iframe
 * behind) or its content changed (stale content never lingers next to
 * fresh content).
 * @param {string[]} lines
 * @param {string} postName
 */
export function stripGeneratedBlocks(lines, postName) {
	const out = [];
	for (let i = 0; i < lines.length; i++) {
		const m = lines[i].match(IFRAME_OPEN_RE);
		if (m && isGeneratedSrc(m[1], postName)) {
			let k = i + 1;
			while (k < lines.length && lines[k] !== '```') k++;
			if (k >= lines.length) {
				throw new Error(`blog.md: found an opening \`\`\`{iframe} fence for a generated fragment with no closing \`\`\` (near line ${i + 1})`);
			}
			if (out.length > 0 && out[out.length - 1] === '') {
				out.pop();
			}
			i = k;
			continue;
		}
		out.push(lines[i]);
	}
	return out;
}

function isGeneratedSrc(src, postName) {
	return src.startsWith(`/${postName}--`) && src.endsWith(FRAGMENT_SUFFIX);
}

/**
 * Inserts one `{iframe}` fence block, preceded by a blank line, immediately
 * after each entry's marker comment line. Assumes `lines` has already been
 * through stripGeneratedBlocks, so this is pure insertion -- no existing
 * block to find or remove. Entries are applied in descending
 * `declaredAtLine` order so earlier insertions never shift the line numbers
 * later entries were computed against.
 * @param {string[]} lines
 * @param {{declaredAtLine: number, iframeSrc: string}[]} entries
 */
export function insertGeneratedBlocks(lines, entries) {
	const out = lines.slice();
	const ordered = [...entries].sort((a, b) => b.declaredAtLine - a.declaredAtLine);
	for (const entry of ordered) {
		const commentIdx = entry.declaredAtLine - 1;
		const block = ['', `\`\`\`{iframe} ${entry.iframeSrc}`, ':width: 100%', '```'];
		out.splice(commentIdx + 1, 0, ...block);
	}
	return out;
}

/**
 * Merges this post's current fragment paths into myst.yml's
 * `project.static_files` list, replacing (not appending to) any existing
 * entries under `<postRelDir>/` so removed excerpts don't leave stale
 * entries behind. Entries from every other post are left untouched. The
 * merged list is always written back sorted, so re-running with an
 * unchanged input set is byte-identical regardless of iteration order.
 * @param {string} mystYmlText
 * @param {string} postRelDir project-root-relative post directory, e.g. "post-foo"
 * @param {string[]} relPaths project-root-relative fragment paths for this post
 */
export function mergeStaticFiles(mystYmlText, postRelDir, relPaths) {
	const lines = mystYmlText.split(/\r?\n/);
	const projectIdx = lines.findIndex((l) => l === 'project:');
	if (projectIdx === -1) {
		throw new Error('myst.yml has no top-level "project:" key');
	}
	let projectEnd = lines.length;
	for (let i = projectIdx + 1; i < lines.length; i++) {
		if (/^\S/.test(lines[i])) {
			projectEnd = i;
			break;
		}
	}

	let staticIdx = -1;
	for (let i = projectIdx + 1; i < projectEnd; i++) {
		if (lines[i] === '  static_files:') {
			staticIdx = i;
			break;
		}
	}

	let existingItems = [];
	let staticEnd = staticIdx === -1 ? -1 : staticIdx + 1;
	if (staticIdx !== -1) {
		let i = staticIdx + 1;
		for (; i < projectEnd; i++) {
			const m = lines[i].match(/^ {4}- '([^']*)'$/);
			if (!m) break;
			existingItems.push(m[1]);
		}
		staticEnd = i;
	}

	const prefix = `${postRelDir}/`;
	const kept = existingItems.filter((p) => !p.startsWith(prefix));
	const merged = [...new Set([...kept, ...relPaths])].sort();

	const newLines = lines.slice();
	if (staticIdx !== -1) {
		newLines.splice(staticIdx, staticEnd - staticIdx);
		if (merged.length > 0) {
			newLines.splice(staticIdx, 0, '  static_files:', ...merged.map((p) => `    - '${p}'`));
		}
	} else if (merged.length > 0) {
		newLines.splice(projectIdx + 1, 0, '  static_files:', ...merged.map((p) => `    - '${p}'`));
	}
	return newLines.join('\n');
}

function main() {
	const [, , postDirArg, nvimInitArg, luaDriverArg] = process.argv;
	if (!postDirArg || !nvimInitArg || !luaDriverArg) {
		fail('usage (internal): render-post.mjs <post-dir> <nvim-init.lua> <tohtml-driver.lua>');
	}

	const postDir = resolve(postDirArg);
	const postName = basename(postDir);
	const blogMdPath = join(postDir, 'blog.md');
	if (!existsSync(blogMdPath)) fail(`${blogMdPath} not found`);

	const mystYmlPath = findProjectConfig(postDir);
	if (!mystYmlPath) fail(`myst.yml not found in ${postDir} or any parent directory`);
	const projectRoot = dirname(mystYmlPath);
	const postRelDir = relative(projectRoot, postDir).replaceAll('\\', '/');

	const originalText = readFileSync(blogMdPath, 'utf8');

	let blocks;
	try {
		blocks = parseTranscriptBlocks(originalText);
	} catch (err) {
		fail(err.message);
	}

	const seenSlugs = new Map();
	const entries = blocks.map((block) => {
		const absSource = resolve(postDir, block.sourcePath);
		if (!existsSync(absSource)) {
			fail(`blog.md:${block.declaredAtLine} references a source file that does not exist: ${block.sourcePath}`);
		}
		const sourceLineCount = readFileSync(absSource, 'utf8').split(/\r?\n/).length;
		if (block.last > sourceLineCount) {
			fail(
				`blog.md:${block.declaredAtLine} :lines: ${block.first}-${block.last} exceeds ` +
					`${block.sourcePath}'s ${sourceLineCount} line(s)`,
			);
		}

		const baseSlug = slugFor(block.sourcePath, block.first, block.last);
		const count = (seenSlugs.get(baseSlug) ?? 0) + 1;
		seenSlugs.set(baseSlug, count);
		const slug = count === 1 ? baseSlug : `${baseSlug}-${count}`;

		const basename_ = fragmentBasename(postName, slug);
		return {
			absSource,
			first: block.first,
			last: block.last,
			declaredAtLine: block.declaredAtLine,
			basename: basename_,
			fragmentPath: join(postDir, basename_),
			iframeSrc: `/${basename_}`,
			staticRelPath: `${postRelDir}/${basename_}`,
		};
	});

	// Output is a pure function of the current blog.md: always clear
	// previously generated fragment files first so a removed/renamed block
	// can never leave an orphan behind.
	for (const name of readdirSync(postDir)) {
		if (name.endsWith(FRAGMENT_SUFFIX)) {
			unlinkSync(join(postDir, name));
		}
	}

	// blog.md and myst.yml bookkeeping happens regardless of whether there is
	// anything left to render, so deleting the last excerpt from a post
	// cleans up its dangling iframe block and static_files entry too.
	const strippedLines = stripGeneratedBlocks(originalText.split(/\r?\n/), postName);
	const freshBlocks = parseTranscriptBlocks(strippedLines.join('\n'));
	const finalEntries = entries.map((entry, idx) => ({ ...entry, declaredAtLine: freshBlocks[idx].declaredAtLine }));
	const newBlogMd = insertGeneratedBlocks(strippedLines, finalEntries).join('\n');
	if (newBlogMd !== originalText) {
		writeFileSync(blogMdPath, newBlogMd);
	}

	const mystYmlText = readFileSync(mystYmlPath, 'utf8');
	const newMystYml = mergeStaticFiles(
		mystYmlText,
		postRelDir,
		finalEntries.map((e) => e.staticRelPath),
	);
	if (newMystYml !== mystYmlText) {
		writeFileSync(mystYmlPath, newMystYml);
	}

	if (finalEntries.length === 0) {
		console.log('render-post: no nvim-transcript comments found in blog.md, nothing to render');
		return;
	}

	const tmpDir = mkdtempSync(join(tmpdir(), 'louiselm-render-post-'));
	try {
		const manifest = finalEntries.map((e, idx) => ({
			file: e.absSource,
			first: e.first,
			last: e.last,
			out: join(tmpDir, `${String(idx + 1).padStart(3, '0')}.raw.html`),
		}));
		const manifestPath = join(tmpDir, 'manifest.json');
		writeFileSync(manifestPath, JSON.stringify(manifest));

		// Escaped the same way vim.fn.fnameescape() would: nvim's `-c` value is
		// parsed as an ex command line by Neovim itself (no shell involved,
		// spawnSync passes argv directly), so a literal space in the driver
		// path would otherwise be read as the end of the :luafile argument.
		const escapedDriverPath = luaDriverArg.replace(/ /g, '\\ ');
		const result = spawnSync('nvim', ['--headless', '-u', nvimInitArg, '-c', `luafile ${escapedDriverPath}`], {
			env: { ...process.env, LOUISELM_TOHTML_MANIFEST: manifestPath },
			encoding: 'utf8',
		});

		if (result.error) fail(`failed to run nvim: ${result.error.message}`);
		if (result.status !== 0) {
			fail(`nvim exited with status ${result.status}\n${result.stderr}${result.stdout}`);
		}

		finalEntries.forEach((entry, idx) => {
			const raw = readFileSync(manifest[idx].out, 'utf8').split('\n');
			const preBlock = extractTagBlock(raw, '<pre>', '</pre>');
			const styleBlock = extractTagBlock(raw, '<style>', '</style>');
			writeFileSync(entry.fragmentPath, buildFragmentDoc(styleBlock, preBlock));
		});

		console.log(`render-post: wrote ${finalEntries.length} fragment(s) to ${postDir}, updated blog.md and myst.yml`);
	} finally {
		rmSync(tmpDir, { recursive: true, force: true });
	}
}

if (import.meta.url === `file://${process.argv[1]}`) {
	main();
}
