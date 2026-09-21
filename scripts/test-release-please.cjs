// Uses the action's exact locked Release Please library with in-memory GitHub
// history/content fixtures. No network, credentials, PRs, or tags are created.
const assert = require('node:assert/strict');
const {readFileSync, mkdtempSync, rmSync, existsSync} = require('node:fs');
const {tmpdir} = require('node:os');
const {join} = require('node:path');
const {spawnSync} = require('node:child_process');
const {test} = require('node:test');
const {Manifest} = require('release-please');
const {setLogger} = require('release-please/build/src/util/logger');
setLogger({debug() {}, info() {}, warn() {}, error() {}});

const config = JSON.parse(readFileSync('release-please-config.json'));
const releaseSha = 'b'.repeat(40);
async function proposal(message, files, previous = '0.4.2', history = []) {
  const github = {
    repository: {owner: 'fixture', repo: 'plugin'},
    async getFileJson(path) {
      return path === 'release-please-config.json' ? config : {'.': previous};
    },
    async *releaseIterator() {
      if (previous !== '0.0.0') yield {tagName: `plugin-v${previous}`, sha: releaseSha};
    },
    async *tagIterator() {},
    async *mergeCommitIterator(branch, {maxResults}) {
      assert.equal(branch, 'main');
      const commits = [{sha: 'a'.repeat(40), message, files}, ...history];
      if (previous !== '0.0.0') commits.push(
        {sha: releaseSha, message: 'chore: release plugin', files: ['VERSION']},
        {sha: 'c'.repeat(40), message: 'feat: already released', files: ['lua/old.lua']},
      );
      yield* commits.slice(0, maxResults);
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
  assert.match(candidate.body.toString(), /release plugin/);
});

test('first release reaches root beyond 1000 commits and the former audit cutoff', async () => {
  const history = Array.from({length: 1100}, (_, i) => ({
    sha: i.toString(16).padStart(40, '0'), message: 'chore: record work', files: ['README.md'],
  }));
  history.splice(50, 0, {
    sha: 'c905fdafa60370d1a07e6853bba741cce9da9870',
    message: 'fix: change at former cutoff', files: ['lua/old.lua'],
  });
  history.push({sha: 'd'.repeat(40), message: 'feat: earliest plugin feature', files: ['lua/first.lua']});
  const [candidate] = await proposal('fix: recent change', ['lua/x.lua'], '0.0.0', history);
  assert.equal(candidate.version.toString(), '0.1.0');
  const changelog = candidate.updates.find(update => update.path === 'CHANGELOG.md').updater.updateContent('');
  assert.match(changelog, /recent change/);
  assert.match(changelog, /change at former cutoff/);
  assert.match(changelog, /earliest plugin feature/);
});

test('subsequent releases stop at the last real plugin release', async () => {
  const [candidate] = await proposal('fix: recent change', ['lua/x.lua']);
  assert.equal(candidate.version.toString(), '0.4.3');
  assert.match(candidate.body.toString(), /recent change/);
  assert.doesNotMatch(candidate.body.toString(), /already released/);
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
  const pin = 'googleapis/release-please-action@45996ed1f6d02564a971a2fa1b5860e934307cf7';
  assert.equal(require('release-please/package.json').version, '17.6.0');
  assert.ok(ci.on.pull_request);
  assert.ok(ci.jobs['plugin-release-contract']);
  const contractSteps = ci.jobs['plugin-release-contract'].steps;
  assert.equal(contractSteps.find(step => step.uses?.startsWith('actions/setup-node@')).with['node-version'], '24');
  assert.ok(contractSteps.some(step => step.run?.includes(`checkout --quiet ${pin.split('@')[1]}`)));
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
  const settingsToken = steps.find(step => step.id === 'immutability-token');
  assert.equal(settingsToken?.uses, token.uses);
  assert.deepEqual(settingsToken.with, {
    'app-id': '${{ secrets.RELEASE_PLEASE_APP_ID }}',
    'private-key': '${{ secrets.RELEASE_PLEASE_APP_PRIVATE_KEY }}',
    'permission-administration': 'read',
  });
  assert.match(workflow.jobs.release.if, /vars\.PLUGIN_RELEASES_ENABLED == 'true'/);
  assert.equal(steps.find(step => step.uses?.startsWith('actions/checkout@')).with.ref, 'main');
  assert.equal(steps.find(step => step.uses?.startsWith('actions/checkout@')).with['persist-credentials'], false);
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
  for (const step of publicationSteps) {
    assert.ok(steps.indexOf(settingsToken) < steps.indexOf(step));
    assert.equal(step.env.GH_TOKEN, '${{ github.token }}');
    assert.equal(step.env.GH_IMMUTABILITY_TOKEN, '${{ steps.immutability-token.outputs.token }}');
  }
});

test('draft gate executes pagination filtering and fails closed on API errors', () => {
  const yaml = require('js-yaml');
  const workflow = yaml.load(readFileSync('.github/workflows/release-please.yml', 'utf8'));
  const script = workflow.jobs.release.steps.find(step => step.id === 'pending').run;
  const directory = mkdtempSync(join(tmpdir(), 'release-drafts-'));
  try {
    for (const [pages, expected] of [
      [[[]], 'true'],
      [[[{draft: false, tag_name: 'plugin-v0.1.0'}, {draft: true, tag_name: 'capture-v0.1.0'}]], 'true'],
      [[[], [{draft: true, tag_name: 'plugin-v0.1.0'}]], 'false'],
    ]) {
      const output = join(directory, expected + '.output');
      const run = spawnSync('bash', ['-e', '-c', 'gh() { printf "%s\\n" "$RELEASE_PAGES"; }\n' + script], {
        encoding: 'utf8',
        env: {...process.env, REPOSITORY: 'fixture/plugin', RELEASE_PAGES: JSON.stringify(pages), GITHUB_OUTPUT: output},
      });
      assert.equal(run.status, 0, run.stderr);
      assert.equal(readFileSync(output, 'utf8').trim().split('\n').at(-1), `ready=${expected}`);
    }
    for (const api of ['gh() { return 1; }', 'gh() { printf "invalid JSON"; }']) {
      const run = spawnSync('bash', ['-e', '-c', api + '\n' + script], {
        encoding: 'utf8', env: {...process.env, REPOSITORY: 'fixture/plugin', GITHUB_OUTPUT: join(directory, 'failure.output')},
      });
      assert.notEqual(run.status, 0);
      assert.equal(existsSync(join(directory, 'failure.output')), false);
    }
  } finally {
    rmSync(directory, {recursive: true, force: true});
  }
});
