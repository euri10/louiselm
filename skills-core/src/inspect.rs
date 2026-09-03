//! Inspection: deterministic, model-free examination of a whole package.
//!
//! Inspection produces two kinds of output. A *fatal* finding means the package
//! cannot be reviewed at all — there is no skill to describe, or a file claims
//! to be text and is not. Everything else is a *mandatory Dossier finding*: a
//! fact a reviewer must be shown before approving, never a verdict about
//! safety. Inspection has no opinion about intent; it reports what is there.
//!
//! The fatal class is deliberately small. Growing it moves judgement out of the
//! reviewer's hands and into a scanner that cannot read prose, which is the
//! failure mode this design exists to avoid.

use std::collections::BTreeMap;

use serde::Serialize;

use crate::{
    canonical::{CanonicalPath, Digest},
    policy::Policy,
    scan::{self, Magic, TextScan},
    store::{Package, StoreError},
};

/// The Inspection schema this build produces.
pub const INSPECTION_SCHEMA: &str = "louiselm.skills.inspection/1";

/// The path every Skill candidate must define its skill in.
pub const SKILL_FILE: &str = "SKILL.md";

/// Number of located occurrences shown per finding.
pub const MAX_SAMPLES: usize = 5;

/// Why a package cannot be reviewed at all.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum FatalKind {
    /// The package defines no `SKILL.md` at its root.
    SkillFileMissing,
    /// `SKILL.md` has no usable frontmatter, name, or description.
    SkillFileFrontmatter,
    /// A file that is not binary is also not valid UTF-8, so its bytes render
    /// differently depending on who reads them.
    TextNotUtf8,
}

/// A reason the package is not reviewable.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct FatalFinding {
    /// Which fatal condition was met.
    pub kind: FatalKind,
    /// The file responsible, when one file is.
    pub path: Option<String>,
    /// Escaped, reviewer-facing explanation.
    pub message: String,
}

/// The kind of fact a finding reports.
///
/// The discriminants are part of the stable order findings are emitted in, so
/// new kinds are appended rather than inserted.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize)]
#[serde(rename_all = "snake_case")]
#[repr(u8)]
pub enum FindingKind {
    /// Hidden, bidirectional, tag, or private-use code points.
    UnicodeHidden = 0,
    /// Code points that imitate an ASCII character.
    UnicodeConfusable = 1,
    /// A control character that can act on a terminal.
    ControlSequence = 2,
    /// A URL.
    Url = 3,
    /// A reference to a credential, key, or credential store.
    CredentialReference = 4,
    /// An encoded payload or a decoder that would expand one.
    EncodedPayload = 5,
    /// Code or commands that reach the network.
    NetworkImport = 6,
    /// Code or commands that start a process or evaluate code.
    ProcessImport = 7,
    /// SVG content that does something rather than draw.
    SvgBehavior = 8,
    /// A file whose extension declares compiled or archived content.
    DeclaredBinary = 9,
    /// A file whose content is binary while its name does not say so.
    UndeclaredBinary = 10,
    /// A file carrying the executable bit.
    Executable = 11,
    /// An image.
    Image = 12,
    /// Content that contradicts the type its name declares.
    ContentTypeMismatch = 13,
    /// A file too large for the policy's scan budget, examined in part only.
    TruncatedScan = 14,
}

impl FindingKind {
    /// Returns the identifier used in finding ids and robot output.
    pub fn name(self) -> &'static str {
        match self {
            Self::UnicodeHidden => "unicode_hidden",
            Self::UnicodeConfusable => "unicode_confusable",
            Self::ControlSequence => "control_sequence",
            Self::Url => "url",
            Self::CredentialReference => "credential_reference",
            Self::EncodedPayload => "encoded_payload",
            Self::NetworkImport => "network_import",
            Self::ProcessImport => "process_import",
            Self::SvgBehavior => "svg_behavior",
            Self::DeclaredBinary => "declared_binary",
            Self::UndeclaredBinary => "undeclared_binary",
            Self::Executable => "executable",
            Self::Image => "image",
            Self::ContentTypeMismatch => "content_type_mismatch",
            Self::TruncatedScan => "truncated_scan",
        }
    }

    /// Returns the sentence shown above a finding's occurrences.
    pub fn message(self) -> &'static str {
        match self {
            Self::UnicodeHidden => {
                "Hidden or directional code points that do not render as themselves."
            }
            Self::UnicodeConfusable => "Code points that imitate ASCII characters.",
            Self::ControlSequence => "Control characters that can act on a terminal.",
            Self::Url => "Network locations named in the content.",
            Self::CredentialReference => "References to credentials or credential stores.",
            Self::EncodedPayload => "Encoded content, or a decoder that would expand it.",
            Self::NetworkImport => "Code or commands that reach the network.",
            Self::ProcessImport => "Code or commands that start a process or evaluate code.",
            Self::SvgBehavior => "SVG content that acts rather than draws.",
            Self::DeclaredBinary => "Compiled or archived content, declared by its name.",
            Self::UndeclaredBinary => "Binary content under a name that does not declare it.",
            Self::Executable => "Files carrying the executable bit.",
            Self::Image => "Image content rendered from these bytes.",
            Self::ContentTypeMismatch => "Content that contradicts the type its name declares.",
            Self::TruncatedScan => "The scan budget stopped short of the whole file.",
        }
    }
}

impl From<crate::scan::Match> for Occurrence {
    fn from(found: crate::scan::Match) -> Self {
        Self {
            byte_offset: found.byte_offset,
            line: found.line,
            evidence: found.evidence,
            detail: found.detail,
        }
    }
}

/// One located occurrence inside a finding.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct Occurrence {
    /// Byte offset from the start of the file.
    pub byte_offset: u64,
    /// One-based line number, or zero for content that has no lines.
    pub line: u64,
    /// Escaped snippet showing the occurrence in context.
    pub evidence: String,
    /// Escaped short fact: a class name, a format, an imitated character.
    pub detail: String,
}

/// One fact about one file that a reviewer must be shown.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct Finding {
    /// Stable identifier, derived from the package digest, kind, and path.
    pub id: String,
    /// What kind of fact this is.
    pub kind: FindingKind,
    /// Package-relative path the fact belongs to.
    pub path: String,
    /// How many occurrences were found, including those not sampled.
    pub occurrences: u64,
    /// Up to [`MAX_SAMPLES`] located occurrences.
    pub samples: Vec<Occurrence>,
    /// Reviewer-facing sentence for the kind.
    pub message: String,
}

/// How a file's bytes were classified.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ContentKind {
    /// Valid UTF-8 with no NUL bytes.
    Text,
    /// SVG markup, which is text that a renderer may execute.
    Svg,
    /// Image content.
    Image,
    /// Anything else.
    Binary,
}

/// What Inspection observed about one packaged file.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct FileFacts {
    /// Package-relative path.
    pub path: String,
    /// Content length in bytes.
    pub size: u64,
    /// Whether the file carries the executable bit.
    pub executable: bool,
    /// How the bytes were classified.
    pub kind: ContentKind,
    /// Format recognized from the leading bytes, when one was.
    pub magic: Option<String>,
}

/// The complete deterministic examination of one package.
#[derive(Clone, Debug, Serialize)]
pub struct Inspection {
    /// Schema identifier.
    pub schema: String,
    /// Package the Inspection describes.
    pub package_digest: String,
    /// Version of the policy applied.
    pub policy_version: String,
    /// Content address of the exact policy bytes applied.
    pub policy_digest: String,
    /// Version of the Unicode profile applied.
    pub unicode_profile_version: String,
    /// Skill name read from frontmatter, when the package has a usable one.
    pub skill_name: Option<String>,
    /// Skill description read from frontmatter, when it has one.
    pub skill_description: Option<String>,
    /// Reasons the package cannot be reviewed; empty when it can.
    pub fatal: Vec<FatalFinding>,
    /// Mandatory Dossier findings, ordered by path then kind.
    pub findings: Vec<Finding>,
    /// Per-file facts, ordered by path.
    pub files: Vec<FileFacts>,
}

impl Inspection {
    /// Inspects every byte of `package` under `policy`.
    pub fn run(package: &Package, policy: &Policy) -> Result<Self, StoreError> {
        let mut inspection = Self {
            schema: INSPECTION_SCHEMA.to_owned(),
            package_digest: package.digest.to_string(),
            policy_version: policy.document().version.clone(),
            policy_digest: policy.digest().to_string(),
            unicode_profile_version: policy.document().unicode.profile_version.clone(),
            skill_name: None,
            skill_description: None,
            fatal: Vec::new(),
            findings: Vec::new(),
            files: Vec::new(),
        };

        if package.manifest.entry(SKILL_FILE).is_none() {
            inspection.fatal.push(FatalFinding {
                kind: FatalKind::SkillFileMissing,
                path: None,
                message: format!("the package defines no {SKILL_FILE} at its root"),
            });
        }

        for entry in &package.manifest.entries {
            let bytes = package.read(&entry.path)?;
            let mut collector = Collector::new(&package.digest, &entry.path);
            let facts = classify(&entry.path, &bytes, entry.executable, policy);

            if entry.executable {
                collector.add(
                    FindingKind::Executable,
                    vec![located_file(&facts, "executable bit set")],
                );
            }
            file_type_findings(&facts, policy, &mut collector);

            match facts.kind {
                ContentKind::Text | ContentKind::Svg => match std::str::from_utf8(&bytes) {
                    Ok(text) => {
                        let is_svg = facts.kind == ContentKind::Svg;
                        scan_text(text, policy, is_svg, &mut collector);
                        if entry.path == SKILL_FILE {
                            match frontmatter(text) {
                                Ok((name, description)) => {
                                    inspection.skill_name = Some(scan::escape(&name));
                                    inspection.skill_description = Some(scan::escape(&description));
                                }
                                Err(reason) => inspection.fatal.push(FatalFinding {
                                    kind: FatalKind::SkillFileFrontmatter,
                                    path: Some(entry.path.clone()),
                                    message: reason,
                                }),
                            }
                        }
                    }
                    Err(error) => inspection.fatal.push(FatalFinding {
                        kind: FatalKind::TextNotUtf8,
                        path: Some(entry.path.clone()),
                        message: format!(
                            "content is neither binary nor valid UTF-8 (at byte {})",
                            error.valid_up_to()
                        ),
                    }),
                },
                ContentKind::Image | ContentKind::Binary => {
                    if entry.path == SKILL_FILE {
                        inspection.fatal.push(FatalFinding {
                            kind: FatalKind::SkillFileFrontmatter,
                            path: Some(entry.path.clone()),
                            message: format!("{SKILL_FILE} is not text"),
                        });
                    }
                }
            }

            inspection.findings.extend(collector.finish());
            inspection.files.push(facts);
        }

        inspection
            .findings
            .sort_by(|left, right| (&left.path, left.kind).cmp(&(&right.path, right.kind)));
        Ok(inspection)
    }

    /// Reports whether the package is reviewable at all.
    pub fn is_fatal(&self) -> bool {
        !self.fatal.is_empty()
    }

    /// Returns the facts recorded for one path.
    pub fn file(&self, path: &str) -> Option<&FileFacts> {
        self.files.iter().find(|facts| facts.path == path)
    }

    /// Returns every executable path, in package order.
    pub fn executables(&self) -> Vec<&str> {
        self.files
            .iter()
            .filter(|facts| facts.executable)
            .map(|facts| facts.path.as_str())
            .collect()
    }

    /// Counts findings of one kind across the package.
    pub fn count(&self, kind: FindingKind) -> u64 {
        self.findings
            .iter()
            .filter(|finding| finding.kind == kind)
            .map(|finding| finding.occurrences)
            .sum()
    }

    /// Summarizes finding counts by kind, for the Dossier header.
    pub fn counts_by_kind(&self) -> BTreeMap<&'static str, u64> {
        let mut counts = BTreeMap::new();
        for finding in &self.findings {
            *counts.entry(finding.kind.name()).or_insert(0) += finding.occurrences;
        }
        counts
    }
}

struct Collector {
    package_digest: Digest,
    path: String,
    findings: Vec<Finding>,
}

impl Collector {
    fn new(package_digest: &Digest, path: &str) -> Self {
        Self {
            package_digest: package_digest.clone(),
            path: path.to_owned(),
            findings: Vec::new(),
        }
    }

    fn add<I>(&mut self, kind: FindingKind, occurrences: I)
    where
        I: IntoIterator,
        I::Item: Into<Occurrence>,
    {
        let occurrences = occurrences
            .into_iter()
            .map(Into::into)
            .collect::<Vec<Occurrence>>();
        if occurrences.is_empty() {
            return;
        }
        let id = Digest::of(
            format!(
                "{}|{}|{}",
                self.package_digest.hex(),
                kind.name(),
                self.path
            )
            .as_bytes(),
        );
        self.findings.push(Finding {
            id: id.short(16).to_owned(),
            kind,
            path: self.path.clone(),
            occurrences: occurrences.len() as u64,
            samples: occurrences.into_iter().take(MAX_SAMPLES).collect(),
            message: kind.message().to_owned(),
        });
    }

    fn finish(self) -> Vec<Finding> {
        self.findings
    }
}

fn classify(path: &str, bytes: &[u8], executable: bool, policy: &Policy) -> FileFacts {
    let extension = CanonicalPath::parse(path, true)
        .ok()
        .and_then(|canonical| canonical.extension());
    let magic = scan::magic_of(bytes);
    let kind = if scan::looks_like_svg(bytes) {
        ContentKind::Svg
    } else if magic.is_some_and(Magic::is_image) {
        ContentKind::Image
    } else if magic.is_some() || scan::contains_nul(bytes) {
        ContentKind::Binary
    } else if extension
        .as_deref()
        .is_some_and(|extension| policy.is_image_extension(extension))
        && !bytes.is_empty()
    {
        ContentKind::Image
    } else {
        ContentKind::Text
    };
    FileFacts {
        path: path.to_owned(),
        size: bytes.len() as u64,
        executable,
        kind,
        magic: magic.map(|magic| magic.name().to_owned()),
    }
}

fn file_type_findings(facts: &FileFacts, policy: &Policy, collector: &mut Collector) {
    let extension = CanonicalPath::parse(&facts.path, true)
        .ok()
        .and_then(|canonical| canonical.extension());
    let declares_binary = extension
        .as_deref()
        .is_some_and(|extension| policy.is_declared_binary_extension(extension));
    let declares_image = extension
        .as_deref()
        .is_some_and(|extension| policy.is_image_extension(extension));

    if declares_binary {
        collector.add(
            FindingKind::DeclaredBinary,
            vec![located_file(
                facts,
                &format!(
                    "declared by extension, content is {}",
                    facts
                        .magic
                        .clone()
                        .unwrap_or_else(|| "unrecognized".to_owned()),
                ),
            )],
        );
    }

    // A name that declares a type is a claim. Only a recognized format can
    // contradict it: unrecognized bytes under an image name are reported as
    // undeclared binary content instead, which says what is actually known.
    if declares_image
        && let Some(magic) = &facts.magic
        && !matches!(magic.as_str(), "png" | "jpeg" | "gif" | "webp" | "bmp")
    {
        collector.add(
            FindingKind::ContentTypeMismatch,
            vec![located_file(
                facts,
                &format!("name declares an image, content is {magic}"),
            )],
        );
    }

    match facts.kind {
        ContentKind::Image => collector.add(
            FindingKind::Image,
            vec![located_file(
                facts,
                &format!(
                    "image content ({})",
                    facts
                        .magic
                        .clone()
                        .unwrap_or_else(|| "declared by name".to_owned()),
                ),
            )],
        ),
        ContentKind::Binary if !declares_binary => collector.add(
            FindingKind::UndeclaredBinary,
            vec![located_file(
                facts,
                &format!(
                    "binary content ({}) under a name that does not declare it",
                    facts
                        .magic
                        .clone()
                        .unwrap_or_else(|| "no known format".to_owned()),
                ),
            )],
        ),
        _ => {}
    }
}

fn located_file(facts: &FileFacts, detail: &str) -> Occurrence {
    Occurrence {
        byte_offset: 0,
        line: 0,
        evidence: scan::escape(&facts.path),
        detail: scan::escape(detail),
    }
}

fn scan_text(text: &str, policy: &Policy, is_svg: bool, collector: &mut Collector) {
    let document = policy.document();
    let scan = TextScan::new(text, document.limits.max_text_scan_bytes);
    let region = scan.region();

    if scan.truncated {
        collector.add(
            FindingKind::TruncatedScan,
            vec![Occurrence {
                byte_offset: scan.scanned_bytes,
                line: scan.line_of(scan.scanned_bytes),
                evidence: String::new(),
                detail: format!(
                    "scanned the first {} of {} bytes",
                    scan.scanned_bytes,
                    text.len()
                ),
            }],
        );
    }

    collector.add(
        FindingKind::UnicodeHidden,
        scan::hidden_characters(region, policy)
            .into_iter()
            .map(|(offset, character, class)| {
                let mut occurrence = scan.located(offset, &class);
                occurrence.detail = class;
                occurrence.evidence =
                    format!("{} (U+{:04X})", occurrence.evidence, character as u32);
                occurrence
            })
            .collect::<Vec<_>>(),
    );
    collector.add(
        FindingKind::UnicodeConfusable,
        scan::confusable_characters(region, policy)
            .into_iter()
            .map(|(offset, character, target)| {
                let mut occurrence = scan.located(offset, &target.to_string());
                occurrence.evidence = format!(
                    "{} (U+{:04X} imitates '{}')",
                    occurrence.evidence, character as u32, target
                );
                occurrence
            })
            .collect::<Vec<_>>(),
    );
    collector.add(
        FindingKind::ControlSequence,
        scan::control_characters(region)
            .into_iter()
            .map(|(offset, character)| {
                let mut occurrence = scan.located(offset, "control character");
                occurrence.evidence =
                    format!("{} (\\u{{{:x}}})", occurrence.evidence, character as u32);
                occurrence
            })
            .collect::<Vec<_>>(),
    );
    collector.add(FindingKind::Url, scan.find_urls(&document.url_schemes));
    collector.add(
        FindingKind::CredentialReference,
        scan.find_any(&document.credential_markers),
    );
    collector.add(
        FindingKind::NetworkImport,
        scan.find_any(&document.network_imports),
    );
    collector.add(
        FindingKind::ProcessImport,
        scan.find_any(&document.process_imports),
    );

    let mut payloads = scan.find_any(&document.encoded_payload.decoder_markers);
    payloads.extend(scan.find_runs(
        document.encoded_payload.min_base64_run,
        scan::is_base64_byte,
        "base64 run",
    ));
    payloads.extend(scan.find_runs(
        document.encoded_payload.min_hex_run,
        scan::is_hex_byte,
        "hexadecimal run",
    ));
    payloads.sort_by_key(|found| found.byte_offset);
    collector.add(FindingKind::EncodedPayload, payloads);

    if is_svg {
        collector.add(
            FindingKind::SvgBehavior,
            scan.find_any(&document.svg_behavior),
        );
    }
}

fn frontmatter(text: &str) -> Result<(String, String), String> {
    let Some(body) = text.strip_prefix("---\n") else {
        return Err(format!(
            "{SKILL_FILE} does not open with a `---` frontmatter block"
        ));
    };
    let Some(end) = body.find("\n---") else {
        return Err(format!("{SKILL_FILE} frontmatter is never closed"));
    };
    let mut fields = BTreeMap::new();
    for line in body[..end].lines() {
        if line.trim().is_empty() {
            continue;
        }
        let Some((key, value)) = line.split_once(':') else {
            return Err(format!(
                "{SKILL_FILE} frontmatter line is not `key: value`: {}",
                scan::escape(line.trim())
            ));
        };
        fields.insert(key.trim().to_ascii_lowercase(), value.trim().to_owned());
    }
    let name = fields
        .get("name")
        .filter(|value| !value.is_empty())
        .ok_or_else(|| format!("{SKILL_FILE} frontmatter declares no name"))?;
    let description = fields
        .get("description")
        .filter(|value| !value.is_empty())
        .ok_or_else(|| format!("{SKILL_FILE} frontmatter declares no description"))?;
    Ok((name.clone(), description.clone()))
}
