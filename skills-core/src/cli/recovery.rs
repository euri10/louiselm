//! Local-only operator ceremony; no robot/stdin/argument secret input.

use std::{collections::BTreeMap, fs, path::Path};

use super::{CliError, now_ms};
use crate::{
    release, scan,
    signer::{Signer, SshKeygenSigner},
    store::Store,
    trust::{
        Role, TrustError, TrustStore,
        paper::{
            self, PaperAuthorization, PaperChange, PaperError, PaperPhrase, ReplacementKey,
            ReplacementProof, terminal::Terminal,
        },
    },
};

const HELP: &str = "recovery paper-enroll --store PATH --authorizer PRIVATE_KEY [--authorizer-role primary|release]
recovery paper-recover --store PATH [--primary NEW_PRIVATE_KEY] [--release NEW_PRIVATE_KEY]
Requires the installed trusted tool, protected production store and a local foreground TTY.
No phrase arguments, stdin, robot output or development override. No key flags: refresh paper only.
Do not run in an Agent terminal, recorded terminal or screen-sharing session.
Full one-YubiKey onboarding and Android passkeys are separate, pending work.";

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
        "paper-enroll" => &["--store", "--authorizer", "--authorizer-role"],
        "paper-recover" => &["--store", "--primary", "--release"],
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
    let authorizer = if command == "paper-enroll" {
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
        return Err(PaperError::UntrustedAuthority.into());
    }
    let store = Store::open(Path::new(path))?;
    paper::terminal::require_production(&store)?;
    let trust = TrustStore::load(&store)?.ok_or(TrustError::NotBootstrapped)?;
    paper::require_hardware_policy(&trust)?;
    rustix::process::setrlimit(
        rustix::process::Resource::Core,
        rustix::process::Rlimit {
            current: Some(0),
            maximum: Some(0),
        },
    )
    .map_err(|error| PaperError::Io(error.into()))?;
    ceremony(&store, &trust, &flags, authorizer)
}

fn ceremony(
    store: &Store,
    trust: &TrustStore,
    flags: &BTreeMap<&str, &str>,
    authorizer: Option<(Role, &str)>,
) -> Result<i32, CliError> {
    let replacements = [(Role::Primary, "--primary"), (Role::Release, "--release")]
        .into_iter()
        .filter_map(|(role, flag)| flags.get(flag).map(|path| (role, *path)))
        .map(|(role, path)| {
            let mut public_path = Path::new(path).as_os_str().to_os_string();
            public_path.push(".pub");
            fs::read_to_string(&public_path)
                .map(|public_key| ReplacementKey { role, public_key })
                .map_err(PaperError::Io)
        })
        .collect::<Result<Vec<_>, _>>()?;
    let mut terminal = Terminal::open()?;
    // No secret exists before the protected terminal opens successfully.
    let next = PaperPhrase::generate()?;
    let change = PaperChange::new(trust, replacements, &next)?;
    let old = if authorizer.is_none() {
        terminal.write("Enter current paper phrase (hidden); Ctrl-C cancels:\r\n")?;
        Some(PaperPhrase::parse(&terminal.read_hidden()?)?)
    } else {
        None
    };
    if old.as_ref().is_some_and(|phrase| {
        trust.paper_verifier.as_ref() != Some(&phrase.verifier(&trust.trust_domain))
    }) {
        return Err(PaperError::Unauthorized.into());
    }
    show_change(&mut terminal, &change)?;
    terminal
        .write("\r\nWrite this NEW recovery phrase on paper. Never reuse a wallet seed.\r\n")?;
    terminal.write(&next.expose_secret())?;
    terminal.write("\r\nKeep both papers until success. Press Enter after writing it.\r\n")?;
    if !terminal.read_hidden()?.is_empty() {
        return Err(PaperError::Cancelled.into());
    }
    terminal.clear()?;
    terminal.write("Re-enter the newly written 24 words (hidden):\r\n")?;
    let confirmation = PaperPhrase::parse(&terminal.read_hidden()?)?;
    if confirmation.verifier(&trust.trust_domain) != change.next_verifier {
        return Err(PaperError::Confirmation.into());
    }
    show_change(&mut terminal, &change)?;
    terminal.write("\r\nType apply then Enter to authorize exactly this change (hidden):\r\n")?;
    if terminal.read_hidden()?.as_str() != "apply" {
        return Err(PaperError::Cancelled.into());
    }
    // Restore before ssh-keygen needs the controlling TTY for PIN/touch.
    terminal.restore()?;
    let bytes = change.canonical_bytes();
    let signature = authorizer
        .map(|(_, key)| SshKeygenSigner::new(Path::new(key)).sign(paper::PAPER_NAMESPACE, &bytes))
        .transpose()?;
    let authorization = match (authorizer, signature.as_deref(), old.as_ref()) {
        (Some((role, _)), Some(signature), _) => PaperAuthorization::SigningKey { role, signature },
        (None, None, Some(phrase)) => PaperAuthorization::Phrase(phrase),
        _ => return Err(PaperError::Unauthorized.into()),
    };
    let proofs = [(Role::Primary, "--primary"), (Role::Release, "--release")]
        .into_iter()
        .filter_map(|(role, flag)| flags.get(flag).map(|key| (role, key)))
        .map(|(role, key)| {
            SshKeygenSigner::new(Path::new(key))
                .sign(paper::POSSESSION_NAMESPACE, &bytes)
                .map(|signature| ReplacementProof { role, signature })
        })
        .collect::<Result<Vec<_>, _>>()?;
    paper::apply(
        store,
        &change,
        authorization,
        &proofs,
        &confirmation,
        now_ms(),
    )?;
    println!(
        "Paper change committed. Only the new paper phrase is valid. Passkey/onboarding readiness is not asserted."
    );
    Ok(0)
}

fn show_change(terminal: &mut Terminal, change: &PaperChange) -> Result<(), PaperError> {
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
