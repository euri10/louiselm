# Private Session caches

`skills-core::cache` captures warm dependency bytes into an immutable measured
base and creates an independent writable overlay inside each Session's private
home. The overlay is an eager copy: it has no filesystem lower layer and shares
no writable inodes with the input or another Session. The base lives in trusted
process memory; no mutable access is exposed. Changing or deleting the input
directory after capture cannot change this base or subsequently seeded overlays.

This implementation favors bounded memory and copy cost over another mount
mechanism. It accepts at most 128 MiB of content, 20,000 entries including
directories, and 64 levels of nesting. Larger caches are refused. An on-disk
immutable image or reflink optimization is outside this initial implementation.

The blocking `CacheBase::capture` API reads a complete tree through pinned
directory/file descriptors. It rejects symbolic links, multiply linked files,
devices, FIFOs, sockets, noncanonical paths, case collisions, and observed
mutation during capture. Paths use the existing printable ASCII canonical
contract and reject backslashes. Binary contents and executable intent are
retained; empty directories and other metadata are omitted. Capture measures
bytes; it does not approve dependencies or make untrusted warm content safe.

The digest binds the `louiselm.cache.base/1` domain and sorted path, executable
bit, size, and content-hash inventory. `SessionInputs.cache_base_digest` is
required, including for an explicitly empty cache. An unresolved base remains
`None` and prevents manifest construction. Changing the base changes the Session
input manifest identity; old records without the field are refused.

Before starting a Session, its lifecycle owner calls `CacheBase::materialize`
with the existing private home, Session id, and assigned identity. The home's
parent must be protected by the launcher, and no Session process may be running
yet. The operation requires the home's exact private ownership/mode and a new
destination. Publication is atomic and never overwrites an existing overlay.
Files receive `0600`/`0700`, directories `0700`, owned by the assigned identity.
The Agent's cache environment/configuration must point at `CacheOverlay::path()`;
the existing private-home bind supplies visibility and mutation isolation.
Never mount the original warm directory. `NamespaceOnly` is supported for local
tests but refuses `CacheOverlay::check_verified`, even with otherwise green
evidence. That check also refuses input substitution, a different Session,
disposed state, or missing/failed isolation evidence from the trusted backend.

`CacheOverlay::store_download` is the broker's byte sink, not a network client or
authorization service. It verifies a supplied exact digest and a 128 MiB limit,
writes an anonymous file on the overlay filesystem, and publishes it with an
atomic no-replace link as `artifact-<sha256 hex>`. A pinned overlay descriptor
supplies the destination; callers cannot supply a path. Existing Agent entries
are refused, never overwritten or treated as valid cached downloads. Unsupported
anonymous-file/link enforcement fails closed. Files are writable by this Session
after publication, so a download receipt is evidence of the fetched bytes, not
proof that later Agent reads are unchanged. Broker policy still owns access,
aggregate quotas, tool-specific placement within the overlay, and Park ingress.

Dropping either handle never deletes an overlay. Park/Resume uses the existing
cgroup lifecycle and preserves cache bytes. Explicit `CacheOverlay::dispose`
requires the matching Session, successfully disposes its process tree first,
then removes only the owned overlay. A replaced path or cleanup failure retains
the handle for investigation/retry. It does not promise physical-media erasure.

Final installed launch/broker wiring, restart ownership, Forensics TTL/pinning,
and retention decisions remain `louiselm-d6fv.5.5`. These APIs and their component
tests do not establish end-to-end Verified posture. The ordinary cache and
Session-manifest suites exercise poisoning and binding; sandbox tests exercise
real private mounts, two Sessions, broker publication, Park/Resume, and Disposal.
The required guest `host_identity` gate additionally runs
`cache::host_identity_cache_isolation_and_lifecycle` under distinct outer UIDs.
