# LouiseLM Android capture

The Android app is a small offline-first recorder with a separate phone-control
enrollment and unlock screen. One tap starts speech capture; stopping atomically
publishes immutable audio and metadata in app-private storage. Pairing and upload
can happen later.

## Build

Install Android SDK Platform 37.0, Build Tools 36.0.0, and Platform Tools. Gradle
must be able to find that SDK through `ANDROID_HOME` or an ignored
`local.properties` file in this directory:

```properties
sdk.dir=/absolute/path/to/Android/Sdk
```

Connect an Android 11 / API 30 or newer device, then run:

```sh
./gradlew test lint assembleDebug
./gradlew installDebug
```

### Side-by-side physical QA

Use the `qa` build when the normal `dev.louiselm.capture` app contains real
pairing or recordings. It uses the same runtime code, with package ID
`dev.louiselm.capture.qa`, label **LouiseLM QA**, and separate app storage and
Android Keystore identity. Debug and release package IDs remain unchanged.
No second Android user or Google account is required.

```sh
./gradlew test lint lintQa assembleDebug assembleQa
# Verify this exact APK before installing; never substitute app-debug.apk.
apkanalyzer manifest application-id app/build/outputs/apk/qa/app-qa.apk
adb -s DEVICE_SERIAL install --user 0 app/build/outputs/apk/qa/app-qa.apk
```

The package check must print `dev.louiselm.capture.qa`. Install without `-r`
for a fresh fixture: an existing QA installation must not be overwritten or
cleared without deciding whether its data is disposable. Open **LouiseLM QA**,
verify it is unpaired with zero recordings, and pair only to the disposable
QA receiver. Preserve the original app, its data, and the production receiver.
This bypasses the need for secondary-user enrollment; it does not repair an
OEM setup-wizard failure. Run the same physical acceptance cases below.

`test` includes pure JUnit tests and Robolectric Activity/View/lifecycle tests
on the JVM; no device is needed for those tests. Activity fixtures retain
Android 14/API 34 physical-regression coverage and exercise receiver permissions
on Android 17/API 37. They use isolated app storage, a shadow recorder, and
separately controlled background/UI queues. Robolectric is pinned to 4.17
for API 37 support; its SharedSecrets module export applies only to JVM tests.
Run just that regression with:

```sh
./gradlew :app:testDebugUnitTest --tests dev.louiselm.capture.MainActivityTest
```

Kotlin warnings and Android Lint warnings/errors fail the build. Narrow exceptions
document receiver SPKI trust and checked credential persistence.
CI uses JDK 25 and runs `test lint lintQa assembleDebug assembleQa`, then checks
the packaged QA application ID;
physical checks remain necessary for hardware, pairing/Keystore and real
background scheduling. Development rules are in
[AGENTS.md](AGENTS.md).

The app uses the system camera to take a full-resolution image of the terminal
QR and bundled ML Kit to decode it offline. Pairing and uploads use HTTPS with
the stable receiver public-key identity carried by the version-2 one-time QR;
routine TLS certificate renewal with that key remains trusted. The long-lived
device credential and receiver identity are encrypted by Android Keystore.
WorkManager retries only network, timeout, rate-limit, and server failures.

On Android 17+, Pair, Sync, and Refresh request Nearby devices permission for
direct access to the private receiver. Denial or revocation leaves offline
recording available. Grant access in app settings and retry the receiver action;
background uploads stop with an actionable failure while permission is missing.
The permission flow is framework-tested; physical Android 17 receiver access,
QR scanning, and recording have not yet been verified.

App data opts out of Android backup. Android 12+ extraction rules explicitly
exclude all app storage domains from cloud backup and device-to-device transfer,
including recordings and device-bound pairing credentials. Older devices retain
`allowBackup=false`. The packaged policy is regression-tested; OEM transfer
behavior has not been physically verified. See
[Android backup rules](https://developer.android.com/identity/data/autobackup).

Configure one private receiver profile before running
`:LouiselmCapturePair`; the receiver refuses pairing while it is loopback-only.
The Android app does not support version-1 exact-certificate pairing offers.

Each pending capture records its receiver-key owner separately from the
immutable audio and capture manifest. Captures made before pairing remain
unowned until the first-pair confirmation states their count. Scanning a QR
with the same receiver identity offers **Keep pairing** or **Renew pairing**.
Keep pairing verifies `/v1/health` through the existing pin and changes only the
endpoint, preserving the device credential and queue. After revocation, scan a
fresh QR and choose Renew pairing: it exchanges the one-use offer for a new
credential and immediately retries pending uploads without moving ownership or
changing original recordings/capture IDs. A rejected or expired offer leaves
the saved pairing and recordings unchanged; Cancel performs neither operation.
Moving pending captures to a different receiver identity requires a separate
confirmation that states the affected count; reassignment resets upload retry
state but never changes or removes the original audio or capture UUID.

The phone status shows the current endpoint, pending count and oldest age, last
successful sync, and the latest actionable upload failure. **Sync now** remains
the manual WorkManager retry path.

## Phone-control workspace

Phone control requires an already-paired receiver that supports the phone-control
protocol. See the [computer administration commands](../capture-service/README.md)
for the opt-in and enrollment steps. Start enrollment on the computer, open
**Phone control**, and choose **Enable phone control**. Compare the eight-digit
code on both devices and approve the phone and its scope on the computer.
**Check approval** completes enrollment;
capture pairing alone does not grant workspace access.

**Unlock workspace** asks Android for a strong biometric or device credential to
authenticate a fresh signing operation. Its
temporary working key remains only in the app process and is discarded on screen
lock, expiry, or screen destruction; switching briefly to another app retains it.
The computer also enforces five minutes of interaction inactivity and a one-hour
absolute limit. Disabling workspace access on the computer leaves capture pairing
and recordings intact. Forgetting the phone also revokes pairing. To read a
Session, enable `phone_control.enabled` in the owning Neovim and unlock the
workspace. **Phone control** lists live Sessions in the approved project and
Agent scope, even without Attention. Each row includes its current state and
exact controller and Session identities; use **More Sessions** to continue a
large list and **Refresh** to start again. Select a row to open that Session,
or enable Attention and tap its Session card. **Sessions** returns to the list.
Disconnected or replaced controllers and disposed Sessions are unavailable;
they never redirect to another Session. The conversation follows current
state and offers **Load older messages** for available history. Updates refresh
the snapshot and recover the visible range when possible; the screen reports
when older messages must be loaded again. It keeps at most 500 entries at once
and offers Refresh to return to the newest messages.
Background refresh does not extend the working period.
Opening a card does not clear or answer Attention. Old cards may no longer
point to a live Session; refresh the inbox instead of assuming the condition
still applies. Physical Keystore and authentication acceptance is tracked in
`louiselm-qbr.12.22`.

The Session screen offers an explicit **Send** for a reviewed text instruction
when the displayed Session is ready. The current receiver still refuses Send
until the crash-safe admission work in `louiselm-qbr.12.4.5` is proven; a refusal
is displayed and the text remains on screen. The phone stores a private,
payload-free operation identity before sending and checks retained owner evidence
while the workspace is open and unlocked. A lost response is not confirmation,
and reopening never resends. Another attempt requires owner evidence that the
original had no effect and was fenced, followed by a Session refresh and review.
The composer keeps a separate phone draft for each receiver and exact
controller/Session. **Unlock phone drafts** opens its independent local key; it
does not grant online authority. Edits save in the background: wait for saving
to finish before leaving or locking. A failed save keeps the last saved version
and reports that the current edit was not saved. Saved drafts remain until
explicit **Discard draft** confirmation. At most 128 drafts, 8 MiB per draft
and 64 MiB total are retained; freeing space requires a discard.

Use **Add to draft** beside an authorized live or saved offline entry to freeze
that entry and its source in the current draft. Later refreshes and removal of a
downloaded copy do not change the selection. **Phone drafts** reopens saved work
offline, including work detached from a missing or revoked source. Detached
context can contain sensitive excerpts; explicitly choose an authorized
destination and review before sending. Reviewed Send with selected context and
consumption of the confirmed snapshot remain `louiselm-qbr.12.12.2`; until then,
Send is unavailable for drafts containing selected context.

Drafts use a device-only key separate from downloads, pairing and online keys.
The local unlock has the same inactivity, maximum duration and screen-lock rules
as offline copies below. A temporary key error leaves drafts in place for retry.
If the device proves the draft key permanently lost, the app explains that the
work cannot be recovered and asks before deleting the affected drafts and
resetting the key. Original recordings and pending action identities are separate.

### Session voice recordings

Open an authorized Session and unlock phone drafts to record a voice instruction.
The screen discloses the planned phone → paired computer → OpenAI transcription
route. **Stop voice recording** saves the original locally and records the request
for transcription. This local slice does not submit a voice job: transcription
is shown as unavailable until the protected receiver route is implemented. No
recording, playback, reconnect, or transcription request sends a Session prompt.

Locking the phone or leaving the Activity stops an active recording. After local
unlock, **Session voice recordings** shows its original Session association and
asks whether to request transcription or keep it without requesting transcription.
Keeping an original does not prevent explicitly requesting transcription later.
The list works offline after restart and can play a saved original; playback stops
when the app backgrounds or locks. A recording exceeding 20 MiB remains on the
phone but cannot use the planned transfer route. A failed or incomplete capture
is retained and reported; an unfinalized audio container may not be recoverable.

Session voice originals use the existing capture storage format in a separate
local collection, outside ordinary thought uploads and draft/cache quotas. Their
associated source and draft revision are encrypted with an independent device-only
key, so resetting a lost draft key does not delete the recording association.
The local draft unlock controls presentation; it grants no workspace authority.

While workspace and phone drafts are unlocked, **Desktop voice history** reads
already archived recordings within the current Read content grant. It labels
the original controller, Session, project and Agent, even if that Session has
ended. A result fills its original draft only if the exact recorded revision is
still present, empty and untouched. Otherwise choose a currently authorized
Session and explicitly **Insert transcript** or **Replace draft with
transcript**. Review and edit the device-local draft before any Send; the
archived audio and original transcript remain unchanged. History requires an
online receiver and is unavailable when the read grant or working period ends.
The display accepts transcripts up to 8 MiB; draft and selected-context limits
may be smaller. Job submission and explicit reviewed Send remain separate work
in `louiselm-qbr.12.13.3` and `louiselm-qbr.12.13.2`.

### Runs and prepared selections

**Runs** browses authorized durable Run records or opens the exact Run from an
Attention card. The screen shows generated-work bounds, Park/expiry and the
Agent's advertised `session/load` capability without promising recovery. Live
controller availability is checked separately: an absent or unreachable owner
does not erase the durable record. A connected owner's current phase and linked
Sessions are observations from that read, not guarantees it remains connected.
Open a linked Session to read it under the exact Run owner and revision; this
view is read-only and does not save offline copies. A stale owner or revision
requires a fresh Run read rather than redirecting to a replacement.

**Prepared Runs** lists only selections explicitly published by the desktop
operator, with protected instructions and limits in their detail view. Viewing
one does not Start a Run, allocate Sessions, grant capability, or start a
deadline. The operator may reopen its waiting controller after a restart; the
phone never launches one automatically. Run support uses optional skills-core
integration and reports unsupported if it is absent; ordinary Sessions do not
depend on it. Phone Start and saved-result review are separate work.

### Saved offline copies

When prompted, unlock offline copies separately to save recently opened Session
pages automatically. Cancelling that prompt leaves online reading available.
**Keep offline** downloads the available older pages for the selected Session;
progress and any missing, redacted, shortened or incomplete material are shown.
Desktop archive history is not yet available. Saved-result previews are also
not yet available for offline download.

Use **Unlock offline copies** and **Offline copies** to read without a computer
or network connection. Each copy shows its original computer, controller and
Session, its last successful refresh and a stale-state warning. A saved copy is
not authority to operate a live Session, including after a Neovim restart.
Copies use a device-only download key, separate from pairing, phone-control and
draft keys. Local unlock requires a strong biometric or device credential, locks
after five minutes without interaction or one hour at most, and clears when the
screen locks or the app process ends. Brief app switching retains the remaining
time but hides protected content; screenshots and recording are blocked where
the system supports it. Text explicitly copied out cannot be recalled.

The default download limit is 512 MiB, adjustable from **Offline copies**, and
the limit is 200 Sessions including kept copies. Unpinned copies expire 30 days
after opening and are evicted least-recently-opened first as space is needed.
Kept copies do not expire or get evicted routinely; unpin or remove them, or
raise the byte limit, when needed. While the app is foregrounded and connected
with both unlocks and current authorization active, it refreshes kept copies
without resetting their open age or either unlock timer. A failed or oversized
refresh keeps the previous copy and
marks it out of date. An unreadable copy can be removed after confirmation;
retry a temporary unlock failure first.

An observed authenticated workspace revocation or a different paired computer
removes the affected downloaded copies, including pins. A network failure alone
does not, and a disconnected phone cannot learn about a remote revocation until
it reconnects. Same-computer endpoint changes retain copies. Drafts, selected
draft context and original recordings are separate and are not removed with a
downloaded copy.

When paired, **Attention inbox** reads the authenticated `/v1/attention`
snapshot through the same pinned receiver identity. It displays only the
closed typed kind, fixed reason, bounded Session or Run identifier, optional
Session name, linked Run or stage, and age. Session names appear above the persistent
ID and survive offline caching. Neovim supplies the chosen name or its local
`session-N` fallback; older stored items without a name still show their ID.
Names are inert, single-line text bounded to 256 UTF-8 bytes. This requires
Attention/receiver interface 2: update the receiver, app and Neovim together.
Local supply withdrawals show the affected package
digest, local endorser label and urgency (`relied_on` or `subject_only`), with a
fixed suggestion to review locally and consider quarantine. Supply Admission
requests show only fixed text and the request reference; profile bytes, Dossier
diffs, paths and Agent prose are never inbox content. Labels are inert text,
and supply cards have accessible headings and grouping. It has no mutation controls; transient network
failures retain the last safely parsed screen, explicitly labelled as cached
and possibly changed. Item age measures the Attention condition, not Session
uptime. Refresh shows **Refreshing inbox…** and disables repeat requests until
the result is displayed. Recording, receiver and inbox controls precede the
cards, and the scrollable page respects system bars and display cutouts.
Transient failures offer refresh, while revoked
pairing or pin failures require pairing/operator repair. The seen generation is
stored only in app-private preferences.

## Attention notifications

The default Android build includes Firebase notification support. Provision the
Firebase Android client through `infra/firebase` and place its generated
`android_firebase_config_json` output in the ignored
`app/src/debug/google-services.json` for the normal app. The build fails with
an actionable missing-configuration error if that file is absent. The private
inbox and recording remain usable without push.

For a config-free offline build, explicitly opt out with
`-PlouiselmFirebase=false`. CI uses this opt-out because Firebase configuration
is private. For QA, supply
`android_qa_sha1_fingerprint` in OpenTofu and use the separate sensitive
`android_qa_firebase_config_json` output at `app/src/qa/google-services.json`.
Follow the [reviewed QA provisioning procedure](https://github.com/euri10/louiselm/blob/main/infra/firebase/README.md#side-by-side-android-notification-qa);
do not replace the normal app registration. Never commit or print either output.
The QA client must use **dev.louiselm.capture.qa** and its signing fingerprint;
a production-package client cannot configure the QA APK.

```sh
# Run from android/, after supplying the configuration for this variant.
./gradlew -PlouiselmFirebase=true :app:assembleQa
```

The build fails if the requested Firebase client configuration is missing or
does not match the package. Keep the ordinary no-configuration gate above in CI.
Install only after checking the package ID and preserving existing data as
described under side-by-side QA. A device with Google Play services and internet
access is required for FCM; Tailscale/private receiver access is separately
required for registration and inbox details.

1. Pair the app to the private receiver, with its notification endpoints enabled.
2. Tap **Enable notifications**, allow Android notification permission (API 33+),
   and allow Nearby devices access when required (API 37+).
3. Keep Tailscale connected while registration reaches the receiver. The receiver
   also needs its configured FCM sender credential; the APK contains no sender key.
4. Trigger a real eligible Attention transition, then background the app. Expect
   only **LouiseLM needs your attention** and fixed generic text. Tap it to open
   and refresh **Attention inbox** over the existing pinned HTTPS connection.

Firebase auto-registration is initially off. Only a successful in-app opt-in and
paired installation registration enable SDK background maintenance. A registration refresh updates
that paired device via authenticated `PUT /v1/attention/installation` with `{"fid":"..."}`; pairing changes
unregister messaging, delete the old Firebase Installation ID (FID), and discard old generation/cache state. WorkManager performs
network-constrained, backoff-based registration and inbox retries, not polling.
Revoked or rejected pairing stops background registration/fetch until pairing is
repaired. Credentials remain Keystore-owned; work requests carry only an opaque
pairing revision and, for SDK callbacks, the FID to compare with the current installation.
Cached inbox data and seen/notified generations are app-private
and scoped to that pairing. Background fetching does not mark anything seen.

The sender uses high-priority, collapsible **data-only** FCM messages with exactly
one decimal `generation` value. It must not send an FCM `notification` block:
Android would display that block in the background before app validation. The
app rejects malformed, duplicate, older, and already-seen wake-ups. It never
puts Session/Run IDs, remote display text, or inbox content into notifications.
Do not use Firebase Console notification campaigns as this protocol's test.

The manifest enables FID mode. The worker uses `register()`/`unregister()` and
`onRegistered()`; callback work for the exact current FID skips `register()` to
avoid a callback/work loop. A stale callback cannot supply the receiver address:
the worker reads it from Firebase Installations and checks it again after registration.
See the [Firebase registration API](https://firebase.google.com/docs/reference/android/com/google/firebase/messaging/FirebaseMessaging).

Update receiver and APK together. The receiver retires stored token addresses while
preserving pairings and confirmed generations. On the first enabled refresh after
the APK update, Android rotates the old installation and registers the new FID.
No re-pairing or capture deletion is needed; push pauses until this registration succeeds.

FCM is best-effort: OS battery policy, connectivity, and force-stop can delay or
prevent delivery. Reopen after force-stop. Neither local gates nor an FCM
submission acknowledgement prove phone delivery; physical acceptance is tracked
in `louiselm-qbr.9.9`.

## Physical acceptance checklist

- Session voice (`louiselm-qbr.12.22`): in the isolated QA app, record the APK
  build, original Session identity and each recording UUID. Check explicit Stop
  and screen lock while recording in both orders, then reopen offline after an
  app restart. Confirm the original plays, its Session association is unchanged,
  an interrupted recording requires a post-unlock choice, and the unavailable
  transcription status is truthful. Confirm ordinary thought capture still
  works and no Session prompt or remote voice job was created.
- FID upgrade (`louiselm-qbr.9.23`): record the APK and receiver builds, upgrade
  both without clearing app data, then reopen/refresh an opted-in pairing.
  Confirm the pairing and one recorded capture UUID survive, push resumes for
  a new generation, and an already-confirmed generation produces no reminder.
  Record actual phone delivery separately from the receiver's submission status.
- Notifications: record the configured APK/package/build and paired test device.
  Deny notification permission first: capture and manual inbox access must remain
  usable. Grant it and enable notifications; confirm receiver installation registration
  without logging the FID. On API 30 verify the notification channel can also
  be disabled without breaking capture.
- Trigger a new eligible Attention generation with the app backgrounded. Verify
  only the fixed generic copy; tap to fetch the matching private inbox. Repeat
  the same/older generation, then a newer one: no duplicate alert, then a new
  alert. Reopen/restart the app and repeat to check durable deduplication.
- Supply Attention (`louiselm-d6fv.8.12`): record the APK and receiver builds.
  Confirm a withdrawal with no live Session shows the exact affected package,
  local endorser and urgency; inspect both urgency levels and a supply Admission
  request. With TalkBack, check card grouping, headings and literal labels.
  The request must show only fixed text and its reference, with no profile or
  Dossier content and no approval/rejection/quarantine controls. Check offline
  restart retains these cards, unchanged state sends no reminders, and every
  notification stays generic. Retained/parked references must keep the withdrawal
  visible until authoritative reconciliation clears it. Automated tests do not
  establish this paired-device acceptance.
- Disconnect Tailscale but leave internet available. Trigger a wake-up: generic
  notification still appears; tapping shows a retryable unavailable inbox, with
  no action resolved. Restore Tailscale and refresh/reopen to consume the
  WorkManager-refreshed private snapshot.
- Revoke pairing and refresh: background registration/fetch must stop. Renew
  pairing and enable again; verify notification delivery resumes for the new
  paired device. Replace the receiver and verify old queued work cannot publish
  an old inbox into the new pairing.
- With a long inbox and large text, scroll to each control and the last card:
  nothing should hide behind the status/navigation bars or a display cutout.
  Press Refresh: **Refreshing inbox…** must appear immediately and stay disabled
  until the result. Offline failure must restore Refresh and label retained
  cards as cached; reconnecting must replace them with current receiver state.
  Dispose an unseen completed Session in Neovim, then refresh: its turn alert
  must disappear while unrelated Attention remains.

- On Android 17, deny Nearby devices access from Pair or Sync; verify offline
  recording still saves. Grant access and retry Pair, Sync, and Refresh. Revoke
  access with pending captures; verify originals remain and Sync resumes after
  access is restored.
- Record while offline, stop, force-stop the app, reopen it, and confirm the
  capture remains queued.
- Start recording, background the app, and confirm `onStop` saves the capture.
- Pair from `:LouiselmCapturePair`, then inspect the same UUID in
  `:LouiselmCaptureInbox`.
- Interrupt an upload, reconnect, and confirm the receiver contains one capture
  with that UUID rather than duplicates.
- Reboot the phone with queued work and confirm WorkManager eventually uploads
  after connectivity returns.
- Revoke the device with `louiselm-capture revoke-device`, record again, and
  confirm upload needs operator attention while the original remains local.
- Scan a fresh QR for that same receiver and choose Renew pairing; verify a
  fresh device registration and successful upload of the retained recording
  under its original UUID. Its local audio and manifest must remain unchanged.
- Renew the receiver certificate with the same key and confirm uploads continue;
  replace the receiver key and confirm the app rejects the new identity.
- Change the configured URL while retaining the receiver key, scan a fresh QR,
  choose Keep pairing, and confirm the endpoint changes without a new device credential or queue
  reassignment.
- With pending captures, scan a QR for a different receiver key. Cancel once
  and confirm ownership is unchanged; confirm once and verify the displayed
  count migrates while every original UUID and audio file remains intact.

Automated tests never use a real microphone, receiver, credential, or network.
