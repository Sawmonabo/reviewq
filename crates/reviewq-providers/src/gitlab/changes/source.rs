use reviewq_app::access::AppFuture;

mod index;
mod window;

pub(super) use index::{CHECKPOINT_BYTES, IndexBuilder, SourceIndex};

// Include the preceding checkpoint plus the maximum UTF-8 boundary lookbehind.
pub(super) const SEEK_BEHIND_BYTES: usize = CHECKPOINT_BYTES + 4;

pub(super) trait SourceRead: Send {
    fn read(&mut self, offset: usize, length: usize) -> AppFuture<'_, Vec<u8>>;
}
