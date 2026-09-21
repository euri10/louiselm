#!/usr/bin/env node
import assert from 'node:assert/strict';
import { existsSync, readFileSync } from 'node:fs';
import { runInNewContext } from 'node:vm';

const firebase = JSON.parse(readFileSync('firebase.json', 'utf8'));
const lock = JSON.parse(readFileSync('demo/assets.lock.json', 'utf8'));
const html = readFileSync('demo/index.html', 'utf8');
const bootstrap = readFileSync('demo/bootstrap.js', 'utf8');
const buildDemo = readFileSync('scripts/build-demo.mjs', 'utf8');
const readme = readFileSync('README.md', 'utf8');
const tutorial = readFileSync('docs/tutorial.md', 'utf8');
const onboarding = readFileSync('docs/onboarding.md', 'utf8');
const myst = readFileSync('myst.yml', 'utf8');
const publicArtifact = readFileSync('scripts/check-public-artifact', 'utf8');
const promotionPath = 'docs/demo-promotion.md';
assert.ok(existsSync(promotionPath), 'demo promotion checklist must exist');
const promotion = existsSync(promotionPath) ? readFileSync(promotionPath, 'utf8') : '';
const ci = readFileSync('.gitlab-ci.yml', 'utf8');
const packageJson = JSON.parse(readFileSync('package.json', 'utf8'));
assert.match(ci, /to-be-continuous\/playwright\/gitlab-ci-playwright@[0-9a-f]{40}/);
assert.ok(ci.includes(`playwright:v${packageJson.devDependencies['@playwright/test']}-noble@sha256:`),
	'CI browser image must match the exact Playwright Test version');
const browserJob = ci.slice(ci.indexOf('\nplaywright:\n'), ci.indexOf('\nsite-deploy:\n'));
assert.match(browserJob, /stage: test/);
assert.match(browserJob, /job: site-build\n\s+artifacts: true/);
assert.match(browserJob, /allow_failure: false/);
assert.match(browserJob, /if: '\$CI_COMMIT_BRANCH \|\| \$CI_MERGE_REQUEST_ID'/);
const deployJob = ci.slice(ci.indexOf('\nsite-deploy:\n'));
assert.match(deployJob, /job: playwright\n\s+artifacts: false/,
	'deployment must wait for the browser gate');

const demoHeaders = (firebase.hosting.headers ?? []).find((entry) => entry.source === '/demo/**');
assert(demoHeaders, 'Firebase must define headers for /demo/**');
const headers = Object.fromEntries(demoHeaders.headers.map(({ key, value }) => [key.toLowerCase(), value]));
assert.equal(headers['cross-origin-opener-policy'], 'same-origin');
assert.equal(headers['cross-origin-embedder-policy'], 'require-corp');

assert.match(lock.neovim.commit, /^[0-9a-f]{40}$/);
assert.equal(
	lock.neovim.url,
	`https://gitlab.bartab.fr/api/v4/projects/163/packages/generic/neovim-wasm/${lock.neovim.commit}/nvim-wasm-emscripten.zip`,
);
assert.equal(
	lock.neovim.upstream_url,
	`https://api.github.com/repos/neovim/neovim/releases/assets/${lock.neovim.upstream_asset_id}`,
);
assert.match(lock.neovim.sha256, /^[0-9a-f]{64}$/);
assert.match(lock.msgpackr.url, /msgpackr-\d+\.\d+\.\d+\.tgz$/);
assert.match(lock.msgpackr.sha256, /^[0-9a-f]{64}$/);
for (const name of ['snacks', 'which_key']) {
	assert.ok(lock[name], `${name} must be pinned`);
	assert.match(lock[name].commit, /^[0-9a-f]{40}$/);
	assert.match(lock[name].url, new RegExp(`/folke/${name === 'which_key' ? 'which-key' : name}\\.nvim/tar\\.gz/${lock[name].commit}$`));
	assert.match(lock[name].sha256, /^[0-9a-f]{64}$/);
	assert.equal(lock[name].license, 'Apache-2.0');
	assert.match(lock[name].license_sha256, /^[0-9a-f]{64}$/);
}

assert(!/<script[^>]+src=["']https?:/i.test(html), 'demo scripts must be self-hosted');
assert(!/<link[^>]+href=["']https?:/i.test(html), 'demo styles must be self-hosted');
assert.equal((html.match(/<script\s+src=/g) ?? []).length, 1, 'fallback must load only the bootstrap');
assert.match(html, /id="guide-step"/);
assert.match(html, /id="type-action"/);
assert.match(html, /id="skip-step"/);
assert.match(html, /id="reset-demo"/);
assert.match(html, /id="fallback-panel"/);
assert.match(html, /id="completion-install-link"[^>]+-primary/);
assert.match(html, /id="fallback-install-link"/);
assert.match(html, /id="profile-enhanced"/);
assert.match(html, /id="profile-core"/);
assert.match(bootstrap, /LouiseLM 交互式演示/);
assert.match(bootstrap, /The guided Agent behavior is scripted/);
assert.match(bootstrap, /LouiselmHandOff/);
assert.match(bootstrap, /LouiselmSessionSwitch/);
assert.match(bootstrap, /LouiselmResume/);
assert.match(bootstrap, /LouiselmDemoLanguage/);
assert.match(bootstrap, /project_root = "\/demo-project", language = language/);
assert.match(bootstrap, /loadScript/);
assert.match(bootstrap, /vendor-bundle\.js/);
assert.match(bootstrap, /state\.profile === 'enhanced'\) scripts\.push\('vendor-bundle\.js'\)/);
assert.match(bootstrap, /state\.profile === 'enhanced'\) groups\.push\(globalThis\.LOUISELM_DEMO_VENDOR\?\.files\)/);
assert.match(bootstrap, /URLSearchParams/);
assert.match(bootstrap, /profile:\s*'enhanced'/);
assert.match(bootstrap, /which-key/);
assert.match(bootstrap, /snacks/);
assert.doesNotMatch(bootstrap, /vim\.(?:system|loop\.spawn)|vim\.fn\.(?:executable|system)/);
assert.match(buildDemo, /replace two LuaJIT-only Windows checks with vim\.fn\.has for Neovim WASM/);
assert.match(buildDemo, /CI_JOB_TOKEN/);
assert.match(buildDemo, /LOUISELM_DEMO_PACKAGE_TOKEN/);
assert(!bootstrap.includes('localStorage'), 'demo progress must not persist');

assert.match(readme, /\[Try LouiseLM in your browser\]\((?:https:\/\/louiselm\.com)?\/demo\/\)/);
assert.match(tutorial, /\[Try LouiseLM in\s+your browser\]\(https:\/\/louiselm\.com\/demo\/\)/);
assert.match(onboarding, /\[try LouiseLM in your\s+browser\]\(https:\/\/louiselm\.com\/demo\/\)/i);
assert.match(myst, /docs\/demo-promotion\.md/);
assert.match(publicArtifact, /demo-promotion/);
for (const profile of ['Core', 'Enhanced']) {
	for (const language of ['English', 'Simplified Chinese']) {
		assert.match(promotion, new RegExp(`\\| ${profile} \\| ${language} \\|`));
	}
}
for (const name of ['neovim', 'snacks', 'which_key']) {
	assert.match(promotion, new RegExp(lock[name].commit));
	assert.match(promotion, new RegExp(lock[name].sha256));
}
for (const check of ['credentials', 'persistence', 'client analytics', 'prompt egress', 'host filesystem']) {
	assert.match(promotion, new RegExp(check, 'i'));
}

// Exercise the shipped bootstrap while its first runtime script is loading,
// then fail that request. Neither state has a chat in which visitors can type.
const nodes = new Map();
for (const id of ['guide-step', 'type-action', 'skip-step']) {
	assert.match(html, new RegExp(`id="${id}"[^>]*\\bhidden\\b`), `${id} must be hidden before JavaScript runs`);
}
const node = (id) => {
	if (!nodes.has(id)) nodes.set(id, {
		hidden: false, dataset: {}, textContent: '',
		addEventListener() {}, setAttribute() {}, replaceChildren() {},
		classList: { add() {} },
	});
	return nodes.get(id);
};
let pendingScript;
const browser = {
	document: {
		querySelector: node, getElementById: node, querySelectorAll: () => [],
		documentElement: {}, dispatchEvent() {}, createElement: () => ({}),
		body: { append(script) { pendingScript = script; } },
	},
	window: { innerWidth: 1440 }, location: { search: '' },
	crossOriginIsolated: true, URLSearchParams,
	CustomEvent: class {}, console: { error() {} }, clearTimeout,
};
runInNewContext(bootstrap, browser);
for (const id of ['guide-step', 'type-action', 'skip-step']) {
	assert.equal(node(id).hidden, true, `${id} must be hidden before chat is ready`);
}
pendingScript.onerror();
await new Promise(setImmediate);
assert.equal(browser.__louiselmDemo.phase, 'failed');
assert.match(node('demo-status').textContent, /Could not start the demo/);
for (const id of ['guide-step', 'type-action', 'skip-step']) {
	assert.equal(node(id).hidden, true, `${id} must remain hidden after startup fails`);
}

console.log('demo configuration tests passed');
