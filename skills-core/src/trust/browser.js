"use strict";
const action = document.querySelector("#action");
const approve = document.querySelector("#approve");
const cancel = document.querySelector("#cancel");
const status = document.querySelector("#status");
const controller = new AbortController();
let done = false;
let submitted = false;
async function post(path, body) {
  const response = await fetch(path, {method: "POST", headers: {"Content-Type": "application/json"}, body: JSON.stringify(body), cache: "no-store"});
  if (!response.ok) throw new Error("Local ceremony refused");
  return response.json();
}
function finish(message) {
  done = true;
  approve.disabled = true;
  cancel.disabled = true;
  status.textContent = message;
}
async function stop() {
  if (done || submitted) return;
  controller.abort();
  finish("Cancelled; no approval submitted. The local ceremony expires automatically if unreachable.");
  try { await post("cancel", {}); } catch (_) { /* Expiry also destroys pending state. */ }
}
cancel.addEventListener("click", stop);
window.addEventListener("pagehide", () => {
  if (!done && !submitted) {
    controller.abort();
    fetch("cancel", {method:"POST", headers:{"Content-Type":"application/json"}, body:"{}", keepalive:true}).catch(() => {});
  }
});
setTimeout(() => { if (!submitted) stop(); }, 300000);
fetch("ceremony.json", {cache:"no-store"}).then(response => {
  if (!response.ok) throw new Error("Local ceremony unavailable");
  return response.json();
}).then(ceremony => {
  if (done) return;
  action.textContent = JSON.stringify(ceremony.action, null, 2);
  status.textContent = "Review every field, then explicitly approve or cancel.";
  approve.disabled = false;
  approve.addEventListener("click", async () => {
    approve.disabled = true;
    try {
      let proof = {};
      if (ceremony.kind === "register") {
        const publicKey = PublicKeyCredential.parseCreationOptionsFromJSON(ceremony.options.publicKey);
        proof = (await navigator.credentials.create({publicKey, signal:controller.signal})).toJSON();
      } else if (ceremony.kind === "authenticate") {
        const publicKey = PublicKeyCredential.parseRequestOptionsFromJSON(ceremony.options.publicKey);
        proof = (await navigator.credentials.get({publicKey, signal:controller.signal})).toJSON();
      } else if (ceremony.kind !== "confirm") {
        throw new Error("Unknown ceremony");
      }
      if (done) return;
      submitted = true;
      cancel.disabled = true;
      status.textContent = "Approval submitted. Waiting for the trusted tool’s result…";
      const result = await post("finish", proof);
      finish(result.message);
    } catch (_) {
      if (done) return;
      if (submitted) finish("Result unknown: inspect the trusted terminal and trust state before retrying. Closing this page cannot undo a committed change.");
      else await stop();
    }
  }, {once:true});
}).catch(() => finish("Local ceremony unavailable. Return to the trusted terminal."));
