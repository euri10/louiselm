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
below are UTF-8 JSON objects, including nested objects. `v` is integer `1`
except for the version-2 Session page, action history responses and local
approval below; `type` is the exact string shown.
Objects have exactly the listed fields; no
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

### Local project administration

These messages use the authenticated desktop-only administration socket and
the existing operator capability. They are not phone endpoints and grant no
Run execution authority. Root and project IDs are private `hex16` identities.

- `{v:1,type:"project_roots",capability:string,after:hex16-or-empty-string}`
  returns `{v:1,type:"project_roots",roots:[{id:hex16,path:string}],next_after:hex16-or-empty-string}`.
  Roots are sorted by ID, at most four per page; the cursor is the last ID
  returned when more remain. Paths are local operator metadata, never phone
  list or push content. A listed root may be historical.
- `{v:1,type:"projects",capability:string,after:hex16-or-empty-string}`
  returns `{v:1,type:"projects",projects:[{id:hex16,path:string,root_ids:[hex16]}],next_after:hex16-or-empty-string}`.
  Projects are sorted by ID, at most four per page; paths and root IDs are
  private local operator metadata. Historical paths are not current identity proof.
- `{v:1,type:"register_project",capability:string,path:string,root_ids:[hex16]}`
  returns `{v:1,type:"project_registered",project_id:hex16}`. The operator
  explicitly selects the project and one or more current roots; each root must
  still be the recorded directory and contain the selected project. This
  records identity relationships only; source/checkout binding and current
  phone authorization remain independent requirements.
- `{v:1,type:"link_project_roots",capability:string,project_id:hex16,root_ids:[hex16]}`
  returns `{v:1,type:"project_roots_linked",project_id:hex16}`. Each selected
  root and the project must still be the recorded current directories, and the
  root must contain the project. This explicit action can make the project's
  historical Runs available under already-approved root scope; it does not add
  an Agent, action permission or source/checkout proof.
- `{v:1,type:"freeze_run_source",capability:string,project_id:hex16,run_id:uuid,snapshot:string,includes:[string]}`
  runs the installed optional source integration against that currently verified
  original project and returns `{v:1,type:"run_source",run_id:uuid,project_id:hex16,source_id:hex16,snapshot_digest:string,base_digest:string,base_commit:string}`.
  The output must be new beneath an owner-controlled parent; selected file paths
  are explicit local inputs and never appear in the reply. The retained record
  attests the owner-observed freeze only, not a reviewed checkout, Run admission,
  registration, or phone authority. A missing integration returns `unsupported`.
  On an uncertain outcome inspect the original operation and output before retrying.
- `{v:1,type:"inspect_run_source",capability:string,run_id:uuid}` returns the
  same `run_source` record, including after the original project disappears. A
  damaged or absent record is unavailable; a retained record is not proof that
  the snapshot files remain present or unchanged.
- `{v:1,type:"prepare_run_checkout",capability:string,run_id:uuid,source_id:hex16,checkout:string}`
  materializes the exact retained source into a new separate checkout and returns
  `{v:1,type:"run_checkout",run_id:uuid,project_id:hex16,source_id:hex16,checkout_id:hex16,snapshot_digest:string,head:string,reviewed:false}`.
  The output is a new owner-controlled directory on branch `run/UUID`; the
  integration rechecks the complete source inventory. Creation is not review,
  publication, admission, or permission. Stale source IDs and duplicate outcomes
  refuse; inspect the original outcome before retrying an uncertain request.
- `{v:1,type:"inspect_run_checkout",capability:string,run_id:uuid}` returns the
  retained `run_checkout` record, not a claim that its checkout remains current
  or reviewed. Neither request is a phone endpoint.
- `{v:1,type:"publish_prepared",capability:string,run_id:uuid,source_id:hex16,checkout_id:hex16,selection:string,registration_id:hex16-or-empty-string,expected_revision:integer}`
  explicitly publishes one reviewed, owner-private selection. A new item uses an
  empty registration ID and revision zero; republication names the exact ID and
  current nonzero revision. Success is
  `{v:1,type:"prepared_registered",registration_id:hex16,run_id:uuid,revision:integer}`.
  The owner rechecks the retained source and clean checkout, snapshots the exact
  private file, and checks that the Run UUID has not already been admitted.
  Publication does not reserve that UUID, Start a Run, approve a capability or
  make the snapshot phone-readable by itself.
- `{v:1,type:"list_prepared",capability:string}` returns
  `{v:1,type:"prepared_list",items:[{id:hex16,run_id:uuid,revision:integer}]}`.
  This local-only listing is bounded to 50 items and contains no instructions or
  paths. It is not a phone authorization decision.
- `{v:1,type:"inspect_prepared",capability:string,registration_id:hex16,revision:integer}`
  returns `{v:1,type:"prepared_inspection",registration_id:hex16,published:boolean,revision:integer,binding:RunBinding,selection:string}`
  for the exact retained revision, including after unpublication. `RunBinding`
  is `{run_id:uuid,project_id:hex16,source_id:hex16,checkout_id:hex16,agents:[string]}`.
  The selection is the private exact UTF-8 snapshot and may contain instructions
  and local paths. This endpoint is desktop-only, not a protected phone detail.
- `{v:1,type:"open_prepared",capability:string,registration_id:hex16,revision:integer}`
  returns `{v:1,type:"prepared_waiting",registration_id:hex16,revision:integer,binding:RunBinding,selection:string}`
  only for the current published revision with an unused Run identity and its
  retained source/checkout chain. This private desktop response permits the
  operator to construct a waiting controller; it does not start a Run, register
  a live controller, approve access, or guarantee the checkout is still clean.
  Any later Start must independently recheck the selection and native authority.
- `{v:1,type:"unpublish_prepared",capability:string,registration_id:hex16,expected_revision:integer}`
  returns `{v:1,type:"prepared_unpublished",registration_id:hex16,revision:integer}`
  only for the current published revision. It retains all private revisions and
  does not cancel an admitted Run. These owner operations require Run support;
  publishing, listing, inspection and opening also return `unsupported` if the optional
  skills integration is absent. None opens a controller or changes an Attention.
- `{v:1,type:"register_root",capability:string,path:string}` returns
  `{v:1,type:"root_registered",root_id:hex16}`. This explicitly records a
  fresh local root identity without granting any phone access.
- `{v:1,type:"inspect_grant",capability:string,device_id:string}` returns
  `{v:1,type:"grant_inspection",device_id:string,grant_id:hex16,grant_revision:integer,scope:Scope,scope_digest:hex32}`.
  The computer operator reviews the exact displayed paths, Agents and groups.
- `{v:1,type:"confirm_legacy_project",capability:string,device_id:string,grant_id:hex16,grant_revision:integer,scope_digest:hex32,project_id:hex16}`
  returns `{v:1,type:"legacy_project_confirmed",device_id:string,project_id:hex16}`.
  It confirms one retained project for this exact legacy path-only grant after
  the operator reviews the displayed scope. A changed grant, revision or scope
  refuses the confirmation; a currently paired device and Read group are
  required. A retained project-to-root relationship may be used after the
  original project directory disappears, but a matching root must still be the
  recorded current directory. Confirmation does not re-pair or grant an action.
- Local enrollment approval is version 2:
  `{v:2,type:"approve",capability:string,device_id:string,enrollment_id:hex16,enrolled_key:hex65,scope:Scope,project_root_ids:[hex16]}`.
  `Scope` is exactly `{projects:[string],agents:[string],groups:[string]}`.
  Empty `project_root_ids` creates a fresh identity for each selected project
  root. Otherwise it has one existing root ID per project in the same order;
  each recorded root must still match the selected directory. Success is
  `{v:2,type:"enrollment_approved",device_id:string,grant_id:hex16,grant_revision:integer,project_root_ids:[hex16]}`.
  Local errors use the version-1 closed error object below. Legacy path-only
  grants do not gain Run scope by being loaded or matched by path.

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
interaction may refresh only an unexpired period and never the absolute limit;
its idle deadline starts at the interaction, not delayed response delivery.
Both limits include computer suspend. An unavailable or inconsistent Linux
boot clock refuses workspace authority instead of extending it.
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
replay, including a differently signed request. Write-operation identities and
their replay protection persist before forwarding. The operation ID is also the
request/response correlation ID.

Version 1 defines action `1`, `read_session`, requiring the `read` group.
Its payload is exactly `before:u64 | limit:u16`, both unsigned big-endian:
`before=0` requests the newest page; otherwise it is an exclusive sequence
cursor returned by the same live subject. Limit is 1–50. The owner returns entries
in ascending sequence order. `expected_revision=0` asks for current state;
otherwise it must equal the current subject revision. The controller and
subject IDs identify one exact live controller and Session incarnation, not a
reusable ACP or local Session ID. A lost/restarted controller, disposed subject,
reused local ID, wrong controller, or mismatched nonzero revision cannot select
a replacement. Other actions need a defined versioned schema before use.

Action `3`, `send_prompt` version 1, requires `operate_sessions`. Its payload
is exactly `01 | text:utf8`: one version byte followed by 1–4095 bytes of valid
UTF-8 without NUL. The phone explicitly sends the reviewed text; no desktop
draft, staged context or skill selection is implied. `interaction` must be `1`,
`expected_revision` must be nonzero, and the controller and subject must be
nonzero exact incarnations. The owner rechecks that revision, current authority
and Session readiness before admission. The signed request and payload digest
bind the entire action; accepting this schema alone grants no authority or
indicates that a Send was admitted, dispatched or completed.

Action `48`, `list_controllers`, requires `read`. Its signed controller and
subject IDs are all-zero 16-byte values: this receiver-wide discovery request
has no Session target. The payload is `after_controller_id[16] | limit:u8`;
the all-zero cursor starts at the first page and limit is 1–50.
`expected_revision` is zero. Success is the closed object
`{v,type:"controller_list",operation_id:hex16,controllers:[hex16],next_after:hex16-or-empty-string}`.
The opaque IDs identify currently attached controllers, sorted ascending;
`next_after` is the last returned ID only when another remains. They reveal no
Session metadata or authority. A subsequent list request is signed for the
exact controller. No attached controllers means `unavailable`.

Action `49`, `list_sessions`, requires `read`. The signed controller ID is
the exact attached incarnation and the subject ID is all zeroes. The payload
is `after_subject_id[16] | limit:u8`; the all-zero cursor starts at the first
page and limit is 1–50. `expected_revision` is zero. The controller verifies
the original phone proof and lists only live Sessions in the approved project
and Agent scope, using its actual registries. Success is the closed object
`{v,type:"session_list",controller_id:hex16,operation_id:hex16,sessions:[SessionSummary],next_after:hex16-or-empty-string}`.
Each `SessionSummary` is exactly `{subject_id:hex16,name:string,agent:string,
project:string,status:string,truncated:boolean}`. Name, Agent and canonical
working directory labels are each bounded to 512 UTF-8 bytes; shortening is
indicated by `truncated`. Status uses the `session_page` values except
`disposed`. Subjects are sorted ascending; `next_after` is the final returned
subject only when another authorized Session remains. Refresh from the first
page to discover new Sessions. A row's controller and subject IDs are an exact
navigation target, not a reusable registry-local ID. A missing or replaced
controller and a disposed Session cannot select a replacement. The receiver
rechecks pairing, grant, period and controller before protected delivery.
For either action `interaction` may be one only for an explicit phone action;
automatic page continuation and background reads use zero.

Action `52`, `list_prepared`, requires `read`. The controller and subject IDs
are all zeroes; `expected_revision` is zero. Its payload is exactly the
16-byte exclusive registration-ID cursor; zero starts from the beginning.
Success is `{v:1,type:"prepared_page",request_id:hex16,items:[PreparedSummary],next_after:hex16-or-empty-string}`,
where each `PreparedSummary` is `{registration_id:hex16,run_id:uuid,revision:integer,state:"prepared"}`.
Each page contains at most one eligible item and no instructions or paths. A
nonempty page always returns that item's ID as `next_after`; request another
page until an empty page returns an empty cursor. This avoids disclosing whether
other phones' registrations exist. Results are live, ordered by registration ID;
refresh from zero to discover later publications. The owner checks the current
Read grant and complete project/Agent scope for each returned row and holds its
Run and publication locks through final authorization. There is no per-phone
registration list, no live-controller requirement, and no Start or approval.

Action `53`, `read_prepared`, requires `read`. The controller ID is all zeroes,
the subject ID is the exact `hex16` registration ID, the payload is empty, and
`expected_revision` is its nonzero published revision. This owner-backed read
does not require a live controller. Success is
`{v:1,type:"prepared_detail",request_id:hex16,registration_id:hex16,run_id:uuid,revision:integer,detail:PreparedDetail}`.
The closed detail is `{state:"prepared",agent:string,beads:[{id:string,prompt:string}],execution:Execution,max_sessions:integer,verification_plan_digest:string,bead_scope:BeadScope,provider_requests:ProviderScope,skill_requests:SkillScope,skills:Skills,network:string}`.
`Execution` is `{kind:"duration",duration_ms:integer}` or
`{kind:"stop_by",expires_at_ms:integer}`. `BeadScope` is exactly
`{project_digest:string,role:"coordinator",issue_ids:[string],effects:[{kind:"comment_add"}],max_mutations:integer,expires_at_ms:0}`.
`ProviderScope` is `{disclosure_profile:string,provider:string,upstream:string,addresses:[string],models:[string],max_effort:string,max_run_requests:integer,expires_at_ms:0}`.
`SkillScope` is `{enabled:boolean,agents:[string],allow_run:boolean,allow_supply:boolean}`;
disabled scope has an empty Agent list and both flags false. `Skills` is
`{mode:"off",view_digest:string}` or
`{mode:"enabled",generation_digest:string,view_digest:string}`; network is
`denied` or `brokered`. There are three to five reviewed Beads and the complete
private selection is bounded to 65,536 bytes. Prompts are exact and can contain
sensitive text or paths; local cache, journal, checkout and selection-file paths
are not included. The owner rechecks pairing, the current working period and
Read grant, original-project and every contributing Agent scope, exact retained
source/checkout identities, unused Run UUID, and published revision while
serializing against unpublication and admission. Missing integration is
`unsupported`; a stale, unpublished or already-admitted item is refused. A read
does not open a controller, admit a Run, activate its deadline or grant a capability.

Action `2`, `read_action_history`, also requires `read`. Its decoded payload is
one of `00 | operation_id[16]` (one record), `01 | limit:u8` (first page), or
`02 | after_operation_id[16] | limit:u8` (next page), with limit 1–50. For one
record, the signed controller and subject IDs identify the original
incarnations, even after their owner disconnects. For pages both IDs are 16 zero
bytes. `expected_revision` and `interaction` must both be zero. The request has
its own fresh operation ID. It never forwards to Neovim or renews the working
period. Capture-service checks the current pairing, grant, lease, signature,
replay guard and each retained operation's locally established project and
Agent scope before delivery. A replacement phone with a matching current read
grant may discover and inspect history; a revoked or out-of-scope phone cannot.

One-record success is `{v:2,type:"action_history",request_id:hex16,record:ActionRecord}`.
A page is `{v:2,type:"action_history_page",request_id:hex16,records:[ActionRecord],
next_after:hex16-or-empty-string}`. `request_id` is the lookup's operation ID.
Records are ordered by their original operation IDs; `next_after` is the last
returned ID when another authorized record remains, otherwise the empty string.
Restart at the first page to discover records added before a prior cursor.

`ActionRecord` is the closed object `{operation_id:hex16,receiver_identity:hex32,
controller_id:hex16,subject_id:hex16,action:integer,payload_hash:hex32,
grant_id:hex16,grant_revision:integer,expected_revision:integer,
recorded_at_ms:integer,updated_at_ms:integer,state:string,
evidence:hex32-or-empty-string,accepted_evidence:hex32-or-empty-string,
dispatched_evidence:hex32-or-empty-string,completed_evidence:hex32-or-empty-string}`.
Its operation ID is the original write.
State is `unknown`, `accepted`, `dispatched`, `completed`, `rejected`, or
`fenced_no_effect`. Empty evidence is used only for `unknown`; other states
carry the native owner's
opaque evidence digest. Acceptance does not mean dispatch; dispatch records
local transport acceptance, not peer receipt. Completion is a successful
native Session callback. The archive retains each known phase's evidence even
when the current outcome becomes unknown.
`fenced_no_effect` may be archived only after the native owner has proved no
effect and durably fenced late execution. Capture-service neither supplies
that proof nor authorizes a new attempt. Missing or damaged evidence returns
`unavailable`, never a claim that the operation was not sent. The journal holds
no payload or result copy. Its size is bounded; exhaustion refuses new evidence
instead of evicting existing records.

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
(.1). A Session Attention item may carry a `phone_control` object containing
exactly `controller_id:hex16` and `subject_id:hex16`, captured from that Session
when the condition was emitted. It is a navigation hint, not read authority.
An item without this hint, or one whose controller or subject has disappeared,
cannot be rebound by its ACP or local Session ID. Opening it does not resolve
Attention; the protected response supplies the current Session state.

The Neovim client connects to `louiselm/workflow/phone-control.sock` beneath
`LOUISELM_CAPTURE_STATE_DIR`, or beneath `XDG_STATE_HOME` (default
`~/.local/state`) when that override is unset. The adjacent
`operator-capability` is the existing shared capability. The integration can
resolve a registry-owned Session with `require("louiselm").phone_control_subject(session)`;
the returned controller and subject identities are valid only for the current
connection and become unavailable on disconnect.

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

Success is the version-2 object `{v:2,type:"session_page",controller_id:hex16,subject_id:hex16,operation_id:hex16,revision:integer,status:string,entries:[Entry],next_before:integer,complete:boolean,source:HistorySource}`.
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
`HistorySource` is exactly `{replay:string,archive:string,redaction:string,missing:boolean,truncated:boolean}`.
`replay` is `not_applicable` or `unverified`; ACP replay does not establish
completeness. `archive` is `unavailable` until an archive is bound by its
owner. `redaction` is `present` when exclusions are known, otherwise `unknown`;
unknown does not assert that no redaction occurred. `missing` identifies
unsupported or missing source material, and `truncated` identifies entries
shortened on this page. A complete page cannot have unverified replay, known
redaction, missing content or truncation. Clients combine page-local omissions
when assembling a snapshot and show the limitations with the content.
Return fewer than `limit` entries if necessary to keep the whole response
within the byte bound; a nonzero cursor must advance. An omitted entry can make
the page empty, or put the cursor before its first visible entry; such a page
sets `source.missing` and remains usable for the next bounded read. Never include unsubmitted
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

## Native action journal, version 1

The existing local preparation and commit frames below are not a proven
crash-safe admission protocol. HTTP action 3 remains unsupported until the
guard, native transition and real-socket proof in louiselm-qbr.12.4.5 pass.
A journal begin, receiver response or timeout is neither Session admission
nor proof of no effect; the conditional design is louiselm-qbr.12.4.8.

Before reserving a verified write, capture may ask its attached controller for
the target's locally owned scope with the closed message
`{v:1,type:"action_scope",controller_id:hex16,subject_id:hex16,operation_id:hex16}`.
The response is `{v:1,type:"action_scope",controller_id:hex16,subject_id:hex16,
operation_id:hex16,project:string,agent:string}`. The controller resolves the
project from the exact live Session and rechecks the incarnation after path
resolution. This owner-only lookup carries no prompt and grants no action or
content authority; the receiver still checks the current grant and durably
reserves the write before forwarding it.

After that reservation, the receiver forwards the signed `send_prompt` frame
with the same fields as `read_session`. The controller verifies the signatures,
current Session and scope, and durably begins the exact owner operation. It
then replies `{v:1,type:"action_prepared",controller_id:hex16,subject_id:hex16,
operation_id:hex16}` without prompting. While serializing revocation with this
admission, the receiver rechecks the current pairing, grant and monotonic lease,
then sends `{v:1,type:"action_commit",controller_id:hex16,operation_id:hex16,
remaining_ms:integer}` over that same authenticated connection. The remaining
time is bounded by both lease deadlines and is never an extension. The owner
subtracts all monotonic time since it sent `action_prepared`, rechecks its
exact Session and revision, and only then calls `Session:prompt`.

The owner replies `{v:1,type:"action_result",operation_id:hex16,state:string,
code:string,evidence:hex32}` only after its evidence is durable. State is
`accepted` with an empty code, or `fenced_no_effect` with `expired`,
`stale_subject`, `busy`, or `unavailable`. Acceptance is not completion. A
disconnect, missing response, or failure to persist the owner report remains
unknown. These local messages are not phone endpoints, and a commit is valid
only for its prepared operation on the same live controller connection.

`louiselm-capture phone-control owner-journal` is a local storage helper for
the trusted native owner, not an HTTPS action or a source of phone authority.
It reads one bounded JSON object on stdin and returns one JSON object. Its
separate journal lives under `louiselm/workflow/phone-action-owner` in the
capture state root; it is not capture-service's action-history archive. The
caller must authenticate the request and establish the exact live owner,
canonical project and configured Agent independently.

The owner journal and capture archive each require a separate receiver-key-bound
guard in `louiselm/capture/tls`. It records the store generation and current
snapshot digest outside the store and its lock. A missing or damaged guard,
missing store or lock, or an older store with a current guard is unavailable;
none initializes an empty history. Initialization and operator recovery are
not yet available, so existing receivers without these records also refuse
action evidence. An interrupted update completes on restart only when its
guard and staged snapshots prove the exact update and preserve prior identities
and owner evidence; otherwise it remains unavailable. Capture-only work and
otherwise authorized phone reads do not depend on this archive.

The closed input is `{v:1,type:"begin",identity:ActionIdentity}`,
`{v:1,type:"inspect",identity:ActionIdentity}`,
`{v:1,type:"inspect_phases",identity:ActionIdentity}`,
`{v:1,type:"fence",identity:ActionIdentity,at_ms:integer}`, or
`{v:1,type:"report",identity:ActionIdentity,outcome:OwnerOutcome,at_ms:integer}`.
`ActionIdentity` contains exactly `operation_id`, `receiver_identity`,
`controller_id`, `subject_id`, `action`, `payload_hash`, `grant_id`,
`grant_revision`, `period_id`, `expected_revision`, `project`, `agent` and
`recorded_at_ms`, using the bounds and encodings above. The project and Agent
come from the native owner, never phone claims. `OwnerOutcome` is exactly
`"unknown"`, `{accepted:{evidence:hex32}}`, `{dispatched:{evidence:hex32}}`,
`{completed:{evidence:hex32}}`, `{rejected:{evidence:hex32}}`, or
`{fenced_no_effect:{evidence:hex32}}`. The fence form is permitted only after
the living native owner proves that a begun attempt had no effect and cannot
execute later; its durable begin prevents another begin but does not invalidate
an already outstanding native gate. The owner reports only a fact it has
established. No payload or prompt is stored.

Begin returns `{v:1,type:"begun"}` only after durable reservation, or
`{v:1,type:"existing",outcome:ExistingOutcome}` for an existing exact identity.
`ExistingOutcome` is `"unknown"` or an `OwnerOutcome`. A repeated begin never
permits another effect.
Inspect returns the same `existing` outcome for an exact identity without
creating a journal or reserving an absent operation. Missing or damaged records
return `unavailable`; different bindings return `conflict`. The reservation's
`recorded_at_ms` is retained metadata, not a request binding.

`inspect_phases` returns the closed object
`{v:1,type:"action_phases",outcome:OwnerOutcome,accepted_evidence:hex32-or-empty-string,dispatched_evidence:hex32-or-empty-string,completed_evidence:hex32-or-empty-string}`.
Each nonempty digest is retained owner evidence. It has the same exact-binding
and missing-store refusals as `inspect`. A later unknown never erases earlier
phase evidence or permits a no-effect fence after acceptance.

Fence returns `{v:1,type:"fenced",evidence:hex32}` only after it wins
reservation and the fence is durable; an already begun operation returns
`{v:1,type:"uncertain"}`. An existing fence returns its original evidence.
An `at_ms` before `recorded_at_ms` or beyond the integer bound is malformed and
does not reserve an operation. Report returns
`{v:1,type:"recorded"}` only after the owner outcome is durable. Errors are
`{v:1,type:"error",code:string}`, with `malformed`, `conflict` or `unavailable`.
The owner must route every effect through begin for a winning fence to prove
that late delivery cannot execute; missing or damaged storage is never proof.

Capture's `reserve_session_prompt` checks retained unknown records and reserves
the new identity in one durable transaction. It accepts an exception only for
an operation the native owner has proved independent of this exact prompt.
Different controller, Session or operation IDs alone are not such proof;
missing or unavailable evidence cannot supply it.
