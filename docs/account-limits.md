---
title: Account limits ACP extension v1
description: The provider-neutral accountLimits v1 wire contract for ACP adapter authors, including complete snapshots, validation, and observation states.
---

# Account limits ACP extension v1

An Agent can advertise the `io.github.euri10.louiselm` `accountLimits`
extension to report normalized Provider entitlement over its existing ACP
connection. Account limits describe remaining quota in exact windows. They are
separate from recorded turn usage, cost, and context-window occupancy.

This page specifies the v1 wire format consumed by LouiseLM. The implementation
is [session/limits.lua](https://github.com/euri10/louiselm/blob/main/lua/louiselm/session/limits.lua);
observation and refresh behavior live in
[session/registry.lua](https://github.com/euri10/louiselm/blob/main/lua/louiselm/session/registry.lua).
The JSON examples below are synthetic protocol examples, not captured account data.

## Capability advertisement

Include the following in the `initialize` response's `result.agentCapabilities`:

```json
{
  "_meta": {
    "io.github.euri10.louiselm": {
      "accountLimits": {
        "version": 1,
        "readMethod": "_io.github.euri10.louiselm/account_limits/read",
        "updatedMethod": "_io.github.euri10.louiselm/account_limits/updated"
      }
    }
  }
}
```

`version` must be the number `1`. Both method names must be strings beginning
with `_`. LouiseLM uses the advertised names; the names above are examples,
not hardcoded dispatch names. Missing or malformed advertisement, or another
version, does not negotiate this extension. Unknown additional fields are ignored.

This metadata belongs inside `agentCapabilities`, not directly on the
`initialize` result. It does not require another client capability or a
separate subscription request.

## Read and update messages

LouiseLM reads through a live, initialized Session advertising the capability.
The request has an empty object as `params`, with no `sessionId`:

```json
{
  "jsonrpc": "2.0",
  "id": 3,
  "method": "_io.github.euri10.louiselm/account_limits/read",
  "params": {}
}
```

Return the complete snapshot directly in `result`:

```json
{
  "jsonrpc": "2.0",
  "id": 3,
  "result": {
    "defaultBucketId": "requests",
    "buckets": [
      {
        "id": "requests",
        "label": "Requests",
        "windows": [
          {
            "usedPercent": 82,
            "windowDurationMins": 300,
            "resetsAt": 4102444800
          },
          {
            "usedPercent": 40.5,
            "windowDurationMins": 10080,
            "resetsAt": 4102452000
          }
        ],
        "reachedType": "short_window",
        "planType": "example-plan",
        "credits": {
          "balance": 7.5,
          "unlimited": false
        }
      }
    ],
    "unlimited": false,
    "resetCredits": {
      "availableCount": 1,
      "credits": [
        {
          "id": "reset-1",
          "expiresAt": 4102448400,
          "title": "Window reset",
          "description": "One available reset credit."
        }
      ]
    }
  }
}
```

An update is a JSON-RPC notification on the advertised `updatedMethod`, with
the same complete snapshot shape directly in `params`. It has no request ID
and is not wrapped in `session/update`, `snapshot`, or `accountLimits`:

```json
{
  "jsonrpc": "2.0",
  "method": "_io.github.euri10.louiselm/account_limits/updated",
  "params": {
    "buckets": [],
    "unlimited": true
  }
}
```

Every accepted read or notification replaces the entire previous snapshot.
Omitted optional fields are cleared; buckets and windows are not merged.
Adapters receiving incremental upstream events must assemble a complete
snapshot before sending it. The notification above replaces any previous
metered buckets with an explicitly unlimited account.

If a read cannot obtain valid data, return a JSON-RPC error with a safe message.
LouiseLM retains the last good snapshot, if any. Do not substitute an empty or
unlimited snapshot for a failed read. V1 has no separate error-notification or
wire `status` field; local observation states are described below.

## Snapshot fields and validation

Field names on the wire are case-sensitive camelCase. Optional fields may be
omitted or JSON `null`; required fields may not. Present values must have the
documented JSON types: numeric strings and string booleans are not coerced.
Unknown fields are ignored at every level. Arrays are dense sequences; emit
`[]` for empty collections.

### Snapshot

| Field | Requirement | Meaning and validation |
| --- | --- | --- |
| `buckets` | Required array of bucket objects | Complete bucket list; may be empty. Bucket IDs must be unique. |
| `defaultBucketId` | Required when `buckets` is nonempty | Nonempty string matching a bucket ID. Omit or use `null` when there are no buckets. |
| `unlimited` | Optional boolean | Explicit account-wide unlimited entitlement. `false` or absence does not imply unlimited. |
| `resetCredits` | Optional reset-credit object | Authoritative available reset-credit count and optional details. |

A top-level `unlimited: true` does not exempt buckets from validation or remove
the default-bucket requirement. The consumer accepts it alongside buckets;
its initial observation status is `unlimited`.

### Bucket

| Field | Requirement | Meaning and validation |
| --- | --- | --- |
| `id` | Required nonempty string | Stable identifier for this metered bucket, unique within the snapshot. |
| `label` | Optional nonempty string | Human-facing bucket name. |
| `windows` | Required array of window objects | Complete quota-window list; may be empty. |
| `reachedType` | Optional nonempty string | Provider classification of a reached limit; no fixed enum. Omit when no reached classification is reported. |
| `planType` | Optional nonempty string | Provider plan identifier or name; no fixed enum. |
| `credits` | Optional credit object | Remaining workspace credit state for this bucket. |

`reachedType` is not inferred from `usedPercent`, and the validator does not
require a particular percentage when it is present. Bucket IDs and labels are
not restricted to a particular Provider or Model.

### Window

| Field | Requirement | Meaning and validation |
| --- | --- | --- |
| `usedPercent` | Required number, inclusive range 0–100 | Consumed capacity, not remaining capacity. Fractions are accepted. |
| `windowDurationMins` | Required positive integer | Exact quota-window duration in minutes. |
| `resetsAt` | Required positive integer | Unix timestamp in **seconds** for the next reset. |

All three fields are required for every window. A past `resetsAt` passes shape
validation but makes the snapshot stale when inspected. Windows need not be
sorted; the validator does not require unique durations or reset timestamps.

Do not invent `windowDurationMins` to fit a quota that reports only a next-reset
time. A reset-only quota cannot be represented as a v1 window without the exact
duration and a trustworthy reset timestamp. A bucket with `windows: []` can
carry independently known credit data, but supplies no percentage, reset
display, or window threshold alerts. An unknown duration is not zero, a guessed
month, or a reason to declare the account unlimited.

### Workspace credits

| Field | Requirement | Meaning and validation |
| --- | --- | --- |
| `balance` | Optional nonnegative number | Remaining workspace credit balance; fractions and zero are accepted. |
| `unlimited` | Optional boolean | Explicitly unlimited workspace credits for this bucket. |

Both fields may be absent; an empty credit object is accepted. This nested
`unlimited` flag does not set the account-wide `unlimited` state. The contract
does not assign a currency or infer entitlement from recorded spending.

### Reset credits

| Field | Requirement | Meaning and validation |
| --- | --- | --- |
| `availableCount` | Required nonnegative integer | Authoritative number of available reset credits. |
| `credits` | Optional array of reset-credit detail objects | May be empty, omitted, or `null`. Its length need not equal `availableCount`. |

Each detail object has these optional fields:

| Field | Type and validation |
| --- | --- |
| `id` | Nonempty string; opaque credit identifier. |
| `expiresAt` | Positive integer Unix timestamp in seconds. |
| `title` | Nonempty string. |
| `description` | Nonempty string. |

An empty detail object is accepted. Detail IDs are not required to be unique.
No available count is inferred from the detail rows or their expiry times.

## LouiseLM observation states

These are client states, not values an adapter sends. State is held per
configured Agent in the Session registry; inspection starts no process and
performs no refresh. Reads use an existing capable Session for that Agent.

| State | Meaning |
| --- | --- |
| `not_observed` | With no cached state, no eligible initialized Session has been observed for this Agent. This is not evidence that the extension is unsupported. |
| `unsupported` | With no cached state, an eligible initialized Session exists, but none advertises a valid v1 capability. No limits request is sent. |
| `loading` | A capable Session exists but no snapshot has been accepted yet, or a refresh is in progress. A previous snapshot may remain available. |
| `fresh` | A valid snapshot contains buckets and does not declare account-wide unlimited entitlement. A bucket may still have no windows. |
| `empty` | A valid snapshot has `buckets: []` without `unlimited: true`. This means no metered bucket data, not zero usage or unlimited entitlement. |
| `unlimited` | A valid snapshot explicitly declares top-level `unlimited: true`. |
| `unavailable` | A read or validation failed with no last good snapshot, or a pending refresh lost its live source. |
| `stale` | A last good snapshot exists but a later read/update failed validation or a read failed; inspection also marks it stale when any window reset time has passed or no live capable Session remains. |

For example, `{"buckets": []}` is an empty snapshot and
`{"buckets": [], "unlimited": true}` is explicitly unlimited. Neither means
unsupported. Invalid payloads are rejected as a whole; they never partially
overwrite the last good snapshot. A valid later snapshot replaces the old
state and clears its error. `updated_at` is the client's local receipt time,
not an adapter-supplied timestamp. Passing a reset time does not replenish
quota locally; a new observation must confirm it.

## Ownership and reference implementations

LouiseLM obtains Account limits only through an Agent-advertised ACP extension.
Neither LouiseLM nor maintainer-maintained adapters may read another tool's
credential store or call a Provider API to obtain Account limits. An external
adapter does not bypass that boundary. `unsupported` is a correct result when
the Agent cannot supply the extension.

[euri10/codex-acp](https://github.com/euri10/codex-acp) and
[euri10/claude-agent-acp](https://github.com/euri10/claude-agent-acp) are
**unsupported reference implementations, with no compatibility promise**.
They are not a supported distribution or installation path. Implement against
this contract and the consumer validation, rather than assuming a fork's
internal Provider payload is the wire format.
