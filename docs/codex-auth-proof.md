# Codex subscription authentication feasibility

`louiselm-qbr.5.1.3.4`, observed 2026-09-18:
**BLOCKED_SUBSCRIPTION_BRIDGE**. The external-token route fails broker-only
custody when used in the confined app-server. A separate broker-owned Codex
authentication helper is a candidate, not an accepted subscription bridge.
The authentication decision now lives in `louiselm-qbr.5.1.3.10`, split from
`louiselm-ow3ok` after its separate sender-guard decision; production requests in
`louiselm-qbr.5.1.3.2` remain blocked. This result does not establish that every
stock-Codex integration is impossible.

## Confirmed production contract

The maintainer confirmed `louiselm-qbr.5.1.3.10` on 2026-09-18:
production requires documented support for login, refresh and broker-originated
subscription requests. Legacy token export remains research-only, even with
an executable pin and passing regression tests. Reusable credentials remain
broker-owned and outside every confined Session; ordinary Codex login and the
existing subscription stay untouched.

If no supported bridge can be established, Verified Codex remains unavailable
while ordinary Codex continues working. Closing the design question establishes
that requirement and fallback, not a supported bridge. Authentication proof
`louiselm-qbr.5.1.3.4` and guarded-composition proof `louiselm-qbr.5.1.3.9`
remain prerequisites for production requests. Vocabulary delta: No change.

## Measured boundary

The opt-in experiment uses the installed standalone `codex-cli 0.153.4`:

- Executable: `/home/lotso/.codex/packages/standalone/releases/0.153.4-x86_64-unknown-linux-musl/bin/codex`
- SHA-256: `56ef98ab4032d317ab26e9b5e5a175650717351edb16ed9cde0cb6d1734d62da`
- API: app-server stdio JSON-RPC, v2 `account/*`, `thread/start`, `turn/start`,
  plus the older `getAuthStatus` method. Experimental capability is enabled
  for external-token login.
- Installed adapter package: `@agentclientprotocol/codex-acp` 1.10.0;
  `dist/index.js` SHA-256
  `4602784c5896fbf05a7d89b09655bacc768d0bf281e0d03a10333ff81da45268`.
- Separately inspected adapter checkout: `/home/lotso/code/codex-acp`, commit
  `effb0fe670a49dfbb5071764b5f8a2e3c09e2393`. This is source evidence, not
  proof that its files reproduce the installed bundle.

`src/CodexJsonRpcConnection.ts:15` starts app-server; `src/index.ts:79`
selects `CODEX_PATH` when provided, otherwise the adapter uses bundled Codex.
The experiment explicitly selects the measured standalone binary; it does not
certify the executable selection of every configured Agent or execute the full
LouiseLM → acp-proxy → codex-acp chain.

## Documented support and its limits

[OpenAI's authentication guide](https://learn.chatgpt.com/docs/auth) distinguishes
ChatGPT subscription login from API-key billing. File storage belongs under
Codex's selected home; keyring storage is another option. A separate broker
identity and fresh home can separate new sign-in state from the operator's
ordinary login. That alone does not establish a broker request API.

The [app-server auth API](https://learn.chatgpt.com/docs/app-server#auth-endpoints)
documents managed browser/device login, automatic refresh, cancellation and
logout. External-token mode instead accepts a host-provided access token and
asks the host to refresh it after an authorization failure. `account/read`
returns account metadata; forcing refresh there applies to managed mode.

The [managed-auth CI guide](https://learn.chatgpt.com/docs/auth/ci-cd-auth)
requires serialized ownership of each auth cache and retention of refreshed
state. It explicitly excludes generic OAuth clients and external-token host
integrations. Thus a broker must not assume that concurrent helpers can safely
share a refresh token, or implement an undocumented OAuth client from this guide.
Its copy-existing-cache recipe is outside this task's fresh-sign-in contract.

The current auth guide and app-server page do not establish a supported raw
subscription Responses transport for our broker. Neither a declared `planType`
nor acceptance of a synthetic JWT proves entitlement or billing. The adapter's
`DEFAULT_OPENAI_BASE_URL` is `https://api.openai.com/v1`; its provider listing
cannot prove where a ChatGPT-authenticated stock Codex request actually goes.
No Provider limits endpoint was called.

## Reproduce the installed-runtime experiment

```sh
python3 scripts/probe-codex-auth.py \
  /home/lotso/.codex/packages/standalone/releases/0.153.4-x86_64-unknown-linux-musl/bin/codex
```

Requires Linux, Bubblewrap and Python's standard library. The executable hash
is mandatory. Exit zero means all stated observations reproduced, **not** that
subscription authentication passed. Missing prerequisites, RPC/HTTP mismatches,
unexpected retries and timeouts fail the probe.

The probe uses `bwrap --unshare-all`, an empty home, private procfs, a cleared
environment, no external routes and no host home, configuration, credential,
repository or `/run` mounts. Only the measured executable, probe and system
executables/libraries are mounted read-only. Synthetic credentials travel over
private app-server pipes and loopback HTTP. Raw frames, headers and token values
are not emitted. Namespace teardown removes scratch state and descendants.
This installed-runtime experiment is separate from deterministic CI suites;
it must never be pointed at an operator's existing auth state.

Apps are disabled in the disposable config: with the default enabled, synthetic
ChatGPT login starts app discovery and the disconnected turn times out before
its model request. This change affects only the probe. No user configuration is
changed. The fake service performs no JWT validation or real token exchange.

| Boundary | Observed result |
| --- | --- |
| Empty home | `account/read` reports no account |
| Managed browser login | Starts, returns a login identifier/URL, cancels without signing in; URL is not emitted |
| Invalid external login | Empty token rejected |
| Synthetic external login | Accepted locally; this is not upstream authentication success |
| v2 account read | Only account metadata and authentication requirement, no token |
| Forced refresh in external mode | No host refresh callback |
| Model request | App-server sends the supplied reusable bearer to the fake Responses endpoint |
| Synthetic 401 | One host refresh callback, then retry with the replacement bearer; turn completes |
| External-token restart | Same disposable home, fresh process reports no account |
| Legacy `getAuthStatus(includeToken=true)` | Exports the external access token **and** a synthetic managed-cache access token |
| Managed-cache restart/logout | Fresh process loads a seeded synthetic cache; logout clears its account |
| Real login, refresh-token rotation, expiry or revocation | Not tested; no real sign-in or external network |
| Concurrent refresh ownership | Not proven; no broker authentication lifecycle exists |
| Subscription service, entitlement and billing | Not tested; fake loopback success is not evidence |
| Credentials absent from the confined Session | External-token candidate fails: app-server itself receives the token; no memory-extraction attack is needed |

The managed cache is generated entirely from synthetic values in the namespace.
Its observed load is format/restart evidence, not evidence that OAuth succeeded.
Logout is local clearing, not proof of upstream revocation.

## The candidate that remains

Do **not** conclude that Codex has no token-export operation merely because
`account/read` lacks one. The installed legacy `getAuthStatus` accepts
`includeToken` and `refreshToken` and returns `authToken`. Its generated local
types are `src/app-server/GetAuthStatusParams.ts` and
`GetAuthStatusResponse.ts`. The probe verifies actual token export, including
managed-cache mode. Current primary auth documentation does not specify this
as a supported broker subscription interface or establish its future stability.

A research-only candidate is one pinned app-server owned by the
broker, outside all Sessions, used solely for fresh managed sign-in and refresh.
The broker could obtain access material on its private pipe through the legacy
method. The confined Codex would remain unauthenticated and address a narrow
local operation endpoint. This is an architectural proposal, not implemented
or approved for production by `louiselm-qbr.5.1.3.10`. It still needs a documented
supported authentication and upstream request contract, and the
independent guarded-composition proof in `louiselm-qbr.5.1.3.9` following
`louiselm-qbr.5.1.3.5`'s negative isolation result.

Remaining proof: establish documented support for the authentication boundary
and the subscription service/operation used by broker-originated requests, and
enforce one refresh owner across restart and overlapping brokers. Legacy API
availability alone no longer qualifies as a production option. A raw-HTTP relay,
reading ordinary Codex credentials, or passing exported
tokens into the confined Codex is not an implicit alternative.

`CodexJsonRpcConnection.ts:44` can log entire app-server frames when
`APP_SERVER_LOGS` is configured (`Logger.ts:12`). Therefore any future private
authentication pipe must bypass that logging path and ACP/proxy/session capture.
No real authentication frame was sent through it in this investigation.

## Operator acceptance after a candidate is reviewable

Do not sign in merely to test the blocked design. Once a candidate meets the
documented-support requirement in `louiselm-qbr.5.1.3.10` and its offline
lifecycle/caller-isolation checks pass, the candidate
must provide an operator-only command with these concrete steps:

1. Validate a dedicated non-root broker UID and a new, empty private auth home.
   The existing custody rules remain mandatory: pinned regular files, one link,
   exact ownership, directory mode 0700, file mode 0600, no symlinks or ambient
   keyring access. Stop on any mismatch. Do not import or inspect another login.
2. In a private operator terminal, start exactly one broker-owned auth helper
   with that home and file storage. Request managed `chatgpt` or
   `chatgptDeviceCode` login. Show the URL/code only there; the operator explicitly
   signs in to the intended existing account. Do not route the ceremony through
   ACP, Session transcripts or an Agent tool. Cancellation/failure must leave
   requests disabled. Record only success/failure and the executable pin.
3. With no API key available and Sessions still credential-free, send one
   approved, bounded model request through the reviewed broker transport.
   Verify its exact subscription service and account/workspace in the private
   operator view. Record only the service, auth mode, approved Model, outcome
   and a redacted acceptance verdict. A generic Responses 200 is insufficient.
4. Exercise managed refresh, access expiry and deliberate revocation of only
   this new login. Check fail-closed behavior, bounded retries and spent request
   units. Concurrent callers must join the single refresh owner; a second broker
   must not rotate the same refresh token independently. Restart must retain
   refreshed state and never restore an old seed. Failure must require explicit
   operator reauthentication. Do not revoke the ordinary Codex login.
5. Repeat the distinct-UID and confined-Session checks for files, argv,
   environments, inherited descriptors, logs, ACP and durable records, including
   refresh responses and failure diagnostics. Verify ordinary Codex still works
   through the operator's normal UI, without copying its credentials. Record
   installed/live acceptance separately from offline results.

There is no live command for that candidate yet. Inventing one would conceal
the unresolved transport and lifecycle work; no login, runtime fork or installed
cutover is authorized by this report.

## Existing custody verification

All 16 existing `broker::provider_credentials::tests` pass:

```sh
systemd-run --user --pipe --wait --collect --working-directory="$PWD" \
  --setenv PATH="$PATH" --setenv CARGO_PROFILE_DEV_DEBUG=line-tables-only \
  ./scripts/test-skills-core --lib broker::provider_credentials::tests
```

The distinct-UID installed gate passed on 2026-09-18 after recovery from the
full guest disk (`louiselm-eze4t`). With operator approval, the regenerable
`/home/vm/skills-core/target` cache was removed. Committed sources at
`e6a75cebbd4a7778b9f466e9de1a88c60ce6850d` were recopied to repair the interrupted
extraction, and available locked crates/index entries were transferred from the
host's public Cargo cache. The guest remained externally disconnected. All
binaries and the library test executable were built with `--all-features
--locked --offline` and line-table debug information, using the separate
`/var/tmp/louiselm-skills-target` cache.

The exact test
`launch_supervisor::system::installed_tests::provider_credentials::privileged_installed_provider_credentials_stay_broker_side`
ran with `LOUISELM_REQUIRE_BROKER_LAUNCH=1`, umask 022 and a 120-second deadline:
**1 passed, 0 failed, 0 ignored**, in 20.80 seconds. Broker UID 4019000 and
Session UID 4020000 exercised the existing custody boundary; this is not OAuth
or subscription acceptance. The VM was stopped after verification.

Only this report and the opt-in probe were added. Existing credential validation
and production Rust/Lua code are unchanged. The complete probe is the affected
executable check; no artificial red-green production fix or full Rust/Lua gate
claim applies to this feasibility result.
