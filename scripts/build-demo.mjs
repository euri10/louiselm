#!/usr/bin/env node
import { execFileSync } from 'node:child_process';
import { createHash } from 'node:crypto';
import {
	copyFileSync,
	existsSync,
	lstatSync,
	mkdirSync,
	mkdtempSync,
	readdirSync,
	readFileSync,
	realpathSync,
	rmSync,
	writeFileSync,
} from 'node:fs';
import { tmpdir } from 'node:os';
import { join, relative, resolve, sep } from 'node:path';

const runtimeMembers = [
	'app.js',
	'msgpack.js',
	'msgpackrpc.js',
	'nvim-worker.js',
	'nvim.data',
	'nvim.js',
	'nvim.wasm',
	'rpc.js',
];
const neovimPackagePrefix = 'https://gitlab.bartab.fr/api/v4/projects/163/packages/generic/neovim-wasm/';

function fail(message) {
	throw new Error(`build-demo: ${message}`);
}

function sha256(bytes) {
	return createHash('sha256').update(bytes).digest('hex');
}

function verify(bytes, expected, name) {
	const actual = sha256(bytes);
	if (actual !== expected) fail(`${name} checksum mismatch: expected ${expected}, got ${actual}`);
}

async function download(url, headers = {}, redirect = 'follow') {
	let lastError;
	for (let attempt = 1; attempt <= 3; attempt++) {
		try {
			const response = await fetch(url, {
				headers: { 'user-agent': 'louiselm-site-build', ...headers },
				redirect,
			});
			if (!response.ok) throw new Error(`${response.status} ${response.statusText}`);
			return Buffer.from(await response.arrayBuffer());
		} catch (error) {
			lastError = error;
		}
	}
	fail(`could not download ${url}: ${lastError}`);
}

function neovimPackageHeaders(url) {
	if (!url.startsWith(neovimPackagePrefix)) fail(`Neovim archive must use ${neovimPackagePrefix}`);
	if (process.env.CI_JOB_TOKEN) return { 'job-token': process.env.CI_JOB_TOKEN };
	if (process.env.LOUISELM_DEMO_PACKAGE_TOKEN) {
		return { 'private-token': process.env.LOUISELM_DEMO_PACKAGE_TOKEN };
	}
	fail('Neovim package download requires CI_JOB_TOKEN or LOUISELM_DEMO_PACKAGE_TOKEN');
}

function run(command, args, options = {}) {
	try {
		return execFileSync(command, args, { encoding: null, maxBuffer: 64 * 1024 * 1024, ...options });
	} catch (error) {
		fail(`${command} failed: ${error.message}`);
	}
}

function collectLuaFiles(root) {
	const files = [];
	const visit = (directory) => {
		for (const name of readdirSync(directory).sort()) {
			const absolute = join(directory, name);
			const stat = lstatSync(absolute);
			if (stat.isSymbolicLink()) fail(`symbolic links are not allowed in the demo bundle: ${absolute}`);
			if (stat.isDirectory()) visit(absolute);
			else if (stat.isFile() && name.endsWith('.lua')) files.push(absolute);
			else fail(`non-Lua file under the demo source root: ${absolute}`);
		}
	}
	visit(root);
	return files;
}

function buildFileBundle(sourceRoot, destination) {
	const pluginRoot = join(sourceRoot, 'lua', 'louiselm');
	const installedRoot = '/home/user/.local/share/nvim/site/pack/louiselm/start/louiselm.nvim';
	const entries = collectLuaFiles(pluginRoot).map((absolute) => ({
		path: `${installedRoot}/${relative(sourceRoot, absolute).split(sep).join('/')}`,
		content: readFileSync(absolute, 'utf8'),
	}));
	entries.push({
		path: `${installedRoot}/docs/tutorial.md`,
		content: readFileSync(join(sourceRoot, 'docs', 'tutorial.md'), 'utf8'),
	});

	const sampleRoot = join(sourceRoot, 'demo', 'sample');
	const visitSample = (directory) => {
		for (const name of readdirSync(directory).sort()) {
			const absolute = join(directory, name);
			const stat = lstatSync(absolute);
			if (stat.isSymbolicLink()) fail(`symbolic links are not allowed in the sample project: ${absolute}`);
			if (stat.isDirectory()) visitSample(absolute);
			else if (stat.isFile()) {
				entries.push({
					path: `/demo-project/${relative(sampleRoot, absolute).split(sep).join('/')}`,
					content: readFileSync(absolute, 'utf8'),
				});
			} else fail(`non-regular sample project entry: ${absolute}`);
		}
	};
	visitSample(sampleRoot);
	entries.sort((left, right) => (left.path < right.path ? -1 : left.path > right.path ? 1 : 0));
	const json = JSON.stringify(entries).replaceAll('<', '\\u003c');
	writeFileSync(destination, `globalThis.LOUISELM_DEMO_FILES = Object.freeze(${json});\n`);
}

function adaptVendorLua(vendor, path, source) {
	if (vendor !== 'snacks') return source;
	const windowsChecks = new Map([
		['lua/snacks/util/init.lua', 'M.is_win = jit.os:find("Windows")'],
		['lua/snacks/picker/util/init.lua', 'local is_win = jit.os:find("Windows")'],
	]);
	const expected = windowsChecks.get(path);
	if (expected == null) return source;
	const replacement = expected.replace('jit.os:find("Windows")', 'vim.fn.has("win32") == 1');
	const adapted = source.replace(expected, replacement);
	if (adapted === source) fail(`the pinned Snacks ${path} no longer contains its LuaJIT OS check`);
	return adapted;
}

async function buildVendorBundle(lock, scratch, target) {
	const installedRoot = '/home/user/.local/share/nvim/site/pack/louiselm/opt';
	const vendors = [
		{ key: 'snacks', installName: 'snacks.nvim', licenseName: 'LICENSE-snacks.txt' },
		{ key: 'which_key', installName: 'which-key.nvim', licenseName: 'LICENSE-which-key.txt' },
	];
	const entries = [];
	const pins = {};
	for (const vendor of vendors) {
		const asset = lock[vendor.key];
		const archive = await download(asset.url);
		verify(archive, asset.sha256, `${vendor.installName} archive`);
		const archivePath = join(scratch, `${vendor.key}.tgz`);
		writeFileSync(archivePath, archive);
		run('tar', ['-xzf', archivePath, '-C', scratch]);
		const sourceRoot = join(scratch, asset.archive_prefix);
		for (const absolute of collectLuaFiles(join(sourceRoot, 'lua'))) {
			const path = relative(sourceRoot, absolute).split(sep).join('/');
			entries.push({
				path: `${installedRoot}/${vendor.installName}/${path}`,
				content: adaptVendorLua(vendor.key, path, readFileSync(absolute, 'utf8')),
			});
		}
		const license = readFileSync(join(scratch, asset.license_path));
		verify(license, asset.license_sha256, `${vendor.installName} license`);
		writeFileSync(join(target, vendor.licenseName), license);
		pins[vendor.key] = {
			version: asset.version,
			commit: asset.commit,
			patches: vendor.key === 'snacks' ? ['replace two LuaJIT-only Windows checks with vim.fn.has for Neovim WASM'] : [],
		};
	}
	entries.sort((left, right) => (left.path < right.path ? -1 : left.path > right.path ? 1 : 0));
	const payload = JSON.stringify({ files: entries, pins }).replaceAll('<', '\\u003c');
	writeFileSync(join(target, 'vendor-bundle.js'), `globalThis.LOUISELM_DEMO_VENDOR = Object.freeze(${payload});\n`);
}

function outputManifest(lock, target) {
	const files = {};
	const visit = (directory) => {
		for (const name of readdirSync(directory).sort()) {
			const absolute = join(directory, name);
			const stat = lstatSync(absolute);
			if (stat.isDirectory()) visit(absolute);
			else if (stat.isFile() && name !== 'asset-manifest.json') {
				const bytes = readFileSync(absolute);
				const path = relative(target, absolute).split(sep).join('/');
				files[path] = { sha256: sha256(bytes), size: bytes.length };
			}
		}
	};
	visit(target);
	return { schema_version: 2, assets: lock, files };
}

function quietRuntimeDebug(source) {
	return `const LOUISELM_RUNTIME_DEBUG = false;\n${source.replaceAll('console.log(', 'if (LOUISELM_RUNTIME_DEBUG) console.log(')}`;
}

async function main() {
	if (process.argv.length !== 4) fail('usage: build-demo.mjs <source-root> <artifact-root>');
	const sourceRoot = realpathSync(resolve(process.argv[2]));
	const artifactRoot = realpathSync(resolve(process.argv[3]));
	const demoSource = join(sourceRoot, 'demo');
	const target = join(artifactRoot, 'demo');
	const runtimeRoot = join(target, 'runtime');
	const lock = JSON.parse(readFileSync(join(demoSource, 'assets.lock.json'), 'utf8'));
	const scratch = mkdtempSync(join(tmpdir(), 'louiselm-demo-'));

	if (!existsSync(join(sourceRoot, 'myst.yml'))) fail('source root does not contain myst.yml');
	rmSync(target, { recursive: true, force: true });
	mkdirSync(runtimeRoot, { recursive: true });

	try {
		const neovimArchive = await download(
			lock.neovim.url,
			{ accept: 'application/octet-stream', ...neovimPackageHeaders(lock.neovim.url) },
			'error',
		);
		verify(neovimArchive, lock.neovim.sha256, 'Neovim WASM archive');
		const neovimArchivePath = join(scratch, 'nvim-wasm-emscripten.zip');
		writeFileSync(neovimArchivePath, neovimArchive);
		run('unzip', ['-q', '-j', neovimArchivePath, ...runtimeMembers, '-d', runtimeRoot]);

		const appPath = join(runtimeRoot, 'app.js');
		const upstreamApp = readFileSync(appPath, 'utf8');
		const localApp = upstreamApp.replace('new WorkerTransport("nvim-worker.js"', 'new WorkerTransport("runtime/nvim-worker.js"');
		if (localApp === upstreamApp) fail('the pinned Neovim app.js no longer contains the worker path');
		writeFileSync(appPath, localApp);
		for (const name of ['rpc.js', 'nvim-worker.js']) {
			const path = join(runtimeRoot, name);
			writeFileSync(path, quietRuntimeDebug(readFileSync(path, 'utf8')));
		}

		const neovimLicense = await download(lock.neovim.license_url);
		verify(neovimLicense, lock.neovim.license_sha256, 'Neovim license');
		writeFileSync(join(target, 'LICENSE-neovim.txt'), neovimLicense);

		const msgpackrArchive = await download(lock.msgpackr.url);
		verify(msgpackrArchive, lock.msgpackr.sha256, 'msgpackr archive');
		const msgpackrArchivePath = join(scratch, 'msgpackr.tgz');
		writeFileSync(msgpackrArchivePath, msgpackrArchive);
		writeFileSync(join(runtimeRoot, 'msgpackr.js'), run('tar', ['-xOzf', msgpackrArchivePath, lock.msgpackr.browser_path]));
		const msgpackrLicense = run('tar', ['-xOzf', msgpackrArchivePath, lock.msgpackr.license_path]);
		verify(msgpackrLicense, lock.msgpackr.license_sha256, 'msgpackr license');
		writeFileSync(join(target, 'LICENSE-msgpackr.txt'), msgpackrLicense);

		for (const name of ['index.html', 'demo.css', 'bootstrap.js']) {
			copyFileSync(join(demoSource, name), join(target, name));
		}
		buildFileBundle(sourceRoot, join(target, 'bundle.js'));
		await buildVendorBundle(lock, scratch, target);
		copyFileSync(join(demoSource, 'assets.lock.json'), join(target, 'assets.lock.json'));
		writeFileSync(join(target, 'asset-manifest.json'), `${JSON.stringify(outputManifest(lock, target), null, 2)}\n`);
	} finally {
		rmSync(scratch, { recursive: true, force: true });
	}
}

await main();
