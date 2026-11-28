use std::time::Duration;

use reviewq_app::changes::{PatchRevision, SourcePage};
use reviewq_domain::identity::{GitLabOrigin, GitSha};
use tokio::sync::watch;

use super::*;

fn key(page: u32) -> IndexKey {
    IndexKey {
        source: MrIdentity::new(
            GitLabOrigin::from_canonical("https://gitlab.example").unwrap(),
            "7".parse().unwrap(),
            "3".parse().unwrap(),
        ),
        revision: "revision".into(),
        offset: (page, 10),
        preferred: None,
    }
}

fn indexed() -> IndexedPage {
    let sha: GitSha = "0".repeat(40).parse().unwrap();
    IndexedPage {
        page: SourcePage {
            revision: PatchRevision {
                id: "revision".into(),
                base: sha.clone(),
                start: sha.clone(),
                head: sha,
            },
            items: Vec::new(),
            page: 1,
            next_page: None,
        },
        small: Vec::new(),
        selected: None,
    }
}

// More callers than places, all building pages from GitLab, must neither delay a page
// already built nor keep a later caller of a page being built, or started while it waited in
// line, from joining it.
#[tokio::test]
async fn a_built_page_answers_while_more_callers_than_places_build_others() {
    let leases = Leases::new(std::env::temp_dir());
    let (_, built) = leases.index(key(1)).await.unwrap();
    built.get_or_init(|| async { indexed() }).await;
    drop(built);
    let (release, released) = watch::channel(false);
    let mut builders = tokio::task::JoinSet::new();
    for page in [2, 2, 2, 3, 3] {
        let (leases, mut released) = (leases.clone(), released.clone());
        builders.spawn(async move {
            let (_place, slot) = leases.index(key(page)).await.unwrap();
            slot.get_or_init(|| async {
                released.wait_for(|done| *done).await.unwrap();
                indexed()
            })
            .await;
        });
    }
    tokio::task::yield_now().await;
    for _ in 0..20 {
        let (place, slot) = tokio::time::timeout(Duration::from_secs(1), leases.index(key(1)))
            .await
            .expect("a built page waited behind pages being built")
            .unwrap();
        assert!(place.is_none() && slot.initialized());
    }
    release.send_replace(true);
    tokio::time::timeout(Duration::from_secs(1), builders.join_all())
        .await
        .expect("a caller of a page being built waited for a place of its own");
    assert_eq!(leases.scan_uses.free(), MAX_LEASES);

    // Two callers of one new page wait in line behind four pages being built, with a caller
    // of another page between them. When a place frees, the first starts the page; the second
    // joins it, instead of waiting for a place of its own, which the page between them takes
    // when the first page is built.
    let (first, first_done) = watch::channel(false);
    let (rest, rest_done) = watch::channel(false);
    let mut held = tokio::task::JoinSet::new();
    let hold = |set: &mut tokio::task::JoinSet<()>, page, mut done: watch::Receiver<bool>| {
        let leases = leases.clone();
        set.spawn(async move {
            let (_place, slot) = leases.index(key(page)).await.unwrap();
            slot.get_or_init(|| async {
                done.wait_for(|done| *done).await.unwrap();
                indexed()
            })
            .await;
        });
    };
    hold(&mut held, 10, first_done);
    for page in 11..14 {
        hold(&mut held, page, rest_done.clone());
    }
    tokio::task::yield_now().await;
    let (built, built_done) = watch::channel(false);
    let mut callers = tokio::task::JoinSet::new();
    hold(&mut callers, 20, built_done.clone());
    tokio::task::yield_now().await;
    hold(&mut held, 21, rest_done.clone());
    tokio::task::yield_now().await;
    hold(&mut callers, 20, built_done);
    tokio::task::yield_now().await;
    first.send_replace(true);
    tokio::time::sleep(Duration::from_millis(50)).await;
    built.send_replace(true);
    tokio::time::timeout(Duration::from_secs(1), callers.join_all())
        .await
        .expect("a caller in line did not join the page another caller started");
    rest.send_replace(true);
    held.join_all().await;
    assert_eq!(leases.scan_uses.free(), MAX_LEASES);
}
