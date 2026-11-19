//! Exercise the public store ports against the worked synthetic source.

use std::error::Error;
use std::path::{Path, PathBuf};
use std::time::Duration;

use reviewq_app::access::{Actor, AppError};
use reviewq_app::hosts::{
    DeliveryObservation, DeliveryOutcome, DeliveryWork, LeadStore, MessageBody, MessagePurpose,
    ObservedInput, RecoverableDeliveryState, RegisterLead, SubmitMessage,
};
use reviewq_app::queue::{MergeRequestState, QueueStore, ReviewSource};
use reviewq_app::workspace::{ConfiguredProject, GrantBinding, WorkspaceConfig, WorkspaceStore};
use reviewq_domain::identity::{GitLabOrigin, MrIdentity, Revision};
use reviewq_domain::session::{AbsolutePath, HostProvider, LeadTarget};
use reviewq_store::SqliteStore;
use serde::Deserialize;

const WAIT: Duration = Duration::from_secs(5);
const CLIENT_ID: &str = "2e0e3466-b939-4f30-8cca-a35342e6669d";

#[derive(Deserialize)]
struct WorkedReview {
    project: String,
    project_id: i64,
    iid: i64,
    title: String,
    head: String,
}

fn absolute(path: &Path) -> AbsolutePath {
    AbsolutePath::from_canonical(path.into()).expect("absolute scratch path")
}

async fn setup(
    db: &Path,
    inputs: &Path,
) -> Result<(SqliteStore, Actor, Actor, LeadTarget), Box<dyn Error>> {
    let root = db.parent().unwrap();
    let data = std::fs::read(inputs.join("reviews.json"))?;
    let mut records: Vec<WorkedReview> = serde_json::from_slice(&data)?;
    records.truncate(2);
    let origin = GitLabOrigin::from_canonical("https://forge.example.invalid")?;
    let source: Vec<_> = records
        .into_iter()
        .map(|record| {
            Ok(ReviewSource {
                identity: MrIdentity::new(
                    origin.clone(),
                    record.project_id.try_into()?,
                    record.iid.try_into()?,
                ),
                project_path: format!("forge/{}", record.project),
                title: record.title,
                head_sha: record.head.parse()?,
                state: MergeRequestState::Open,
                updated_at_ms: 1_793_491_200_000,
            })
        })
        .collect::<Result<_, reviewq_domain::identity::InvalidIdentity>>()?;
    let projects: Vec<_> = source
        .iter()
        .map(|row| ConfiguredProject {
            origin: origin.clone(),
            project_id: row.identity.project_id(),
        })
        .collect();
    let grants: Vec<_> = projects
        .iter()
        .zip(["alpha", "beta"])
        .map(|(project, label)| GrantBinding {
            id: format!("grant-{label}").parse().unwrap(),
            issuing_project: project.clone(),
            checkout: absolute(&root.join(label)),
            common_dir: absolute(&root.join(label).join(".git")),
            epoch: 1_i64.try_into().unwrap(),
        })
        .collect();
    let [a, b] =
        [0, 1].map(|index| Actor::checkout_grant(grants[index].id.clone(), grants[index].epoch));
    let store = SqliteStore::open(absolute(db)).await?;
    let operator = Actor::local_operator();
    store
        .replace_workspace(
            operator.clone(),
            Revision::ZERO,
            WorkspaceConfig { projects, grants },
        )
        .await?;
    store
        .observe_reviews(operator.clone(), source.clone())
        .await?;
    let lead = store
        .register_lead(
            operator,
            RegisterLead {
                review: source[0].clone(),
                provider: HostProvider::Codex,
                profile: absolute(&root.join("native")),
                native_id: "fixture-active".parse()?,
                session_key: "fixture-active".parse()?,
                cwd: absolute(&root.join("alpha")),
                native_source: None,
            },
        )
        .await?
        .value;
    Ok((store, a, b, lead))
}

fn message(target: LeadTarget) -> SubmitMessage {
    SubmitMessage {
        target,
        submission_id: CLIENT_ID.parse().unwrap(),
        text: MessageBody::new(" \u{1f9ea}\n ").unwrap(),
        purpose: MessagePurpose::User,
        report_ref: None,
    }
}

async fn exercise(db: &Path, backup: &Path, inputs: &Path) -> Result<(), Box<dyn Error>> {
    let (store, a, b, lead) = setup(db, inputs).await?;
    let page = store.read_queue(a.clone(), None, 1).await?;
    let native = Actor::native_session(lead.binding().identity().clone());
    assert_eq!(
        store.read_queue(native.clone(), None, 1).await?.value.total,
        1
    );
    assert_eq!(
        store.read_queue(native, page.value.next, 1).await,
        Err(AppError::CursorExpired)
    );

    let request = message(lead.clone());
    let first = store.admit_message(a.clone(), request.clone()).await?;
    assert_eq!(store.admit_message(a, request.clone()).await?, first);
    let second = store.admit_message(b, request).await?;
    assert_ne!(first.value.delivery_id, second.value.delivery_id);
    assert_eq!(store.backup(absolute(backup)).await?, *store.identity());

    let id = first.value.delivery_id;
    let before = store
        .claim_delivery(id.clone(), RecoverableDeliveryState::Queued)
        .await?
        .unwrap();
    let retry = store
        .claim_delivery(id.clone(), RecoverableDeliveryState::Dispatching)
        .await?
        .unwrap();
    assert_eq!(retry.work, DeliveryWork::Reconcile);
    assert_eq!(
        store.authorize_dispatch(retry.clone()).await,
        Err(AppError::Forbidden)
    );
    let observed = ObservedInput {
        turn_id: "known-turn".parse()?,
        entry_id: "known-item".parse()?,
    };
    let started = store
        .finish_delivery(
            retry,
            DeliveryObservation::Started {
                delivery_id: id,
                input: observed.clone(),
            },
        )
        .await?;
    assert_eq!(started.outcome, DeliveryOutcome::Started(observed));
    assert_eq!(
        store
            .finish_delivery(before, DeliveryObservation::Uncertain)
            .await,
        Err(AppError::Conflict)
    );
    store.close(WAIT).await?;
    println!(
        "scoped pagination, duplicate admission, same client ID across grants, coherent backup and lost-reply evidence"
    );
    Ok(())
}

async fn leave_wal(db: &Path, inputs: &Path) -> Result<(), Box<dyn Error>> {
    let (store, a, _, lead) = setup(db, inputs).await?;
    let intent = store.admit_message(a, message(lead)).await?;
    store
        .claim_delivery(intent.value.delivery_id, RecoverableDeliveryState::Queued)
        .await?;
    std::process::exit(0);
}

async fn check_wal(db: &Path) -> Result<(), Box<dyn Error>> {
    let identity = SqliteStore::probe(absolute(db)).await?;
    let store = SqliteStore::open(absolute(db)).await?;
    let recoverable = store.recoverable_deliveries(None, 1).await?;
    assert_eq!(store.identity(), &identity);
    assert_eq!(recoverable[0].state, RecoverableDeliveryState::Dispatching);
    store.close(WAIT).await?;
    println!("retained WAL restored the committed delivery and its dispatch state");
    Ok(())
}

fn main() -> Result<(), Box<dyn Error>> {
    let args: Vec<_> = std::env::args_os().skip(1).map(PathBuf::from).collect();
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_time()
        .build()?;
    runtime.block_on(async {
        match args.as_slice() {
            [mode, db, backup, inputs] if mode.as_os_str() == "exercise" => exercise(db, backup, inputs).await,
            [mode, db, inputs] if mode.as_os_str() == "leave-wal" => leave_wal(db, inputs).await,
            [mode, db] if mode.as_os_str() == "check-wal" => check_wal(db).await,
            [mode, db] if mode.as_os_str() == "probe" => {
                SqliteStore::probe(absolute(db)).await?;
                Ok(())
            }
            [mode, db] if mode.as_os_str() == "open" => {
                SqliteStore::open(absolute(db)).await?.close(WAIT).await?;
                Ok(())
            }
            _ => Err("expected exercise DB BACKUP INPUTS, leave-wal DB INPUTS, check-wal DB, probe DB or open DB".into()),
        }
    })
}
