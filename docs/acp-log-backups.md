# ACP log backups

Tracked by `louiselm-acp-daily-backups-kb31`. This is an **opt-in operator command**,
not an enabled backup service. It does not install itself, enable timers or generate
credentials. Local backup, cloud initialization/copy and reviewed retention are
separate commands. Effective setup and the scheduled restore drill remain in
`kb31.3`. No change is made to Neovim configuration.

The approved plan is hourly encrypted local snapshots outside the state tree,
copied to a private bucket in GCP project `louiselm` when connected. Local
retention keeps 48 hourly and 7 daily snapshots; cloud retention keeps
48 hourly, 30 daily and 12 monthly snapshots. The EUR 5/month target is an
alerts-only budget, not a cost estimate or spending cap. **Nothing runs until the
operator configures and enables it. Retention is manual-only.** Protection rules
can retain more than the policy counts, especially while offline. Watch disk space.

Cloud infrastructure belongs to the separate `infra/foundation` repository, in
its `louiselm-backups/` root (tracked by `infra-d0m`). That root defines the private
EU bucket and alert budget with isolated state and a reviewed-plan/manual-apply
pipeline. It does not enable the laptop command, cloud copying or retention.

## Scope and consistency

Use the effective adapter state root, normally
`~/.local/state/acp-llm-adapter`, or its configured override. The whole selected
directory is included: direct/proxy Session logs, connection logs and metadata.
Native agent histories and recovered USB archives are excluded. Entries named
`recovered-partials` are excluded at any depth. Other symlinks are preserved as
links, not traversed; a saved link is not a backup of its external target.

Restic owns the encrypted timestamped tree inventory and content integrity.
After a successful backup, `check --read-data` reads and verifies the repository
before advancing `last_verified`. This is more work than checking only an index,
and its duration grows with repository size. No source or Restic output is sent
to notifications or error messages. Explicit `preview` displays file metadata.

This is **not an atomic filesystem snapshot**. Active files may capture bytes
from different times or end in a partial JSONL record; completion does not prove
that every event up to the verification timestamp is present. Status always says
`non-atomic-live-source` and records whether the pre/post file inventory changed.
That comparison is a diagnostic, not proof of immutability. Appends/rotation can
be picked up on the next run; unreadable/missing files or an incomplete Restic
result fail the run. Quiesce writers if an application-consistent generation is
required. Never silently import staged fragments as complete live logs.

A missing source or one without nonempty regular JSONL logs fails without
initializing a repository or deleting earlier snapshots. A failed backup/check
preserves the previous verified snapshot identifier and records a failed attempt.
If the process is killed, a `running` attempt can remain in status: it is not a
successful backup. A local lock prevents overlapping local backup, init, preview
and restore. Cloud operations have their own lock and status file, so an offline
copy does not hold up the hourly local job. Retention takes both locks. Restic
also maintains its native repository locks; failures never trigger force-unlocking.

## Prepare configuration (not enablement)

Runtime: Linux, Python 3.10+ standard library, Restic 0.19.1 (the tested version).
Restic is a separate operator dependency, not a Lua/plugin dependency. Install
the command as `~/.local/bin/acp-log-backup` when accepting the effective setup.
No installer is provided that silently changes your services.

Create a private JSON file (0600), by default
`${XDG_CONFIG_HOME:-$HOME/.config}/louiselm/backup.json`. The schema is closed;
the three local fields are required and must be absolute paths. Replace these examples
with verified paths; literal `~` and environment variables are not expanded:

```json
{
  "source": "/home/lotso/.local/state/acp-llm-adapter",
  "destination": "/home/lotso/Backups/louiselm-acp",
  "password_file": "/home/lotso/.config/louiselm/backup-password"
}
```

The destination's parent must exist. The command creates the destination as 0700
or requires that mode on an existing owned directory. Repository, cache, lock
and status live inside it, outside both source and state roots. It must not be a
broad ancestor of the source. The password must be a nonempty owned regular file
with no group/other permissions, outside source, state and destination. Keep the
configuration outside state too. Do not put any secret in this repository.

Store the recovery password in the password manager and **verify opening that
database on another device**, including any key file and account recovery. The
reported Nextcloud/Google Drive sync arrangement has not yet been verified. A
local password file alone is not independent recovery.

Missing default configuration makes `run` and `status` inert. A missing explicit
`--config` path is an error. A malformed config never falls back to defaults.

## Operator commands

These commands are intentionally separate. Initialization is never attempted
automatically in response to a failed repository operation:

```sh
acp-log-backup init
acp-log-backup run
acp-log-backup status
acp-log-backup preview FULL_SNAPSHOT_ID
acp-log-backup restore FULL_SNAPSHOT_ID /absolute/new-staging-directory
```

Use the full snapshot ID from `status`. `preview` reads the encrypted inventory
without restoring. Restore requires a **nonexistent** target outside source,
state and backup directories, uses `--overwrite never --verify`, and never
merges into live state. On failure it leaves the partial staging directory for
inspection; retry into a different new directory. Restored symlinks remain links;
do not follow them while inspecting untrusted archive contents.

Only successfully copied cloud snapshots protect against losing the laptop.
Local-only configuration protects against source-tree deletion, not disk failure.

## Explicit cloud setup and recovery

After independent password recovery is verified, provision the dedicated bucket-only
uploader credential out of band. Add the optional absolute `cloud_credentials_file`
path to the configuration. The file must be private (0600), owned, and outside
source, state and backup directories; it must identify
`louiselm-acp-backup@louiselm.iam.gserviceaccount.com`. Do not use personal ADC or a
CI apply identity. The wrapper strips inherited `RESTIC_*` and `GOOGLE_*` overrides.

The destination is fixed to `gs:louiselm-acp-backups:/restic`. Both repositories
use the configured recovery password; Restic still generates independent repository
encryption keys. Initialize explicitly with shared chunker parameters for copy
deduplication. A failed operation never automatically initializes either repository:

```sh
acp-log-backup cloud-init
acp-log-backup cloud-copy
acp-log-backup status
acp-log-backup cloud-list
acp-log-backup cloud-verify FULL_CLOUD_SNAPSHOT_ID
acp-log-backup cloud-preview FULL_CLOUD_SNAPSHOT_ID
acp-log-backup cloud-restore FULL_CLOUD_SNAPSHOT_ID /absolute/new-staging-directory
```

`cloud-copy` sends only IDs recorded after a successful local backup and full
integrity check. An incomplete or unchecked snapshot is not eligible merely because
Restic saved it. Copy retry reuses Restic's existing-copy detection; it validates
the remote snapshot's original ID, tree, timestamp and source paths before recording
the local-to-cloud ID receipt. Repository identities bind those receipts to the
repositories actually initialized. A changed identity fails closed.

`last_copy.metadata_checked_at` means copy and metadata checking succeeded, **not**
that all cloud data was downloaded or restored. Its snapshot count includes eligible
snapshots already present. `cloud-verify` explicitly downloads/checks all repository
data and records a selected existing snapshot as `last_verified`. It can incur
egress charges. `cloud-restore` always verifies the staged files. A successful staged restore
records a separate `last_restore`; failures do not advance those successful times.

After laptop loss, recover the password and an authorized bucket credential on
the replacement machine. Use `cloud-list`, then `cloud-preview`/`cloud-restore`.
These recovery commands do **not** need the old source, local repository or status
files. Do not run `cloud-init` to recover an existing repository. Resuming writes
after lost initialization/status records requires an explicit identity review;
do not fabricate copy receipts. All restore targets must be fresh staging paths.

## Reviewed retention, never an automatic prune

```sh
acp-log-backup retention-preview local
acp-log-backup retention-preview cloud
acp-log-backup retention-apply local EXACT_APPROVAL_FROM_LOCAL_PREVIEW
```

Review `remove` and `protected` IDs before using the corresponding approval token.
The token binds the scope, repository identities, catalogs, policy, ledger and
receipts. A changed catalog or protection set requires a new preview. Restic's
calendar policy is grouped by host and source paths; occupied periods are counted,
not a hard number of elapsed hours. Unknown/unverified snapshots, uncopied local
snapshots, the latest verified local snapshot, the latest fully verified cloud
snapshot (and its local original), and the latest successfully restored cloud
snapshot are protected. Cloud copies of snapshots still present locally are also
protected, so the next copy does not undo cloud retention; review/apply local
retention first. These protections can exceed the nominal retention counts.

Missing/empty source, failed local backup, failed cloud copy, failed full cloud
verification or a missing last-good snapshot refuses retention. Applying first
fully checks **both** repositories, then deletes only the exact reviewed snapshot
IDs. Native prune runs without repacking: wholly unused packs can be removed, but
partly used packs remain to avoid scratch-space and egress amplification. GCS's
seven-day soft delete can continue charging for removed objects during that window.

If interrupted, retention status may remain `running`, or say `failed` after some
IDs were removed. Inspect a fresh preview; never assume rollback or reuse stale
approval. The independent source logs are never deleted. Manage these repositories
through this wrapper: direct external Restic mutations bypass its cross-command locks.

## Scheduling and acceptance

`contrib/systemd/louiselm-acp-backup.{service,timer}` and
`louiselm-acp-cloud-copy.{service,timer}` are uninstalled templates.
They expect the command on the maintainer's user-bin path. Each backup service
prepends `~/.local/bin` to its own PATH to find the installed Restic runtime;
the user manager's global environment is unchanged. The local timer uses an hourly schedule with a
one-minute accuracy window. `Persistent=true` catches a missed calendar run when
the timer becomes active again; it does not wake a powered-off laptop or create
one snapshot for every missed hour. Cloud copy retries every 15 minutes, independently
of local backup. A running service is not started a second time. User services normally
require login. A missing default config is inert for both scheduled commands; a local-only
config also makes cloud retries inert. Neither timer invokes retention or full cloud reads.

Before enabling: confirm effective source/config paths, independent password
recovery, available disk space and failure visibility. The current command has
JSON status and sanitized stderr. Both service templates route failures to
`louiselm-acp-backup-failure.service`, which uses `notify-send` with static,
payload-free text. Verify that dependency and desktop notification delivery on
the actual user manager; journal status remains available if the desktop is absent.
Observe an actual scheduled
run and restore a selected snapshot into staging. Unit syntax checks are not
evidence that scheduling is active.

```sh
systemctl --user list-timers --all 'louiselm-acp-*.timer'
systemctl --user status louiselm-acp-backup.service
journalctl --user -u louiselm-acp-backup.service
```

## Checks

```sh
python3 scripts/test-acp-log-backup.py
systemd-analyze --user verify contrib/systemd/louiselm-acp-*.service contrib/systemd/louiselm-acp-*.timer
git diff --check
```

The tests use temporary synthetic sources and passwords. The real-Restic test
checks scope, source deletion, staged byte equality and corruption detection.
Cloud tests substitute only the GCS transport with a temporary local repository;
Restic encryption/copy/check/restore and native retention remain real. They cover
lost copy responses, offline/full-destination process failures, receipt and identity
rejection, corrupted encrypted data, independent locks, uncopied protections and stale
retention approval. These are **not live GCS tests**, physical disk-full tests or
power-loss proof. Real-Restic tests are skipped when the runtime is absent; CI supplies
the pinned, checksum-verified binary. The parent still requires effective GCS setup,
off-device password recovery, actual scheduled restore and budget recipient acceptance.

References: [Restic scripting](https://restic.readthedocs.io/en/stable/075_scripting.html),
[restore semantics](https://restic.readthedocs.io/en/stable/050_restore.html),
[systemd timers](https://www.freedesktop.org/software/systemd/man/latest/systemd.timer.html).
