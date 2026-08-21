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
ingest-local --file PATH --recorded-at-ms N --duration-ms N --mime TYPE [--id UUID]
list
status
retry CAPTURE_UUID
transcribe-once
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

Android uploads stream to a private temporary file, are limited to 20 MiB,
must carry their original SHA-256 digest, and become visible only after an
atomic directory rename. Retrying the same complete record and UUID is safe;
changing audio or metadata under an existing UUID is rejected.

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
