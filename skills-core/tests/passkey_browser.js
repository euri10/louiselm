// Playwright CLI function, run only against trust::browser::tests::virtual_browser.
// Never point this at an installed-authority ceremony or a personal browser.
async page => {
  if (!/^http:\/\/localhost:\d+\/[a-f0-9-]+\/$/.test(page.url())) throw new Error("Not a loopback fixture");
  const ceremony = await page.evaluate(async () => (await fetch("ceremony.json")).json());
  const label = ceremony.action.trust_domain || ceremony.action.operation || "";
  if (!label.startsWith("PUBLIC ") && label !== "CANCEL THIS PUBLIC FIXTURE") throw new Error("Not the public test fixture");
  const errors = [];
  page.on("pageerror", error => errors.push(error.message));
  await page.getByRole("heading", {name:"Exact proposed action"}).waitFor();
  if (ceremony.kind === "confirm") {
    await page.getByRole("button", {name:"Cancel", exact:true}).click();
    await page.getByRole("status").filter({hasText:"Cancelled"}).waitFor();
  } else {
    if (ceremony.kind === "register" && ceremony.action.operation !== "PUBLIC FIXTURE REPLACE") {
      const cdp = await page.context().newCDPSession(page);
      await cdp.send("WebAuthn.enable");
      await cdp.send("WebAuthn.addVirtualAuthenticator", {options:{protocol:"ctap2", transport:"internal", hasResidentKey:true, hasUserVerification:true, isUserVerified:true, automaticPresenceSimulation:true, defaultBackupEligibility:true, defaultBackupState:true}});
    }
    const shown = JSON.parse(await page.locator("#action").textContent());
    if (JSON.stringify(shown) !== JSON.stringify(ceremony.action)) throw new Error("Displayed action differs");
    await page.getByRole("button", {name:"Approve this action", exact:true}).click();
    await page.getByRole("status").filter({hasText:ceremony.kind === "register" ? "Public fixture registration verified" : "Public fixture assertion verified"}).waitFor({timeout:5000});
  }
  if (!await page.getByRole("button", {name:"Approve this action", exact:true}).isDisabled()) throw new Error("Replay button enabled");
  if (errors.length) throw new Error(errors.join("\n"));
  return page.getByRole("status").textContent();
}
