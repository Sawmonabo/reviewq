use crate::access::AppError;

/// Disposable candidate coverage for one observed and authorized native source.
pub trait SearchIndex: Send + Sync {
    fn reader(&self, key: [u8; 32], trigram: &str) -> Option<Box<dyn CandidateIndex>>;
    fn writer(&self, key: [u8; 32]) -> Result<Box<dyn CandidateWriter>, AppError>;
}

pub trait CandidateIndex {
    fn covered_through(&self) -> u64;

    /// Return an inclusive search position. Uncertified records remain candidates.
    fn next(&self, record: u64, reverse: bool) -> Result<Option<u64>, AppError>;
}

pub trait CandidateWriter {
    fn begin_record(&mut self, start: u64) -> Result<(), AppError>;
    fn folded_chunk(&mut self, folded: &str) -> Result<(), AppError>;
    fn commit_record(&mut self, end: u64, certified: bool) -> Result<(), AppError>;

    /// Publish only complete coverage. Discard the current uncommitted record.
    fn publish(self: Box<Self>) -> Result<(), AppError>;
}
