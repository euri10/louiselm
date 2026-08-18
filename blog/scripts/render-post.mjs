#!/usr/bin/env node
// render-post.mjs <post-dir> <nvim-init.lua> <tohtml-driver.lua>
//
// Implementation helper for render-post.sh (see that file's header for the
// full user-facing description, naming scheme, and idempotency contract).
// Invoked only by render-post.sh, not meant to be run directly.
//
// Responsibilities:
//   1. Parse <post-dir>/blog.md for `:class: nvim-transcript` literalinclude
//      blocks (source path + :lines: range).
//   2. Spawn ONE headless Neovim process (tohtml-driver.lua) that renders
//      every excerpt through :TOhtml using the real user config.
//   3. Extract the <pre>...</pre> fragment and <style> CSS from each raw
//      TOhtml document, namespace every CSS selector under `.nvim-transcript`,
//      dedupe the CSS across the whole post, and write the fragment/CSS
//      files into <post-dir>.
import { existsSync, mkdtempSync, readdirSync, readFileSync, rmSync, unlinkSync, writeFileSync } from 'node:fs';
import { tmpdir } from 'node:os';
import { join, resolve } from 'node:path';
import { spawnSync } from 'node:child_process';

const WRAPPER_CLASS = 'nvim-transcript';
const CSS_FILENAME = `${WRAPPER_CLASS}.css`;
const FRAGMENT_SUFFIX = `.${WRAPPER_CLASS}.html`;

function fail(message) {
	console.error(`render-post: ${message}`);
	process.exit(1);
}

/**
 * Scans blog.md for fenced `{literalinclude}` blocks tagged
 * `:class: nvim-transcript` and returns their source path and 1-based,
 * inclusive :lines: range.
 *
 * Only a single contiguous "N-M" :lines: range is supported (matching the
 * one concrete example in the spec); MyST's fuller :lines: grammar (e.g.
 * "1,3,5-10,20-") is out of scope here and raises a clear error instead of
 * silently mis-rendering it.
 *
 * @param {string} text contents of blog.md
 * @returns {{sourcePath: string, first: number, last: number, declaredAtLine: number}[]}
 */
export function parseTranscriptBlocks(text) {
	const lines = text.split(/\r?\n/);
	const blocks = [];

	for (let i = 0; i < lines.length; i++) {
		const open = lines[i].match(/^(`{3,}|:{3,})\{literalinclude\}\s+(\S+)\s*$/);
		if (!open) continue;

		const fenceToken = open[1];
		const fenceChar = fenceToken[0];
		const fenceLen = fenceToken.length;
		const sourcePath = open[2];
		const declaredAtLine = i + 1;
		const closeRe = new RegExp(`^\\${fenceChar}{${fenceLen},}\\s*$`);

		let closeIdx = -1;
		const options = {};
		let j = i + 1;
		for (; j < lines.length; j++) {
			if (closeRe.test(lines[j])) {
				closeIdx = j;
				break;
			}
			const opt = lines[j].match(/^:(\S+):\s*(.*)$/);
			if (opt) {
				options[opt[1]] = opt[2].trim();
			} else if (lines[j].trim() !== '') {
				// Unexpected non-option, non-blank content inside what we assumed
				// was an options-only leaf directive -- stop trusting this block
				// rather than silently misparsing it.
				break;
			}
		}

		if (closeIdx === -1) {
			throw new Error(`unclosed {literalinclude} fence starting at blog.md:${declaredAtLine}`);
		}
		i = closeIdx;

		const classes = (options.class ?? '').split(/\s+/).filter(Boolean);
		if (!classes.includes('nvim-transcript')) continue;

		const linesOpt = options.lines;
		if (!linesOpt) {
			throw new Error(`nvim-transcript block at blog.md:${declaredAtLine} is missing a :lines: option`);
		}
		const range = linesOpt.match(/^(\d+)-(\d+)$/);
		if (!range) {
			throw new Error(
				`nvim-transcript block at blog.md:${declaredAtLine} has an unsupported :lines: value ` +
					`${JSON.stringify(linesOpt)} (only a single "N-M" range is supported)`,
			);
		}
		const first = Number(range[1]);
		const last = Number(range[2]);
		if (first < 1 || last < first) {
			throw new Error(`nvim-transcript block at blog.md:${declaredAtLine} has an invalid :lines: range ${linesOpt}`);
		}

		blocks.push({ sourcePath, first, last, declaredAtLine });
	}

	return blocks;
}

/**
 * Deterministic output slug for an excerpt: its source path (relative to
 * the post dir, extension dropped, `/` -> `-`) plus its line range, e.g.
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
 * Namespaces a single TOhtml-generated CSS selector under `.nvim-transcript`
 * so it can never affect anything outside that wrapper:
 *   `*`            -> `.nvim-transcript *`
 *   `body`         -> `.nvim-transcript`
 *   `.ClassName`   -> `.nvim-transcript .ClassName`
 * Throws on any other shape rather than guessing -- TOhtml is only known to
 * emit these three selector shapes (verified against real output).
 * @param {string} selector
 */
export function namespaceSelector(selector) {
	if (selector === '*') return `.${WRAPPER_CLASS} *`;
	if (selector === 'body') return `.${WRAPPER_CLASS}`;
	if (selector.startsWith('.')) return `.${WRAPPER_CLASS} ${selector}`;
	throw new Error(`unexpected TOhtml CSS selector, refusing to guess how to namespace it safely: ${JSON.stringify(selector)}`);
}

/**
 * Parses the CSS rule lines found between a TOhtml document's <style> tags
 * (as returned by extractTagBlock, tags included) and merges their
 * namespaced form into `cssRules` (selector -> declaration body).
 * @param {string[]} styleBlock
 * @param {Map<string,string>} cssRules
 */
export function collectCss(styleBlock, cssRules) {
	for (const line of styleBlock.slice(1, -1)) {
		if (line.trim() === '') continue;
		const rule = line.match(/^(.*?)\s*\{(.*)\}\s*$/);
		if (!rule) throw new Error(`could not parse TOhtml CSS rule: ${JSON.stringify(line)}`);
		const selector = namespaceSelector(rule[1].trim());
		const body = rule[2].trim();
		const existing = cssRules.get(selector);
		if (existing !== undefined && existing !== body) {
			throw new Error(
				`TOhtml produced two different rule bodies for the same namespaced selector ` +
					`${selector} (${JSON.stringify(existing)} vs ${JSON.stringify(body)}) -- ` +
					`refusing to silently pick one`,
			);
		}
		cssRules.set(selector, body);
	}
}

function formatRule(selector, body) {
	return body === '' ? `${selector} {}` : `${selector} { ${body} }`;
}

function main() {
	const [, , postDirArg, nvimInitArg, luaDriverArg] = process.argv;
	if (!postDirArg || !nvimInitArg || !luaDriverArg) {
		fail('usage (internal): render-post.mjs <post-dir> <nvim-init.lua> <tohtml-driver.lua>');
	}

	const postDir = resolve(postDirArg);
	const blogMdPath = join(postDir, 'blog.md');
	if (!existsSync(blogMdPath)) fail(`${blogMdPath} not found`);

	let blocks;
	try {
		blocks = parseTranscriptBlocks(readFileSync(blogMdPath, 'utf8'));
	} catch (err) {
		fail(err.message);
	}

	const seenSlugs = new Map();
	const entries = blocks.map((block, idx) => {
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

		return {
			id: String(idx + 1).padStart(3, '0'),
			absSource,
			first: block.first,
			last: block.last,
			fragmentPath: join(postDir, `${slug}${FRAGMENT_SUFFIX}`),
		};
	});

	// Output is a pure function of the current blog.md: always clear
	// previously generated files first so a removed/renamed block can never
	// leave an orphaned fragment behind.
	for (const name of readdirSync(postDir)) {
		if (name === CSS_FILENAME || name.endsWith(FRAGMENT_SUFFIX)) {
			unlinkSync(join(postDir, name));
		}
	}

	if (entries.length === 0) {
		console.log('render-post: no :class: nvim-transcript blocks found in blog.md, nothing to do');
		return;
	}

	const tmpDir = mkdtempSync(join(tmpdir(), 'louiselm-render-post-'));
	try {
		const manifest = entries.map((e) => ({
			file: e.absSource,
			first: e.first,
			last: e.last,
			out: join(tmpDir, `${e.id}.raw.html`),
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

		const cssRules = new Map();
		entries.forEach((entry, idx) => {
			const raw = readFileSync(manifest[idx].out, 'utf8').split('\n');

			const preBlock = extractTagBlock(raw, '<pre>', '</pre>');
			const fragment = [`<div class="${WRAPPER_CLASS}">`, ...preBlock, '</div>', ''].join('\n');
			writeFileSync(entry.fragmentPath, fragment);

			collectCss(extractTagBlock(raw, '<style>', '</style>'), cssRules);
		});

		const cssLines = [
			'/* Generated by blog/scripts/render-post.sh -- do not edit by hand. */',
			...[...cssRules.keys()].sort().map((selector) => formatRule(selector, cssRules.get(selector))),
			'',
		];
		writeFileSync(join(postDir, CSS_FILENAME), cssLines.join('\n'));

		console.log(`render-post: wrote ${entries.length} fragment(s) and ${CSS_FILENAME} to ${postDir}`);
	} finally {
		rmSync(tmpDir, { recursive: true, force: true });
	}
}

if (import.meta.url === `file://${process.argv[1]}`) {
	main();
}
