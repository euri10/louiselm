// Uses the action's exact locked Release Please library with in-memory GitHub
// history/content fixtures. No network, credentials, PRs, or tags are created.
const assert = require('node:assert/strict');
const {readFileSync} = require('node:fs');
const {test} = require('node:test');
const {Manifest} = require('release-please');
const {setLogger} = require('release-please/build/src/util/logger');
setLogger({debug() {}, info() {}, warn() {}, error() {}});

const config = JSON.parse(readFileSync('release-please-config.json'));
const baseline = config['bootstrap-sha'];
async function proposal(message, files, previous = '0.4.2') {
  const github = {
    repository: {owner: 'fixture', repo: 'plugin'},
    async getFileJson(path) {
      return path === 'release-please-config.json' ? config : {'.': previous};
    },
    async *releaseIterator() {},
    async *tagIterator() {},
    async *mergeCommitIterator() {
      yield {sha: 'a'.repeat(40), message, files};
      yield {sha: baseline, message: 'feat: excluded history', files: ['lua/old.lua']};
    },
  };
  const manifest = await Manifest.fromManifest(github, 'main');
  return manifest.buildPullRequests();
}

test('excluded components and tracker/site directories never bump plugin', async () => {
  for (const directory of config.packages['.']['exclude-paths']) {
    assert.deepEqual(await proposal('feat!: other component', [`${directory}/file`]), [], directory);
  }
  assert.deepEqual(await proposal('build(site): update tooling', ['package.json']), []);
  assert.deepEqual(await proposal('chore: update receiver installer', ['scripts/install-capture-service']), []);
});

test('plugin and intentional shared changes select patch/minor, never automatic 1.0', async () => {
  for (const [message, version] of [
    ['fix: correct initialization', '0.4.3'],
    ['feat: add capability', '0.5.0'],
    ['feat!: change interface\n\nBREAKING CHANGE: update the enabled capture companion.', '0.5.0'],
  ]) {
    const [candidate] = await proposal(message, ['lua/louiselm/acp/init.lua', 'capture-service/src/lib.rs']);
    assert.equal(candidate.version.toString(), version);
    assert.match(candidate.headRefName, /components--plugin$/);
    assert.match(candidate.body.toString(), new RegExp(version.replaceAll('.', '\\.')));
    if (message.includes('BREAKING')) assert.match(candidate.body.toString(), /update the enabled capture companion/);
  }
  assert.equal((await proposal('fix: explain installation', ['README.md']))[0].version.toString(), '0.4.3');
  assert.equal((await proposal('feat!: alpha breaking change', ['lua/x.lua'], '0.99.9'))[0].version.toString(), '0.100.0');
});

test('first release is explicit and projection survives the real updaters', async () => {
  const [candidate] = await proposal('feat: release plugin', ['lua/x.lua'], '0.0.0');
  assert.equal(candidate.version.toString(), '0.1.0');
  const updated = Object.fromEntries(candidate.updates.map(update => [
    update.path, update.updater.updateContent(readFileSync(update.path, 'utf8')),
  ]));
  assert.equal(updated.VERSION.trim(), '0.1.0');
  assert.equal(JSON.parse(updated['.release-please-manifest.json'])['.'], '0.1.0');
  assert.equal(updated['lua/louiselm/version.lua'], readFileSync('lua/louiselm/version.lua', 'utf8').replace(/version = "[^"]+"/, 'version = "0.1.0"'));
  assert.match(updated['CHANGELOG.md'], /0\.1\.0/);
  assert.doesNotMatch(updated['CHANGELOG.md'], /excluded history/);
  assert.match(candidate.body.toString(), /release plugin/);
});

test('merged PR prepares a draft at the exact approved SHA without an early tag', async () => {
  const [proposalPr] = await proposal('feat: release plugin', ['lua/x.lua'], '0.0.0');
  const sha = 'c'.repeat(40);
  const github = {
    repository: {owner: 'fixture', repo: 'plugin'},
    async getFileJson(path) {
      return path === 'release-please-config.json' ? config : {'.': '0.1.0'};
    },
    async *pullRequestIterator() {
      yield {
        number: 4, sha, title: proposalPr.title.toString(), body: proposalPr.body.toString(),
        headBranchName: proposalPr.headRefName, baseBranchName: 'main', labels: ['autorelease: pending'],
      };
    },
  };
  const manifest = await Manifest.fromManifest(github, 'main');
  const [candidate] = await manifest.buildReleases();
  assert.equal(candidate.sha, sha);
  assert.equal(candidate.tag.toString(), 'plugin-v0.1.0');
  assert.equal(candidate.draft, true);
  assert.notEqual(candidate.forceTag, true);
  assert.equal(candidate.prerelease, true);
});

test('workflow retains ordinary bot-PR CI and guards publication separately', () => {
  const yaml = require('js-yaml'); // already in the pinned tool's lockfile
  const workflow = yaml.load(readFileSync('.github/workflows/release-please.yml', 'utf8'));
  const ci = yaml.load(readFileSync('.github/workflows/ci.yml', 'utf8'));
  const pin = 'googleapis/release-please-action@5c625bfb5d1ff62eadeeb3772007f7f66fdcf071';
  assert.equal(require('release-please/package.json').version, '17.3.0');
  assert.ok(ci.on.pull_request);
  assert.ok(ci.jobs['plugin-release-contract']);
  assert.equal(ci.permissions.contents, 'read');
  assert.deepEqual(workflow.on.workflow_run.workflows, ['CI']);
  assert.equal(workflow.concurrency['cancel-in-progress'], false);
  const steps = workflow.jobs.release.steps;
  const token = steps.find(step => step.id === 'release-token');
  assert.equal(token?.uses, 'actions/create-github-app-token@bcd2ba49218906704ab6c1aa796996da409d3eb1');
  assert.deepEqual(token.with, {
    'app-id': '${{ secrets.RELEASE_PLEASE_APP_ID }}',
    'private-key': '${{ secrets.RELEASE_PLEASE_APP_PRIVATE_KEY }}',
    'permission-contents': 'write',
    'permission-issues': 'write',
    'permission-pull-requests': 'write',
  }); // Default token scope is this repository; default cleanup revokes it.
  assert.match(workflow.jobs.release.if, /vars\.PLUGIN_RELEASES_ENABLED == 'true'/);
  assert.equal(steps.find(step => step.uses?.startsWith('actions/checkout@')).with.ref, 'main');
  const actions = steps.filter(step => step.uses?.startsWith('googleapis/release-please-action@'));
  assert.equal(actions.length, 2);
  for (const action of actions) {
    assert.equal(action.uses, pin);
    assert.equal(action.with.token, '${{ steps.release-token.outputs.token }}');
    assert.ok(steps.indexOf(token) < steps.indexOf(action));
    assert.equal(action.with['target-branch'], 'main');
  }
  assert.equal(actions[0].with['skip-github-pull-request'], true);
  assert.equal(actions[1].with['skip-github-release'], true);
  assert.equal(actions[1].if, "steps.pending.outputs.ready == 'true'");
  const publicationSteps = steps.filter(step => step.run?.includes('publish-plugin-release.py'));
  assert.equal(publicationSteps.length, 2);
  assert.ok(steps.indexOf(publicationSteps[0]) < steps.indexOf(actions[0]));
  assert.ok(steps.indexOf(publicationSteps[1]) < steps.indexOf(actions[1]));
  for (const step of publicationSteps) assert.equal(step.env.GH_TOKEN, '${{ github.token }}');
});
