const { test, expect } = require('@playwright/test');

async function checkLayout(page, width, height) {
  await page.setViewportSize({ width, height });
  // Allow ResizeObserver, RPC and the resulting redraw to settle.
  await page.waitForTimeout(300);
  const result = await page.evaluate(() => {
    const bounds = (selector) => document.querySelector(selector).getBoundingClientRect().toJSON();
    const grid = document.getElementById('grid');
    const range = document.createRange();
    range.selectNodeContents(grid);
    return {
      viewport: [innerWidth, innerHeight],
      document: [document.documentElement.scrollWidth, document.documentElement.scrollHeight],
      guide: bounds('.demo-guide'), editor: bounds('.editor-panel'),
      grid: bounds('#grid'), content: range.getBoundingClientRect().toJSON(),
      clipped: [...document.querySelectorAll('.demo-guide *')].filter((element) => {
        const rect = element.getBoundingClientRect();
        return rect.height > 0 && (element.scrollHeight > element.clientHeight + 1 ||
          rect.bottom > document.querySelector('.demo-guide').getBoundingClientRect().bottom);
      }).map((element) => element.id || element.className),
    };
  });
  const check = (ok, message) => { if (!ok) throw new Error(`${message}: ${JSON.stringify(result)}`); };
  check(result.document[1] <= height + 1, 'document scrolls vertically');
  check(result.document[0] <= width + 1, 'document scrolls horizontally');
  check(result.guide.bottom <= result.editor.top, 'guide must be above Neovim');
  check(result.editor.bottom <= height + 1, 'terminal bottom is outside viewport');
  check(result.editor.height > height / 2, 'Neovim must receive most of the viewport');
  check(result.content.bottom <= result.grid.bottom + 1, 'rendered Neovim rows overflow');
  check(result.content.right <= result.grid.right + 1, 'rendered Neovim columns overflow');
  check(result.clipped.length === 0, 'guide content is clipped');
  check(result.content.width > 0 && result.content.height > 0, 'Neovim grid must render content');
}

for (const profile of ['core', 'enhanced']) {
  for (const language of ['en', 'zh-CN']) {
    test(`${profile} / ${language}: every guide state fits at desktop sizes`, async ({ page }) => {
      const response = await page.goto(`/demo/?profile=${profile}&language=${language}`);
      expect(response.headers()['cross-origin-opener-policy']).toBe('same-origin');
      expect(response.headers()['cross-origin-embedder-policy']).toBe('require-corp');
      await page.waitForFunction(() => ['ready', 'failed'].includes(globalThis.__louiselmDemo?.phase), null, { timeout: 45000 });
      const state = await page.evaluate(() => globalThis.__louiselmDemo);
      expect(state.phase, state.error || 'demo startup').toBe('ready');
      expect(state.initialBuffer).toMatch(/^louiselm:\/\/demo-/);
      // Freeze automatic progress timers so each guide step can be measured at
      // every size; Skip still drives the real renderer and step-entry actions.
      await page.clock.install({ time: new Date(0) });
      await page.clock.pauseAt(new Date(60_000));
      // Exercise real Skip/rendering, including the longest instruction and completion.
      for (let expected = 0; expected <= 10; expected += 1) {
        const step = await page.evaluate(() => globalThis.__louiselmDemo.step);
        if (step !== expected) throw new Error(`Expected guide step ${expected}, got ${step}`);
        for (const [width, height] of [[1920, 1030], [1440, 900], [1280, 720]]) {
          await test.step(`guide ${expected + 1}/11 at ${width}x${height}`, async () => {
            await checkLayout(page, width, height);
          });
        }
        if (expected < 10) await page.locator('#skip-step').click();
      }
      await expect(page.locator('#completion-panel')).toBeVisible();
    });
  }
}
