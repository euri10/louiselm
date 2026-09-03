//! Command-line wiring for the trusted skill tool.
//!
//! Every command recomputes what it reports from stored bytes. Exit status is
//! part of the contract, because an unattended caller has to be able to tell
//! "reviewed and clean" from "reviewed and not admissible" without parsing
//! prose:
//!
//! * `0` — the command succeeded and what it examined is admissible.
//! * `1` — the command failed; nothing was published and nothing is claimed.
//! * `2` — the command succeeded and what it examined is **not** admissible:
//!   verification failed, or Inspection produced a fatal finding.

use std::{
    env,
    path::{Path, PathBuf},
    time::{SystemTime, UNIX_EPOCH},
};

use serde::Serialize;
use thiserror::Error;

use crate::{
    admission::{self, AdmissionError, AdmissionRequest},
    canonical::{Digest, DigestError},
    dossier::{Dossier, DossierError, DossierRequest, ReviewDepth},
    install::{self, InstallError},
    policy::{Policy, PolicyError},
    quarantine::{self, QuarantineError},
    release::{
        self, AssembleRequest, ComponentInput, ComponentKind, ReleaseError, SourceIdentity,
        ToolchainIdentity,
    },
    render, robot,
    signer::{Signer, SshKeygenSigner},
    sshsig::{SkPolicy, TRUST_NAMESPACE},
    store::{PublishOutcome, Store, StoreError},
    trust::{Role, TrustError, TrustStore},
    witness::GitWitness,
};

/// The trust domain used when the operator names none.
pub const DEFAULT_TRUST_DOMAIN: &str = "louiselm/skills";

/// Exit status for a command whose subject is not admissible.
pub const EXIT_NOT_ADMISSIBLE: i32 = 2;

/// A command that could not be completed.
#[derive(Debug, Error)]
pub enum CliError {
    /// The command line is not valid.
    #[error("{0}")]
    Invalid(String),
    /// A digest argument is malformed.
    #[error(transparent)]
    Digest(#[from] DigestError),
    /// The policy could not be loaded.
    #[error(transparent)]
    Policy(#[from] PolicyError),
    /// A store operation failed.
    #[error(transparent)]
    Store(#[from] StoreError),
    /// A Dossier could not be built.
    #[error(transparent)]
    Dossier(#[from] DossierError),
    /// Robot output could not be serialized.
    #[error("cannot serialize output: {0}")]
    Serialize(#[from] serde_json::Error),
    /// A trust operation failed.
    #[error(transparent)]
    Trust(#[from] TrustError),
    /// An Admission or lifecycle step failed.
    #[error(transparent)]
    Admission(#[from] AdmissionError),
    /// A quarantine operation failed.
    #[error(transparent)]
    Quarantine(#[from] QuarantineError),
    /// Signing failed.
    #[error(transparent)]
    Signer(#[from] crate::signer::SignerError),
    /// A release operation failed.
    #[error(transparent)]
    Release(#[from] ReleaseError),
    /// An install operation failed.
    #[error(transparent)]
    Install(#[from] InstallError),
    /// A file named on the command line could not be read.
    #[error("cannot read '{path}': {source}")]
    Read {
        /// Path the caller named.
        path: String,
        /// Underlying failure.
        source: std::io::Error,
    },
}

/// What packaging a candidate produced.
#[derive(Debug, Serialize)]
struct PackageResult {
    schema: &'static str,
    digest: String,
    outcome: &'static str,
    entry_count: usize,
    total_bytes: u64,
    policy_digest: String,
}

/// What verifying a package produced.
#[derive(Debug, Serialize)]
struct VerifyResult {
    schema: &'static str,
    digest: String,
    recomputed_digest: String,
    intact: bool,
    failures: Vec<String>,
}

/// Runs the command named on the command line, returning its exit status.
pub fn run() -> Result<i32, CliError> {
    let arguments = env::args().skip(1).collect::<Vec<_>>();
    let Some(command) = arguments.first().map(String::as_str) else {
        print_help();
        return Ok(0);
    };
    if matches!(command, "help" | "--help" | "-h") {
        print_help();
        return Ok(0);
    }
    let options = Options::parse(&arguments[1..])?;

    match command {
        "package" => package(&options),
        "verify" => verify(&options),
        "inspect" => inspect(&options),
        "dossier" => dossier(&options),
        "list" => list(&options),
        "policy" => policy(&options),
        "trust" => trust(&options),
        "generation" => generation(&options),
        "quarantine" => quarantine_command(&options),
        "release" => release_command(&options),
        other => Err(CliError::Invalid(format!("unknown command '{other}'"))),
    }
}

struct Options {
    positional: Vec<String>,
    members: Vec<String>,
    views: Vec<String>,
    primary: Option<String>,
    recovery: Option<String>,
    trust_domain: Option<String>,
    role: Option<String>,
    key: Option<String>,
    signature: Option<PathBuf>,
    remote: Option<PathBuf>,
    branch: Option<String>,
    workdir: Option<PathBuf>,
    reason: Option<String>,
    output: Option<PathBuf>,
    source: Option<PathBuf>,
    bundle: Option<PathBuf>,
    prefix: Option<PathBuf>,
    require_hardware: bool,
    confirm: bool,
    store: Option<PathBuf>,
    policy: Option<PathBuf>,
    policy_digest: Option<String>,
    against: Option<String>,
    review_depth: ReviewDepth,
    assessment_model: Option<String>,
    assessment_prompt: Option<String>,
    captured_at: Option<u64>,
    robot: bool,
    digest_only: bool,
}

impl Options {
    fn parse(arguments: &[String]) -> Result<Self, CliError> {
        let mut parsed = Self {
            positional: Vec::new(),
            members: Vec::new(),
            views: Vec::new(),
            primary: None,
            recovery: None,
            trust_domain: None,
            role: None,
            key: None,
            signature: None,
            remote: None,
            branch: None,
            workdir: None,
            reason: None,
            output: None,
            source: None,
            bundle: None,
            prefix: None,
            require_hardware: false,
            confirm: false,
            store: None,
            policy: None,
            policy_digest: None,
            against: None,
            review_depth: ReviewDepth::Unstated,
            assessment_model: None,
            assessment_prompt: None,
            captured_at: None,
            robot: false,
            digest_only: false,
        };
        let mut index = 0;
        while index < arguments.len() {
            let argument = arguments[index].as_str();
            let value = |name: &str| -> Result<String, CliError> {
                arguments
                    .get(index + 1)
                    .cloned()
                    .ok_or_else(|| CliError::Invalid(format!("{name} needs a value")))
            };
            match argument {
                "--robot-json" => parsed.robot = true,
                "--digest" => parsed.digest_only = true,
                "--require-hardware" => parsed.require_hardware = true,
                "--confirm" => parsed.confirm = true,
                "--member" => {
                    parsed.members.push(value("--member")?);
                    index += 1;
                }
                "--view" => {
                    parsed.views.push(value("--view")?);
                    index += 1;
                }
                "--primary" => {
                    parsed.primary = Some(value("--primary")?);
                    index += 1;
                }
                "--recovery" => {
                    parsed.recovery = Some(value("--recovery")?);
                    index += 1;
                }
                "--trust-domain" => {
                    parsed.trust_domain = Some(value("--trust-domain")?);
                    index += 1;
                }
                "--role" => {
                    parsed.role = Some(value("--role")?);
                    index += 1;
                }
                "--key" => {
                    parsed.key = Some(value("--key")?);
                    index += 1;
                }
                "--signature" => {
                    parsed.signature = Some(PathBuf::from(value("--signature")?));
                    index += 1;
                }
                "--remote" => {
                    parsed.remote = Some(PathBuf::from(value("--remote")?));
                    index += 1;
                }
                "--branch" => {
                    parsed.branch = Some(value("--branch")?);
                    index += 1;
                }
                "--workdir" => {
                    parsed.workdir = Some(PathBuf::from(value("--workdir")?));
                    index += 1;
                }
                "--reason" => {
                    parsed.reason = Some(value("--reason")?);
                    index += 1;
                }
                "--output" => {
                    parsed.output = Some(PathBuf::from(value("--output")?));
                    index += 1;
                }
                "--source" => {
                    parsed.source = Some(PathBuf::from(value("--source")?));
                    index += 1;
                }
                "--bundle" => {
                    parsed.bundle = Some(PathBuf::from(value("--bundle")?));
                    index += 1;
                }
                "--prefix" => {
                    parsed.prefix = Some(PathBuf::from(value("--prefix")?));
                    index += 1;
                }
                "--store" => {
                    parsed.store = Some(PathBuf::from(value("--store")?));
                    index += 1;
                }
                "--policy" => {
                    parsed.policy = Some(PathBuf::from(value("--policy")?));
                    index += 1;
                }
                "--policy-digest" => {
                    parsed.policy_digest = Some(value("--policy-digest")?);
                    index += 1;
                }
                "--against" => {
                    parsed.against = Some(value("--against")?);
                    index += 1;
                }
                "--review-depth" => {
                    let raw = value("--review-depth")?;
                    parsed.review_depth = ReviewDepth::parse(&raw).ok_or_else(|| {
                        CliError::Invalid(format!(
                            "--review-depth must be unstated, skimmed, read, or reproduced, not '{raw}'"
                        ))
                    })?;
                    index += 1;
                }
                "--assessment-model" => {
                    parsed.assessment_model = Some(value("--assessment-model")?);
                    index += 1;
                }
                "--assessment-prompt" => {
                    parsed.assessment_prompt = Some(value("--assessment-prompt")?);
                    index += 1;
                }
                "--captured-at" => {
                    let raw = value("--captured-at")?;
                    parsed.captured_at = Some(raw.parse().map_err(|_| {
                        CliError::Invalid(format!(
                            "--captured-at must be milliseconds, not '{raw}'"
                        ))
                    })?);
                    index += 1;
                }
                other if other.starts_with('-') => {
                    return Err(CliError::Invalid(format!("unknown option '{other}'")));
                }
                other => parsed.positional.push(other.to_owned()),
            }
            index += 1;
        }
        Ok(parsed)
    }

    fn subject(&self, command: &str) -> Result<&str, CliError> {
        self.positional
            .first()
            .map(String::as_str)
            .ok_or_else(|| CliError::Invalid(format!("{command} needs an argument")))
    }

    fn policy(&self) -> Result<Policy, CliError> {
        match (&self.policy, &self.policy_digest) {
            (None, _) => Ok(Policy::embedded()),
            (Some(_), None) => Err(CliError::Invalid(
                "--policy requires --policy-digest: a policy accepted because it parsed is a policy an attacker may rewrite".to_owned(),
            )),
            (Some(path), Some(digest)) => {
                Ok(Policy::load(path, &Digest::parse(digest)?)?)
            }
        }
    }

    fn store(&self) -> Result<Store, CliError> {
        let root = match &self.store {
            Some(path) => path.clone(),
            None => default_store_root()?,
        };
        Ok(Store::open(&root)?)
    }

    fn sk_policy(&self) -> SkPolicy {
        if self.require_hardware {
            SkPolicy::require_presence_and_verification()
        } else {
            SkPolicy::none()
        }
    }

    /// Reads a public key from a file, or takes it literally.
    ///
    /// Both spellings appear in practice: `--primary ~/.ssh/id_admission.pub`
    /// during a ceremony, and a pasted `sk-ssh-ed25519 AAAA...` when the key
    /// came from somewhere else.
    fn key_material(&self, flag: &str, value: Option<&str>) -> Result<String, CliError> {
        let value = value.ok_or_else(|| CliError::Invalid(format!("{flag} needs a public key")))?;
        let path = Path::new(value);
        if path.is_file() {
            return Ok(read_text(path)?.trim().to_owned());
        }
        Ok(value.trim().to_owned())
    }

    fn required_role(&self) -> Result<Role, CliError> {
        let raw = self
            .role
            .as_deref()
            .ok_or_else(|| CliError::Invalid("--role is required".to_owned()))?;
        Role::parse(raw).ok_or_else(|| CliError::Invalid(format!("'{raw}' is not a role")))
    }

    fn required_reason(&self) -> Result<&str, CliError> {
        self.reason.as_deref().ok_or_else(|| {
            CliError::Invalid(
                "--reason is required: a quarantine nobody can explain is a quarantine nobody lifts"
                    .to_owned(),
            )
        })
    }

    fn required_bundle(&self) -> Result<PathBuf, CliError> {
        self.bundle
            .clone()
            .ok_or_else(|| CliError::Invalid("--bundle is required".to_owned()))
    }

    fn install_prefix(&self) -> PathBuf {
        self.prefix
            .clone()
            .unwrap_or_else(|| PathBuf::from(install::DEFAULT_PREFIX))
    }

    fn signing_key(&self) -> Result<PathBuf, CliError> {
        self.key
            .as_deref()
            .map(PathBuf::from)
            .ok_or_else(|| CliError::Invalid("--key is required to sign".to_owned()))
    }

    fn generation_digest(&self) -> Result<Digest, CliError> {
        let raw = self.positional.get(1).ok_or_else(|| {
            CliError::Invalid("this command needs a generation digest".to_owned())
        })?;
        Ok(Digest::parse(raw)?)
    }

    fn admission_members(&self) -> Result<Vec<(Digest, ReviewDepth)>, CliError> {
        if self.members.is_empty() {
            return Err(CliError::Invalid(
                "generation admit needs at least one --member <digest>[:<review-depth>]".to_owned(),
            ));
        }
        self.members
            .iter()
            .map(|raw| {
                // A digest is spelled `sha256:<hex>`, so splitting on the last
                // colon would read the hex as a review depth. Try the whole
                // string as a digest first; only then treat a suffix as depth.
                if let Ok(digest) = Digest::parse(raw) {
                    return Ok((digest, ReviewDepth::Unstated));
                }
                let (digest, depth) = raw
                    .rsplit_once(':')
                    .ok_or_else(|| CliError::Invalid(format!("'{raw}' is not a digest")))?;
                let depth = ReviewDepth::parse(depth)
                    .ok_or_else(|| CliError::Invalid(format!("'{depth}' is not a review depth")))?;
                Ok((Digest::parse(digest)?, depth))
            })
            .collect()
    }

    fn view_roots(&self) -> Result<std::collections::BTreeMap<String, String>, CliError> {
        self.views
            .iter()
            .map(|raw| {
                raw.split_once('=')
                    .map(|(provider, root)| (provider.to_owned(), root.to_owned()))
                    .ok_or_else(|| {
                        CliError::Invalid(format!("--view expects <provider>=<root>, got '{raw}'"))
                    })
            })
            .collect()
    }

    fn witness(&self) -> Result<GitWitness, CliError> {
        let remote = self.remote.as_ref().ok_or_else(|| {
            CliError::Invalid("--remote is required to witness a Generation".to_owned())
        })?;
        let branch = self.branch.as_deref().unwrap_or("skill-generations");
        let workdir = match &self.workdir {
            Some(path) => path.clone(),
            None => self.store()?.root().join("witness-work"),
        };
        Ok(GitWitness::new(remote, branch, &workdir))
    }

    fn dossier_request<'a>(
        &'a self,
        digest: &'a Digest,
        base: Option<&'a Digest>,
    ) -> DossierRequest<'a> {
        let mut request = DossierRequest::new(digest).with_review_depth(self.review_depth);
        if let Some(base) = base {
            request = request.against(base);
        }
        if let (Some(model), Some(prompt)) = (&self.assessment_model, &self.assessment_prompt) {
            request = request.with_assessment_key(model, prompt);
        }
        request
    }
}

fn package(options: &Options) -> Result<i32, CliError> {
    let source = PathBuf::from(options.subject("package")?);
    let policy = options.policy()?;
    let store = options.store()?;
    let captured_at = match options.captured_at {
        Some(value) => value,
        None => now_ms(),
    };
    let (package, outcome) = store.capture(&source, &policy, captured_at)?;
    let result = PackageResult {
        schema: "louiselm.skills.package-result/1",
        digest: package.digest.to_string(),
        outcome: match outcome {
            PublishOutcome::Created => "created",
            PublishOutcome::Existing => "existing",
        },
        entry_count: package.manifest.entries.len(),
        total_bytes: package.manifest.total_size(),
        policy_digest: policy.digest().to_string(),
    };
    if options.robot {
        println!("{}", robot::payload(&result)?);
    } else {
        println!(
            "{} {} ({} file(s), {} byte(s))",
            result.outcome, result.digest, result.entry_count, result.total_bytes,
        );
    }
    Ok(0)
}

fn verify(options: &Options) -> Result<i32, CliError> {
    let digest = Digest::parse(options.subject("verify")?)?;
    let policy = options.policy()?;
    let report = options.store()?.verify(&digest, &policy)?;
    let result = VerifyResult {
        schema: "louiselm.skills.verify-result/1",
        digest: report.digest.to_string(),
        recomputed_digest: report.recomputed_digest.to_string(),
        intact: report.is_intact(),
        failures: report
            .failures
            .iter()
            .map(|failure| crate::scan::escape(&failure.summary()))
            .collect(),
    };
    if options.robot {
        println!("{}", robot::payload(&result)?);
    } else if result.intact {
        println!("{} verified against its stored bytes", result.digest);
    } else {
        println!("{} FAILED verification", result.digest);
        for failure in &result.failures {
            println!("  - {failure}");
        }
    }
    Ok(if result.intact {
        0
    } else {
        EXIT_NOT_ADMISSIBLE
    })
}

fn inspect(options: &Options) -> Result<i32, CliError> {
    let digest = Digest::parse(options.subject("inspect")?)?;
    let policy = options.policy()?;
    let store = options.store()?;
    let package = store.open_package(&digest, &policy)?;
    let inspection = crate::inspect::Inspection::run(&package, &policy)?;
    if options.robot {
        println!("{}", robot::payload(&inspection)?);
    } else {
        for (kind, count) in inspection.counts_by_kind() {
            println!("{kind:<22} {count}");
        }
        for fatal in &inspection.fatal {
            println!("fatal: {}", fatal.message);
        }
    }
    Ok(if inspection.is_fatal() {
        EXIT_NOT_ADMISSIBLE
    } else {
        0
    })
}

fn dossier(options: &Options) -> Result<i32, CliError> {
    let digest = Digest::parse(options.subject("dossier")?)?;
    let base = options.against.as_deref().map(Digest::parse).transpose()?;
    let policy = options.policy()?;
    let store = options.store()?;
    let dossier = Dossier::build(
        &store,
        &policy,
        &options.dossier_request(&digest, base.as_ref()),
    )?;
    if options.robot {
        println!("{}", robot::json(&dossier)?);
    } else {
        print!("{}", render::human(&dossier));
    }
    Ok(if dossier.reviewable() {
        0
    } else {
        EXIT_NOT_ADMISSIBLE
    })
}

fn list(options: &Options) -> Result<i32, CliError> {
    let store = options.store()?;
    let digests = store.list()?;
    if options.robot {
        let rendered = digests.iter().map(Digest::to_string).collect::<Vec<_>>();
        println!("{}", robot::payload(&rendered)?);
    } else {
        for digest in &digests {
            println!("{digest}");
        }
    }
    Ok(0)
}

fn policy(options: &Options) -> Result<i32, CliError> {
    let policy = options.policy()?;
    if options.digest_only {
        println!("{}", policy.digest());
    } else if options.policy.is_some() {
        println!("{}", robot::payload(policy.document())?);
    } else {
        println!(
            "{}",
            String::from_utf8_lossy(Policy::embedded_bytes()).trim_end()
        );
    }
    Ok(0)
}

fn trust(options: &Options) -> Result<i32, CliError> {
    let store = options.store()?;
    match options.subject("trust")? {
        "bootstrap" => {
            let primary = options.key_material("--primary", options.primary.as_deref())?;
            let recovery = options.key_material("--recovery", options.recovery.as_deref())?;
            let trust = TrustStore::bootstrap(
                &store,
                options
                    .trust_domain
                    .as_deref()
                    .unwrap_or(DEFAULT_TRUST_DOMAIN),
                &primary,
                &recovery,
                options.sk_policy(),
                now_ms(),
            )?;
            report(options, &trust, |trust| {
                format!("trust bootstrapped for {}", trust.trust_domain)
            })
        }
        "show" => {
            let trust = TrustStore::load(&store)?.ok_or(TrustError::NotBootstrapped)?;
            report(options, &trust, |trust| {
                let mut lines = vec![format!(
                    "domain {} (change sequence {})",
                    trust.trust_domain, trust.sequence
                )];
                for key in &trust.keys {
                    lines.push(format!("  {:<9} {}", key.role.name(), key.public_key));
                }
                lines.join("\n")
            })
        }
        "rotation-payload" => {
            let trust = TrustStore::load(&store)?.ok_or(TrustError::NotBootstrapped)?;
            let change = trust.rotation_payload(
                options.required_role()?,
                &options.key_material("--key", options.key.as_deref())?,
                options.sk_policy(),
            );
            // Printed without a trailing newline: these are the exact bytes the
            // recovery key signs, and a newline would change them.
            print!("{}", String::from_utf8_lossy(&change.canonical_bytes()));
            Ok(0)
        }
        "rotate" => {
            let trust = TrustStore::load(&store)?.ok_or(TrustError::NotBootstrapped)?;
            let change = trust.rotation_payload(
                options.required_role()?,
                &options.key_material("--key", options.key.as_deref())?,
                options.sk_policy(),
            );
            let signature_path = options.signature.as_ref().ok_or_else(|| {
                CliError::Invalid(format!(
                    "trust rotate needs --signature: sign the rotation payload in the {TRUST_NAMESPACE} namespace with the recovery key"
                ))
            })?;
            let signature = read_text(signature_path)?;
            let rotated = TrustStore::rotate(&store, &change, &signature, now_ms())?;
            report(options, &rotated, |trust| {
                format!("trust change {} applied", trust.sequence)
            })
        }
        "reset" => {
            if !options.confirm {
                return Err(CliError::Invalid(
                    "trust reset discards every enrolled key and invalidates every Generation they signed; pass --confirm".to_owned(),
                ));
            }
            TrustStore::reset(&store)?;
            println!("trust reset; re-enroll and re-admit before any verified Session");
            Ok(0)
        }
        other => Err(CliError::Invalid(format!(
            "unknown trust command '{other}'"
        ))),
    }
}

fn generation(options: &Options) -> Result<i32, CliError> {
    let store = options.store()?;
    let policy = options.policy()?;
    match options.subject("generation")? {
        "admit" => {
            let key = options.signing_key()?;
            let record = admission::admit(
                &store,
                &policy,
                &AdmissionRequest {
                    members: options.admission_members()?,
                    view_roots: options.view_roots()?,
                    signer: &SshKeygenSigner::new(&key),
                    admitted_at_ms: now_ms(),
                },
            )?;
            report(options, &record, |record| {
                format!(
                    "generation {} signed at sequence {}; witness it before it governs anything",
                    record.generation, record.payload.sequence
                )
            })
        }
        "witness" => {
            let digest = options.generation_digest()?;
            let record = admission::witness(&store, &digest, &options.witness()?, now_ms())?;
            report(options, &record, |record| {
                format!("generation {} witnessed", record.generation)
            })
        }
        "activate" => {
            let digest = options.generation_digest()?;
            let record = admission::activate(&store, &digest, now_ms())?;
            report(options, &record, |record| {
                format!(
                    "generation {} is current at sequence {}",
                    record.generation, record.payload.sequence
                )
            })
        }
        "status" => {
            let status = admission::status(&store)?;
            let admissible = status.state == Some(crate::generation::GenerationState::Current);
            if options.robot {
                println!("{}", robot::payload(&status)?);
            } else {
                println!("{}", render::generation_status(&status));
            }
            Ok(if admissible { 0 } else { EXIT_NOT_ADMISSIBLE })
        }
        "list" => {
            let records = admission::list(&store)?;
            if options.robot {
                println!("{}", robot::payload(&records)?);
            } else {
                for record in &records {
                    println!(
                        "{:>4} {} {}",
                        record.payload.sequence,
                        record.state.name(),
                        record.generation
                    );
                }
            }
            Ok(0)
        }
        other => Err(CliError::Invalid(format!(
            "unknown generation command '{other}'"
        ))),
    }
}

fn quarantine_command(options: &Options) -> Result<i32, CliError> {
    let store = options.store()?;
    match options.subject("quarantine")? {
        "exclude" => {
            let packages = options.positional[1..].to_vec();
            if packages.is_empty() {
                return Err(CliError::Invalid(
                    "quarantine exclude needs at least one package digest".to_owned(),
                ));
            }
            for package in &packages {
                Digest::parse(package)?;
            }
            let quarantine =
                quarantine::exclude(&store, &packages, options.required_reason()?, now_ms())?;
            report(options, &quarantine, |quarantine| {
                format!("{} package(s) excluded", quarantine.excluded.len())
            })
        }
        "all" => {
            let quarantine =
                quarantine::exclude_everything(&store, options.required_reason()?, now_ms())?;
            report(options, &quarantine, |_| {
                "every member of the current Generation is excluded".to_owned()
            })
        }
        "show" => match quarantine::load(&store)? {
            Some(quarantine) => report(options, &quarantine, |quarantine| {
                let mut lines = vec![format!(
                    "excluded {} package(s), everything={}",
                    quarantine.excluded.len(),
                    quarantine.excludes_everything
                )];
                lines.extend(
                    quarantine
                        .excluded
                        .iter()
                        .map(|digest| format!("  {digest}")),
                );
                lines.extend(
                    quarantine
                        .reasons
                        .iter()
                        .map(|reason| format!("  # {reason}")),
                );
                lines.join("\n")
            }),
            None => {
                println!("no quarantine is active");
                Ok(0)
            }
        },
        "clear" => {
            quarantine::clear(&store, now_ms())?;
            Ok(0)
        }
        other => Err(CliError::Invalid(format!(
            "unknown quarantine command '{other}'"
        ))),
    }
}

fn release_command(options: &Options) -> Result<i32, CliError> {
    match options.subject("release")? {
        "build" => {
            let source = options.source.clone().unwrap_or_else(|| PathBuf::from("."));
            let output = options.output.clone().ok_or_else(|| {
                CliError::Invalid("release build needs --output <bundle-dir>".to_owned())
            })?;
            let lock = source.join("Cargo.lock");
            let dependencies =
                Digest::of(&std::fs::read(&lock).map_err(|error| CliError::Read {
                    path: lock.display().to_string(),
                    source: error,
                })?);
            let identity = SourceIdentity::of(&source, &dependencies.to_string())?;
            let toolchain = ToolchainIdentity::detect()?;

            // --locked, so a lockfile that would have been updated is a
            // refusal rather than a silent difference between what was
            // reviewed and what was built.
            let status = std::process::Command::new("cargo")
                .current_dir(&source)
                .args(["build", "--release", "--locked"])
                .status()
                .map_err(|error| ReleaseError::Tool {
                    tool: "cargo".to_owned(),
                    reason: error.to_string(),
                })?;
            if !status.success() {
                return Err(CliError::Release(ReleaseError::Tool {
                    tool: "cargo".to_owned(),
                    reason: "release build failed".to_owned(),
                }));
            }

            let manifest = release::assemble(
                &AssembleRequest {
                    source: identity,
                    toolchain,
                    policy: &Policy::embedded(),
                    components: vec![ComponentInput {
                        name: "louiselm-skills".to_owned(),
                        path: source.join("target/release/louiselm-skills"),
                        kind: ComponentKind::Executable,
                    }],
                    built_at_ms: now_ms(),
                },
                &output,
            )?;
            report(options, &manifest, |manifest| {
                format!(
                    "release {} built from {} ({} component(s)); sign it before installing",
                    manifest.release_id,
                    manifest.source.commit,
                    manifest.components.len()
                )
            })
        }
        "sign" => {
            let bundle = options.required_bundle()?;
            let key = options.signing_key()?;
            let manifest_path = bundle.join("manifest.json");
            let bytes = std::fs::read(&manifest_path).map_err(|error| CliError::Read {
                path: manifest_path.display().to_string(),
                source: error,
            })?;
            let signature = SshKeygenSigner::new(&key).sign(release::RELEASE_NAMESPACE, &bytes)?;
            std::fs::write(bundle.join("manifest.sig"), &signature).map_err(|error| {
                CliError::Read {
                    path: bundle.join("manifest.sig").display().to_string(),
                    source: error,
                }
            })?;
            println!("signed {}", bundle.display());
            Ok(0)
        }
        "verify" => {
            let bundle = options.required_bundle()?;
            let trust = TrustStore::load(&options.store()?)?.ok_or(TrustError::NotBootstrapped)?;
            let manifest = release::verify_bundle(&bundle, &trust)?;
            report(options, &manifest, |manifest| {
                format!("release {} verifies", manifest.release_id)
            })
        }
        "install" => {
            let bundle = options.required_bundle()?;
            let prefix = options.install_prefix();
            let state = install::install(&options.store()?, &bundle, &prefix, now_ms())?;
            report(options, &state, |state| {
                format!(
                    "release {} installed at {}",
                    state.release_id,
                    prefix.display()
                )
            })
        }
        "status" => {
            let status = install::status(&options.install_prefix())?;
            let trusted = status.trusted;
            if options.robot {
                println!("{}", robot::payload(&status)?);
            } else {
                println!("{}", render::install_status(&status));
            }
            Ok(if trusted { 0 } else { EXIT_NOT_ADMISSIBLE })
        }
        "identity" => {
            let identity = release::running_identity();
            let verified = identity.verified;
            if options.robot {
                println!("{}", robot::payload(&identity)?);
            } else {
                println!(
                    "{} — {}",
                    if verified { "verified" } else { "unverified" },
                    identity.detail
                );
            }
            Ok(if verified { 0 } else { EXIT_NOT_ADMISSIBLE })
        }
        other => Err(CliError::Invalid(format!(
            "unknown release command '{other}'"
        ))),
    }
}

fn report<T: Serialize>(
    options: &Options,
    value: &T,
    human: impl Fn(&T) -> String,
) -> Result<i32, CliError> {
    if options.robot {
        println!("{}", robot::payload(value)?);
    } else {
        println!("{}", human(value));
    }
    Ok(0)
}

fn read_text(path: &Path) -> Result<String, CliError> {
    std::fs::read_to_string(path).map_err(|source| CliError::Read {
        path: path.display().to_string(),
        source,
    })
}

fn default_store_root() -> Result<PathBuf, CliError> {
    if let Ok(explicit) = env::var("LOUISELM_SKILLS_STORE") {
        return Ok(PathBuf::from(explicit));
    }
    if let Ok(state) = env::var("XDG_STATE_HOME") {
        return Ok(Path::new(&state).join("louiselm/skills"));
    }
    let home = env::var("HOME").map_err(|_| {
        CliError::Invalid(
            "no store location: set LOUISELM_SKILLS_STORE, XDG_STATE_HOME, or HOME".to_owned(),
        )
    })?;
    Ok(Path::new(&home).join(".local/state/louiselm/skills"))
}

fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|elapsed| elapsed.as_millis() as u64)
        .unwrap_or_default()
}

fn print_help() {
    println!(
        "louiselm-skills — package Skill candidates, admit Skill Generations

Packaging and review:
  louiselm-skills package <candidate-dir> [--captured-at <ms>]
  louiselm-skills verify <digest>
  louiselm-skills inspect <digest>
  louiselm-skills dossier <digest> [--against <digest>] [--review-depth <depth>]
                                   [--assessment-model <m> --assessment-prompt <p>]
  louiselm-skills list
  louiselm-skills policy [--digest]

Trust roles:
  louiselm-skills trust bootstrap --primary <key> --recovery <key>
                                  [--trust-domain <d>] [--require-hardware]
  louiselm-skills trust show
  louiselm-skills trust rotation-payload --role primary --key <key>
  louiselm-skills trust rotate --role primary --key <key> --signature <file>
  louiselm-skills trust reset --confirm

Skill Generations:
  louiselm-skills generation admit --member <digest>[:<depth>] ... --key <privkey>
                                   [--view <provider>=<root>]
  louiselm-skills generation witness <digest> --remote <url> [--branch <b>]
  louiselm-skills generation activate <digest>
  louiselm-skills generation status
  louiselm-skills generation list

Trusted release:
  louiselm-skills release build --output <dir> [--source <dir>]
  louiselm-skills release sign --bundle <dir> --key <privkey>
  louiselm-skills release verify --bundle <dir>
  louiselm-skills release install --bundle <dir> [--prefix <dir>]
  louiselm-skills release status [--prefix <dir>]
  louiselm-skills release identity

Emergency quarantine (narrows only; no token needed):
  louiselm-skills quarantine exclude <digest>... --reason <text>
  louiselm-skills quarantine all --reason <text>
  louiselm-skills quarantine show

Options:
  --store <dir>          Store root; defaults to $LOUISELM_SKILLS_STORE, then
                         $XDG_STATE_HOME/louiselm/skills, then ~/.local/state/louiselm/skills.
  --policy <file>        Replacement Inspection policy. Requires --policy-digest.
  --policy-digest <d>    The digest the replacement policy must have.
  --review-depth <d>     unstated | skimmed | read | reproduced. A recorded claim, not a proof.
  --require-hardware     Enrolled keys must be FIDO keys that report touch and user verification.
  --robot-json           Emit the machine-readable view instead of the human one.

Exit status:
  0  succeeded; the subject is admissible
  1  failed; nothing was published and nothing is claimed
  2  succeeded; the subject is NOT admissible (verification failed, a fatal finding,
     or no Skill Generation is in force)"
    );
}
