use notify::{Event, EventKind, RecommendedWatcher, RecursiveMode, Watcher};
use reviewq_app::access::{AppError, AppFuture};
use reviewq_app::history::NativeHistoryWatch;
use tokio::sync::mpsc;

use super::locator::Source;

struct NativeFileWatch {
    _watcher: RecommendedWatcher,
    receiver: mpsc::Receiver<()>,
}

impl NativeHistoryWatch for NativeFileWatch {
    fn changed(&mut self) -> AppFuture<'_, bool> {
        Box::pin(async move { Ok(self.receiver.recv().await.is_some()) })
    }
}

/// Watch exact verified file changes, including replacement in its parent.
pub(super) fn watch(source: Source) -> Result<Box<dyn NativeHistoryWatch>, AppError> {
    let (publish, receiver) = mpsc::channel(1);
    let path = source.path;
    let filter = path.clone();
    let mut watcher = notify::recommended_watcher(move |event: notify::Result<Event>| {
        let changed = match event {
            Ok(event) => {
                !matches!(event.kind, EventKind::Access(_))
                    && event.paths.iter().any(|path| path == &filter)
            }
            Err(_) => true,
        };
        if changed {
            let _ = publish.try_send(());
        }
    })
    .map_err(|_| AppError::Unavailable)?;
    watcher
        .watch(
            path.parent().ok_or(AppError::NotFound)?,
            RecursiveMode::NonRecursive,
        )
        .map_err(|_| AppError::Unavailable)?;
    Ok(Box::new(NativeFileWatch {
        _watcher: watcher,
        receiver,
    }))
}
