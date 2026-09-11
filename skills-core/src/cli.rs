//! Command-line wiring for the trusted skill tool.
//!
//! Every command recomputes what it reports from stored bytes. Exit status is
//! part of the contract, because an unattended caller has to be able to tell
//! "reviewed and clean" from "reviewed and not admissible" without parsing
//! prose:
//!
//! * `0` — the command succeeded and what it examined is admissible.
//! * `1` — the command failed; no success is claimed. Inspect state before
//!   retrying a persistence failure, which can follow an atomic publication.
//! * `2` — the command succeeded and what it examined is **not** admissible:
//!   verification failed, or Inspection produced a fatal finding.

use std::{
    env,
    path::{Path, PathBuf},
    time::{SystemTime, UNIX_EPOCH},
};

use serde::Serialize;
use thiserror::Error;

mod preflight;
mod recovery;
mod workspace;

use crate::{
    admission::{self, AdmissionError, AdmissionMember, AdmissionRequest},
    canonical::{Digest, DigestError},
    dossier::{Dossier, DossierError, DossierRequest, ReviewDepth},
    install::{self, InstallError},
    launcher_install::{
        self, IdentityPool, InstallRequest as LauncherInstallRequest, LauncherError, LauncherPaths,
        RotationRequest, SystemCommandRunner,
    },
    policy::{Policy, PolicyError},
    quarantine::{self, QuarantineError},
    release::{
        self, AssembleRequest, ComponentInput, ComponentKind, ReleaseError, SourceIdentity,
        ToolchainIdentity,
    },
    render, robot,
    signer::SshKeygenSigner,
    sshsig::SkPolicy,
    store::{PublishOutcome, Store, StoreError},
    trust::{TrustError, TrustStore},
    witness::GitWitness,
};

/// The trust domain used when the operator names none.
pub const DEFAULT_TRUST_DOMAIN: &str = "louiselm/skills";

/// Exit status for a command whose subject is not admissible.
pub const EXIT_NOT_ADMISSIBLE: i32 = 2;

/// A command that could not be completed.
#[derive(Debug, Error)]
pub enum CliError {
    /// A local recovery ceremony was refused.
    #[error(transparent)]
    Recovery(#[from] crate::trust::recovery::RecoveryError),
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
    /// An Instruction view could not be materialized or verified.
    #[error("{}", crate::scan::escape(&.0.to_string()))]
    View(#[from] crate::instruction_view::ViewError),
    /// A launcher authority operation failed.
    #[error(transparent)]
    Launcher(#[from] LauncherError),
    /// A launch registry named for Agent expansion or views could not be read.
    #[error(transparent)]
    Registry(#[from] crate::registry::RegistryError),
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
///
/// # Errors
/// Returns argument/configuration errors or the selected command's storage, trust, signing, registry, release, or launch failures.
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
    // Secret-channel commands have a closed parser whose diagnostics never
    // repeat unknown arguments, even when someone mistakenly supplies a phrase.
    if command == "recovery" {
        return recovery::run(&arguments[1..]);
    }
    if command == "preflight" {
        return preflight::run(&arguments[1..]);
    }
    if command == "workspace" {
        return workspace::run(&arguments[1..]);
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
        "view" => view_command(&options),
        "quarantine" => quarantine_command(&options),
        "release" => release_command(&options),
        "launcher" => launcher_command(&options),
        other => Err(CliError::Invalid(format!("unknown command '{other}'"))),
    }
}

#[expect(
    clippy::struct_excessive_bools,
    reason = "Independent command-line switches mirror the grammar; they are not exclusive states."
)]
struct Options {
    positional: Vec<String>,
    members: Vec<String>,
    all_agents: bool,
    registry: Option<PathBuf>,
    primary: Option<String>,
    release_key: Option<String>,
    trust_domain: Option<String>,
    key: Option<String>,
    remote: Option<PathBuf>,
    branch: Option<String>,
    workdir: Option<PathBuf>,
    reason: Option<String>,
    output: Option<PathBuf>,
    source: Option<PathBuf>,
    bundle: Option<PathBuf>,
    prefix: Option<PathBuf>,
    operator: Option<String>,
    broker_uid: Option<u32>,
    broker_gid: Option<u32>,
    uid_start: Option<u32>,
    gid_start: Option<u32>,
    slots: Option<u32>,
    rotation_id: Option<String>,
    expected_key_id: Option<String>,
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
    #[expect(
        clippy::too_many_lines,
        reason = "One flat CLI grammar table keeps flag spelling, parsing and duplicate checks together."
    )]
    fn parse(arguments: &[String]) -> Result<Self, CliError> {
        let mut parsed = Self {
            positional: Vec::new(),
            members: Vec::new(),
            all_agents: false,
            registry: None,
            primary: None,
            release_key: None,
            trust_domain: None,
            key: None,
            remote: None,
            branch: None,
            workdir: None,
            reason: None,
            output: None,
            source: None,
            bundle: None,
            prefix: None,
            operator: None,
            broker_uid: None,
            broker_gid: None,
            uid_start: None,
            gid_start: None,
            slots: None,
            rotation_id: None,
            expected_key_id: None,
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
                "--all-agents" => parsed.all_agents = true,
                "--registry" => {
                    parsed.registry = Some(PathBuf::from(value("--registry")?));
                    index += 1;
                }
                "--primary" => {
                    parsed.primary = Some(value("--primary")?);
                    index += 1;
                }
                "--release" => {
                    parsed.release_key = Some(value("--release")?);
                    index += 1;
                }
                "--trust-domain" => {
                    parsed.trust_domain = Some(value("--trust-domain")?);
                    index += 1;
                }
                "--key" => {
                    parsed.key = Some(value("--key")?);
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
                "--operator" => {
                    parsed.operator = Some(value("--operator")?);
                    index += 1;
                }
                "--broker-uid" => {
                    parsed.broker_uid = Some(parse_u32("--broker-uid", &value("--broker-uid")?)?);
                    index += 1;
                }
                "--broker-gid" => {
                    parsed.broker_gid = Some(parse_u32("--broker-gid", &value("--broker-gid")?)?);
                    index += 1;
                }
                "--uid-start" => {
                    parsed.uid_start = Some(parse_u32("--uid-start", &value("--uid-start")?)?);
                    index += 1;
                }
                "--gid-start" => {
                    parsed.gid_start = Some(parse_u32("--gid-start", &value("--gid-start")?)?);
                    index += 1;
                }
                "--slots" => {
                    parsed.slots = Some(parse_u32("--slots", &value("--slots")?)?);
                    index += 1;
                }
                "--rotation-id" => {
                    parsed.rotation_id = Some(value("--rotation-id")?);
                    index += 1;
                }
                "--expected-key-id" => {
                    parsed.expected_key_id = Some(value("--expected-key-id")?);
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
    fn key_material(flag: &str, value: Option<&str>) -> Result<String, CliError> {
        let value = value.ok_or_else(|| CliError::Invalid(format!("{flag} needs a public key")))?;
        let path = Path::new(value);
        if path.is_file() {
            return Ok(read_text(path)?.trim().to_owned());
        }
        Ok(value.trim().to_owned())
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

    fn launcher_install_request(&self) -> Result<LauncherInstallRequest, CliError> {
        Ok(LauncherInstallRequest {
            operator: required(self.operator.as_ref(), "--operator")?.to_owned(),
            broker_uid: *required(self.broker_uid.as_ref(), "--broker-uid")?,
            broker_gid: *required(self.broker_gid.as_ref(), "--broker-gid")?,
            pool: IdentityPool {
                uid_start: *required(self.uid_start.as_ref(), "--uid-start")?,
                gid_start: *required(self.gid_start.as_ref(), "--gid-start")?,
                slots: *required(self.slots.as_ref(), "--slots")?,
            },
        })
    }

    fn rotation_request(&self) -> Result<RotationRequest, CliError> {
        Ok(RotationRequest {
            rotation_id: required(self.rotation_id.as_ref(), "--rotation-id")?.to_owned(),
            expected_active_key_id: required(self.expected_key_id.as_ref(), "--expected-key-id")?
                .to_owned(),
        })
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

    fn admission_members(&self) -> Result<Vec<AdmissionMember>, CliError> {
        if self.members.is_empty() {
            return Err(CliError::Invalid(
                "generation admit needs at least one --member <digest>[:<review-depth>][=<agent>,...]"
                    .to_owned(),
            ));
        }
        // --all-agents supplies the scope for members that do not name one. It
        // expands here, at signing time, so the payload always carries literal
        // names and never a wildcard whose meaning the registry could change
        // afterwards (louiselm-5qzq).
        let expanded = if self.all_agents {
            Some(self.registered_agents()?)
        } else {
            None
        };
        self.members
            .iter()
            .map(|raw| {
                let (subject, agents) = match raw.split_once('=') {
                    Some((subject, agents)) => (
                        subject,
                        agents
                            .split(',')
                            .map(str::trim)
                            .filter(|name| !name.is_empty())
                            .map(ToOwned::to_owned)
                            .collect::<Vec<_>>(),
                    ),
                    None => (
                        raw.as_str(),
                        expanded.clone().ok_or_else(|| {
                            CliError::Invalid(format!(
                                "'{raw}' names no Agent: append =<agent>,... or pass --all-agents"
                            ))
                        })?,
                    ),
                };
                let (package, depth) = Self::member_subject(subject)?;
                Ok(AdmissionMember {
                    package,
                    depth,
                    agents,
                })
            })
            .collect()
    }

    fn member_subject(raw: &str) -> Result<(Digest, ReviewDepth), CliError> {
        // A digest is spelled `sha256:<hex>`, so splitting on the last colon
        // would read the hex as a review depth. Try the whole string as a
        // digest first; only then treat a suffix as depth.
        if let Ok(digest) = Digest::parse(raw) {
            return Ok((digest, ReviewDepth::Unstated));
        }
        let (digest, depth) = raw
            .rsplit_once(':')
            .ok_or_else(|| CliError::Invalid(format!("'{raw}' is not a digest")))?;
        let depth = ReviewDepth::parse(depth)
            .ok_or_else(|| CliError::Invalid(format!("'{depth}' is not a review depth")))?;
        Ok((Digest::parse(digest)?, depth))
    }

    fn registered_agents(&self) -> Result<Vec<String>, CliError> {
        let root = self.registry.as_ref().ok_or_else(|| {
            CliError::Invalid(
                "--all-agents needs --registry <dir> to expand to literal Agent names".to_owned(),
            )
        })?;
        let agents = crate::registry::Registry::open(root)?.agent_ids();
        if agents.is_empty() {
            return Err(CliError::Invalid(format!(
                "--all-agents found no Agent in {}",
                root.display()
            )));
        }
        Ok(agents)
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
            let primary = Options::key_material("--primary", options.primary.as_deref())?;
            let release = Options::key_material("--release", options.release_key.as_deref())?;
            let trust = TrustStore::bootstrap(
                &store,
                options
                    .trust_domain
                    .as_deref()
                    .unwrap_or(DEFAULT_TRUST_DOMAIN),
                &primary,
                &release,
                options.sk_policy(),
                now_ms(),
            )?;
            report(options, &trust, |trust| {
                format!(
                    "provisional signing trust for {}; recovery is NOT ready; this store cannot be promoted",
                    trust.trust_domain
                )
            })
        }
        "show" => {
            let status = crate::trust::status::read(&store)?;
            report(options, &status, |status| {
                format!(
                    "domain {}: recovery_ready={} next_action={}",
                    status.trust_domain.as_deref().unwrap_or("(not enrolled)"),
                    status.recovery_ready,
                    status.next_action
                )
            })
        }
        "reset" => {
            if store.provenance()?.trusted {
                return Err(TrustError::ProvisionalOnly.into());
            }
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

fn view_command(options: &Options) -> Result<i32, CliError> {
    use crate::instruction_view;

    let store = options.store()?;
    let output = match options.subject("view")? {
        "materialize" => {
            let registry = options.registry.as_ref().ok_or_else(|| {
                CliError::Invalid("view materialize needs --registry <dir>".to_owned())
            })?;
            instruction_view::materialize(
                &store,
                &options.policy()?,
                &crate::registry::Registry::open(registry)?,
            )?
            .iter()
            .map(|(agent, view)| (agent.clone(), view_output(view)))
            .collect::<serde_json::Map<_, _>>()
            .into()
        }
        "empty" => view_output(&instruction_view::empty(&store)?),
        other => return Err(CliError::Invalid(format!("unknown view command '{other}'"))),
    };
    report(options, &output, ToString::to_string)
}

fn view_output(view: &crate::instruction_view::InstructionView) -> serde_json::Value {
    serde_json::json!({
        "digest": view.digest().to_string(),
        "root": view.root(),
        "skills_root": view.skills_root(),
    })
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
        "show" => {
            if let Some(quarantine) = quarantine::load(&store)? {
                report(options, &quarantine, |quarantine| {
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
                })
            } else {
                println!("no quarantine is active");
                Ok(0)
            }
        }
        "clear" => {
            quarantine::clear(&store, now_ms())?;
            Ok(0)
        }
        other => Err(CliError::Invalid(format!(
            "unknown quarantine command '{other}'"
        ))),
    }
}

fn release_component_inputs(source: &Path) -> Vec<ComponentInput> {
    ["louiselm-skills", "louiselm-launch"]
        .into_iter()
        .map(|name| ComponentInput {
            name: name.to_owned(),
            path: source.join("target/release").join(name),
            kind: ComponentKind::Executable,
        })
        .collect()
}

#[expect(
    clippy::too_many_lines,
    reason = "One release subcommand dispatch keeps each signing and publication transaction visible."
)]
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
                    components: release_component_inputs(&source),
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
            release::sign_bundle(&options.store()?, &bundle, &SshKeygenSigner::new(&key))?;
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

fn launcher_command(options: &Options) -> Result<i32, CliError> {
    if options.prefix.is_some() {
        return Err(CliError::Invalid(
            "launcher paths are fixed; --prefix is not supported".to_owned(),
        ));
    }
    if options.positional.len() != 1 {
        return Err(CliError::Invalid(
            "launcher accepts exactly one subcommand".to_owned(),
        ));
    }
    let paths = LauncherPaths::system();
    match options.subject("launcher")? {
        "install" => {
            let request = options.launcher_install_request()?;
            require_verified_running_release(&paths)?;
            let status =
                launcher_install::install(&paths, &SystemCommandRunner, &request, now_ms())?;
            report_launcher_status(options, &status)
        }
        "rotate-key" => {
            let request = options.rotation_request()?;
            require_verified_running_release(&paths)?;
            let outcome =
                launcher_install::rotate(&paths, &SystemCommandRunner, &request, now_ms())?;
            report(options, &outcome, |outcome| {
                format!(
                    "launcher key {} ({})",
                    outcome.key_id,
                    if outcome.created {
                        "rotated"
                    } else {
                        "already rotated"
                    }
                )
            })
        }
        "status" => {
            let status = launcher_install::status(&paths);
            report_launcher_status(options, &status)
        }
        other => Err(CliError::Invalid(format!(
            "unknown launcher command '{other}'"
        ))),
    }
}

fn require_verified_running_release(paths: &LauncherPaths) -> Result<(), CliError> {
    let identity = release::running_identity();
    if !identity.verified {
        return Err(CliError::Invalid(format!(
            "launcher authority requires the current verified release ({}): {}",
            identity
                .failure_code
                .as_deref()
                .unwrap_or("unverified_release"),
            identity.detail
        )));
    }
    let installed = install::load_state(&paths.release_prefix)?.ok_or_else(|| {
        CliError::Invalid("the fixed launcher prefix has no current release".to_owned())
    })?;
    if identity.release_id.as_deref() != Some(installed.release_id.as_str()) {
        return Err(CliError::Invalid(format!(
            "running release {} does not match the fixed launcher's current release {}",
            identity.release_id.as_deref().unwrap_or("unknown"),
            installed.release_id
        )));
    }
    Ok(())
}

fn report_launcher_status(
    options: &Options,
    status: &launcher_install::LauncherStatus,
) -> Result<i32, CliError> {
    if options.robot {
        println!("{}", robot::payload(status)?);
    } else {
        println!("{}", render::launcher_status(status));
    }
    Ok(if status.trusted {
        0
    } else {
        EXIT_NOT_ADMISSIBLE
    })
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

fn required<'a, T>(value: Option<&'a T>, flag: &str) -> Result<&'a T, CliError> {
    value.ok_or_else(|| CliError::Invalid(format!("{flag} is required")))
}

fn parse_u32(flag: &str, value: &str) -> Result<u32, CliError> {
    value.parse().map_err(|_| {
        CliError::Invalid(format!("{flag} must be an unsigned integer, not '{value}'"))
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
        .map(|elapsed| u64::try_from(elapsed.as_millis()).unwrap_or(u64::MAX))
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
  louiselm-skills preflight --help   (prospective artifact snapshot, never launch authority)
  louiselm-skills workspace --help   (freeze source and materialize private Git)

Trust roles:
  louiselm-skills recovery --help     (local-only paper/passkey recovery)
  louiselm-skills trust bootstrap --primary <key> --release <key>
                                  [--trust-domain <d>] [--require-hardware]
  louiselm-skills trust show
  louiselm-skills trust reset --confirm

Skill Generations:
  louiselm-skills generation admit --member <digest>[:<depth>][=<agent>,...] ...
                                   --key <privkey>
                                   [--all-agents --registry <dir>]
  louiselm-skills generation witness <digest> --remote <url> [--branch <b>]
  louiselm-skills generation activate <digest>
  louiselm-skills generation status
  louiselm-skills generation list
  louiselm-skills view materialize --registry <dir>
  louiselm-skills view empty

Trusted release:
  louiselm-skills release build --output <dir> [--source <dir>]
  louiselm-skills release sign --bundle <dir> --key <privkey>
  louiselm-skills release verify --bundle <dir>
  louiselm-skills release install --bundle <dir> [--prefix <dir>]
  louiselm-skills release status [--prefix <dir>]
  louiselm-skills release identity

Privileged launcher authority (install/rotation require current verified release):
  louiselm-skills launcher install --operator <user> --broker-uid <id>
                                    --broker-gid <id> --uid-start <id>
                                    --gid-start <id> --slots <count>
  louiselm-skills launcher rotate-key --rotation-id <id> --expected-key-id <id>
  louiselm-skills launcher status

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

#[cfg(test)]
#[allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    reason = "Test fixtures abort on setup failure and assert failures directly."
)]
mod tests {
    use super::*;

    fn arguments(values: &[&str]) -> Vec<String> {
        values.iter().map(|value| (*value).to_owned()).collect()
    }

    #[test]
    fn launcher_install_options_are_explicit_and_numeric() {
        let options = Options::parse(&arguments(&[
            "install",
            "--operator",
            "louise",
            "--broker-uid",
            "1500",
            "--broker-gid",
            "1500",
            "--uid-start",
            "200000",
            "--gid-start",
            "300000",
            "--slots",
            "4",
        ]))
        .expect("launcher options parse");

        let request = options.launcher_install_request().unwrap();
        assert_eq!(request.operator, "louise");
        assert_eq!(request.broker_uid, 1_500);
        assert_eq!(request.broker_gid, 1_500);
        assert_eq!(request.pool.uid_start, 200_000);
        assert_eq!(request.pool.gid_start, 300_000);
        assert_eq!(request.pool.slots, 4);

        let error = Options::parse(&arguments(&["install", "--slots", "many"]))
            .err()
            .expect("non-numeric pool size is refused");
        assert!(
            error
                .to_string()
                .contains("--slots must be an unsigned integer")
        );
    }

    #[test]
    fn a_development_build_cannot_install_launcher_authority() {
        let error = require_verified_running_release(&LauncherPaths::system())
            .expect_err("the test executable is not a current installed release");
        assert!(error.to_string().contains("current verified release"));
    }

    #[test]
    fn release_build_declares_both_installed_executables() {
        let source = Path::new("/reviewed/source");
        let components = release_component_inputs(source);
        assert_eq!(
            components
                .iter()
                .map(|component| component.name.as_str())
                .collect::<Vec<_>>(),
            vec!["louiselm-skills", "louiselm-launch"]
        );
        assert_eq!(
            components[1].path,
            source.join("target/release/louiselm-launch")
        );
    }
}
