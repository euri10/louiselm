# LouiseLM capture service

`louiselm-capture` is the local owner of immutable speech recordings,
transcription state, pairing credentials, and the authenticated Android upload
boundary. It deliberately does not interpret or delete ideas.

## Explicit capability selection

`serve` starts only capabilities explicitly set to `true` in its environment.
Configure the installed user service through `~/.config/louiselm/capture.env`:

```sh
LOUISELM_ATTENTION_ENABLED=true
LOUISELM_RUNS_ENABLED=false
LOUISELM_RECEIVER_ENABLED=false
LOUISELM_PHONE_CONTROL_ENABLED=false
LOUISELM_TRANSCRIPTION_ENABLED=false
LOUISELM_PUSH_ENABLED=false
```

This Attention-only example creates its local store and authenticated socket,
without recording storage, Run storage, TLS, pairing, transcription or push.
All choices default to false. With none enabled, `serve` refuses startup
before creating state. An installed tool, network profile or credential never
enables a capability. Invalid switch values are errors.

Enable Runs separately and set `LOUISELM_BEADS_WORKSPACE` and the absolute
`LOUISELM_REAL_BR` executable for retained-claim cleanup. Disabling Run support
while undisposed Runs remain is refused; settle those Runs first. Toggles never
delete durable records or pairing state.

Enable the receiver separately before the phone setup below. Transcription
requires its own opt-in and `LOUISELM_CAPTURE_OPENAI_API_KEY`; it sends pending
audio to the OpenAI API. Push requires its own opt-in, Attention, and
`GOOGLE_APPLICATION_CREDENTIALS`; pairing credentials alone never authorize
outbound delivery. Explicit one-shot CLI commands such as `transcribe-once`
remain deliberate operator actions.

Install or update the binary manually using `scripts/install-capture-service`
from the checkout, inspect the example environment file, then start or restart
the service at a safe boundary. Plugin setup and health never run the installer
or restart the service. Editor choices (`attention.enabled`, `capture.enabled`,
`workflows.enabled`) enable client integrations and cannot control a shared daemon.

## Commands

Commands use clap-generated help. Run `louiselm-capture --help` or append
`--help` to a subcommand, such as `louiselm-capture run reserve --help`.
Help needs no configured storage. Unknown flags, duplicate single-value options,
invalid values and extra positionals fail with exit 1 before operational effects;
diagnostics do not repeat supplied arguments. Arguments after `run generate --`
are forwarded to Beads unchanged.

`louiselm-capture --version` reports the compiled Cargo version. `metadata`
returns bounded package and interface identity without opening any state.
Plugin clients prefix commands with `--require-interface=1`; incompatible
interfaces fail before mutations. Run and Attention socket snapshots advertise
the same identity. See `docs/releases.md` for exact capture download pins and
safe service updates; release numbers need not match the plugin version.

```text
metadata
--version
configure-network --profile lan|overlay|private --bind IP:PORT --url HTTPS_URL
serve
pair
revoke-device DEVICE_UUID
retry-notifications
ingest-local --file PATH --recorded-at-ms N --duration-ms N --mime TYPE [--id UUID]
list
status
retry CAPTURE_UUID
transcribe-once
run list
run park --id UUID --session-id ID --agent NAME --acp-session-id ID --cwd PATH --load-session true --claims ISSUE_IDS
attention list
attention status
phone-control start --device-id DEVICE_UUID
phone-control inspect --device-id DEVICE_UUID
phone-control cancel --device-id DEVICE_UUID --enrollment-id HEX16
phone-control approve --device-id DEVICE_UUID --enrollment-id HEX16 --enrolled-key HEX65 --project /absolute/project --agent AGENT --group read
phone-control project-roots [--after HEX16]
phone-control projects [--after HEX16]
phone-control register-root --path /absolute/root
phone-control register-project --path /absolute/project --root-id HEX16
phone-control link-project-roots --project-id HEX16 --root-id HEX16
phone-control freeze-run-source --project-id HEX16 --run-id UUID --snapshot /private/new-snapshot [--include relative-file]
phone-control inspect-run-source --run-id UUID
phone-control prepare-run-checkout --run-id UUID --source-id HEX16 --checkout /private/new-checkout
phone-control inspect-run-checkout --run-id UUID
phone-control publish-prepared --run-id UUID --source-id HEX16 --checkout-id HEX16 --selection /private/selection.json [--registration-id HEX16 --expected-revision N]
phone-control list-prepared
phone-control inspect-prepared --registration-id HEX16 --revision N
phone-control unpublish-prepared --registration-id HEX16 --expected-revision N
phone-control inspect-grant --device-id DEVICE_UUID
phone-control remove-action-details --operation-id HEX16
phone-control approve-legacy-scope --device-id DEVICE_UUID --grant-id HEX16 --grant-revision N --scope-digest HEX32 --enrolled-key HEX65 --project /absolute/project --agent AGENT --group read --confirm-complete-scope
phone-control confirm-legacy-project --device-id DEVICE_UUID --grant-id HEX16 --grant-revision N --scope-digest HEX32 --project-id HEX16
phone-control recover-legacy-project --device-id DEVICE_UUID --grant-id HEX16 --grant-revision N --scope-digest HEX32 --project-id HEX16 --discard-damaged-confirmations
phone-control disable --device-id DEVICE_UUID
phone-control forget --device-id DEVICE_UUID
```

Phone control also requires `LOUISELM_PHONE_CONTROL_ENABLED=true` and the
receiver opt-in. Its routes and local sockets are absent when unset. Pairing a
device only enables capture: on the computer, start enrollment for that paired
device, compare the eight-digit code with the phone, inspect the submitted
public key, then approve that exact key and the intended project roots, Agent
names and permission groups. Repeat `--project`, `--agent` and `--group` as
needed; groups are `read`, `operate_sessions`, `control_runs` and
`saved_results`. Approval returns private root IDs. To give another phone an
existing root identity, repeat `--project-root-id` in the same order as
`--project`; the recorded directories must still match. `project-roots` and
`projects` page local identities, `register-root` records an identity without
granting access, and `register-project` explicitly records a project under
selected roots. To relate a later-approved root to the same project, explicitly
use `link-project-roots` while both directories still match their recorded
identities. This can make that project's historical Runs visible to phones
already granted the root; it grants no new Agents or action groups.
Legacy path-only grants need computer-side scope re-approval before they can
unlock the workspace. Capture pairing stays active. Run `inspect-grant` and
review the device, receiver and enrolled key; its displayed old scope is
unverified and must not be treated as evidence of the original selection.
Use `approve-legacy-scope` with the displayed grant ID, revision, digest and
key, and explicitly supply the complete intended projects, Agents and groups.
Repeat each option as needed; an omitted list is empty. The required
`--confirm-complete-scope` affirms that all three lists are deliberate. The
response shows the canonical approved scope and whether prior individual
project confirmations remain valid. This preserves the phone's existing grant
and pairing, ends any old working period, and needs no new pairing. If the
inspection changes before approval, inspect and review again.

If the grant store is unreadable, structurally invalid, or has an invalid base
scope seal, capture and pairing remain available, but phone control returns
`unavailable` for every phone. The service logs a warning and leaves the
damaged store untouched.
With the receiver reachable, `revoke-device` or `phone-control forget` still
revokes a phone's pairing; workspace-only `disable` cannot safely edit an
unreadable store. Offline `revoke-device` currently reports unconfirmed and
does not change pairing, because the admission cutoff cannot yet be proved.

To reset an unrecoverable store, the desktop operator must stop the receiver,
move `louiselm/capture/phone-control/grants.json` under the capture state root
(normally `~/.local/state`) to a new private filename without overwriting an
existing file, and restart the receiver. Keep the damaged file for inspection;
do not restore unverified or stale grants. This explicit reset discards all
workspace approvals, not capture pairings or recordings. Each phone needs a
fresh enrollment, key comparison and complete scope approval before control
resumes. A phone whose pairing was revoked must also be paired again.

For protected Run reads, confirm each exact legacy project separately with
`confirm-legacy-project` after scope re-approval. Existing intact project
confirmations are retained. Scope re-approval does not repair damaged project
confirmations. If `inspect-grant` reports `project_confirmations_valid=false`,
review the approved base scope and the exact project from `projects`, then use
`recover-legacy-project` with the current grant ID, revision, scope digest,
selected project ID, and `--discard-damaged-confirmations`. This explicitly
replaces the entire untrusted confirmation list for that phone with just the
selected project. Use `confirm-legacy-project` for any additional projects.
It does not change the base scope or pairing. After an uncertain response,
inspect again; recovery refuses once the confirmation list is valid.

Before any grant-store write while confirmations are damaged, the service
retains the original bytes in a private `grants-damaged-*.json` file alongside
`grants.json`; if preservation fails, the write is refused. These snapshots
can contain sensitive local paths and are never used as authority. Keep or
remove them deliberately after review; never restore an unverified snapshot.
If `forget` reports `unavailable` because the grant write failed, pairing
revocation is still attempted; inspect device status before retrying.
Project confirmation alone does not make Runs phone-readable; source/checkout
binding remains required.
For contained Run preparation, `freeze-run-source` explicitly freezes a selected
original project using the optional root-owned installed `louiselm-skills` and
records that event for the exact new Run UUID. The snapshot must be a new path
under an owner-controlled parent; its contents can be sensitive. Repeat
`--include` only for reviewed relative files. Missing skills integration is
`unsupported` and does not disable ordinary Sessions. After an uncertain result,
inspect the original Run source and output before retrying; never reuse an
existing Run or infer a binding from the snapshot path. This source proof alone
does not establish a reviewed checkout, publish a draft, admit a Run, or grant
phone access.
`prepare-run-checkout` materializes that exact frozen snapshot into a new
independent checkout on `run/UUID`, retaining its identity and initial HEAD.
It remains explicitly unreviewed: the operator must review the exact checkout
and selection before any later publication. Inspection after restart reports
the retained creation record, not whether the checkout is still present or
unchanged. An uncertain result must be inspected before trying again.
After reviewing the selection and checkout, the operator can explicitly
`publish-prepared` a private mode-0600 selection. It snapshots the file and
retains its exact revision even if that file is later changed or deleted.
Republication requires the current registration ID and revision; unpublication
hides it while retaining the private snapshot. Inspection can print sensitive
instructions and paths to the local terminal. The protected receiver can read
an exact registered revision and discover eligible registrations with current
phone scope. Android can browse these inactive drafts and inspect their exact
protected instructions and limits while unlocked. Neither operation starts a
Run or opens a waiting controller.
The local `open-prepared` command loads only the current published revision for
an explicit waiting controller; its private output can contain instructions and
paths. Loading it creates no controller and grants no authority.
The local owner-only `admit-prepared` seam binds the exact published selection
and its SHA-256 to a new durable Run before Session attachment. Its response
contains a sensitive generated-work token. This is accounting and provenance,
not Start or broker authority. A controller opened from a registered selection
uses it after broker authorization and before attaching its first Session.
Protected durable Run reads now expose bounded accounting and lifecycle for an
owner-bound Run under current project and complete Agent scope, even without a
live controller. Android can browse these Runs or open an exact Run Attention
item to inspect its durable accounting and Park state. Controller status and
linked Session navigation are not yet connected.
`disable` revokes workspace access while retaining capture;
`forget` revokes both. Re-enabling needs a new enrollment and approval. The
Neovim controller must use the same capture state directory as the receiver
and opt in with `phone_control = { enabled = true }`. With Attention enabled,
a Session card can open its bounded current conversation after the
phone unlocks. An old controller or Session identity fails closed; opening a
card neither resolves Attention nor admits a Session action.

Phone action evidence is retained without automatic expiry in the private
capture state directory, without prompts or result copies. A currently
authorized Read grant can inspect scoped history while Neovim is offline;
history checks do not renew the working period. Missing or damaged evidence is
unavailable, not proof that an action was never sent. The first phone write
still requires the separate Send integration and its native-owner safeguards.

To remove the archived phase details of one positively completed ordinary
phone Send, run the following command while the receiver is running. The
operation ID is in phone action history.

```sh
louiselm-capture phone-control remove-action-details --operation-id HEX16
```

Pending, uncertain and other action types are refused.
The permanent identity and duplicate guard remain, and authorized history
shows that details were removed. This neither authorizes another Send nor
deletes the native owner's journal or conversation.

On a trusted personal desktop, phone-control startup establishes the receiver's
native journal, capture archive, and shared admission barrier once. An existing
approved phone and its project and Agent scope are retained across ordinary
service and editor restarts; startup does not grant a new scope or operating
period. A legacy read-only enrollment is transitioned once. If initialization
is interrupted, or an established store, its guard, the pairing state, or the
barrier is missing or inconsistent, phone Send stays unavailable and the
remaining evidence is preserved. Capture and authorized phone reads remain
available where their own state is healthy. Restoring older local state does not
prove that a previously admitted action had no effect. Do not re-enable phone
Send after a known restore until the separate recovery procedure is available;
a coordinated rollback of local evidence cannot be detected from that evidence.

If the phone-admission barrier is missing, damaged, or replaced, preserve the
receiver state, owner journal, and capture archive. Do not recreate its lock
files or clear the history. There is currently no supported barrier-recovery
command. A service restart or an unarmed reboot does not prove a safe cutoff.
A newly reviewed attempt at affected work requires the native owner to prove
no effect and durably fence delayed delivery, as well as current authority.

When explicitly enabled, the receiver binds TLS on `127.0.0.1:7391` unless
configured otherwise, and `pair` refuses while
that safe, phone-unreachable default is active. Configure exactly one private
profile, then restart the service and pair:

```sh
# Home LAN only: captures made away from home stay queued on the phone.
louiselm-capture configure-network --profile lan \
  --bind 192.168.1.20:7391 \
  --url https://192.168.1.20:7391

# Private overlay: use the address reported by externally managed Tailscale or
# WireGuard. LouiseLM detects the shared 100.64.0.0/10 range as private but does
# not install or manage the overlay.
louiselm-capture configure-network --profile overlay \
  --bind 100.100.20.30:7391 \
  --url https://desktop.example.ts.net:7391

systemctl --user restart louiselm-capture.service
louiselm-capture status
louiselm-capture pair
```

`private` is the explicit advanced profile for unusual private networks. All
profiles reject wildcard binds, public bind addresses, and literal public
advertised IPs. Public internet exposure, port forwarding, simultaneous
listeners, cloud discovery, and relay are intentionally unsupported.

The profile is stored at
`~/.config/louiselm/capture-network.json` (under `XDG_CONFIG_HOME` when set), so
`serve`, `pair`, and `status` cannot drift onto separate bind and advertised
addresses. `pair` prints a ten-minute, one-use QR payload containing the
receiver URL, stable public-key identity, and pairing token. Pairing returns a
revocable device credential; only hashes of tokens and credentials are
persisted by the receiver. Renewing the TLS certificate with the same persisted
receiver key does not require re-pairing; replacing the key does.

`status` reports the selected profile, bind and advertised URL, whether that
configuration is phone-reachable, the paired-device count, and each device's
last successful delivery time. A paired device with only the loopback default
is reported as degraded. The bounded, unauthenticated `/v1/health` response
contains only `status` and the public receiver-key identity so an already
paired phone can verify a new endpoint before saving it.

`louiselm-capture pairing-status` reads the existing paired-device summaries
through the on-demand systemd host service. It accepts aliases of the registered
capture state directory and refuses another directory or an unavailable service.
It does not create a missing pairing registry. The service requires the supported
host setup described below. The query does not authorize phone Send or recovery;
the existing `status` and capture commands do not depend on this service.

Android uploads stream to a private temporary file, are limited to 20 MiB,
must carry their original SHA-256 digest, and become visible only after an
atomic directory rename. Retrying the same complete record and UUID is safe;
changing audio or metadata under an existing UUID is rejected. Both newly
created and retry-safe existing uploads update the authenticated device's
durable delivery timestamp only after canonical storage succeeds.

## Local data

With default XDG paths:

```text
~/.local/share/louiselm/captures/<uuid>/
  audio.wav | audio.m4a | audio.ogg | audio.webm
  capture.json       immutable manifest
  state.json         mutable transcription state
  transcript.json    immutable successful transcript, when available

~/.local/state/louiselm/capture/
  pairing/pairing.json
  tls/receiver-cert.pem
  tls/receiver-key.pem
  uploads/

~/.local/state/louiselm/workflow/
  attention/attention.json
  attention/attention.lock
  attention.sock
  operator-capability
```

Directories and files are forced to owner-only permissions on Unix. Original
audio is never deleted automatically. A successful Neovim local ingest removes
only its temporary recorder file after the canonical store has synced it.

Override XDG roots for tests or deployments with
`LOUISELM_CAPTURE_CONFIG_DIR`, `LOUISELM_CAPTURE_DATA_DIR`, and
`LOUISELM_CAPTURE_STATE_DIR`. These overrides are roots; the service still
appends `louiselm/...`.

The [phone host reference candidate](https://github.com/euri10/louiselm/blob/main/docs/phone-host-reference.md) provides
fixture provisioning and diagnostics for a separate managed profile. It is not
required for the trusted personal-desktop initialization described above.
Recovery of lost or ambiguous admission evidence remains unavailable without
the separate recovery proof.

## Transcription

Set `LOUISELM_TRANSCRIPTION_ENABLED=true` and supply
`LOUISELM_CAPTURE_OPENAI_API_KEY` to enable
the background worker, and optionally set
`LOUISELM_TRANSCRIPTION_MODEL` (default `gpt-transcribe`). Transient network,
rate-limit, and server failures use durable exponential backoff. Authentication,
configuration, and rejected-input failures stop until `retry CAPTURE_UUID`.
Errors are sanitized before persistence and responses never include credentials,
audio, prompts, or provider response bodies.

If the enabled worker has a missing or blank key, or a blank configured Model,
`serve` reports transcription unavailable and keeps other configured capabilities,
including ordinary capture upload, running. No voice job is admitted until the
configuration is repaired and the service restarts. A transcription-only service
with invalid configuration exits with an error.

The default follows OpenAI's
[file-transcription guide](https://developers.openai.com/api/docs/guides/speech-to-text),
checked 2026-10-14: `gpt-transcribe` transcribes completed recordings through
`/v1/audio/transcriptions`. OpenAI's
[deprecations page](https://developers.openai.com/api/docs/deprecations) schedules
the former default, `gpt-4o-transcribe`, for removal on 2027-02-26.
An explicit Model override is passed through unchanged, including a pinned
version; empty values make transcription unavailable. Updating the binary does not replace a Model
set in an existing `capture.env`. Remove that setting to follow the default,
or explicitly select a supported file-transcription Model. Top-level, `serve`,
and `transcribe-once` help list the default and environment choices.

Set up a separate OpenAI project for capture transcription before supplying the
key. In that project's settings:

1. Create a project-owned service account for `louiselm-capture`, rather than a
   personal or organization-wide key. Give it a custom project role with only
   **Model capabilities: Request**; remove any default Member or Owner role.
   Create a **Restricted** service-account key with that permission and every
   other permission set to **None**. Do not add
   Models Read or Files Write: the worker only posts a multipart audio file to
   `/v1/audio/transcriptions`; it never lists models or uses `/v1/files`. If the
   service-account creation flow issues an unrestricted key first, restrict it
   in the project's API Keys settings before installing it. A key's permissions
   cannot exceed its owner's project role.
2. Under **Project settings → Limits → Spend**, set a monthly spend limit and
   enable **Enforce a hard limit**; an alert alone does not stop requests. For
   the default model, estimate `$0.0045 × expected audio minutes/month`, then
   round up about twice that amount for a working cap (for example, $5 for
   roughly 500 minutes/month). Review the price and limit when changing models
   or usage. Enforcement can lag slightly, so this is not an exact ceiling.
3. Set an expiry date and schedule replacement before it. Create the replacement
   with the same restrictions, install it as
   `LOUISELM_CAPTURE_OPENAI_API_KEY` in the service's private environment, and
   restart the active capture service at a safe point.
   Verify one short real capture reaches `completed` for its UUID in
   `louiselm-capture list`, then revoke the old key. Do not copy that command's
   transcript output into an issue. Never put key bytes in an issue, command
   argument, log, or transcript.

There is no free positive probe for this minimal key: `GET /v1/models` needs a
separate permission and does not exercise transcription. A few seconds of test
audio makes one tiny paid request. Before enabling the worker, check
`louiselm-capture status`: it immediately processes all pending captures and
due retries, which can make the first test larger than one request. A too-narrow
or invalid key yields an authentication/permission failure; that capture
remains failed until the key is corrected and
`louiselm-capture retry CAPTURE_UUID` is run. A reached hard
spend limit instead returns HTTP 429 and follows the worker's transient retry
path; fix the limit or wait for its monthly reset before retrying.

Permission and spend-limit basis, checked 2026-10-01: OpenAI's
[permission guide](https://developers.openai.com/api/docs/guides/rbac) assigns
`/v1/audio` to Model capabilities Request, and its
[spend-limit guide](https://developers.openai.com/api/docs/guides/spend-limits)
describes hard enforcement. Model capabilities Request also covers
other model endpoints; the separate project and enforced cap bound that broader
permission. Recheck the dashboard labels and these sources when rotating.

Transcription pricing, checked 2026-10-14: the
[pricing page](https://developers.openai.com/api/docs/pricing) lists
`gpt-transcribe` at $0.0045/minute.

The installed systemd unit reads `capture.env` directly; it does not source
`~/.zsh.secrets` or expand a value like `$LOUISELM_CAPTURE_OPENAI_API_KEY`.
Put the capture key's literal value in the owner-only `capture.env`. Pointing
the unit at a shell secrets file containing other keys would expose those keys
to the capture process too.

No API key is required for recording, pairing, upload, listing, or retention.

## Durable workflow Parks

When Run support is enabled, the service owns cold-Parked workflow records.
Configure its cleanup prerequisites in `~/.config/louiselm/capture.env` so the
background reaper can release expired claims:

```text
LOUISELM_RUNS_ENABLED=true
LOUISELM_BEADS_WORKSPACE=/absolute/path/to/louiselm
LOUISELM_REAL_BR=/absolute/path/to/br
```

The systemd unit grants write access to that workspace's `.beads` directory;
rerun `scripts/install-capture-service` after changing the unit or environment,
then restart the user service. `run list` reports only unexpired cold Parks;
expired records remain retained for cleanup retry and forensics.
Local Run observer snapshots include `load_session`: null until known, then the
attached Agent's advertised capability. A true value does not guarantee that
history is available or that cold recovery is lossless.

The Attention store keeps only current unresolved typed conditions. `attention list`
prints its bounded snapshot; `attention status` prints generation, unresolved count,
and fixed-kind counts. `status` includes the same summary and reports a sanitized
storage error if Attention state cannot be read. The local `attention.sock` accepts
only operator-capability mutations and no Agent-authored prose. Session items may
carry an optional inert `session_name` (nonblank, single-line, at most 256 UTF-8
bytes), supplied by the trusted editor controller from the chosen name or local
`session-N` fallback. This label never changes item identity or generic push
payloads. It is stored with the condition; subsequent events use the current name.
Attention and receiver interfaces are revision 2; deploy the editor, receiver and
Android app together. Stored items without a name remain valid. Paired devices can
`GET /v1/attention` with their pairing credential; that route is read-only.

All snapshot consumers reconcile local Run Park conditions against the durable
Run store, including after editor or receiver restart. Successful resume, disposal, or
expiry removes the stale condition and advances the Attention generation; late
local upserts cannot recreate it. Valid Parks retain their existing inactivity
eligibility. Pending or failed resume preserves the condition and generation
until successful finalization, disposal or expiry. Another editor rediscovering
the same unresolved Park retains its original creation time, so discovery alone
cannot trigger a fresh notification. A new Park after resolution still creates
a new condition. This applies only to the
deterministic `run_parked:<Run UUID>`
condition identity emitted by the Neovim controller (SHA-256 truncated to UUID
bytes with v4/variant bits). Broker operation identities and unrelated Session
conditions are untouched; a missing local Run alone is not proof of resolution.
Malformed Run state fails explicitly without erasing Attention. Reconciliation
does not dispose Runs, release claims, or delete their retained records.

Broker projections use the separate `/run/louiselm-attention/project.sock`
endpoint, authenticated by kernel UID and a dedicated producer credential.
It exposes only `project`; the operator socket rejects that verb. From the
repository root, after installing the launcher identities and capture binary:

```sh
sudo python3 scripts/install-broker-attention.py
# Switch only after retained Runs and active recordings are safe.
systemctl --user disable --now louiselm-capture.service
sudo systemctl daemon-reload
sudo systemctl enable --now louiselm-capture.service
```

The command derives the capture identity from the installed operator, creates
the private broker credential and root-owned policies, and provisions a boot
runtime directory through systemd-tmpfiles. It also installs a system unit using
the shipped hardening and the installed operator's numeric UID/GID, home, binary
and `capture.env`. The process stays unprivileged. System-manager filesystem
hardening preserves actual root and broker UIDs; a hardened user unit can map
them to overflow UID 65534 and cannot authenticate this deployment. No UID is
reinterpreted as root or as the broker.

Provisioning never starts, stops or restarts a service. Do not run both units
against the same state. After switching, manage the service with
`sudo systemctl … louiselm-capture.service`, without `--user`, including binary
updates from `scripts/install-capture-service`. Verify the enabled endpoint is
ready after starting; `Type=simple` startup alone is not readiness. The operator's
home must be an absolute path containing only letters, digits, `/`, `-`, `_` and
`.`; custom service paths or write allowances need explicit administrator review.

The provisioner preserves existing credentials
and refuses unexpected ownership or configuration, including an existing system
unit whose bytes differ. Without
`/etc/louiselm-capture-broker.json`, the projection endpoint stays disabled and
ordinary Attention/Run/observer behavior remains available. Full provisioning
and authentication details are in `docs/broker-lifecycle.md`.

## Optional Android push

For Neovim-created Attention items, delivery becomes eligible after 30 seconds
without keyboard/mouse input, paste input, or focus returning to Neovim. Each
`nvim_paste()` call, including scripted calls and streamed chunks, restarts the
delay; paste content is not inspected or retained by Attention. This delay is
shared across Sessions: actively using another Session postpones delivery,
while Agent output and programmatic cursor movement do not. Viewing a completed
Session, starting its next prompt, or disposing it clears its pending turn alert. Eligibility
is distinct from FCM submission and eventual phone delivery.

A later plugin that replaces `vim.paste` without delegating to the installed
handler bypasses paste activity detection; Attention does not overwrite it.

With push explicitly enabled, `serve` submits eligible Attention generations to every active
paired device with a registered Firebase Installation ID (FID). The inbox remains available without
push configuration. Enable the sender with `LOUISELM_PUSH_ENABLED=true` and
`LOUISELM_ATTENTION_ENABLED=true`, and set `GOOGLE_APPLICATION_CREDENTIALS`
in the service's `capture.env` to an external Google **service-account JSON key
file**, then restart the service. On Unix, the key file must be owner-only
(`0600`); its parent directory should also be private. The sender uses that
file's `project_id` and requires `cloudmessaging.messages.create` on the target
project. Key provisioning and Android receiving code are separate work.

The adapter supports Google's standard `googleapis.com` service-account
format, with the fixed OAuth endpoint `https://oauth2.googleapis.com/token`.
It does not run ADC discovery, metadata-server requests, external-account
commands, or URLs supplied in credentials. OAuth access tokens remain in memory
and expire within an hour. The RSA signing, base64, and HTTP-date parsers reuse
packages already present in the lockfile; there is no Firebase Admin SDK.

An authenticated phone registers or rotates its own installation with:

```http
PUT /v1/attention/installation
Authorization: Bearer <paired-device-credential>
Content-Type: application/json

{"fid":"<Firebase-Installation-ID>"}
```

Success returns `204` with no body. The object accepts only `fid`, containing
22 URL-safe base64 characters; JSON requests are limited to 8 KiB. There is no
caller-selected device ID. One FID cannot be registered to multiple paired
devices. Registration, rotation, and revocation share the pairing transaction.
FIDs and submission state are private fields in `pairing/pairing.json`, and
`revoke-device` removes them with the device credential. Revocation waits for an
already-started bounded submission; after it returns, no future submission can
use that pairing. This applies to confirmed revocation; an offline unconfirmed
result leaves the pairing and registration active. HTTP pairing/authentication
runs off the async executor while waiting for this lock.

The data-only FCM message contains only the HTTP v1 `message.fid` routing field, a
decimal-string `generation`, high Android priority, and collapse key
`louiselm-attention`. It deliberately has no `notification` block: Android's
background SDK display would bypass local generation validation and deduplication.
The app owns the fixed generic text and local notification tag. No Attention kind, Session/Run/issue identity, path,
prompt, transcript, or caller-authored display text is serialized to Google.
The authenticated private inbox remains the source of work details.

Delivery passes run once per second and refresh eligibility/generation before
each device's attempt. A confirmed generation is not submitted
again, including after restart or installation rotation. Unchanged unresolved state
does not produce reminders. Pending retries use the latest eligible snapshot;
an empty or wholly ineligible inbox causes no submission. Transient failures
use durable exponential backoff starting at 60 seconds, capped at 64 minutes,
and honor a longer numeric or HTTP-date `Retry-After`. An unregistered FID disables
only its registration. After SDK registration succeeds, an authenticated phone
may register that same FID again to re-enable delivery; confirmed generations
remain suppressed. Repeating an enabled registration preserves its backoff.

Registry schema 3 retires schema-2 token addresses on first open, under the pairing
lock and with atomic persistence. It preserves pairing credentials, sender health
and confirmed generations but disables old targets until Android registers a FID.
Update the receiver and APK together; the old token endpoint is removed. Capture
storage and pairing QR formats are unchanged. The upstream contract is documented
in [FCM HTTP v1 Message](https://firebase.google.com/docs/reference/fcm/rest/v1/projects.messages#Message).

`status.notifications` is a versioned record containing sender health, a fixed
next action, and each registered device's enabled flag, confirmed generation,
attempt count, and retry deadline. It never contains FIDs or raw provider
errors. Authentication/configuration failures persist across restart and stop
automatic attempts. Correct the key or permissions, restart `serve`, then run
`retry-notifications` to clear the stop. No configured key means health is
`unconfigured`, and no Google request occurs.

The sender persists a retry deadline **before** each request and the confirmed
generation after acceptance. A crash or connection failure between Google's
acceptance and the local confirmation can leave the result uncertain, so this
is not an exactly-once delivery guarantee. Such retries use the collapse key;
Android must additionally deduplicate generations. Each HTTP request has a
10-second total timeout and does not follow redirects. The worker owns pending
requests and joins its bounded pass when the serving endpoints stop.

The implementation follows Google's [HTTP v1 authentication flow](https://firebase.google.com/docs/cloud-messaging/send/v1-api),
[service-account assertions](https://developers.google.com/identity/protocols/oauth2/service-account),
and [FCM retry/error contract](https://firebase.google.com/docs/cloud-messaging/error-codes).
Automated checks use disposable signing keys and a fake HTTP transport; they do
not contact Google. Physical Android wake-up and Tailscale acceptance belong to
`louiselm-qbr.9.9` after the Android receiver in `louiselm-qbr.9.7` is implemented.

## Operator lifecycle

```sh
louiselm-capture status
louiselm-capture list
louiselm-capture retry <capture-uuid>
louiselm-capture revoke-device <device-uuid>
systemctl --user restart louiselm-capture.service
```

There is intentionally no delete command in this first slice. Removing a
capture is an explicit filesystem/retention decision until a safe product-level
retention UX exists.
