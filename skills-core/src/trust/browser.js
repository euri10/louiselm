"use strict";
const action = document.querySelector("#action");
const approve = document.querySelector("#approve");
const cancel = document.querySelector("#cancel");
const status = document.querySelector("#status");
const controller = new AbortController();
let done = false;
let submitted = false;
let deadline = 0;
let expiryTimer;
async function post(path, body) {
  const response = await fetch(path, {method: "POST", headers: {"Content-Type": "application/json"}, body: JSON.stringify(body), cache: "no-store"});
  if (!response.ok) throw new Error("Local ceremony refused");
  return response.json();
}
function finish(message) {
  done = true;
  clearTimeout(expiryTimer);
  approve.disabled = true;
  cancel.disabled = true;
  status.textContent = message;
}
function expired() {
  if (performance.now() < deadline) return false;
  if (!done && !submitted) {
    controller.abort();
    finish("Ceremony expired; no approval submitted. Inspect recovery status in the trusted terminal before starting a new ceremony. A passkey may have been saved without being enrolled.");
  }
  return true;
}
async function stop() {
  if (done || submitted) return;
  controller.abort();
  finish("Cancelled; no approval submitted. The local ceremony expires automatically if unreachable.");
  try { await post("cancel", {}); } catch (_) { /* Expiry also destroys pending state. */ }
}
async function failed(error, kind) {
  // Never display/send native messages, options, or credential material.
  const codes = ["InvalidStateError", "NotAllowedError", "AbortError", "NotSupportedError", "SecurityError", "ConstraintError", "TypeError", "UnknownError"];
  const code = codes.includes(error?.name) ? error.name : "UnknownError";
  const operation = kind === "register" ? "registration" : kind === "authenticate" ? "authentication" : "confirmation";
  controller.abort();
  finish(`Passkey ${operation} failed (${code}); no approval submitted. Inspect recovery status in the trusted terminal before retrying. Keep existing passkeys and papers; a new passkey may have been saved without being enrolled.`);
  try { await post("failed", {code}); } catch (_) { /* Expiry also destroys pending state. */ }
}
cancel.addEventListener("click", stop);
window.addEventListener("pagehide", () => {
  if (!done && !submitted) {
    controller.abort();
    fetch("cancel", {method:"POST", headers:{"Content-Type":"application/json"}, body:"{}", keepalive:true}).catch(() => {});
  }
});
// Anchor to request start: subtract the full round trip conservatively instead
// of relying on matching host/VM clocks or granting extra response transit time.
const requestedAt = performance.now();
fetch("ceremony.json", {cache:"no-store"}).then(response => {
  if (!response.ok) throw new Error("Local ceremony unavailable");
  return response.json();
}).then(ceremony => {
  if (done) return;
  if (!Number.isSafeInteger(ceremony.remaining_ms) || ceremony.remaining_ms < 0) {
    throw new Error("Invalid ceremony lifetime");
  }
  deadline = requestedAt + ceremony.remaining_ms;
  if (expired()) return;
  expiryTimer = setTimeout(expired, Math.ceil(deadline - performance.now()));
  action.textContent = JSON.stringify(ceremony.action, null, 2);
  status.textContent = "Review every field, then explicitly approve or cancel.";
  approve.disabled = false;
  approve.addEventListener("click", async () => {
    if (done || expired()) return;
    approve.disabled = true;
    try {
      let proof = {};
      if (ceremony.kind === "register") {
        const publicKey = PublicKeyCredential.parseCreationOptionsFromJSON(ceremony.options.publicKey);
        publicKey.timeout = Math.min(publicKey.timeout ?? Infinity, deadline - performance.now());
        proof = (await navigator.credentials.create({publicKey, signal:controller.signal})).toJSON();
      } else if (ceremony.kind === "authenticate") {
        const publicKey = PublicKeyCredential.parseRequestOptionsFromJSON(ceremony.options.publicKey);
        publicKey.timeout = Math.min(publicKey.timeout ?? Infinity, deadline - performance.now());
        proof = (await navigator.credentials.get({publicKey, signal:controller.signal})).toJSON();
      } else if (ceremony.kind !== "confirm") {
        throw new Error("Unknown ceremony");
      }
      if (done || expired()) return;
      submitted = true;
      cancel.disabled = true;
      status.textContent = "Approval submitted. Waiting for the trusted tool’s result…";
      const result = await post("finish", proof);
      finish(result.message);
    } catch (error) {
      if (done) return;
      if (submitted) finish("Result unknown: inspect the trusted terminal and trust state before retrying. Closing this page cannot undo a committed change.");
      else if (!expired()) await failed(error, ceremony.kind);
    }
  }, {once:true});
}).catch(() => finish("Local ceremony unavailable. Return to the trusted terminal."));
