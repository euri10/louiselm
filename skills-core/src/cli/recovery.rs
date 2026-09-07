//! Local operator ceremonies; no Agent, robot, stdin or argument secret channel.

use super::{CliError, now_ms};
use crate::{
    scan,
    signer::SshKeygenSigner,
    store::Store,
    trust::{
        Role, TrustError, TrustStore,
        browser::Browser,
        paper::PaperPhrase,
        passkey::{self, PendingAuthentication, PendingRegistration, Registration},
        recovery::{
            self, RecoveryAuthorization, RecoveryChange, RecoveryConfirmation, RecoveryError,
            ReplacementKey, ReplacementProof,
        },
        terminal::{self, Terminal},
    },
};
use std::{collections::BTreeMap, fs, path::Path, time::Instant};
mod setup;
type Flags<'a> = BTreeMap<&'a str, &'a str>;

const HELP: &str = "recovery setup --store PATH --primary PRIVATE_KEY --release PRIVATE_KEY [--trust-domain DOMAIN]
recovery status --store PATH
recovery change --store PATH --via primary|release|paper|passkey [--authorizer PRIVATE_KEY]
                [--primary NEW_PRIVATE_KEY] [--release NEW_PRIVATE_KEY] [--paper replace] [--passkey replace]
recovery reset --store PATH
Mutations require the trusted installed tool, protected production store and local foreground TTY.
Setup confirms BOTH signing keys, a password-manager-backed passkey and written paper before enrollment.
--via primary/release requires --authorizer. --via paper requires --paper replace (single-use).
Only explicitly named replacements change. A new passkey never authorizes itself.
Status is public JSON; no mutation accepts robot output, phrase arguments, stdin or development overrides.
Reset discards ALL trust and history authorization; use only after losing every usable method.
Do not run secret ceremonies in an Agent terminal, recorded terminal or screen-sharing session.
Open the ephemeral localhost URL in your normal browser, never a root browser.
Software readiness is not personal-hardware acceptance or Verified posture.";

fn invalid() -> CliError {
    CliError::Invalid(
        "invalid recovery arguments; use recovery --help (never pass a phrase as an argument)"
            .to_owned(),
    )
}

fn parse(arguments: &[String]) -> Result<(&str, Flags<'_>), CliError> {
    let command = arguments.first().map(String::as_str).ok_or_else(invalid)?;
    let allowed: &[&str] = match command {
        "setup" => &["--store", "--primary", "--release", "--trust-domain"],
        "status" | "reset" => &["--store"],
        "change" => &[
            "--store",
            "--via",
            "--authorizer",
            "--primary",
            "--release",
            "--paper",
            "--passkey",
        ],
        _ => return Err(invalid()),
    };
    let mut flags = Flags::new();
    for pair in arguments[1..].chunks(2) {
        let [flag, value] = pair else {
            return Err(invalid());
        };
        if !allowed.contains(&flag.as_str())
            || value.is_empty()
            || value.starts_with("--")
            || flags.insert(flag.as_str(), value.as_str()).is_some()
        {
            return Err(invalid());
        }
    }
    if !flags.contains_key("--store") {
        return Err(invalid());
    }
    if command == "setup" && (!flags.contains_key("--primary") || !flags.contains_key("--release"))
    {
        return Err(invalid());
    }
    for name in ["--paper", "--passkey"] {
        if flags.get(name).is_some_and(|value| *value != "replace") {
            return Err(invalid());
        }
    }
    if command == "change" {
        match flags.get("--via").copied() {
            Some("primary" | "release") if flags.contains_key("--authorizer") => (),
            Some("paper")
                if !flags.contains_key("--authorizer") && flags.contains_key("--paper") => {}
            Some("passkey") if !flags.contains_key("--authorizer") => (),
            _ => return Err(invalid()),
        }
        if !["--primary", "--release", "--paper", "--passkey"]
            .iter()
            .any(|name| flags.contains_key(name))
        {
            return Err(invalid());
        }
    }
    Ok((command, flags))
}

pub(super) fn run(arguments: &[String]) -> Result<i32, CliError> {
    if arguments == ["--help"] {
        println!("{HELP}");
        return Ok(0);
    }
    let (command, flags) = parse(arguments)?;
    let path = Path::new(flags.get("--store").ok_or_else(invalid)?);
    if command == "status" {
        // Do not manufacture provenance for an absent store.
        fs::metadata(path.join("provenance.json")).map_err(RecoveryError::Io)?;
        return setup::status(&Store::open(path)?);
    }
    // Includes existing provenance/trust paths; runs BEFORE Store::open can write.
    terminal::require_store_path(path)?;
    let store = Store::open(path)?;
    terminal::require_production_root(&store)?;
    rustix::process::setrlimit(
        rustix::process::Resource::Core,
        rustix::process::Rlimit {
            current: Some(0),
            maximum: Some(0),
        },
    )
    .map_err(|error| RecoveryError::Io(error.into()))?;
    if command == "setup" {
        return setup::enroll(&store, &flags);
    }
    terminal::require_production(&store)?;
    let trust = TrustStore::load(&store)?.ok_or(TrustError::NotBootstrapped)?;
    if command == "reset" {
        return setup::reset(&store, &trust);
    }
    recovery::require_hardware_policy(&trust)?;
    change(&store, &trust, &flags)
}

fn replacement_keys(flags: &Flags<'_>) -> Result<Vec<ReplacementKey>, RecoveryError> {
    [(Role::Primary, "--primary"), (Role::Release, "--release")]
        .into_iter()
        .filter_map(|(role, flag)| flags.get(flag).map(|path| (role, *path)))
        .map(|(role, path)| {
            let mut public_path = Path::new(path).as_os_str().to_os_string();
            public_path.push(".pub");
            fs::read_to_string(&public_path)
                .map(|public_key| ReplacementKey { role, public_key })
                .map_err(RecoveryError::Io)
        })
        .collect()
}

fn proofs(
    flags: &Flags<'_>,
    namespace: &str,
    bytes: &[u8],
    deadline: Instant,
) -> Result<Vec<ReplacementProof>, CliError> {
    [(Role::Primary, "--primary"), (Role::Release, "--release")]
        .into_iter()
        .filter_map(|(role, flag)| flags.get(flag).map(|key| (role, key)))
        .map(|(role, key)| {
            SshKeygenSigner::new(Path::new(key))
                .sign_until(namespace, bytes, deadline)
                .map(|signature| ReplacementProof { role, signature })
                .map_err(CliError::from)
        })
        .collect()
}

fn paper(terminal: &mut Terminal, deadline: Instant) -> Result<PaperPhrase, RecoveryError> {
    let next = PaperPhrase::generate()?;
    terminal.write(
        "Write this NEW paper phrase. Never reuse a wallet seed or enter it in the browser.\r\n",
    )?;
    terminal.write(&next.expose_secret())?;
    terminal.write("\r\nKeep both papers until success. Press Enter after writing it.\r\n")?;
    if !terminal.read_hidden_until(deadline)?.is_empty() {
        return Err(RecoveryError::Cancelled);
    }
    terminal.clear()?;
    terminal.write("Re-enter the newly written 24 words (hidden):\r\n")?;
    let confirmed = PaperPhrase::parse(&terminal.read_hidden_until(deadline)?)?;
    if confirmed.expose_secret() != next.expose_secret() {
        return Err(RecoveryError::Confirmation);
    }
    terminal.clear()?;
    Ok(confirmed)
}

fn browser_url(terminal: &mut Terminal, browser: &Browser) -> Result<(), RecoveryError> {
    terminal.write(&format!(
        "\r\nOpen in your normal, non-root browser before the ceremony expires:\r\n{}\r\n",
        browser.url()
    ))
}

fn register(
    terminal: &mut Terminal,
    trust: &TrustStore,
    deadline: Instant,
) -> Result<Registration, RecoveryError> {
    let browser = Browser::bind()?.until(deadline);
    let (mut pending, options) = PendingRegistration::start(trust, browser.port()?)?;
    terminal.restore()?;
    browser_url(terminal, &browser)?;
    browser.run("register", &serde_json::json!({"operation":"Create a password-manager-backed recovery passkey; NOT enrolled yet", "trust_domain":trust.trust_domain, "predecessor":trust.digest().to_string()}), &serde_json::to_value(options).map_err(|_| RecoveryError::Passkey)?, |value| {
        let response = serde_json::from_value(value).map_err(|_| RecoveryError::Passkey)?;
        let registration = pending.finish(trust, &response)?;
        if !registration.credential().backed_up()? { return Err(RecoveryError::InvalidChange("choose a backed-up password-manager passkey, not a device-bound credential")); }
        Ok((registration, "Passkey verified, but NOT enrolled. Return to the trusted terminal for the exact change and independent authorization."))
    })
}

fn review(bytes: &[u8], via: &str, deadline: Instant) -> Result<Terminal, RecoveryError> {
    let mut terminal = Terminal::open()?;
    terminal.write(&format!("\r\nExact public plan:\r\n{}\r\nAuthorization: {}\r\nType apply then Enter to authorize exactly this plan (hidden):\r\n", scan::escape(&String::from_utf8_lossy(bytes)), scan::escape(via)))?;
    if terminal.read_hidden_until(deadline)?.as_str() != "apply" {
        return Err(RecoveryError::Cancelled);
    }
    terminal.restore()?;
    Ok(terminal)
}

fn prepare_change(
    trust: &TrustStore,
    flags: &Flags<'_>,
    paper: Option<&PaperPhrase>,
    registration: Option<&Registration>,
) -> Result<RecoveryChange, RecoveryError> {
    let keys = replacement_keys(flags)?;
    if keys.is_empty() && paper.is_none() {
        return RecoveryChange::enroll_passkey(trust, registration.ok_or(RecoveryError::Passkey)?);
    }
    let mut change = RecoveryChange::new(trust, keys, paper)?;
    if let Some(registration) = registration {
        change.next_passkey = Some(registration.credential().clone());
        registration.check(&change)?;
    }
    recovery::validate(trust, &change)?;
    Ok(change)
}

fn change(store: &Store, trust: &TrustStore, flags: &Flags<'_>) -> Result<i32, CliError> {
    let deadline = Instant::now() + passkey::TIMEOUT;
    let via = flags.get("--via").ok_or_else(invalid)?;
    let mut terminal = Terminal::open()?;
    terminal.write(&format!(
        "\r\nChanging trust domain: {}\r\n",
        scan::escape(&trust.trust_domain)
    ))?;
    let old = if *via == "paper" {
        terminal.write("Enter CURRENT paper phrase (hidden); Ctrl-C cancels:\r\n")?;
        let phrase = PaperPhrase::parse(&terminal.read_hidden_until(deadline)?)?;
        if trust.paper_verifier.as_ref() != Some(&phrase.verifier(&trust.trust_domain)) {
            return Err(RecoveryError::Unauthorized.into());
        }
        Some(phrase)
    } else {
        None
    };
    let paper = flags
        .contains_key("--paper")
        .then(|| paper(&mut terminal, deadline))
        .transpose()?;
    let registration = flags
        .contains_key("--passkey")
        .then(|| register(&mut terminal, trust, deadline))
        .transpose()?;
    terminal.restore()?;
    let change = prepare_change(trust, flags, paper.as_ref(), registration.as_ref())?;
    let bytes = change.canonical_bytes();
    let mut terminal = review(&bytes, via, deadline)?;
    let proofs = proofs(flags, recovery::POSSESSION_NAMESPACE, &bytes, deadline)?;
    let signature = flags
        .get("--authorizer")
        .map(|key| {
            SshKeygenSigner::new(Path::new(key)).sign_until(
                recovery::RECOVERY_NAMESPACE,
                &bytes,
                deadline,
            )
        })
        .transpose()?;
    let confirmation = RecoveryConfirmation {
        paper: paper.as_ref(),
        registration: registration.as_ref(),
    };
    let browser = Browser::bind()?.until(deadline);
    let plan = serde_json::to_value(&change).map_err(|_| RecoveryError::Passkey)?;
    if *via == "passkey" {
        let (mut pending, options) = PendingAuthentication::start(trust, &change, browser.port()?)?;
        browser_url(&mut terminal, &browser)?;
        browser.run("authenticate", &plan, &serde_json::to_value(options).map_err(|_| RecoveryError::Passkey)?, |value| {
            let response = serde_json::from_value(value).map_err(|_| RecoveryError::Passkey)?;
            let approval = pending.finish(&response)?;
            recovery::apply(store, &change, RecoveryAuthorization::Passkey(approval), &proofs, confirmation, now_ms())?;
            Ok(((), "Committed: exactly the displayed recovery change. Recorded history remains verifiable."))
        })?;
    } else {
        let authorization = if let Some(old) = &old {
            RecoveryAuthorization::Phrase(old)
        } else {
            RecoveryAuthorization::SigningKey {
                role: Role::parse(via).ok_or_else(invalid)?,
                signature: signature.as_deref().ok_or_else(invalid)?,
            }
        };
        browser_url(&mut terminal, &browser)?;
        browser.run("confirm", &plan, &serde_json::json!({}), |value| {
            if value != serde_json::json!({}) || Instant::now() >= deadline { return Err(RecoveryError::Cancelled); }
            recovery::apply(store, &change, authorization, &proofs, confirmation, now_ms())?;
            Ok(((), "Committed: exactly the displayed recovery change. Only named replacements changed."))
        })?;
    }
    terminal.write("\r\nRecovery change committed. Any replaced method/key is retired. Keep only the new paper if paper changed.\r\n")?;
    Ok(0)
}
