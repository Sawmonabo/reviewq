use std::io::Read;
use std::os::unix::fs::MetadataExt;
use std::path::{Path, PathBuf};

use clap::{ArgGroup, Args, Subcommand, ValueEnum};
use reviewq_domain::identity::{GitSha, ReportHash};
use reviewq_protocol::GitShaDto;
use reviewq_protocol::reports::{
    AcceptedReference, AcceptedReport, Assessment, CodexCommand, Correction, FindingEdit, Handoff,
    HandoffReceipt, HandoffState, NativeReportAddress, REPORT_HANDOFF_EXAMPLE, ReportHandoffInput,
    ReportHashDto, ReportRevision, ReportSource, ReportVerdict, SectionEdit,
    validate_report_handoff,
};
use serde::Deserialize;
use sha2::{Digest, Sha256};

use super::Failure;
use super::workflow::{self, Context, Profile, ReplyError};

const ACCEPTED: &str = "/api/v1/native/reports/accepted";
const HANDOFF: &str = "/api/v1/native/reports/handoff";
const STATUS: &str = "/api/v1/native/reports/handoff-status";
const MAX_METADATA_BYTES: u64 = 96 * 1024;

const MAX_HANDOFF_BYTES: u64 = 128 * 1024;

#[derive(Subcommand)]
pub(super) enum Report {
    /// Print the shared structured report handoff JSON example.
    Example,
    /// Stage the shared structured report handoff JSON in this lead.
    Handoff {
        #[arg(long, value_name = "JSON")]
        file: PathBuf,
    },
    /// Stage an exact file or the current turn's final response.
    Complete(Complete),
    /// Record a completed review that leaves the accepted report unchanged.
    Unchanged {
        #[arg(long, value_name = "JSON")]
        assessment_file: Option<PathBuf>,
    },
    /// Record why the current lead needs input.
    NeedsInput {
        #[arg(long, value_parser = reason)]
        reason: String,
        #[arg(long, value_name = "JSON")]
        assessment_file: Option<PathBuf>,
    },
    /// Read the accepted receipt in the current lead.
    Accepted,
}

#[derive(Args)]
#[command(group(ArgGroup::new("source").required(true).args(["file", "response"])))]
#[command(group(ArgGroup::new("revision").required(true).args(["reviewed_sha", "unknown_revision"])))]
pub(super) struct Complete {
    #[arg(long, value_name = "FILE")]
    file: Option<PathBuf>,
    #[arg(long)]
    response: bool,
    #[arg(long)]
    reviewed_sha: Option<GitSha>,
    #[arg(long, value_parser = reason)]
    unknown_revision: Option<String>,
    #[arg(long, value_enum)]
    verdict: Verdict,
    #[arg(long, value_name = "JSON")]
    assessment_file: Option<PathBuf>,
    #[arg(long)]
    amend_current: bool,
}

#[derive(Clone, Copy, ValueEnum)]
#[value(rename_all = "snake_case")]
enum Verdict {
    Approve,
    RequestChanges,
    Blocked,
    NotSpecified,
}

impl From<Verdict> for ReportVerdict {
    fn from(value: Verdict) -> Self {
        match value {
            Verdict::Approve => Self::Approve,
            Verdict::RequestChanges => Self::RequestChanges,
            Verdict::Blocked => Self::Blocked,
            Verdict::NotSpecified => Self::NotSpecified,
        }
    }
}

#[derive(Default, Deserialize)]
#[serde(default, rename_all = "camelCase", deny_unknown_fields)]
struct Metadata {
    assessments: Vec<Assessment>,
    corrects_report: Option<Correction>,
    finding_changes: Vec<FindingEdit>,
    section_changes: Vec<SectionEdit>,
}

impl Metadata {
    fn read(path: Option<&Path>) -> Result<Self, Failure> {
        path.map(|path| workflow::json_file(path, MAX_METADATA_BYTES))
            .transpose()
            .map(|value| value.unwrap_or_default())
    }

    fn assessments(self) -> Result<Vec<Assessment>, Failure> {
        if self.corrects_report.is_some()
            || !self.finding_changes.is_empty()
            || !self.section_changes.is_empty()
        {
            return Err("Report corrections require report complete.".into());
        }
        Ok(self.assessments)
    }
}

pub(super) async fn execute(
    profile: Option<PathBuf>,
    scope: Profile,
    command: Report,
) -> Result<(), Failure> {
    let command = match command {
        Report::Example => {
            print!("{REPORT_HANDOFF_EXAMPLE}");
            return Ok(());
        }
        Report::Handoff { file } => {
            let mut handoff = read_handoff(&file)?;
            let context = Context::connect(profile, scope, true).await?;
            bind_handoff_source(&mut handoff)?;
            return submit(&context, handoff).await;
        }
        command => command,
    };
    let context = Context::connect(profile, scope, !matches!(&command, Report::Accepted)).await?;
    let handoff = match command {
        Report::Example | Report::Handoff { .. } => unreachable!(),
        Report::Accepted => return workflow::print(&accepted(&context).await?, false),
        Report::Complete(input) => complete(&context, input).await?,
        Report::Unchanged { assessment_file } => {
            let assessments = Metadata::read(assessment_file.as_deref())?.assessments()?;
            let report = accepted(&context)
                .await?
                .ok_or("This lead has no accepted report to retain.")?;
            Handoff::Unchanged {
                accepted: AcceptedReference {
                    receipt_id: report.receipt_id,
                    sha256: report.sha256,
                },
                source: report
                    .source
                    .ok_or("The accepted report has no verified native source.")?,
                assessments,
            }
        }
        Report::NeedsInput {
            reason,
            assessment_file,
        } => Handoff::NeedsInput {
            reason,
            assessments: Metadata::read(assessment_file.as_deref())?.assessments()?,
        },
    };
    submit(&context, handoff).await
}

async fn submit(context: &Context, handoff: Handoff) -> Result<(), Failure> {
    let body = serde_json::to_vec(&CodexCommand {
        session_key: context.session.clone(),
        call_id: context.call()?,
        handoff,
    })?;
    let receipt: HandoffReceipt = match context.post(HANDOFF, &body).await {
        Ok(value) => value,
        Err(ReplyError::ReplyLost) => context
            .post(STATUS, &body)
            .await
            .map_err(|_| ReplyError::ReplyLost.report("report handoff", &context.session))?,
        Err(error) => return Err(error.report("report handoff", &context.session)),
    };
    workflow::print(&receipt, true)?;
    if matches!(receipt.state, HandoffState::Failed) {
        return Err("The report handoff failed.".into());
    }
    Ok(())
}

async fn accepted(context: &Context) -> Result<Option<AcceptedReport>, Failure> {
    let body = serde_json::to_vec(&NativeReportAddress {
        session_key: context.session.clone(),
    })?;
    context
        .post(ACCEPTED, &body)
        .await
        .map_err(|error| error.report("report lookup", &context.session))
}

fn read_handoff(file: &Path) -> Result<Handoff, Failure> {
    let input = workflow::json_file(file, MAX_HANDOFF_BYTES)?;
    let errors = validate_report_handoff(&input);
    if !errors.is_empty() {
        let messages = errors
            .into_iter()
            .map(|error| {
                let path = if error.path.is_empty() {
                    "/"
                } else {
                    &error.path
                };
                format!("{path} {}", error.message)
            })
            .collect::<Vec<_>>();
        return Err(format!("Invalid report handoff.\n{}", messages.join("\n")).into());
    }
    let ReportHandoffInput { handoff } = serde_json::from_value(input)?;
    Ok(handoff)
}

fn bind_handoff_source(handoff: &mut Handoff) -> Result<(), Failure> {
    if let Handoff::Final { source, .. } | Handoff::Unchanged { source, .. } = handoff
        && let ReportSource::File { path, sha256 } = source
    {
        let bound = report_file(Path::new(path))?;
        if sha256.is_some()
            && !matches!(&bound, ReportSource::File { sha256: actual, .. } if actual == sha256)
        {
            return Err("The declared SHA-256 does not match the report file.".into());
        }
        *source = bound;
    }
    Ok(())
}

async fn complete(context: &Context, input: Complete) -> Result<Handoff, Failure> {
    let metadata = Metadata::read(input.assessment_file.as_deref())?;
    if input.amend_current
        && (metadata.corrects_report.is_some()
            || !metadata.finding_changes.is_empty()
            || !metadata.section_changes.is_empty())
    {
        return Err("Submit structured report corrections without --amend-current.".into());
    }
    let source = match (input.file, input.response) {
        (Some(path), false) => report_file(&path)?,
        (None, true) => ReportSource::Response,
        _ => return Err("Choose one report source.".into()),
    };
    let sha = input
        .reviewed_sha
        .as_ref()
        .map(|sha| GitShaDto::new(sha.as_str()).ok_or("Invalid reviewed SHA."))
        .transpose()?;
    let amends = if input.amend_current {
        let report = accepted(context)
            .await?
            .ok_or("This lead has no accepted report to amend.")?;
        Some(AcceptedReference {
            receipt_id: report.receipt_id,
            sha256: report.sha256,
        })
    } else {
        None
    };
    Ok(Handoff::Final {
        source,
        revision: match (sha, input.unknown_revision) {
            (Some(sha), None) => ReportRevision::Exact { sha },
            (None, Some(reason)) => ReportRevision::Unknown { sha: (), reason },
            _ => return Err("Choose one reviewed revision or unknown revision reason.".into()),
        },
        verdict: input.verdict.into(),
        report: None,
        amends,
        corrects: metadata.corrects_report.map(Box::new),
        finding_changes: metadata.finding_changes,
        section_changes: metadata.section_changes,
        assessments: metadata.assessments,
    })
}

fn report_file(path: &Path) -> Result<ReportSource, Failure> {
    let canonical = path.canonicalize()?;
    let path = canonical
        .to_str()
        .ok_or("The report path must be UTF-8.")?
        .to_owned();
    let mut file = workflow::regular_file(&canonical)?;
    let before = file.metadata()?;
    if before.len() == 0 {
        return Err("The report file is empty.".into());
    }
    let mut remaining = before.len();
    let mut buffer = vec![0; 64 * 1024];
    let mut digest = Sha256::new();
    while remaining > 0 {
        let length = remaining.min(buffer.len() as u64) as usize;
        file.read_exact(&mut buffer[..length])?;
        digest.update(&buffer[..length]);
        remaining -= length as u64;
    }
    let after = file.metadata()?;
    if (before.len(), before.ctime(), before.ctime_nsec())
        != (after.len(), after.ctime(), after.ctime_nsec())
    {
        return Err("The report file changed while it was being read.".into());
    }
    let hash = ReportHash::from_sha256(digest.finalize().into());
    Ok(ReportSource::File {
        path,
        sha256: Some(ReportHashDto::new(hash.as_str()).ok_or("Could not encode the report hash.")?),
    })
}

fn reason(value: &str) -> Result<String, &'static str> {
    if value.trim().is_empty() || value.len() > 1024 {
        return Err("Provide a reason of 1–1,024 bytes.");
    }
    Ok(value.into())
}
