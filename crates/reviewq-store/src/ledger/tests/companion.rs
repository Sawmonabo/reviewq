//! The restarted Claude companion of `publish_is_conditional`'s run: unknown after a restart,
//! a permission prompt stored unbound and when a report binds it, a delivery authorized under
//! its offer's fence, and when the process hosting the companion is proven dead, and when a
//! link needs the companion's proof.

use std::time::Duration;

use reviewq_app::access::{Actor, AppError};
use reviewq_app::history::FilePosition;
use reviewq_app::hosts::{
    ClaudeAnchor, ClaudeClassicEvent, ClaudeHeartbeat, ClaudeHost, ClaudeStore, ClaudeTurnResult,
    ClaudeWorkflowStore, HostActivity, HostPhase, LeadStore, MessageBody, MessagePurpose,
    SubmitMessage,
};

use reviewq_domain::session::LeadTarget;

use super::{activity, lead_idle};
use crate::SqliteStore;
use crate::queue::lead_for_run;

pub(super) async fn restarted_companion(store: &SqliteStore) {
    // After a restart a companion that has not reported yet is unknown: neither idle for
    // dispatch nor settled, whatever it last reported, until the watch lets it be gone.
    let companion = "INSERT INTO claude_companions(session_key,generation,sequence,seen_at_ms,
        turn_id,latest_prompt_manual,proof_sha256) VALUES('claude:lead',printf('%032d',0),1,1000,'turn-1',0,zeroblob(32))";
    sql(store, companion).await;
    let lead = store.read(|db| Ok(lead_for_run(db, &"run".parse().unwrap())?.unwrap()));
    let lead = lead.await.unwrap();
    let live = async || {
        let context = store.claude_workflow_context(lead.clone(), None);
        let context = context.await.unwrap();
        (context.active_turn.is_some(), context.idle)
    };
    activity(store, "working").await;
    assert_eq!(lead_idle(store).await, [false]);
    assert_eq!(live().await, (false, false));
    // A permission prompt from Claude's own hook shows the session present, and is kept.
    prompt(store, &lead).await;
    activity(store, "idle").await;
    assert_eq!(lead_idle(store).await, [false]);
    assert_eq!(live().await, (false, false));
    // It ends with its turn, so the next report of a new turn raises no alert.
    finish(store, &lead, "turn-1").await;
    let (zero, one) = ("0".repeat(32), "1".repeat(32));
    heartbeat(store, &lead, report(&zero, 2, HostPhase::Working, "turn-2"))
        .await
        .unwrap();
    assert_eq!(shown(store).await, ("working".into(), None));
    // One that lands after its turn ended is dropped by a report naming that ended turn.
    finish(store, &lead, "turn-2").await;
    sql(store, "UPDATE claude_companions SET seen_at_ms=1000").await;
    prompt(store, &lead).await;
    heartbeat(store, &lead, report(&zero, 3, HostPhase::Idle, "turn-2"))
        .await
        .unwrap();
    assert_eq!(shown(store).await, ("idle".into(), None));
    // A message admitted for the idle lead is offered to its companion and authorized at the
    // end of the conversation, under the offer's fence.
    let message = SubmitMessage {
        target: lead.clone(),
        submission_id: "00000000-0000-4000-8000-000000000001".parse().unwrap(),
        text: MessageBody::new("hello").unwrap(),
        purpose: MessagePurpose::User,
        report_ref: None,
    };
    let admitted = store.admit_message(Actor::local_operator(), message);
    admitted.await.unwrap();
    let offer = store.offer_claude_delivery(lead.clone(), zero.clone());
    let offer = offer.await.unwrap().unwrap();
    let file = FilePosition {
        offset: 7,
        high_water: 7,
        witness: [0; 32],
    };
    let anchor = ClaudeAnchor {
        source_generation: "a".repeat(64),
        file,
    };
    let authorized =
        store.authorize_claude_delivery(lead.clone(), offer.clone(), anchor.clone(), None);
    authorized.await.unwrap();
    let send = store.claude_delivery(lead.clone(), offer).await.unwrap();
    assert_eq!(send.anchor, Some(anchor));
    sql(store, "DELETE FROM claude_deliveries").await;
    sql(store, "DELETE FROM outbox").await;
    sql(store, "DELETE FROM messages").await;
    // A host on an earlier boot is proven dead, so a newer generation takes over at once, and
    // a prompt stored meanwhile binds to the running turn it reports, with one alert.
    sql(store, "UPDATE claude_companions SET seen_at_ms=1000").await;
    prompt(store, &lead).await;
    let restarted = "UPDATE claude_companions SET host_trusted=1, host_boot='an earlier boot'";
    sql(store, restarted).await;
    heartbeat(store, &lead, report(&one, 1, HostPhase::Working, "turn-3"))
        .await
        .unwrap();
    assert_eq!(
        shown(store).await,
        ("blocked".into(), Some("turn-3".into()))
    );
    // Its host is this test's own process, trusted as two reports 5 seconds apart make it.
    sql(
        store,
        "UPDATE claude_companions SET seen_at_ms=1000, host_trusted=1",
    )
    .await;
    // A ReviewQ stopped for a minute has not watched it, and its live host proves nothing:
    // the companion is still unknown. A host process that no longer exists ends it at once,
    // and a report of its generation that outlived the process is refused.
    store.companions.watched_long_enough();
    store.companions.stopped_for(Duration::from_secs(60));
    store.end_companions().await.unwrap();
    assert_eq!(lead_idle(store).await, [false]);
    // A link of the session needs the companion's proof while the companion is unknown, and
    // none once it is gone.
    let link = async |proof| {
        let session = "claude:lead".parse().unwrap();
        store.prove_claude_companion(session, proof, true).await
    };
    assert!(matches!(link(None).await, Err(AppError::Unauthorized)));
    sql(store, "UPDATE claude_companions SET host_pid=4294967295").await;
    store.end_companions().await.unwrap();
    assert_eq!(lead_idle(store).await, [true]);
    link(None).await.unwrap();
    let late = heartbeat(store, &lead, report(&one, 2, HostPhase::Working, "turn-3")).await;
    assert!(matches!(late, Err(AppError::StaleRevision)));
    assert_eq!(lead_idle(store).await, [true]);
}

async fn sql(store: &SqliteStore, sql: &'static str) {
    store
        .write(move |tx| Ok(tx.sql.execute(sql, [])?))
        .await
        .unwrap();
}

async fn prompt(store: &SqliteStore, lead: &LeadTarget) {
    let event = ClaudeClassicEvent::PermissionPending;
    store
        .record_claude_classic(lead.clone(), event)
        .await
        .unwrap();
}

async fn finish(store: &SqliteStore, lead: &LeadTarget, turn: &str) {
    let (turn, generation) = (turn.parse().unwrap(), "0".repeat(32));
    let result = ClaudeTurnResult {
        turn,
        generation,
        normal: true,
    };
    store
        .record_claude_turn(lead.clone(), result)
        .await
        .unwrap();
}

async fn heartbeat(
    store: &SqliteStore,
    lead: &LeadTarget,
    report: ClaudeHeartbeat,
) -> Result<(), AppError> {
    let reported = store.record_claude_heartbeat(lead.clone(), report);
    reported.await.map(|_| ())
}

/// A report of `generation` naming `turn` at step 3, from this test's own process.
fn report(generation: &str, sequence: u64, phase: HostPhase, turn: &str) -> ClaudeHeartbeat {
    ClaudeHeartbeat {
        generation: generation.into(),
        sequence,
        activity: Some(HostActivity {
            phase,
            has_questions: false,
        }),
        turn: Some(turn.parse().unwrap()),
        step: Some(3),
        question_id: None,
        latest_prompt: None,
        latest_prompt_turn: None,
        latest_prompt_manual: false,
        model: None,
        effort: None,
        host: ClaudeHost::of(std::process::id()),
        proof: [0; 32],
    }
}

/// The lead's shown activity and the turns of its permission alerts.
async fn shown(store: &SqliteStore) -> (String, Option<String>) {
    let shown = store.read(|db| {
        let sql = "SELECT host_activity, (SELECT group_concat(event_ref) FROM notifications
            WHERE kind='agent_permission') FROM sessions WHERE session_key='claude:lead'";
        Ok(db.query_row(sql, [], |row| Ok((row.get(0)?, row.get(1)?)))?)
    });
    shown.await.unwrap()
}
