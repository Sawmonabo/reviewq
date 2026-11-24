use std::io::Read;
use std::os::unix::fs::MetadataExt;
use std::path::{Path, PathBuf};

use clap::Subcommand;
use reviewq_domain::identity::ReportHash;
use reviewq_protocol::reports::limits::MAX_REASON_BYTES;
use reviewq_protocol::reports::{
    AcceptedReference, AcceptedReport, Assessment, CodexCommand, Handoff, HandoffReceipt,
    HandoffState, NativeReportAddress, REPORT_AMENDMENT_SOURCE_MESSAGE, REPORT_HANDOFF_EXAMPLE,
    ReportHandoffInput, ReportHashDto, ReportSource, validate_report_handoff,
};
use serde::Deserialize;
use sha2::{Digest, Sha256};

use super::Failure;
use super::workflow::{self, Context, Profile, ReplyError};

const ACCEPTED: &str = "/api/v1/native/reports/accepted";
const HANDOFF: &str = "/api/v1/native/reports/handoff";
const STATUS: &str = "/api/v1/native/reports/handoff-status";
const MAX_METADATA_BYTES: u64 = 96 * 1024;
const MAX_HANDOFF_BYTES: u64 = reviewq_app::reports::METADATA_BYTES as u64;

#[derive(Subcommand)]
pub(super) enum Report {
    /// Print the shared structured report handoff JSON example.
    Example,
    /// Stage the shared structured report handoff JSON in this lead.
    Handoff {
        #[arg(long, value_name = "JSON")]
        file: PathBuf,
    },
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

/// The assessment file holds only thread assessments.
#[derive(Default, Deserialize)]
#[serde(default, deny_unknown_fields)]
struct Metadata {
    assessments: Vec<Assessment>,
}

impl Metadata {
    fn assessments(path: Option<&Path>) -> Result<Vec<Assessment>, Failure> {
        Ok(path
            .map(|path| workflow::json_file::<Self>(path, MAX_METADATA_BYTES))
            .transpose()?
            .unwrap_or_default()
            .assessments)
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
        Report::Unchanged { assessment_file } => {
            let assessments = Metadata::assessments(assessment_file.as_deref())?;
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
            assessments: Metadata::assessments(assessment_file.as_deref())?,
        },
    };
    check(&serde_json::json!({ "handoff": serde_json::to_value(&handoff)? }))?;
    submit(&context, handoff).await
}

async fn submit(context: &Context, handoff: Handoff) -> Result<(), Failure> {
    if handoff.changes_amended_source() {
        return Err(REPORT_AMENDMENT_SOURCE_MESSAGE.into());
    }
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
    check(&input)?;
    let ReportHandoffInput { handoff } = serde_json::from_value(input)?;
    Ok(handoff)
}

/// Reports every schema and value error of a tool input as `path message` lines.
fn check(input: &serde_json::Value) -> Result<(), Failure> {
    let errors = validate_report_handoff(input);
    if !errors.is_empty() {
        return Err(workflow::invalid_handoff(errors));
    }
    Ok(())
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

fn reason(value: &str) -> Result<String, String> {
    if value.trim().is_empty() || value.len() > MAX_REASON_BYTES {
        return Err(format!("Provide a reason of 1–{MAX_REASON_BYTES} bytes."));
    }
    Ok(value.into())
}
