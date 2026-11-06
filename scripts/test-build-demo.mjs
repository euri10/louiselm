#!/usr/bin/env node
import assert from 'node:assert/strict';
import { spawnSync } from 'node:child_process';
import { createHash } from 'node:crypto';
import { mkdirSync, mkdtempSync, readdirSync, readFileSync, rmSync, symlinkSync, writeFileSync } from 'node:fs';
import { tmpdir } from 'node:os';
import { dirname, join, resolve } from 'node:path';
import test from 'node:test';

const script = resolve('scripts/build-demo.mjs');
const digest = (bytes) => createHash('sha256').update(bytes).digest('hex');

function fixture(t) {
	const root = mkdtempSync(join(tmpdir(), 'louiselm-demo-test-'));
	t.after(() => rmSync(root, { recursive: true, force: true }));
	const source = join(root, 'source');
	const output = join(root, 'output');
	const assets = join(root, 'assets');
	for (const path of [source, output, assets]) mkdirSync(path);
	const write = (path, bytes) => {
		mkdirSync(dirname(path), { recursive: true });
		writeFileSync(path, bytes);
	};
	const command = (program, args, cwd) => {
		const result = spawnSync(program, args, { cwd, encoding: 'utf8', timeout: 10000 });
		assert.equal(result.status, 0, result.stderr);
	};
	const pin = (bytes) => {
		const sha256 = digest(bytes);
		write(join(assets, sha256), bytes);
		return { url: 'https://127.0.0.1:1/unused-offline-asset', sha256 };
	};
	write(join(source, 'myst.yml'), 'version: 1\n');
	write(join(source, 'lua/louiselm/init.lua'), 'return {}\n');
	write(join(source, 'docs/tutorial.md'), '# Tutorial\n');
	write(join(source, 'demo/sample/README.md'), '# Sample\n');
	for (const file of ['index.html', 'demo.css', 'bootstrap.js']) write(join(source, 'demo', file), file);
	const runtime = join(root, 'runtime');
	for (const file of ['msgpack.js', 'msgpackrpc.js', 'nvim-worker.js', 'nvim.data', 'nvim.js', 'nvim.wasm', 'rpc.js']) {
		write(join(runtime, file), file);
	}
	write(join(runtime, 'app.js'), 'new WorkerTransport("nvim-worker.js");');
	command('python3', ['-m', 'zipfile', '-c', join(root, 'nvim.zip'), ...readdirSync(runtime)], runtime);
	const neovim = pin(readFileSync(join(root, 'nvim.zip')));
	neovim.url = 'https://gitlab.bartab.fr/api/v4/projects/163/packages/generic/neovim-wasm/fixture/nvim.zip';
	const license = pin(Buffer.from('Neovim fixture license\n'));
	neovim.license_url = license.url;
	neovim.license_sha256 = license.sha256;
	const archive = (name, files) => {
		for (const [path, bytes] of Object.entries(files)) write(join(root, name, path), bytes);
		const path = join(root, `${name}.tgz`);
		command('tar', ['-czf', path, '-C', root, name], root);
		return pin(readFileSync(path));
	};
	const msgpackr = {
		...archive('package', { 'dist/index.js': 'export {};', LICENSE: 'msgpackr license' }),
		browser_path: 'package/dist/index.js', license_path: 'package/LICENSE',
		license_sha256: digest('msgpackr license'),
	};
	const vendor = (name, files) => ({
		...archive(name, { ...files, LICENSE: `${name} license` }),
		archive_prefix: name, license_path: `${name}/LICENSE`,
		license_sha256: digest(`${name} license`), version: 'fixture', commit: 'fixture',
	});
	const lock = {
		neovim, msgpackr,
		snacks: vendor('snacks', {
			'lua/snacks/util/init.lua': 'M.is_win = jit.os:find("Windows")',
			'lua/snacks/picker/util/init.lua': 'local is_win = jit.os:find("Windows")',
		}),
		which_key: vendor('which-key', { 'lua/which-key/init.lua': 'return {}' }),
	};
	const saveLock = () => write(join(source, 'demo/assets.lock.json'), JSON.stringify(lock));
	saveLock();
	const build = (offline = true) => spawnSync(process.execPath, [
		script, source, output, ...(offline ? ['--offline-assets', assets] : []),
	], {
		env: { PATH: process.env.PATH, TMPDIR: root },
		encoding: 'utf8', timeout: 10000,
	});
	return { root, source, output, assets, lock, saveLock, build, license };
}

test('offline build consumes every pinned archive without credentials', (t) => {
	const f = fixture(t);
	const result = f.build();
	assert.equal(result.status, 0, result.stderr);
	const demo = join(f.output, 'demo');
	assert.match(readFileSync(join(demo, 'runtime/app.js'), 'utf8'), /runtime\/nvim-worker.js/);
	assert.match(readFileSync(join(demo, 'bundle.js'), 'utf8'), /# Tutorial/);
	assert.match(readFileSync(join(demo, 'vendor-bundle.js'), 'utf8'), /vim\.fn\.has/);
	const manifest = JSON.parse(readFileSync(join(demo, 'asset-manifest.json'), 'utf8'));
	assert.deepEqual(manifest.assets, f.lock);
	for (const [path, identity] of Object.entries(manifest.files)) {
		const bytes = readFileSync(join(demo, path));
		assert.equal(identity.sha256, digest(bytes), path);
		assert.equal(identity.size, bytes.length, path);
	}
});

for (const asset of ['neovim', 'license', 'msgpackr', 'snacks', 'which_key']) {
	for (const damage of ['missing', 'changed']) {
		test(`offline ${asset} ${damage} refuses without falling back to a download`, (t) => {
			const f = fixture(t);
			const selected = asset === 'license' ? f.license : f.lock[asset];
			const path = join(f.assets, selected.sha256);
			if (damage === 'missing') rmSync(path);
			else writeFileSync(path, 'changed bytes');
			const result = f.build();
			assert.equal(result.status, 1, result.stderr);
			assert.match(result.stderr, damage === 'missing' ? /offline asset unavailable/ : /checksum mismatch/);
			assert.doesNotMatch(result.stderr, /requires CI_JOB_TOKEN|could not download/);
		});
	}
}

test('offline digest cannot select a path outside the asset directory', (t) => {
	const f = fixture(t);
	f.lock.neovim.sha256 = '../outside';
	f.saveLock();
	const result = f.build();
	assert.equal(result.status, 1, result.stderr);
	assert.match(result.stderr, /invalid asset digest/);
});

for (const kind of ['symlink', 'directory']) {
	test(`offline asset refuses a ${kind}`, (t) => {
		const f = fixture(t);
		const path = join(f.assets, f.lock.neovim.sha256);
		const bytes = readFileSync(path);
		rmSync(path);
		if (kind === 'directory') mkdirSync(path);
		else {
			const outside = join(f.root, 'outside');
			writeFileSync(outside, bytes);
			symlinkSync(outside, path);
		}
		const result = f.build();
		assert.equal(result.status, 1, result.stderr);
		assert.match(result.stderr, /offline asset unavailable/);
	});
}

test('unset offline option preserves ordinary package authentication requirement', (t) => {
	const f = fixture(t);
	const result = f.build(false);
	assert.equal(result.status, 1, result.stderr);
	assert.match(result.stderr, /Neovim package download requires CI_JOB_TOKEN or LOUISELM_DEMO_PACKAGE_TOKEN/);
});
