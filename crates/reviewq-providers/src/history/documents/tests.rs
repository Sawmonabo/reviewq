use std::time::Duration;

use reviewq_app::history::FilePosition;

use super::*;

fn target() -> SessionTarget {
    SessionTarget::new(
        "review".parse().unwrap(),
        "run".parse().unwrap(),
        "session".parse().unwrap(),
    )
}

fn reference(offset: u64) -> FileContentRef {
    FileContentRef {
        generation: "generation".into(),
        entry: "entry".parse().unwrap(),
        part: None,
        position: FilePosition {
            offset,
            high_water: offset,
            witness: [0; 32],
        },
    }
}

/// A user's search reaching the document at `offset`, as it claims and locks it on a
/// blocking thread.
async fn search(documents: &Arc<Documents>, offset: u64) -> Result<(), AppError> {
    let documents = documents.clone();
    tokio::task::spawn_blocking(move || {
        let stop = AtomicBool::new(false);
        let claim = documents.claim_blocking(target(), reference(offset), &stop)?;
        reviewq_app::wait::blocking(claim.handle.clone().lock_owned(), &stop).map(drop)
    })
    .await
    .unwrap()
}

// Three window reads of one document used to take every place, and a search reaching a
// document then, or reaching the locked one, was refused at once with `Busy` (a 429).
#[tokio::test]
async fn a_search_during_window_reads_of_one_document_waits_its_turn() {
    let documents = Arc::new(Documents::default());
    let mut windows = Vec::new();
    for _ in 0..3 {
        windows.push(documents.claim(target(), reference(1)).await.unwrap());
    }
    let reading = windows[0].handle.clone().lock_owned().await;
    tokio::time::timeout(Duration::from_secs(1), search(&documents, 2))
        .await
        .expect("a search waited for a place held by one document's readers")
        .unwrap();
    let same = tokio::spawn({
        let documents = documents.clone();
        async move { search(&documents, 1).await }
    });
    tokio::time::sleep(Duration::from_millis(100)).await;
    assert!(
        !same.is_finished(),
        "the search did not wait for the document's lock"
    );
    drop(reading);
    tokio::time::timeout(Duration::from_secs(1), same)
        .await
        .expect("the search did not get the lock once it was free")
        .unwrap()
        .unwrap();
}
