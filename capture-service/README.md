# LouiseLM capture service

`louiselm-capture` is the local owner of immutable speech recordings,
transcription state, pairing credentials, and the authenticated Android upload
boundary. It deliberately does not interpret or delete ideas.

## Commands

```text
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
run park --id UUID --session-id ID --agent NAME --acp-session-id ID --cwd PATH --load-session true --claims ISSUE_IDS --expires-at-ms N
attention list
attention status
```

The default user service binds TLS on `127.0.0.1:7391` and `pair` refuses while
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

## Transcription

Set `OPENAI_API_KEY` to enable the background worker and optionally set
`LOUISELM_TRANSCRIPTION_MODEL` (default `gpt-4o-transcribe`). Transient network,
rate-limit, and server failures use durable exponential backoff. Authentication,
configuration, and rejected-input failures stop until `retry CAPTURE_UUID`.
Errors are sanitized before persistence and responses never include credentials,
audio, prompts, or provider response bodies.

No API key is required for recording, pairing, upload, listing, or retention.

## Durable workflow Parks

The service also owns cold-Parked workflow records. Configure the Beads
workspace in `~/.config/louiselm/capture.env` so the background reaper can
release expired claims:

```text
LOUISELM_BEADS_WORKSPACE=/absolute/path/to/louiselm
```

The systemd unit grants write access to that workspace's `.beads` directory;
rerun `scripts/install-capture-service` after changing the unit or environment,
then restart the user service. `run list` reports only unexpired cold Parks;
expired records remain retained for cleanup retry and forensics.

The Attention store keeps only current unresolved typed conditions. `attention list`
prints its bounded snapshot; `attention status` prints generation, unresolved count,
and fixed-kind counts. `status` includes the same summary and reports a sanitized
storage error if Attention state cannot be read. The local `attention.sock` accepts
only operator-capability mutations and no Agent-authored text. Paired devices can
`GET /v1/attention` with their pairing credential; that route is read-only.

All snapshot consumers reconcile local Run Park conditions against the durable
Run store, including after editor or receiver restart. Resume, disposal, or
expiry removes the stale condition and advances the Attention generation; late
local upserts cannot recreate it. Valid Parks retain their existing inactivity
eligibility. This applies only to the deterministic `run_parked:<Run UUID>`
condition identity emitted by the Neovim controller (SHA-256 truncated to UUID
bytes with v4/variant bits). Broker operation identities and unrelated Session
conditions are untouched; a missing local Run alone is not proof of resolution.
Malformed Run state fails explicitly without erasing Attention. Reconciliation
does not dispose Runs, release claims, or delete their retained records.

Broker projections use the separate `/run/louiselm-attention/project.sock`
endpoint, authenticated by kernel UID and a dedicated producer credential.
It exposes only `project`; the operator socket rejects that verb. From the
repository root, after installing the launcher identities and updated
capture-service user unit:

```sh
sudo python3 scripts/install-broker-attention.py
systemctl --user daemon-reload
systemctl --user restart louiselm-capture.service
```

The command derives the capture identity from the installed operator, creates
the private broker credential and root-owned policies, and provisions a boot
runtime directory through systemd-tmpfiles. It preserves existing credentials
and refuses unexpected ownership or configuration. Without
`/etc/louiselm-capture-broker.json`, the projection endpoint stays disabled and
ordinary Attention/Run/observer behavior remains available. Full provisioning
and authentication details are in `docs/broker-lifecycle.md`.

## Optional Android push

`serve` submits eligible Attention generations independently to every active
paired device with a registered FCM token. The inbox remains available without
push configuration. Enable the sender by setting `GOOGLE_APPLICATION_CREDENTIALS`
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

An authenticated phone registers or rotates its own token with:

```http
PUT /v1/attention/token
Authorization: Bearer <paired-device-credential>
Content-Type: application/json

{"token":"<FCM-registration-token>"}
```

Success returns `204` with no body. The object accepts only `token`, containing
1–4096 visible ASCII bytes; JSON requests are limited to 8 KiB. There is no
caller-selected device ID. One token cannot be registered to multiple paired
devices. Registration, rotation, and revocation share the pairing transaction.
Tokens and submission state are private fields in `pairing/pairing.json`, and
`revoke-device` removes them with the device credential. Revocation waits for an
already-started bounded submission; after it returns, no future submission can
use that pairing. HTTP pairing/authentication runs off the async executor while
waiting for this lock.

The FCM message contains only the required routing token, fixed text
(`LouiseLM` / `Attention is waiting in LouiseLM.`), a decimal-string `generation`,
and fixed Android priority/collapse/tag fields. Both collapse key and notification
tag are `louiselm-attention`. No Attention kind, Session/Run/issue identity, path,
prompt, transcript, or caller-authored display text is serialized to Google.
The authenticated private inbox remains the source of work details.

Delivery passes run once per second and refresh eligibility/generation before
each device's attempt. A confirmed generation is not submitted
again, including after restart or token rotation. Unchanged unresolved state
does not produce reminders. Pending retries use the latest eligible snapshot;
an empty or wholly ineligible inbox causes no submission. Transient failures
use durable exponential backoff starting at 60 seconds, capped at 64 minutes,
and honor a longer numeric or HTTP-date `Retry-After`. Invalid tokens disable
only their registration; a different token re-enables that device. Repeating
the same invalid token does not reset its state.

`status.notifications` is a versioned record containing sender health, a fixed
next action, and each registered device's enabled flag, confirmed generation,
attempt count, and retry deadline. It never contains tokens or raw provider
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
