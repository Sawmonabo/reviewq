//! How a pass holds, reopens and settles as the lead's activity changes.

use std::os::unix::fs::PermissionsExt;

use reviewq_app::ledger::{LedgerStore, RecordWrite};
use reviewq_domain::ledger::{PassState, Settlement, settle};
use reviewq_domain::session::AbsolutePath;

use super::{SHA, activity, call, fields, lead, open, run, short};
use crate::SqliteStore;

#[test]
fn pass_hold_and_start_follow_lead_activity() {
    run(async {
        let temp = tempfile::tempdir().unwrap();
        std::fs::set_permissions(temp.path(), std::fs::Permissions::from_mode(0o700)).unwrap();
        let path = AbsolutePath::from_canonical(temp.path().join("passes.sqlite3")).unwrap();
        let store = open(path).await;

        // The lead was seen working before its launch's pass opened, so the pass has started
        // and settles once the lead is idle.
        activity(&store, "working").await;
        store
            .write(|tx| {
                tx.sql.execute(
                    "INSERT INTO launch_attempts(id,actor,submission_id,review_id,request_digest,
                       settings_revision,source_sha,provider,checkout,common_dir,invocation,profile,
                       native_id,run_id,session_key,state,created_at_ms,updated_at_ms)
                     VALUES('launch',zeroblob(32),'launch','review',zeroblob(32),0,?1,'claude',
                       CAST('/work' AS BLOB),CAST('/work' AS BLOB),'{}',CAST('/claude' AS BLOB),'lead',
                       'run','claude:lead','confirmed',0,0)",
                    [SHA],
                )?;
                crate::ledger::passes::open_for_launch(&tx.sql, "launch", 0)
            })
            .await
            .unwrap();
        // A lead whose activity is unknown, as for a Codex thread running in another
        // process, does not settle; the same lead stored idle does.
        activity(&store, None).await;
        assert_eq!(settlements(&store).await, [Settlement::Nothing]);
        activity(&store, "idle").await;
        assert_eq!(settlements(&store).await, [Settlement::Unfinished]);

        // A hold made before the host reported its turn as working survives the host's first
        // sight of that turn, and reopens when the next turn starts without any call.
        store
            .hold_pass(call("ask"), "Which branch is the base?".into())
            .await
            .unwrap()
            .unwrap();
        let state = async || {
            let page = store.read_ledger(lead(), short()).await.unwrap();
            page.pass.map(|pass| (pass.state, pass.question))
        };
        // The question is read back while the pass waits and kept once it reopens, so what
        // the pass owes can name it.
        let question = || Some("Which branch is the base?".to_owned());
        let waiting = Some((PassState::Waiting, question()));
        activity(&store, "working").await;
        assert_eq!(state().await, waiting);
        activity(&store, "idle").await;
        assert_eq!(state().await, waiting);
        // Losing and regaining sight of the idle lead is no new turn.
        activity(&store, None).await;
        activity(&store, "idle").await;
        assert_eq!(state().await, waiting);
        activity(&store, "working").await;
        assert_eq!(state().await, Some((PassState::Open, question())));

        // The lead's own record write proves its turn is running, so an idle state stored
        // before it is stale: the run settles only once the host reports the lead idle again.
        activity(&store, "idle").await;
        store
            .write_record(call("file"), RecordWrite::CreateFinding(Box::new(fields())))
            .await
            .unwrap()
            .unwrap();
        assert_eq!(settlements(&store).await, [Settlement::Nothing]);
        activity(&store, "idle").await;
        assert_eq!(settlements(&store).await, [Settlement::Unfinished]);
        store.close(SqliteStore::CLOSE_DEADLINE).await.unwrap();
    });
}

/// What settling does for each candidate run.
async fn settlements(store: &SqliteStore) -> Vec<Settlement> {
    let candidates = store.runs_to_settle().await.unwrap();
    candidates.iter().map(|run| settle(run.facts)).collect()
}
