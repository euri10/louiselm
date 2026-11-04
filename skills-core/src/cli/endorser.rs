//! `endorser list|add|remove|restore`: the operator's local endorser layer.
//!
//! Adding or restoring raises trust and is signed by the Primary key given
//! with `--key`. Removing lowers trust and needs no signature. A new local
//! endorser requires a hardware key with touch and PIN unless the signed
//! addition explicitly says `--allow-software-key`.

use serde::Serialize;

use super::{CliError, Options, now_ms, report};
use crate::{
    endorsement::{
        EndorserRole,
        local::{self, LocalLayer},
        trust::{self, EndorserEntry, MAX_LIFETIME_MS},
    },
    signer::SshKeygenSigner,
    sshsig::SkPolicy,
};

const DAY_MS: u64 = 24 * 60 * 60 * 1000;

#[derive(Serialize)]
struct Listing<'a> {
    effective: &'a [EndorserEntry],
    defaults: Vec<EndorserEntry>,
    local: &'a LocalLayer,
}

pub(super) fn run(options: &Options) -> Result<i32, CliError> {
    let store = options.store()?;
    match options.subject("endorser")? {
        "list" => {
            let (set, layer) = local::effective(&store)?;
            let listing = Listing {
                effective: set.entries(),
                defaults: trust::embedded_defaults(),
                local: &layer,
            };
            report(options, &listing, human_listing)
        }
        "add" => {
            let entry = EndorserEntry {
                label: options
                    .label
                    .clone()
                    .ok_or_else(|| CliError::Invalid("--label is required".into()))?,
                max_lifetime_ms: lifetime(options.max_lifetime_days)?,
                public_key: endorser_key(options)?,
                roles: roles(&options.roles)?,
                sk_policy: if options.allow_software_key {
                    SkPolicy::none()
                } else {
                    SkPolicy::require_presence_and_verification()
                },
            };
            let signer = SshKeygenSigner::new(&options.signing_key()?);
            local::add(&store, &entry, &signer, now_ms())?;
            report(options, &entry, |entry| {
                format!(
                    "endorser '{}' trusted locally; it may focus review, never admit",
                    entry.label
                )
            })
        }
        "remove" => {
            let key = endorser_key(options)?;
            local::remove(&store, &key)?;
            report(options, &key, |_| {
                "endorser removed; its Endorsements no longer focus review".to_owned()
            })
        }
        "restore" => {
            let key = endorser_key(options)?;
            let signer = SshKeygenSigner::new(&options.signing_key()?);
            local::restore(&store, &key, &signer, now_ms())?;
            report(options, &key, |_| "endorser removal lifted".to_owned())
        }
        other => Err(CliError::Invalid(format!(
            "unknown endorser verb '{other}'"
        ))),
    }
}

pub(super) fn endorser_key(options: &Options) -> Result<String, CliError> {
    let material = Options::key_material("--endorser", options.endorser.as_deref())?;
    // Keep `<algorithm> <base64>` only; a `.pub` file's comment is local.
    Ok(material
        .split_whitespace()
        .take(2)
        .collect::<Vec<_>>()
        .join(" "))
}

fn roles(raw: &[String]) -> Result<Vec<EndorserRole>, CliError> {
    let mut roles = Vec::new();
    for role in raw {
        let parsed = match role.as_str() {
            "publisher" => EndorserRole::Publisher,
            "reviewer" => EndorserRole::Reviewer,
            other => {
                return Err(CliError::Invalid(format!(
                    "unknown endorser role '{}'; use publisher or reviewer",
                    crate::scan::escape(other)
                )));
            }
        };
        if !roles.contains(&parsed) {
            roles.push(parsed);
        }
    }
    Ok(roles)
}

fn lifetime(days: Option<u64>) -> Result<u64, CliError> {
    match days {
        None => Ok(MAX_LIFETIME_MS),
        Some(days) => days
            .checked_mul(DAY_MS)
            .filter(|ms| (1..=MAX_LIFETIME_MS).contains(ms))
            .ok_or_else(|| CliError::Invalid("--max-lifetime-days must be 1 to 180".into())),
    }
}

fn human_listing(listing: &Listing<'_>) -> String {
    let mut lines = Vec::new();
    if listing.effective.is_empty() {
        lines.push("no trusted endorsers; Endorsements cannot focus review".to_owned());
    }
    for entry in listing.effective {
        let source = if listing.defaults.contains(entry) {
            "release default"
        } else {
            "local, Primary-signed"
        };
        let roles: Vec<&str> = entry
            .roles
            .iter()
            .map(|role| match role {
                EndorserRole::Publisher => "publisher",
                EndorserRole::Reviewer => "reviewer",
            })
            .collect();
        lines.push(format!(
            "{} [{}] {} ({source}, max {} days{})",
            entry.label,
            roles.join(","),
            entry.public_key,
            entry.max_lifetime_ms / DAY_MS,
            if entry.sk_policy.require_hardware {
                ", hardware"
            } else {
                ", software key allowed"
            },
        ));
    }
    for key in &listing.local.removals {
        lines.push(format!("removed: {key}"));
    }
    for dropped in &listing.local.dropped {
        lines.push(format!(
            "NOT trusted (did not verify): {} — {}",
            dropped.public_key, dropped.reason
        ));
    }
    lines.join("\n")
}
