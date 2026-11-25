# Phone-control wire contract, version 1

This is the frozen interface for louiselm-qbr.12.29 and its initial consumers
(.30 receiver authority, .31 Neovim controller, .32 Android unlock). It specifies
bytes and boundaries, not an implementation or installed acceptance. The
maintainer-confirmed authority decision is louiselm-qbr.12.23. Capture pairing
alone never grants control; capture transports and verifies phone intent, while
Neovim owns Session policy, state and admission. The desktop account and the
enrolled Android app are trusted; this is not isolation from unrestricted
same-user software or a compromised phone.

## Common rules

All network traffic uses the existing paired-device bearer and receiver-key-pinned
HTTPS. A bearer is necessary but insufficient for protected access. All messages
below are UTF-8 JSON objects, including nested objects. `v` is integer `1` and
`type` is the exact string shown. Objects have exactly the listed fields; no
duplicates, unknown fields, implicit defaults, coercions, nulls, or trailing
values are accepted. Unknown versions, message types, action values or enum
values yield `unsupported`; unknown fields and invalid encodings yield
`malformed`. Do not ignore unknown signed fields. Adding fields or changing
semantics requires a new version; no JSON reserialization is signed.

Unless another bound is stated, a JSON body or socket line is at most 262,144
bytes including the newline on a socket, a string at most 8,192 UTF-8 bytes,
and an array at most 50 elements. Check bounds while reading, before allocation
or state changes. Strings must be valid Unicode, without embedded NULs. JSON
integers are nonnegative and at most 9,007,199,254,740,991 (2^53-1); no
fractional or exponent notation. `hexN` means exactly N bytes rendered as
2N lowercase hexadecimal characters, without separators. Identifiers below
are opaque random 16-byte values (`hex16`), generated with a CSPRNG, never
reused within their authority lifetime; they are not caller-selected paths,
local `session-N` values, or necessarily UUIDs. SHA-256 digests are `hex32`.

Public keys are ECDSA P-256 SEC1 uncompressed points: exactly 65 bytes, first
byte `04`, followed by 32-byte big-endian X and Y. Validate the curve and point.
`receiver_identity` is the SHA-256 of the already pinned receiver's DER
SubjectPublicKeyInfo, not of its certificate. The enrolled-key hash is SHA-256
of its exact 65-byte SEC1 point. Android may use SPKI/PKCS#8 internally; neither
private key is transmitted. The permanent private key is non-exportable and
authentication-protected; the working private key exists only in app-process
memory. No biometric/PIN value is transmitted.

Signatures use ECDSA P-256 with SHA-256 over the exact byte transcript described
below and are **ASN.1 DER** `SEQUENCE(INTEGER r, INTEGER s)`, encoded as
lowercase hex, at most 72 bytes. Require a single minimally encoded DER value,
valid positive scalars and no trailing bytes; do not accept 64-byte IEEE P1363
raw signatures. Either valid ECDSA S value is accepted; replay identity is the
signed challenge/operation ID, never signature bytes. Use platform JCA
`SHA256withECDSA` and `aws-lc-rs` `ECDSA_P256_SHA256_ASN1`, not a new primitive.

Every signed transcript starts with ASCII `louiselm-phone-control`, one zero
byte, version byte `01`, and a one-byte message kind. Other fields follow in
the exact listed order, without separators or field names. Hex fields decode to
their fixed bytes. Integers marked `u64` are exactly eight big-endian bytes;
although the encoding is unsigned, their accepted range is the JSON range
above. One-byte fields are exactly one byte. No optional fields or trailing
bytes exist in a transcript.

## Enrollment and unlock over HTTPS

The computer's authenticated local operator starts one pending enrollment for
an already paired device, with a fresh enrollment ID, a CSPRNG-generated
eight-decimal-digit matching code (leading zeroes retained), and a 120-second
monotonic expiry. Only one offer is active per paired device; replacing,
expiring or cancelling it invalidates the previous offer. The bearer identifies
the device; a caller-provided device ID cannot select another device. Limit
failed attempts per offer to five, then invalidate it. Display the code on both
devices and require the operator to compare it and explicitly approve the exact
key, projects, configured Agents and four independent groups on the computer.
No offer, submission, notification, or successful comparison is approval.

All five HTTPS endpoints use POST with these exact request/response objects.
Errors use the outcome object below. `expires_at_ms` is an informational Unix
millisecond timestamp; the computer enforces expiry using its monotonic clock.

| Path | Request | Successful response |
| --- | --- | --- |
| `/v1/phone-control/enrollment/offer` | `{v,type:"enrollment_offer"}` | `{v,type:"enrollment_offer",enrollment_id:hex16,matching_code:string,receiver_identity:hex32,expires_at_ms:integer}` |
| `/v1/phone-control/enrollment/submit` | `{v,type:"enrollment_submit",enrollment_id:hex16,matching_code:string,enrolled_key:hex65}` | `{v,type:"enrollment_pending",enrollment_id:hex16}` |
| `/v1/phone-control/enrollment/status` | `{v,type:"enrollment_status",enrollment_id:hex16}` | `{v,type:"enrollment_granted",enrollment_id:hex16,grant_id:hex16,grant_revision:integer,enrolled_key_hash:hex32,receiver_identity:hex32}` or `{v,type:"enrollment_pending",enrollment_id:hex16}` |
| `/v1/phone-control/challenge` | `{v,type:"challenge",grant_id:hex16,grant_revision:integer}` | `{v,type:"challenge",grant_id:hex16,grant_revision:integer,receiver_identity:hex32,challenge_id:hex16,challenge:hex32,expires_at_ms:integer}` |
| `/v1/phone-control/unlock` | `{v,type:"unlock",proof:Proof}` | `{v,type:"unlocked",grant_id:hex16,grant_revision:integer,period_id:hex16,idle_expires_at_ms:integer,absolute_expires_at_ms:integer}` |

The approved grant is persisted with a nonzero revision and the exact enrolled
key and scope. Scope has four independent groups: `read`, `operate_sessions`,
`control_runs`, `saved_results`. Approved project roots and configured Agent
names are chosen locally; no phone-supplied cwd, executable, environment or
role is a scope grant. A revision change invalidates old proofs and periods.
Re-enabling requires fresh local approval. Disable workspace keeps capture;
Forget phone revokes both.

A challenge is 32 fresh random bytes plus a fresh challenge ID, bound to the
paired device, grant revision and receiver identity; it expires after 120
seconds, is single-use, and a newer challenge replaces the outstanding one for
that grant. Only the computer may issue or consume it. A proof succeeds only
against that live, unconsumed challenge and current grant, atomically consuming
the challenge. Invalid proofs must not start a period. Disconnection or loss
of challenge state fails closed. New periods get a fresh random period ID.

`Proof` has exactly `grant_id:hex16`, `grant_revision:integer`,
`enrolled_key_hash:hex32`, `receiver_identity:hex32`, `challenge_id:hex16`,
`challenge:hex32`, `working_key:hex65`, and `signature:hex` (DER as above).
The permanent key signs kind `01` followed by:

```text
grant_id[16] | grant_revision:u64 | enrolled_key_hash[32] |
receiver_identity[32] | challenge_id[16] | challenge[32] | working_key[65]
```

The trusted computer enforces five minutes of actual phone-interaction
inactivity and a one-hour absolute period limit. Background polling, output,
sync, failed requests and replay never refresh inactivity. A valid explicit
interaction may refresh only an unexpired period and never the absolute limit.
Locking the phone or process death destroys the working key; brief app switching
does not reset either timer. Recheck revocation, scope and both deadlines before
protected delivery and admission, serialized with their owning state changes.
Already admitted work is not rolled back. The owner verifies the proof and
request; an `unlocked` or `approved` Boolean from the receiver is not authority.

## Signed request and first payload

`POST /v1/phone-control/request` accepts `{v,type:"request",request:Request}`.
`Request` has exactly `grant_id:hex16`, `grant_revision:integer`,
`period_id:hex16`, `receiver_identity:hex32`, `controller_id:hex16`,
`subject_id:hex16`, `action:integer`, `payload:hex`, `payload_hash:hex32`,
`operation_id:hex16`, `expected_revision:integer`, `interaction:integer`, and
`signature:hex`. The working key signs kind `02` followed by:

```text
grant_id[16] | grant_revision:u64 | period_id[16] | receiver_identity[32] |
controller_id[16] | subject_id[16] | action:u8 | payload_hash[32] |
operation_id[16] | expected_revision:u64 | interaction:u8
```

`payload_hash` is SHA-256 of the decoded exact payload bytes and must match
before any protected delivery or effect. `interaction` is `0` for background
and `1` for an actual explicit phone interaction, not a poll or callback. It is
signed but does not independently establish current authority. `operation_id`
is fresh per logical request and is reserved before admission: a second use is
replay, including a differently signed request. Durable write-outcome recovery
and its permanent replay markers remain louiselm-qbr.12.24/.33, not this task.
The operation ID is also the request/response correlation ID.

Version 1 defines only action `1`, `read_session`, requiring the `read` group.
Its payload is exactly `before:u64 | limit:u16`, both unsigned big-endian:
`before=0` requests the newest page; otherwise it is an exclusive sequence
cursor returned by the same live subject. Limit is 1–50. The owner returns entries
in ascending sequence order. `expected_revision=0` asks for current state;
otherwise it must equal the current subject revision. The controller and
subject IDs identify one exact live controller and Session incarnation, not a
reusable ACP or local Session ID. A lost/restarted controller, disposed subject,
reused local ID, wrong controller, or mismatched nonzero revision cannot select
a replacement. Other actions need a defined versioned schema before use.

## Local controller channel

Follow the existing `attention_socket`/`run_socket` pattern: the receiver owns
an exclusively locked Unix listener, a mode-0600 socket in a private directory,
stale-socket collision checks, and a protected owner-only operator capability.
The connection is newline-delimited UTF-8 JSON, bounded as above; partial or
oversize frames close the connection. The capability file uses the existing
operator capability lifecycle and is never logged. This same-user channel is
not a claim of isolation against other unrestricted desktop-user processes.

The first client message is exactly `{v,type:"attach",capability:string}`;
the receiver validates the capability before registering the connection and
replies `{v,type:"attached",controller_id:hex16}`. A fresh random controller
ID belongs only to that connection; disconnect, disposal or receiver restart
invalidates it and all of its subjects. Reattach always allocates a new ID;
no new computer approval is needed for a still-approved grant. There is at most
one outstanding read per connection, so even a closed error response has an
unambiguous request. The attach capability is the existing 36-character
canonical lowercase UUID form. Neovim gives
each live Session a fresh random `subject_id`, never reuses it after disposal,
and checks its actual registry and configured Agent/project on every read.
Matching a phone navigation hint to that incarnation belongs to integration
(.1); an old Attention item or bare ACP ID alone is not proof of identity.

For a read, receiver sends the controller exactly
`{v,type:"read_session",controller_id:hex16,grant:Grant,period:Period,proof:Proof,request:Request}`.
`Grant` is `{grant_id:hex16,grant_revision:integer,enrolled_key:hex65,projects:[string],agents:[string],groups:[string]}`;
projects are locally approved canonical absolute real paths, agents are exact
configured names, and groups are the four values above, each list unique and
bounded to 50. The controller derives the current Session's actual working
directory and Agent from its registry, resolves the directory to its canonical
real path, and requires the exact approved root or a descendant at a path-segment
boundary and an exact Agent match. Failure to resolve is `unavailable`, not a
permissive match. `Period` is
`{period_id:hex16,challenge_id:hex16,idle_expires_at_ms:integer,absolute_expires_at_ms:integer}`.
The grant and period are the receiver's current trusted local records, not
phone-provided claims. The receiver forwards the original proof, request and
payload unchanged. The controller verifies both signatures, key hash, receiver,
grant/revision, challenge-to-period binding, controller, subject, action,
payload digest and its own scope/state; the receiver has already consumed the
challenge and reserved the operation. The receiver rechecks its live period and
revocation immediately before protected delivery, regardless of displayed
timestamps. The authenticated connection and current local records supply
freshness; these timestamps or a Boolean alone never prove phone authentication.
Unavailable or ambiguous current state fails closed. Neither verification nor
the local transport grants authority to answer an ACP permission request.

The controller runs `louiselm-capture phone-control verify` asynchronously,
passing exactly one `read_session` object on standard input. The command loads
the local receiver key identity from the capture state directory and prints
exactly `{v:1,type:"verified"}` or the closed error object followed by a newline;
both are exit status zero. Internal I/O failures use a nonzero status. Missing
receiver identity is `unavailable`; verification never creates an identity.
This checks the signed bindings only. The controller still checks its own live
controller, subject, project, Agent and state, and the receiver must still
check current grant, lease and replay state before delivery or admission.

Success is `{v,type:"session_page",controller_id:hex16,subject_id:hex16,operation_id:hex16,revision:integer,status:string,entries:[Entry],next_before:integer,complete:boolean}`.
The receiver returns this same closed object over HTTPS after the final authority
recheck. `revision` is nonzero and increases on any projected state/content
change within the subject. Status is one of `starting`, `ready`, `admitting`,
`configuring`, `preparing`, `prompting`, `running`, `waiting_permission`,
`cancelling`, `error`, `disposed`. A disposed subject returns `stale_subject`
instead of a page. `next_before=0` means no older page; otherwise it is the
exclusive sequence cursor. Sequence values are positive, unique, increasing
within this subject, and never identify entries of a replacement subject.
An unknown cursor returns `stale_subject`. To keep a multi-page read on one
snapshot, use the first page's nonzero revision in subsequent requests.
`complete=false` means projected content for this page or its current state is
unavailable or omitted; additional older pages alone do not make it false.
Return fewer than `limit` entries if necessary to keep the whole response
within the byte bound; a nonzero cursor must advance. Never include unsubmitted
desktop drafts/Staged context, ambient files, raw ACP objects, arbitrary RPC,
Lua, shell or environment data.

Every `Entry` contains exactly the fields of one variant:

- `{kind:"message",seq:integer,role:string,text:string,truncated:boolean}`;
  role is `user` or `assistant`.
- `{kind:"tool",seq:integer,id:string,title:string,status:string,content:string,truncated:boolean}`;
  status is `pending`, `in_progress`, `completed`, `failed`, or `unknown`.
- `{kind:"question",seq:integer,request_id:string,question:string,options:[Option],multi_select:boolean,truncated:boolean}`.
  Each `Option` is exactly `{label:string,description:string,preview:string}`;
  empty strings represent absent descriptions/previews. Options may be empty.
- `{kind:"plan",seq:integer,request_id:string,text:string,truncated:boolean}`.

An entry's strings and arrays obey the common limits. If the owner must shorten
content to represent a single entry, it cuts only on a UTF-8 boundary and sets
`truncated=true`; never silently present shortened content as complete. Missing
or unsupported content must be indicated, not represented as an approval or a
successful action. Older history, richer projections and write operations are
separate slices and do not authorize exposing raw owner objects.

## Outcomes and fixtures

All failures on either transport are exactly `{v,type:"error",code:string}`;
there is no remote error text or additional field. The closed code set is:

| Code | Meaning |
| --- | --- |
| `unauthenticated` | Missing/invalid bearer, signature or key, altered digest, consumed challenge or replayed operation. |
| `expired` | Challenge or working period timed out. |
| `revoked` | Pairing, grant or working authority was explicitly revoked. |
| `out_of_scope` | Current grant lacks the project, Agent or required group. |
| `stale_subject` | Controller/Session incarnation or expected revision no longer matches. |
| `unavailable` | Required owner, live state, storage or local channel cannot be established. |
| `malformed` | Invalid shape, duplicate/unknown field, size, number, key encoding or DER. |
| `unsupported` | Unknown version, type, action, enum or unavailable optional integration. |

Where multiple refusals apply, callers must not infer hidden protected state
from the chosen code. Return no protected content before full validation.

The shared test-only [fixture](../android/app/src/test/resources/phone-control-v1.json)
contains fixed private keys, exact bytes, signatures and valid/negative
contexts. **Never use those keys outside tests.** The proof vector was signed
with the JVM JCA provider; the request vector with `aws-lc-rs`; both Rust and
Android JVM suites verify both, rebuild the transcripts, and exercise altered
bytes/payload, wrong keys, stale challenge, cross-subject and replay. Random
fresh signatures are also exercised. The tests model freshness contexts only;
they do not claim that a production authority, real Keystore, local socket or
physical Android flow exists. Those are the downstream slices and .22.
