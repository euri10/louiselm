# Hostile launcher conformance

`scripts/launcher-conformance --disposable-guest` is the required gate for
`louiselm-d6fv.4.8`. Run it only through the disposable VM or in disposable CI,
never with desktop sudo. CI invokes the same recipe.

```sh
git archive HEAD skills-core scripts/launcher-conformance tests/fixtures/verified_posture_v1.json |
  ./scripts/launcher-vm exec tar -x -C /home/vm
./scripts/launcher-vm exec env \
  PATH=/home/vm/.cargo/bin:/usr/bin:/bin \
  CARGO_NET_OFFLINE=true CARGO_BUILD_JOBS=2 \
  CARGO_TARGET_DIR=/var/tmp/louiselm-skills-target \
  /home/vm/scripts/launcher-conformance --disposable-guest
```

Select uncommitted paths explicitly when transferring work under test. The gate
builds as the guest operator, then runs root checks with private fixture modes.
UID/GID 60000 and 60001 are the simultaneous Sessions; 60002 is the broker
process double. Allocated accounts/groups, live processes using those UIDs,
missing root, mapped user namespaces, missing tools or kernel controls fail.
The ordinary Cargo suite explicitly ignores the privileged matrix; only the
required recipe is conformance evidence. Test-name inventory prevents an empty
filter from silently passing.

The network fixture is a throwaway **network namespace**, not a user namespace.
It has only loopback, with local test addresses `198.18.0.1` and `fd00:10::1`:
no external interface, default route, bridge or egress. The Agent gets a second,
empty network namespace through production Bubblewrap. This proves inner
network denial independently of QEMU's restricted network. No host or guest
global network/security setting is changed.

## Observations

The same deterministic Rust test Agent issues each probe outside and inside
the production `HostIdentity` backend. Results distinguish allowed operations,
specific kernel denials and unexpected errors; timeouts, absent controls,
duplicate/contradictory results and empty reports cannot pass. Each request has
a deadline, fixtures own process/service cleanup, and the recipe has an outer
deadline. There are 45 named attack checks plus a hostile lifecycle scenario:

| Boundary | Checks | Positive control and denial |
| --- | ---: | --- |
| Outside files, native config/MCP sentinels and symlinks | 10 | Same UID reads harmless targets outside; confined reads fail. No real Provider parser or credentials. |
| Runtime/config writes | 3 | Same UID writes deliberately writable fixture files outside; confined writes return `EROFS`. |
| Ambient environment | 1 | Seeded value visible outside, absent inside. |
| Inherited file/socket FDs | 2 | Deliberately non-CLOEXEC authority works outside; bootstrap refuses before Agent execution; same clean plan then launches. |
| Operator/launcher/broker/second-Session processes; reciprocal Session files/processes | 17 | Live proc reads, actual `strace` ptrace attachment and `SIGCONT` delivery succeed outside; confined attempts fail; targets still answer afterward. |
| Pathname/abstract sockets, D-Bus/systemd and Docker-shaped execution services | 4 | Fixed requests execute a harmless command under the fixture service identity; confined attempts cannot connect or increment execution counts. |
| IPv4/IPv6 unicast-address and loopback TCP | 4 | Real request/reply exchanges before and after confined connection denials. |
| Reciprocal Session channels, private and outer paths | 4 | Declared owner channel and outside control work; other Session cannot connect. |

The forking workload proves a live descendant heartbeat, whole-tree Park,
Resume, foreground work stopping after Interrupt, and zero-survivor Disposal.
A shell trap acknowledgement is not required: namespace supervision can die
before the shell executes that trap. The recipe additionally consumes the
existing `ln30` registry suite and three-scenario production relay/loss test;
neither is reimplemented here.

The suite found `louiselm-d6fv.4.8.1.1`: Bubblewrap did not close an undeclared
socket descriptor; the confined workload successfully wrote through it. The
fresh single-threaded bootstrap now rejects undeclared descriptors after the
three declared transfers and before READY/exec. No unsafe raw-FD close or new
crate was added. The independent bootstrap regression fails before the fix and
preserves declared stdio/status/gate transfers after it.

Recorded 2026-09-06: ten consecutive complete guest rounds passed after fixing
the Interrupt fixture oracle. Environment: Debian 13, kernel
`6.12.107+deb13-cloud-amd64`, Bubblewrap 0.12.0, Rust 1.97.1. Each matrix took
about 1.3 seconds, followed by the existing registry and relay checks. Required
invocations reported no skips. This is local evidence, not a hosted CI result.

## Authority limits

Real: bootstrap, Bubblewrap, assigned outer credentials, mount/PID/network
boundaries, cgroups, descriptor refusal, and production relay/loss composition.
Doubles: Agent, operator/launcher/broker target processes, service endpoints,
signer and broker used by the composite relay test. The service requests model
service-mediated execution, not D-Bus/Docker protocol implementation. Provider
native-source packaging and masking remains `louiselm-d6fv.3`.

This gate does **not** issue a runtime attestation or enable Verified posture.
`IsolationEvidence::check` separately rejects incomplete/contradictory dimension
records, but backend mechanism booleans are not a persisted conformance report.
`louiselm-ucj1` records the unresolved binding of host conformance to production
Verified admission, blocking the `louiselm-d6fv.9` cutover. Installed authority
still needs `louiselm-d6fv.4.9` and the genuine release ceremony `louiselm-lm70`.
