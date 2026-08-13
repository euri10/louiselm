# LouiseLM capture service

`louiselm-capture` is the local owner of immutable speech recordings,
transcription state, pairing credentials, and the authenticated Android upload
boundary. It deliberately does not interpret or delete ideas.

## Commands

```text
serve --bind IP:PORT
pair --url HTTPS_URL
revoke-device DEVICE_UUID
ingest-local --file PATH --recorded-at-ms N --duration-ms N --mime TYPE [--id UUID]
list
status
retry CAPTURE_UUID
transcribe-once
```

The default user service binds TLS on `127.0.0.1:7391`. Set
`LOUISELM_CAPTURE_BIND` explicitly to a LAN address before pairing a phone.
`pair` prints a ten-minute, one-use QR payload containing the receiver URL,
exact certificate fingerprint, and pairing token. Pairing returns a revocable
device credential; only hashes of tokens and credentials are persisted by the
receiver.

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
`LOUISELM_CAPTURE_DATA_DIR` and `LOUISELM_CAPTURE_STATE_DIR`. These overrides
are roots; the service still appends `louiselm/...`.

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
