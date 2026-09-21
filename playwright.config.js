const { defineConfig } = require('@playwright/test');

module.exports = defineConfig({
	testDir: './scripts',
	testMatch: 'test-demo-layout.js',
	forbidOnly: !!process.env.CI,
	workers: 1,
	timeout: 120_000,
	outputDir: 'reports/playwright-results',
	reporter: [['list'], ['junit', { outputFile: 'reports/playwright.xunit.xml' }]],
	use: {
		browserName: 'chromium',
		baseURL: 'http://127.0.0.1:8765',
		viewport: { width: 1440, height: 900 },
		trace: 'retain-on-failure',
		screenshot: 'only-on-failure',
	},
	webServer: {
		command: 'node scripts/serve-demo.mjs',
		url: 'http://127.0.0.1:8765/demo/',
		reuseExistingServer: false,
		timeout: 10_000,
	},
});
