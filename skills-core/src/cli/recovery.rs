//! Local-only operator ceremony; no robot/stdin/argument secret input.

use std::{collections::BTreeMap, fs, path::Path};

use super::{CliError, now_ms};
use crate::{
    release, scan,
    signer::{Signer, SshKeygenSigner},
    store::Store,
    trust::{
        Role, TrustError, TrustStore,
        browser::Browser,
        paper::PaperPhrase,
        passkey::{PendingAuthentication, PendingRegistration},
        recovery::{
            self, RecoveryAuthorization, RecoveryChange, RecoveryError, ReplacementKey,
            ReplacementProof,
        },
        terminal::Terminal,
    },
};

const HELP: &str = "recovery paper-enroll --store PATH --authorizer PRIVATE_KEY [--authorizer-role primary|release]
recovery paper-recover --store PATH [--primary NEW_PRIVATE_KEY] [--release NEW_PRIVATE_KEY]
recovery passkey-enroll --store PATH --authorizer PRIVATE_KEY [--authorizer-role primary|release]
recovery passkey-recover --store PATH [--primary NEW_PRIVATE_KEY] [--release NEW_PRIVATE_KEY] [--paper replace]
Requires the installed trusted tool, protected production store and a local foreground TTY.
No phrase arguments, stdin, robot output or development override. paper-recover without key flags refreshes paper only.
Do not run in an Agent terminal, recorded terminal or screen-sharing session.
Passkey enrollment/replacement requires a current signing key. Passkey recovery leaves paper unchanged unless --paper replace.
Open the ephemeral localhost URL in your normal browser, never a root browser.
Full one-YubiKey onboarding and real Android acceptance are separate, pending work.";

fn invalid() -> CliError {
    CliError::Invalid(
        "invalid recovery arguments; use recovery --help (never pass a phrase as an argument)"
            .to_owned(),
    )
}

pub(super) fn run(arguments: &[String]) -> Result<i32, CliError> {
    if arguments.len() == 1 && arguments[0] == "--help" {
        println!("{HELP}");
        return Ok(0);
    }
    let Some(command) = arguments.first().map(String::as_str) else {
        return Err(invalid());
    };
    let allowed: &[&str] = match command {
        "paper-enroll" | "passkey-enroll" => &["--store", "--authorizer", "--authorizer-role"],
        "paper-recover" => &["--store", "--primary", "--release"],
        "passkey-recover" => &["--store", "--primary", "--release", "--paper"],
        _ => return Err(invalid()),
    };
    let mut flags = BTreeMap::new();
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
    let path = flags.get("--store").ok_or_else(invalid)?;
    if flags
        .get("--paper")
        .is_some_and(|value| *value != "replace")
    {
        return Err(invalid());
    }
    let authorizer = if matches!(command, "paper-enroll" | "passkey-enroll") {
        let key = flags.get("--authorizer").ok_or_else(invalid)?;
        let role = match flags.get("--authorizer-role").copied().unwrap_or("primary") {
            "primary" => Role::Primary,
            "release" => Role::Release,
            _ => return Err(invalid()),
        };
        Some((role, *key))
    } else {
        None
    };
    // Refuse before opening/creating a store or requesting any secret.
    if !release::running_identity().verified {
        return Err(RecoveryError::UntrustedAuthority.into());
    }
    let store = Store::open(Path::new(path))?;
    crate::trust::terminal::require_production(&store)?;
    let trust = TrustStore::load(&store)?.ok_or(TrustError::NotBootstrapped)?;
    recovery::require_hardware_policy(&trust)?;
    rustix::process::setrlimit(
        rustix::process::Resource::Core,
        rustix::process::Rlimit {
            current: Some(0),
            maximum: Some(0),
        },
    )
    .map_err(|error| RecoveryError::Io(error.into()))?;
    match command {
        "passkey-enroll" => {
            let (role, key) = authorizer.ok_or_else(invalid)?;
            enroll_passkey(&store, &trust, role, key)
        }
        "passkey-recover" => recover_passkey(&store, &trust, &flags),
        _ => ceremony(&store, &trust, &flags, authorizer),
    }
}

fn replacement_keys(flags: &BTreeMap<&str, &str>) -> Result<Vec<ReplacementKey>, RecoveryError> {
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

fn ceremony(
    store: &Store,
    trust: &TrustStore,
    flags: &BTreeMap<&str, &str>,
    authorizer: Option<(Role, &str)>,
) -> Result<i32, CliError> {
    let replacements = replacement_keys(flags)?;
    let mut terminal = Terminal::open()?;
    // No secret exists before the protected terminal opens successfully.
    let next = PaperPhrase::generate()?;
    let change = RecoveryChange::new(trust, replacements, Some(&next))?;
    let old = if authorizer.is_none() {
        terminal.write("Enter current paper phrase (hidden); Ctrl-C cancels:\r\n")?;
        Some(PaperPhrase::parse(&terminal.read_hidden()?)?)
    } else {
        None
    };
    if old.as_ref().is_some_and(|phrase| {
        trust.paper_verifier.as_ref() != Some(&phrase.verifier(&trust.trust_domain))
    }) {
        return Err(RecoveryError::Unauthorized.into());
    }
    show_change(&mut terminal, &change)?;
    terminal
        .write("\r\nWrite this NEW recovery phrase on paper. Never reuse a wallet seed.\r\n")?;
    terminal.write(&next.expose_secret())?;
    terminal.write("\r\nKeep both papers until success. Press Enter after writing it.\r\n")?;
    if !terminal.read_hidden()?.is_empty() {
        return Err(RecoveryError::Cancelled.into());
    }
    terminal.clear()?;
    terminal.write("Re-enter the newly written 24 words (hidden):\r\n")?;
    let confirmation = PaperPhrase::parse(&terminal.read_hidden()?)?;
    if Some(confirmation.verifier(&trust.trust_domain)) != change.next_verifier {
        return Err(RecoveryError::Confirmation.into());
    }
    show_change(&mut terminal, &change)?;
    terminal.write("\r\nType apply then Enter to authorize exactly this change (hidden):\r\n")?;
    if terminal.read_hidden()?.as_str() != "apply" {
        return Err(RecoveryError::Cancelled.into());
    }
    // Restore before ssh-keygen needs the controlling TTY for PIN/touch.
    terminal.restore()?;
    let bytes = change.canonical_bytes();
    let signature = authorizer
        .map(|(_, key)| {
            SshKeygenSigner::new(Path::new(key)).sign(recovery::RECOVERY_NAMESPACE, &bytes)
        })
        .transpose()?;
    let authorization = match (authorizer, signature.as_deref(), old.as_ref()) {
        (Some((role, _)), Some(signature), _) => {
            RecoveryAuthorization::SigningKey { role, signature }
        }
        (None, None, Some(phrase)) => RecoveryAuthorization::Phrase(phrase),
        _ => return Err(RecoveryError::Unauthorized.into()),
    };
    let proofs = [(Role::Primary, "--primary"), (Role::Release, "--release")]
        .into_iter()
        .filter_map(|(role, flag)| flags.get(flag).map(|key| (role, key)))
        .map(|(role, key)| {
            SshKeygenSigner::new(Path::new(key))
                .sign(recovery::POSSESSION_NAMESPACE, &bytes)
                .map(|signature| ReplacementProof { role, signature })
        })
        .collect::<Result<Vec<_>, _>>()?;
    recovery::apply(
        store,
        &change,
        authorization,
        &proofs,
        recovery::RecoveryConfirmation {
            paper: Some(&confirmation),
            registration: None,
        },
        now_ms(),
    )?;
    println!(
        "Paper change committed. Only the new paper phrase is valid. Passkey/onboarding readiness is not asserted."
    );
    Ok(0)
}

fn show_change(terminal: &mut Terminal, change: &RecoveryChange) -> Result<(), RecoveryError> {
    terminal.write(&format!(
        "\r\nTrust domain: {}\r\nChange sequence: {}\r\n",
        scan::escape(&change.trust_domain),
        change.sequence
    ))?;
    if change.replacements.is_empty() {
        terminal.write("Signing keys unchanged; replace/enroll paper recovery only.\r\n")?;
    }
    for replacement in &change.replacements {
        terminal.write(&format!(
            "Replace {} key with: {}\r\n",
            replacement.role.name(),
            scan::escape(&replacement.public_key)
        ))?;
    }
    terminal
        .write("Any prior paper phrase becomes invalid. Other recovery methods stay unchanged.\r\n")
}

fn browser_url(terminal: &mut Terminal, browser: &Browser) -> Result<(), RecoveryError> {
    terminal.write(&format!(
        "\r\nOpen in your normal, non-root browser (expires in five minutes):\r\n{}\r\n",
        browser.url()
    ))
}

fn public_plan(change: &RecoveryChange) -> Result<serde_json::Value, RecoveryError> {
    serde_json::to_value(change).map_err(|_| RecoveryError::Passkey)
}

fn enroll_passkey(
    store: &Store,
    trust: &TrustStore,
    role: Role,
    key: &str,
) -> Result<i32, CliError> {
    let mut terminal = Terminal::open()?;
    let browser = Browser::bind()?;
    let (mut pending, options) = PendingRegistration::start(trust, browser.port()?)?;
    terminal.restore()?;
    browser_url(&mut terminal, &browser)?;
    let registration = browser.run("register", &serde_json::json!({"operation":"Prove possession of a new recovery passkey; no enrollment yet", "trust_domain":trust.trust_domain, "predecessor":trust.digest().to_string()}), &serde_json::to_value(options).map_err(|_| RecoveryError::Passkey)?, |value| {
        let response = serde_json::from_value(value).map_err(|_| RecoveryError::Passkey)?;
        pending.finish(trust, &response).map(|registration| (registration, "Passkey verified, but NOT enrolled. Return to the trusted terminal for signing-key authorization and the final review page."))
    })?;
    let change = RecoveryChange::enroll_passkey(trust, &registration)?;
    let mut terminal = Terminal::open()?;
    terminal.write(&format!("\r\nExact recovery change:\r\n{}\r\nPaper and ordinary signing keys remain unchanged.\r\nType apply then Enter to authorize with your current signing key:\r\n", scan::escape(&String::from_utf8_lossy(&change.canonical_bytes()))))?;
    if terminal
        .read_hidden_until(registration.deadline())?
        .as_str()
        != "apply"
    {
        return Err(RecoveryError::Cancelled.into());
    }
    terminal.restore()?;
    let signature = SshKeygenSigner::new(Path::new(key)).sign_until(
        recovery::RECOVERY_NAMESPACE,
        &change.canonical_bytes(),
        registration.deadline(),
    )?;
    let browser = Browser::bind()?.until(registration.deadline());
    browser_url(&mut terminal, &browser)?;
    browser.run("confirm", &public_plan(&change)?, &serde_json::json!({}), |value| {
        if value != serde_json::json!({}) { return Err(RecoveryError::Passkey); }
        recovery::apply(store, &change, RecoveryAuthorization::SigningKey{role, signature:&signature}, &[], recovery::RecoveryConfirmation{paper:None, registration:Some(&registration)}, now_ms())?;
        Ok(((), "Committed: recovery passkey enrolled. Paper and signing keys unchanged. Any previous passkey is retired."))
    })?;
    terminal.write("\r\nPasskey enrollment committed. Paper and signing keys unchanged.\r\n")?;
    Ok(0)
}

fn recover_passkey(
    store: &Store,
    trust: &TrustStore,
    flags: &BTreeMap<&str, &str>,
) -> Result<i32, CliError> {
    let mut terminal = Terminal::open()?;
    let next = flags
        .contains_key("--paper")
        .then(PaperPhrase::generate)
        .transpose()?;
    let confirmation = if let Some(next) = &next {
        terminal.write("Write this NEW paper phrase. Never put it in the browser.\r\n")?;
        terminal.write(&next.expose_secret())?;
        terminal.write("\r\nPress Enter after writing it; keep both papers until success.\r\n")?;
        if !terminal.read_hidden()?.is_empty() {
            return Err(RecoveryError::Cancelled.into());
        }
        terminal.clear()?;
        terminal.write("Re-enter the new phrase (hidden):\r\n")?;
        Some(PaperPhrase::parse(&terminal.read_hidden()?)?)
    } else {
        None
    };
    let change = RecoveryChange::new(trust, replacement_keys(flags)?, next.as_ref())?;
    if confirmation
        .as_ref()
        .map(|phrase| phrase.verifier(&trust.trust_domain))
        != change.next_verifier
    {
        return Err(RecoveryError::Confirmation.into());
    }
    terminal.write(&format!(
        "\r\nExact recovery change:\r\n{}\r\nReview this same change in the browser.\r\n",
        scan::escape(&String::from_utf8_lossy(&change.canonical_bytes()))
    ))?;
    terminal.restore()?;
    let proofs = [(Role::Primary, "--primary"), (Role::Release, "--release")]
        .into_iter()
        .filter_map(|(role, flag)| flags.get(flag).map(|key| (role, key)))
        .map(|(role, key)| {
            SshKeygenSigner::new(Path::new(key))
                .sign(recovery::POSSESSION_NAMESPACE, &change.canonical_bytes())
                .map(|signature| ReplacementProof { role, signature })
        })
        .collect::<Result<Vec<_>, _>>()?;
    let browser = Browser::bind()?;
    let (mut pending, options) = PendingAuthentication::start(trust, &change, browser.port()?)?;
    browser_url(&mut terminal, &browser)?;
    browser.run("authenticate", &public_plan(&change)?, &serde_json::to_value(options).map_err(|_| RecoveryError::Passkey)?, |value| {
        let response = serde_json::from_value(value).map_err(|_| RecoveryError::Passkey)?;
        let approval = pending.finish(&response)?;
        recovery::apply(store, &change, RecoveryAuthorization::Passkey(approval), &proofs, recovery::RecoveryConfirmation{paper:confirmation.as_ref(), registration:None}, now_ms())?;
        Ok(((), "Committed: exactly the displayed recovery change. Replaced signing keys cannot authorize new approvals; recorded history remains verifiable."))
    })?;
    terminal.write("\r\nRecovery change committed. Only methods explicitly displayed as replacements changed.\r\n")?;
    Ok(0)
}
