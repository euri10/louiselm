(() => {
	'use strict';

	const root = '/home/user/.local/share/nvim/site/pack/louiselm/start/louiselm.nvim';
	const elements = {
		shell: document.querySelector('.demo-shell'),
		grid: document.getElementById('grid'),
		runtimeStatus: document.getElementById('status'),
		demoStatus: document.getElementById('demo-status'),
		eyebrow: document.getElementById('demo-eyebrow'),
		title: document.getElementById('demo-title'),
		summary: document.getElementById('demo-summary'),
		profileTitle: document.getElementById('profile-title'),
		profileEnhancedTitle: document.getElementById('profile-enhanced-title'),
		profileEnhancedBody: document.getElementById('profile-enhanced-body'),
		profileCoreTitle: document.getElementById('profile-core-title'),
		profileCoreBody: document.getElementById('profile-core-body'),
		profileNote: document.getElementById('profile-note'),
		statusLabel: document.getElementById('status-label'),
		completionInstallLink: document.getElementById('completion-install-link'),
		fallbackInstallLink: document.getElementById('fallback-install-link'),
		step: document.getElementById('guide-step'),
		stepProgress: document.getElementById('step-progress'),
		stepTitle: document.getElementById('step-title'),
		stepBody: document.getElementById('step-body'),
		stepAction: document.getElementById('step-action'),
		stepCheck: document.getElementById('step-check'),
		typeAction: document.getElementById('type-action'),
		skipStep: document.getElementById('skip-step'),
		resetDemo: document.getElementById('reset-demo'),
		completion: document.getElementById('completion-panel'),
		completionTitle: document.getElementById('completion-title'),
		completionBody: document.getElementById('completion-body'),
		replayDemo: document.getElementById('replay-demo'),
		replayOtherProfile: document.getElementById('replay-other-profile'),
		fallback: document.getElementById('fallback-panel'),
		fallbackTitle: document.getElementById('fallback-title'),
		fallbackBody: document.getElementById('fallback-body'),
		fallbackList: document.getElementById('fallback-list'),
		tutorLink: document.getElementById('tutor-link'),
	};

	const COPY = {
		en: {
			eyebrow: 'Experimental browser demo',
			title: 'LouiseLM Interactive Demo',
			summary: 'Real Neovim and LouiseLM, running locally in this tab. The guided Agent behavior is scripted.',
			profileTitle: 'Choose the Neovim experience',
			profileEnhancedTitle: 'Enhanced',
			profileEnhancedBody: 'Searchable Snacks pickers and which-key hints. Preselected.',
			profileCoreTitle: 'Core',
			profileCoreBody: 'LouiseLM with Neovim’s native numbered selectors.',
			profileNote:
				'Snacks and which-key are pinned, self-hosted demo extras—not LouiseLM dependencies. Changing profile restarts the tour.',
			replayOther: 'Replay in {profile}',
			statusLabel: 'Status',
			install: 'Install LouiseLM locally',
			type: 'Type it for me',
			firstPrompt: 'Find and fix the calculator bug.',
			retryPrompt: 'Try the calculator fix again.',
			scriptedProposal: 'I inspected the attached calculator',
			scriptedRejected: 'No file changed.',
			scriptedHandoff: 'Handoff received.',
			skip: 'Skip',
			reset: 'Reset',
			replay: 'Replay from the start',
			completionTitle: 'You completed the LouiseLM tour',
			completionBody:
				'You used real Session, permission, Resume, limit, and Handoff UI—without installing anything.',
			fallbackTitle: 'Read the static tour',
			fallbackBody:
				'The interactive demo needs a desktop browser, cross-origin isolation, and a physical keyboard.',
			fallbackItems: [
				'Prompt a named Agent Session with attached editor context.',
				'Review and accept or reject a proposed file edit.',
				'Resume, switch, inspect limits, and review a Handoff.',
			],
			tutor: 'Open the static Tutor',
			statuses: {
				booting: 'Starting Neovim WebAssembly…',
				installing: 'Loading LouiseLM and the disposable project…',
				ready: 'Ready—Neovim {version} is running locally in this tab.',
				failed: 'Could not start the demo: {error}',
				unsupported: 'Interactive runtime unavailable; the static tour is ready below.',
			},
			steps: [
				{
					title: 'Ask about the bug',
					body: 'A disposable Lua project is already attached to your-codex-here.',
					action: 'Focus Neovim, press i, type anything about the bug, then press Enter.',
					check: 'Waiting for the real diff review…',
				},
				{
					title: 'Reject the first edit',
					body: 'This is LouiseLM’s real permission-backed diff buffer. The Agent cannot edit until you decide.',
					action: 'Press d to reject. The file must remain unchanged.',
					check: 'Waiting for a rejection with no file change…',
				},
				{
					title: 'Retry the turn',
					body: 'Rejection returns control to the same Session; no hidden action was applied.',
					action: 'At the prompt, type another non-empty request and press Enter.',
					check: 'Waiting for the second diff review…',
				},
				{
					title: 'Accept the reviewed edit',
					body: 'The proposal is still one visible line: subtraction becomes addition.',
					action: 'Press a. The accepted edit will change the in-memory file.',
					check: 'Waiting for calculator.lua to contain left + right…',
				},
				{
					title: 'Resume a saved Session',
					body: 'Resume discovers Agent-side history and replays it into a separate LouiseLM buffer.',
					action: 'Run :LouiselmResume, choose the seeded Session, then press Enter.',
					check: 'Waiting for source=loaded and replayed turns…',
				},
				{
					title: 'Create a Session on another Agent',
					body: 'Sessions are independent. Agent names choose which configured adapter owns each one.',
					action: 'Run :LouiselmSessionNew, choose your-claude-here, then press Enter.',
					check: 'Waiting for a your-claude-here Session…',
				},
				{
					title: 'Switch back without losing either Session',
					body: 'Switching changes the active buffer; it does not flatten or discard either transcript.',
					action: 'Run :LouiselmSessionSwitch and choose the original Fix calculator Session.',
					check: 'Waiting for the original Session…',
				},
				{
					title: 'Inspect a simulated Agent limit',
					body: 'The demo has now published a local reached-limit event for your-codex-here.',
					action: 'Run :LouiselmLimits to inspect the normal account-limit buffer.',
					check: 'Waiting for the reached rate_limit detail…',
				},
				{
					title: 'Review a Handoff to another Agent',
					body: 'A Handoff creates a target Session, then lets you edit the compact transcript and takeover task.',
					action:
						'Run :LouiselmHandOff, choose your-claude-here, replace the takeover-task placeholder, then press Ctrl-S.',
					check: 'Waiting for the reviewed Handoff to reach its target Session…',
				},
				{
					title: 'Return to the original Session',
					body: 'The Handoff target remains separate and the source Session is still available.',
					action: 'Run :LouiselmSessionSwitch and choose Fix calculator.',
					check: 'Waiting for the original Session one last time…',
				},
			],
		},
		'zh-CN': {
			eyebrow: '实验性浏览器演示',
			title: 'LouiseLM 交互式演示',
			summary: '真正的 Neovim 与 LouiseLM 在此标签页本地运行；引导中的 Agent 行为由脚本驱动。',
			profileTitle: '选择 Neovim 体验',
			profileEnhancedTitle: '增强版',
			profileEnhancedBody: '可搜索的 Snacks 选择器与 which-key 按键提示；默认选中。',
			profileCoreTitle: '核心版',
			profileCoreBody: 'LouiseLM 配合 Neovim 原生编号选择器。',
			profileNote: 'Snacks 与 which-key 是固定版本、自托管的演示附加项，并非 LouiseLM 依赖。切换体验会重新开始导览。',
			replayOther: '使用{profile}重新导览',
			statusLabel: '状态',
			install: '在本机安装 LouiseLM',
			type: '帮我输入',
			firstPrompt: '查找并修复计算器错误。',
			retryPrompt: '请再次尝试修复计算器。',
			scriptedProposal: '我检查了已附加的计算器',
			scriptedRejected: '文件没有变化。',
			scriptedHandoff: '已收到 Handoff。',
			skip: '跳过',
			reset: '重置',
			replay: '从头再来',
			completionTitle: '你已完成 LouiseLM 导览',
			completionBody: '你使用了真实的 Session、权限、Resume、限额与 Handoff 界面，无需安装任何内容。',
			fallbackTitle: '阅读静态导览',
			fallbackBody: '交互式演示需要支持跨源隔离的桌面浏览器和实体键盘。',
			fallbackItems: [
				'向带编辑器上下文的命名 Agent Session 提交提示。',
				'审查、接受或拒绝文件修改。',
				'恢复与切换 Session、查看限额并审查 Handoff。',
			],
			tutor: '打开静态 Tutor',
			statuses: {
				booting: '正在启动 Neovim WebAssembly…',
				installing: '正在加载 LouiseLM 与一次性项目…',
				ready: '已就绪——Neovim {version} 正在此标签页本地运行。',
				failed: '演示启动失败：{error}',
				unsupported: '无法启动交互式运行时；请阅读下方静态导览。',
			},
			steps: [
				{
					title: '询问代码错误',
					body: '一次性 Lua 项目已附加到 your-codex-here。',
					action: '聚焦 Neovim，按 i，输入任何与错误有关的内容，然后按 Enter。',
					check: '正在等待真实的差异审查…',
				},
				{
					title: '拒绝第一次修改',
					body: '这是 LouiseLM 真实的权限差异缓冲区；在你决定前，Agent 无法修改文件。',
					action: '按 d 拒绝；文件必须保持不变。',
					check: '正在等待拒绝且文件不变…',
				},
				{
					title: '重试这一轮',
					body: '拒绝后会回到同一 Session，任何隐藏修改都不会发生。',
					action: '在提示行输入另一条非空请求，然后按 Enter。',
					check: '正在等待第二次差异审查…',
				},
				{
					title: '接受审查后的修改',
					body: '提案仍只有一行：把减法改为加法。',
					action: '按 a；接受后会修改内存文件。',
					check: '正在等待 calculator.lua 出现 left + right…',
				},
				{
					title: '恢复已保存的 Session',
					body: 'Resume 会发现 Agent 端历史，并在独立的 LouiseLM 缓冲区中重放。',
					action: '运行 :LouiselmResume，选择预置 Session，然后按 Enter。',
					check: '正在等待 source=loaded 与重放内容…',
				},
				{
					title: '在另一个 Agent 上创建 Session',
					body: '各 Session 相互独立；Agent 名称决定由哪个已配置适配器负责。',
					action: '运行 :LouiselmSessionNew，选择 your-claude-here，再按 Enter。',
					check: '正在等待 your-claude-here Session…',
				},
				{
					title: '切回且不丢失 Session',
					body: '切换只改变当前缓冲区，不会合并或丢弃任何对话记录。',
					action: '运行 :LouiselmSessionSwitch，并选择原始 Fix calculator Session。',
					check: '正在等待原始 Session…',
				},
				{
					title: '查看模拟的 Agent 限额',
					body: '演示刚刚为 your-codex-here 发布了本地限额耗尽事件。',
					action: '运行 :LouiselmLimits，查看正常的账户限额缓冲区。',
					check: '正在等待 rate_limit 详情…',
				},
				{
					title: '审查交给另一个 Agent 的 Handoff',
					body: 'Handoff 会创建目标 Session，并允许你编辑压缩后的对话与接管任务。',
					action:
						'运行 :LouiselmHandOff，选择 your-claude-here，替换 takeover task 占位符，然后按 Ctrl-S。',
					check: '正在等待目标 Session 收到已审查的 Handoff…',
				},
				{
					title: '返回原始 Session',
					body: 'Handoff 目标仍然独立，来源 Session 也依然可用。',
					action: '运行 :LouiselmSessionSwitch，并选择 Fix calculator。',
					check: '最后一次等待原始 Session…',
				},
			],
		},
	};

	const state = {
		phase: 'booting',
		error: null,
		version: null,
		language: 'en',
		profile: 'enhanced',
		step: 0,
		initialBuffer: null,
	};
	const requestedProfile = new URLSearchParams(location.search).get('profile');
	if (requestedProfile === 'core') state.profile = 'core';
	const requestedLanguage = new URLSearchParams(location.search).get('language');
	if (requestedLanguage === 'zh-CN') state.language = 'zh-CN';
	globalThis.__louiselmDemo = state;
	const BROKEN_EXPRESSION = 'return left - right';
	const FIXED_EXPRESSION = 'return left + right';
	const stepDefinitions = [
		{ prefill: { kind: 'prompt', value: () => text().firstPrompt }, check: (view) => view.current.startsWith('louiselm-diff://') },
		{ check: (view) => view.file.includes(BROKEN_EXPRESSION) && view.allText.includes(text().scriptedRejected) },
		{
			prefill: { kind: 'prompt', value: () => text().retryPrompt },
			check: (view) => view.current.startsWith('louiselm-diff://') && occurrences(view.allText, text().scriptedProposal) >= 2,
		},
		{ check: (view) => view.file.includes(FIXED_EXPRESSION) },
		{ prefill: { kind: 'command', value: 'LouiselmResume' }, check: (view) => view.currentText.includes('source=loaded') && view.currentText.includes('scripted-resume-1') },
		{ prefill: { kind: 'command', value: 'LouiselmSessionNew' }, check: (view) => view.allText.includes('your-claude-here/scripted-new-') },
		{ prefill: { kind: 'command', value: 'LouiselmSessionSwitch' }, check: (view) => view.current === state.initialBuffer },
		{
			prefill: { kind: 'command', value: 'LouiselmLimits' },
			enter: () => requestLua('vim.cmd.LouiselmDemoLimit(); return true'),
			check: (view) => view.current.startsWith('louiselm://limits/') && view.currentText.includes('Reached: rate_limit'),
		},
		{
			prefill: { kind: 'command', value: 'LouiselmHandOff' },
			enter: () => requestLua('if vim.api.nvim_buf_get_name(0):match("^louiselm://limits/") then vim.api.nvim_buf_delete(0, { force = true }) end; return true'),
			check: (view) => view.allText.includes(text().scriptedHandoff),
		},
		{ prefill: { kind: 'command', value: 'LouiselmSessionSwitch' }, check: (view) => view.current === state.initialBuffer },
	];
	let checking = false;
	let checkTimer;

	function text() {
		return COPY[state.language];
	}

	function statusText() {
		const template = text().statuses[state.phase] || text().statuses.booting;
		return template.replace('{version}', state.version || '').replace('{error}', state.error || 'unknown error');
	}

	function setPhase(phase) {
		state.phase = phase;
		elements.demoStatus.textContent = statusText();
		renderStep();
		document.dispatchEvent(new CustomEvent('louiselm-demo-state', { detail: { ...state } }));
	}

	function renderLanguage() {
		const value = text();
		document.documentElement.lang = state.language;
		document.title = value.title;
		elements.eyebrow.textContent = value.eyebrow;
		elements.title.textContent = value.title;
		elements.summary.textContent = value.summary;
		elements.profileTitle.textContent = value.profileTitle;
		elements.profileEnhancedTitle.textContent = value.profileEnhancedTitle;
		elements.profileEnhancedBody.textContent = value.profileEnhancedBody;
		elements.profileCoreTitle.textContent = value.profileCoreTitle;
		elements.profileCoreBody.textContent = value.profileCoreBody;
		elements.profileNote.textContent = value.profileNote;
		elements.statusLabel.textContent = value.statusLabel;
		elements.completionInstallLink.textContent = value.install;
		elements.fallbackInstallLink.textContent = value.install;
		elements.typeAction.textContent = value.type;
		elements.skipStep.textContent = value.skip;
		elements.resetDemo.textContent = value.reset;
		elements.replayDemo.textContent = value.replay;
		elements.completionTitle.textContent = value.completionTitle;
		elements.completionBody.textContent = value.completionBody;
		const otherProfile = state.profile === 'enhanced' ? value.profileCoreTitle : value.profileEnhancedTitle;
		elements.replayOtherProfile.textContent = value.replayOther.replace('{profile}', otherProfile);
		elements.fallbackTitle.textContent = value.fallbackTitle;
		elements.fallbackBody.textContent = value.fallbackBody;
		elements.fallbackList.replaceChildren(
			...value.fallbackItems.map((item) => Object.assign(document.createElement('li'), { textContent: item })),
		);
		elements.tutorLink.textContent = value.tutor;
		elements.demoStatus.textContent = statusText();
		for (const button of document.querySelectorAll('[data-language]')) {
			button.setAttribute('aria-pressed', String(button.dataset.language === state.language));
		}
		for (const button of document.querySelectorAll('[data-profile]')) {
			button.setAttribute('aria-pressed', String(button.dataset.profile === state.profile));
		}
		elements.shell.dataset.profile = state.profile;
		renderStep();
	}

	function renderStep() {
		if (state.phase !== 'ready') {
			elements.step.hidden = true;
			elements.completion.hidden = true;
			elements.typeAction.hidden = true;
			elements.skipStep.hidden = true;
			return;
		}
		if (state.step >= stepDefinitions.length) {
			elements.step.hidden = true;
			elements.completion.hidden = false;
			elements.typeAction.hidden = true;
			elements.skipStep.hidden = true;
			return;
		}
		const current = text().steps[state.step];
		elements.step.hidden = false;
		elements.completion.hidden = true;
		elements.stepProgress.textContent = `${state.language === 'en' ? 'Step' : '步骤'} ${state.step + 1} ${state.language === 'en' ? 'of' : '/'} ${stepDefinitions.length} · ${state.language === 'en' ? 'about 7 minutes' : '约 7 分钟'}`;
		elements.stepTitle.textContent = current.title;
		elements.stepBody.textContent = current.body;
		elements.stepAction.textContent = current.action;
		elements.stepCheck.textContent = current.check;
		elements.typeAction.hidden = stepDefinitions[state.step].prefill == null;
		elements.skipStep.hidden = false;
	}

	function showFallback() {
		clearTimeout(checkTimer);
		elements.shell.classList.add('-fallback');
		elements.fallback.hidden = false;
		setPhase('unsupported');
	}

	function fail(error) {
		state.error = error instanceof Error ? error.message : String(error);
		setPhase('failed');
		console.error('LouiseLM demo bootstrap failed', error);
	}

	function loadScript(path) {
		return new Promise((resolve, reject) => {
			const script = document.createElement('script');
			script.src = path;
			script.onload = resolve;
			script.onerror = () => reject(new Error(`could not load ${path}`));
			document.body.append(script);
		});
	}

	async function loadRuntime() {
		const scripts = [
			'runtime/msgpackr.js',
			'runtime/msgpack.js',
			'runtime/msgpackrpc.js',
			'runtime/rpc.js',
			'runtime/app.js',
			'bundle.js',
		];
		if (state.profile === 'enhanced') scripts.push('vendor-bundle.js');
		for (const path of scripts) {
			await loadScript(path);
		}
	}

	function waitForUi() {
		return new Promise((resolve, reject) => {
			const timeout = setTimeout(() => {
				observer.disconnect();
				reject(new Error('Neovim did not attach within 45 seconds'));
			}, 45_000);
			const inspect = () => {
				if (elements.runtimeStatus.textContent === 'UI attached') {
					clearTimeout(timeout);
					observer.disconnect();
					resolve();
				}
			};
			const observer = new MutationObserver(inspect);
			observer.observe(elements.runtimeStatus, { childList: true, characterData: true, subtree: true });
			inspect();
		});
	}

	function fitGrid() {
		const measure = document.createElement('canvas').getContext('2d');
		if (!measure) throw new Error('Could not measure the Neovim font');
		let previousSize = '';
		const observer = new ResizeObserver(([entry]) => {
			const style = getComputedStyle(elements.grid);
			measure.font = `${style.fontSize} ${style.fontFamily}`;
			const columns = Math.max(1, Math.floor(entry.contentRect.width / measure.measureText('M').width));
			const rows = Math.max(1, Math.floor(entry.contentRect.height / parseFloat(style.lineHeight)));
			const size = `${columns}:${rows}`;
			if (size === previousSize) return;
			previousSize = size;
			globalThis.nvim.request('nvim_ui_try_resize', [columns, rows]).catch((error) => {
				observer.disconnect();
				fail(error);
			});
		});
		// The document owns this observer, including across back/forward-cache restores.
		observer.observe(elements.grid);
	}

	async function writeFile(path, content) {
		const chunkSize = 12_000;
		for (let offset = 0; offset < content.length || offset === 0; offset += chunkSize) {
			let end = Math.min(offset + chunkSize, content.length);
			if (end < content.length && /[\uD800-\uDBFF]/.test(content[end - 1])) end -= 1;
			const chunk = content.slice(offset, end);
			await globalThis.nvim.request('nvim_exec_lua', [
				`local path, bytes, append = ...
vim.fn.mkdir(vim.fs.dirname(path), "p")
local file, open_error = io.open(path, append and "ab" or "wb")
if not file then error(open_error) end
local ok, write_error = file:write(bytes)
file:close()
if not ok then error(write_error) end
return true`,
				[path, chunk, offset > 0],
			]);
			if (content.length === 0) break;
			if (end !== offset + chunkSize) offset = end - chunkSize;
		}
	}

	async function installFiles() {
		const groups = [globalThis.LOUISELM_DEMO_FILES];
		if (state.profile === 'enhanced') groups.push(globalThis.LOUISELM_DEMO_VENDOR?.files);
		for (const files of groups) {
			if (!Array.isArray(files) || files.length === 0) throw new Error('demo file bundle is empty');
			for (const file of files) await writeFile(file.path, file.content);
		}
	}

	function requestLua(source, args = []) {
		return globalThis.nvim.request('nvim_exec_lua', [source, args]);
	}

	async function initializeLouiseLM() {
		return requestLua(
			`local root, profile, language = ...
vim.opt.runtimepath:prepend(root)
vim.api.nvim_set_current_dir("/demo-project")
local runtime, runtime_error = require("louiselm.dev.demo").start({ project_root = "/demo-project", language = language })
if not runtime then error(runtime_error or "LouiseLM demo failed") end
if profile == "enhanced" then
  local vendor_root = "/home/user/.local/share/nvim/site/pack/louiselm/opt"
  vim.opt.runtimepath:prepend(vendor_root .. "/which-key.nvim")
  vim.opt.runtimepath:prepend(vendor_root .. "/snacks.nvim")
  require("snacks").setup({ picker = { enabled = true, ui_select = true } })
  vim.g.mapleader = " "
  vim.keymap.set("n", "<leader>lr", "<cmd>LouiselmResume<cr>", { desc = "LouiseLM resume session" })
  vim.keymap.set("n", "<leader>lsn", "<cmd>LouiselmSessionNew<cr>", { desc = "LouiseLM session new" })
  vim.keymap.set("n", "<leader>lsw", "<cmd>LouiselmSessionSwitch<cr>", { desc = "LouiseLM session switch" })
  vim.keymap.set("n", "<leader>lH", "<cmd>LouiselmHandOff<cr>", { desc = "LouiseLM hand off session" })
  vim.keymap.set("n", "<leader>lL", "<cmd>LouiselmLimits<cr>", { desc = "LouiseLM inspect limits" })
  local which_key = require("which-key")
  which_key.setup({
    preset = "modern",
    delay = 120,
    notify = false,
    triggers = { { "<leader>", mode = "n" } },
    plugins = {
      marks = false,
      registers = false,
      spelling = { enabled = false },
      presets = {
        operators = false,
        motions = false,
        text_objects = false,
        windows = false,
        nav = false,
        z = false,
        g = false,
      },
    },
    icons = { mappings = false },
  })
  which_key.add({ { "<leader>l", group = "LouiseLM" } })
end
local version = vim.version()
return {
  version = string.format("%d.%d.%d%s", version.major, version.minor, version.patch, version.prerelease and "-dev" or ""),
  current_buffer = vim.api.nvim_buf_get_name(0),
  profile = profile,
}`,
			[root, state.profile, state.language],
		);
	}

	function occurrences(value, needle) {
		let count = 0;
		let offset = 0;
		while ((offset = value.indexOf(needle, offset)) !== -1) {
			count += 1;
			offset += needle.length;
		}
		return count;
	}

	async function inspectNvim() {
		const result = await requestLua(`local result = {
  current = vim.api.nvim_buf_get_name(0),
  buffers = {},
  file = "",
}
for _, buffer in ipairs(vim.api.nvim_list_bufs()) do
  if vim.api.nvim_buf_is_valid(buffer) then
    local name = vim.api.nvim_buf_get_name(buffer)
    if name:match("^louiselm[:%-]") then
      result.buffers[name] = table.concat(vim.api.nvim_buf_get_lines(buffer, 0, -1, false), "\\n")
    end
  end
end
local ok, lines = pcall(vim.fn.readfile, "/demo-project/lua/calculator.lua")
if ok then result.file = table.concat(lines, "\\n") end
return result`);
		result.currentText = result.buffers[result.current] || '';
		result.allText = Object.values(result.buffers).join('\n');
		return result;
	}

	async function enterStep() {
		renderStep();
		const enter = stepDefinitions[state.step]?.enter;
		if (enter) {
			try {
				await enter();
			} catch (error) {
				console.warn('LouiseLM demo step setup failed', error);
			}
		}
		scheduleCheck(100);
	}

	function advance() {
		state.step += 1;
		document.dispatchEvent(new CustomEvent('louiselm-demo-state', { detail: { ...state } }));
		enterStep();
	}

	function scheduleCheck(delay = 300) {
		clearTimeout(checkTimer);
		checkTimer = setTimeout(checkProgress, delay);
	}

	async function checkProgress() {
		if (checking || state.phase !== 'ready' || state.step >= stepDefinitions.length) return;
		checking = true;
		try {
			const view = await inspectNvim();
			if (stepDefinitions[state.step].check(view)) {
				elements.stepCheck.textContent = state.language === 'en' ? 'Verified in Neovim ✓' : '已在 Neovim 中验证 ✓';
				setTimeout(advance, 450);
				return;
			}
		} catch (error) {
			console.debug('LouiseLM demo verification is waiting for Neovim input', error);
		} finally {
			checking = false;
		}
		scheduleCheck();
	}

	async function prefillPrompt(value) {
		await requestLua(
			`local value = ...
local buffer = vim.api.nvim_get_current_buf()
if not vim.api.nvim_buf_get_name(buffer):match("^louiselm://demo%-") then error("switch to a Session prompt first") end
local line = vim.api.nvim_buf_line_count(buffer) - 1
local current = vim.api.nvim_buf_get_lines(buffer, line, line + 1, false)[1] or "> "
vim.api.nvim_buf_set_lines(buffer, line, line + 1, false, { current .. value })
vim.api.nvim_win_set_cursor(0, { line + 1, #(current .. value) })
return true`,
			[value],
		);
		globalThis.nvim.notify('nvim_input', ['A']);
		elements.grid.focus();
	}

	function prefillCommand(value) {
		globalThis.nvim.notify('nvim_input', [`\u001b:${value}`]);
		elements.grid.focus();
	}

	async function typeAction() {
		const prefill = stepDefinitions[state.step]?.prefill;
		if (!prefill || state.phase !== 'ready') return;
		try {
			const value = typeof prefill.value === 'function' ? prefill.value() : prefill.value;
			if (prefill.kind === 'prompt') await prefillPrompt(value);
			else prefillCommand(value);
		} catch (error) {
			elements.stepCheck.textContent = error instanceof Error ? error.message : String(error);
		}
	}

	function restartWithProfile(profile) {
		if (profile !== 'core' && profile !== 'enhanced') return;
		const url = new URL(location.href);
		url.searchParams.set('profile', profile);
		url.searchParams.set('language', state.language);
		location.assign(url);
	}

	async function boot() {
		if (!globalThis.crossOriginIsolated || window.innerWidth < 1024) {
			showFallback();
			return;
		}
		setPhase('booting');
		await loadRuntime();
		await waitForUi();
		fitGrid();
		setPhase('installing');
		await installFiles();
		const initialized = await initializeLouiseLM();
		state.version = initialized.version;
		state.initialBuffer = initialized.current_buffer;
		elements.runtimeStatus.textContent = `LouiseLM ready · ${state.profile === 'enhanced' ? 'Enhanced' : 'Core'}`;
		setPhase('ready');
		enterStep();
	}

	for (const button of document.querySelectorAll('[data-language]')) {
		button.addEventListener('click', async () => {
			const language = button.dataset.language;
			try {
				if (state.phase === 'ready') {
					await requestLua('local language = ...; vim.api.nvim_cmd({ cmd = "LouiselmDemoLanguage", args = { language } }, {}); return true', [language]);
				}
				state.language = language;
				renderLanguage();
			} catch (error) {
				elements.stepCheck.textContent = error instanceof Error ? error.message : String(error);
			}
		});
	}
	for (const button of document.querySelectorAll('[data-profile]')) {
		button.addEventListener('click', () => {
			if (button.dataset.profile !== state.profile) restartWithProfile(button.dataset.profile);
		});
	}
	elements.typeAction.addEventListener('click', typeAction);
	elements.skipStep.addEventListener('click', advance);
	elements.resetDemo.addEventListener('click', () => location.reload());
	elements.replayDemo.addEventListener('click', () => location.reload());
	elements.replayOtherProfile.addEventListener('click', () => {
		restartWithProfile(state.profile === 'enhanced' ? 'core' : 'enhanced');
	});
	renderLanguage();
	boot().catch(fail);
})();
