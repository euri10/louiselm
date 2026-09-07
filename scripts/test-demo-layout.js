// Run with playwright-cli run-code --filename=scripts/test-demo-layout.js
// after opening an isolated, built /demo/ route; see docs/demo-promotion.md.
async (page) => {
  const measurements = [];
  const base = page.url().split('?')[0];
  async function checkLayout(width, height) {
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
    return { viewport: result.viewport, guide: result.guide.height, editor: result.editor.height };
  }
  for (const profile of ['core', 'enhanced']) {
    for (const language of ['en', 'zh-CN']) {
      await page.goto(`${base}?profile=${profile}&language=${language}`);
      await page.waitForFunction(() => globalThis.__louiselmDemo?.phase === 'ready', null, { timeout: 45000 });
      // Freeze automatic progress timers so each guide step can be measured at
      // every size; Skip still drives the real renderer and step-entry actions.
      await page.clock.install();
      await page.clock.pauseAt(await page.evaluate(() => Date.now()));
      // Exercise real Skip/rendering, including the longest instruction and completion.
      const steps = [];
      for (let expected = 0; expected <= 10; expected += 1) {
        const step = await page.evaluate(() => globalThis.__louiselmDemo.step);
        if (step !== expected) throw new Error(`Expected guide step ${expected}, got ${step}`);
        const sizes = [];
        for (const [width, height] of [[1920, 1030], [1440, 900], [1280, 720]]) {
          sizes.push(await checkLayout(width, height));
        }
        steps.push({ step, sizes });
        if (expected < 10) await page.locator('#skip-step').click();
      }
      await page.clock.resume();
      measurements.push({ profile, language, steps });
    }
  }
  return measurements;
}
