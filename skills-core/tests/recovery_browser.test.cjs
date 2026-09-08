// Execute the shipped client with deterministic time and async browser doubles.
// C14/se0i, codex/01a07d34-4adb-7491-8d0a-3f23cd0796e1: phone registration
// began after server expiry. Only that observed ordering is reproduced here;
// all options/proofs are public invented fixtures, never real credentials.
const assert = require("node:assert/strict");
const {readFileSync} = require("node:fs");
const {join} = require("node:path");
const {setImmediate: settle} = require("node:timers/promises");
const {test} = require("node:test");
const {runInNewContext} = require("node:vm");
const source = readFileSync(join(__dirname, "../src/trust/browser.js"), "utf8");

function pending() {
  let resolve, reject;
  const promise = new Promise((yes, no) => { resolve = yes; reject = no; });
  return {promise, resolve, reject};
}

async function page({kind = "register", remaining = 1000, transfer = 0, timeout = 300000, parseError} = {}) {
  let now = 0;
  let nextTimer = 0;
  const timers = new Map();
  const events = {};
  const elements = Object.fromEntries(["action", "approve", "cancel", "status"].map(name => [name, {
    disabled: name === "approve", textContent: "",
    addEventListener(event, callback) { events[`${name}:${event}`] = callback; },
  }]));
  const document = pending();
  const credential = pending();
  const result = pending();
  const calls = [];
  const proofs = [];
  runInNewContext(source, {
    document: {querySelector: selector => elements[selector.slice(1)]},
    window: {addEventListener: (event, callback) => { events[event] = callback; }},
    AbortController,
    performance: {now: () => now},
    setTimeout: (callback, delay) => {
      const id = ++nextTimer;
      timers.set(id, {at: now + Math.trunc(delay), callback});
      return id;
    },
    clearTimeout: id => timers.delete(id),
    fetch: async (path, options) => {
      calls.push({path, options});
      if (path === "ceremony.json") return document.promise;
      if (path === "finish") return result.promise;
      return {ok: true, json: async () => ({message: "Cancelled"})};
    },
    PublicKeyCredential: {
      parseCreationOptionsFromJSON: value => { if (parseError) throw parseError; return {...value}; },
      parseRequestOptionsFromJSON: value => { if (parseError) throw parseError; return {...value}; },
    },
    navigator: {credentials: Object.fromEntries(["create", "get"].map(method => [method, options => {
      proofs.push({method, ...options});
      return credential.promise;
    }]))},
  });
  now = transfer;
  document.resolve({ok: true, json: async () => ({kind, remaining_ms: remaining,
    action: {operation: "PUBLIC FIXTURE"}, options: {publicKey: {timeout}}})});
  await settle();
  return {
    elements, events, calls, proofs, credential, result,
    click: () => events["approve:click"]?.(),
    async advance(milliseconds, runTimers = true) {
      now += milliseconds;
      if (runTimers) for (const [id, timer] of [...timers]) {
        if (timer.at <= now) { timers.delete(id); timer.callback(); }
      }
      await settle();
    },
  };
}

for (const kind of ["register", "authenticate"]) {
  test(`${kind}: late entry and review do not restart the server budget`, async () => {
    const p = await page({kind, remaining: 1000, transfer: 200});
    await p.advance(300);
    const clicked = p.click();
    assert.equal(p.proofs[0].publicKey.timeout, 500);
    assert.equal(p.proofs[0].method, kind === "register" ? "create" : "get");
    await p.advance(500);
    assert.equal(p.proofs[0].signal.aborted, true);
    assert.match(p.elements.status.textContent, /expired.*no approval submitted/i);
    assert.equal(p.elements.approve.disabled, true);
    assert.equal(p.elements.cancel.disabled, true);
    p.credential.resolve({toJSON: () => ({public: "candidate"})});
    await clicked;
    assert.equal(p.calls.some(call => call.path === "finish"), false);
  });
}

test("expired or malformed lifetime never enables approval", async () => {
  for (const remaining of [0, -1, "1000", null, NaN, 0.5, Infinity]) {
    const p = await page({remaining});
    assert.equal(p.elements.approve.disabled, true, `remaining=${remaining}`);
    assert.equal(p.proofs.length, 0);
  }
  const p = await page({remaining: 1000, transfer: 1000});
  assert.equal(p.elements.approve.disabled, true);
  assert.match(p.elements.status.textContent, /expired/i);
});

test("fractional transit time cannot lose the expiry timer", async () => {
  const p = await page({transfer: 200.5});
  await p.advance(799);
  await p.advance(1);
  assert.match(p.elements.status.textContent, /expired/i);
  assert.equal(p.elements.approve.disabled, true);
});

test("suspended timers cannot permit a late click or late credential submission", async () => {
  const lateClick = await page({kind: "confirm"});
  await lateClick.advance(1000, false);
  const lateClicked = lateClick.click();
  assert.equal(lateClick.calls.some(call => call.path === "finish"), false);
  await lateClicked;
  assert.match(lateClick.elements.status.textContent, /expired/i);

  const lateProof = await page();
  const clicked = lateProof.click();
  await lateProof.advance(1000, false);
  lateProof.credential.resolve({toJSON: () => ({public: "candidate"})});
  await clicked;
  assert.equal(lateProof.calls.some(call => call.path === "finish"), false);
  assert.match(lateProof.elements.status.textContent, /expired/i);
});

test("a shorter WebAuthn timeout is preserved", async () => {
  const p = await page({timeout: 100});
  const clicked = p.click();
  assert.equal(p.proofs[0].publicKey.timeout, 100);
  p.credential.reject(new Error("Public authenticator refusal"));
  await clicked;
  assert.match(p.elements.status.textContent, /registration failed \(UnknownError\)/);
  assert.equal(p.calls.some(call => call.path === "finish"), false);
});

for (const kind of ["register", "authenticate"]) {
  test(`${kind}: option parsing errors never open an authenticator or leak details`, async () => {
    const p = await page({kind, parseError: new TypeError("UNSAFE FIXTURE DETAIL")});
    await p.click();
    assert.equal(p.proofs.length, 0);
    assert.match(p.elements.status.textContent, /failed \(TypeError\)/);
    assert.doesNotMatch(p.elements.status.textContent, /UNSAFE FIXTURE DETAIL|Cancelled/);
    assert.deepEqual(p.calls.filter(call => call.path !== "ceremony.json").map(call =>
      ({path:call.path, body:JSON.parse(call.options.body)})), [{path:"failed", body:{code:"TypeError"}}]);
  });

  test(`${kind}: pre-submit errors are bounded diagnostics, not explicit cancellation`, async () => {
    // 5bcg/C14: phone selection/biometric preceded generic cancellation.
    // Error names below are synthetic hypotheses, not a captured native error.
    for (const name of ["InvalidStateError", "NotAllowedError", "AbortError", "NotSupportedError",
      "SecurityError", "ConstraintError", "TypeError", "UnknownError", "UNSAFE FIXTURE DETAIL"]) {
      const p = await page({kind});
      const clicked = p.click();
      p.credential.reject(Object.assign(new Error("UNSAFE FIXTURE DETAIL"), {name}));
      await clicked;
      const code = name === "UNSAFE FIXTURE DETAIL" ? "UnknownError" : name;
      assert.match(p.elements.status.textContent, new RegExp(`${kind === "register" ? "registration" : "authentication"} failed \\(${code}\\)`));
      assert.doesNotMatch(p.elements.status.textContent, /Cancelled;|UNSAFE FIXTURE DETAIL/);
      assert.equal(p.elements.approve.disabled, true);
      assert.equal(p.elements.cancel.disabled, true);
      assert.equal(p.proofs[0].signal.aborted, true);
      assert.equal(p.calls.some(call => ["finish", "cancel"].includes(call.path)), false);
      const report = p.calls.find(call => call.path === "failed");
      assert.deepEqual(JSON.parse(report.options.body), {code});
      await p.advance(2000);
      await p.events["cancel:click"]();
      p.events.pagehide();
      assert.equal(p.calls.filter(call => call.path === "failed").length, 1);
      assert.equal(p.calls.some(call => ["finish", "cancel"].includes(call.path)), false);
    }
  });
}

test("cancellation aborts the prompt and ignores its later result", async () => {
  const p = await page();
  const clicked = p.click();
  await p.events["cancel:click"]();
  assert.equal(p.proofs[0].signal.aborted, true);
  p.credential.resolve({toJSON: () => ({public: "candidate"})});
  await clicked;
  assert.match(p.elements.status.textContent, /Cancelled/);
  assert.equal(p.calls.some(call => call.path === "finish"), false);
});

for (const success of [true, false]) {
  test(`expiry after submission preserves ${success ? "commit" : "unknown result"}`, async () => {
    const p = await page({kind: "confirm"});
    const clicked = p.click();
    assert.equal(p.calls.some(call => call.path === "finish"), true);
    await p.advance(1000);
    assert.match(p.elements.status.textContent, /Approval submitted/);
    if (success) p.result.resolve({ok: true, json: async () => ({message: "Committed"})});
    else p.result.reject(new Error("Public lost response fixture"));
    await clicked;
    assert.match(p.elements.status.textContent, success ? /Committed/ : /Result unknown/);
    assert.equal(p.elements.cancel.disabled, true);
  });
}
