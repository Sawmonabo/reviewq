use crate::access::AppError;

/// Disposable candidate coverage for one observed and authorized native source.
pub trait SearchIndex: Send + Sync {
    fn reader(&self, key: [u8; 32], trigram: &str) -> Option<Box<dyn CandidateIndex>>;
    fn proof(&self, source: [u8; 32]) -> Option<IndexProof>;
    fn writer(
        &self,
        key: [u8; 32],
        reuse: Option<IndexProof>,
    ) -> Result<Box<dyn CandidateWriter>, AppError>;
}

#[derive(Clone, Copy, Eq, PartialEq)]
pub struct IndexProof {
    pub source: [u8; 32],
    pub observation: [u8; 32],
    pub high_water: u64,
    pub covered_through: u64,
    pub prefix_hash: [u8; 32],
    pub capped: bool,
}

pub trait CandidateIndex {
    fn covered_through(&self) -> u64;

    /// Return an inclusive search position. Uncertified records remain candidates.
    fn next(&self, record: u64, reverse: bool) -> Result<Option<u64>, AppError>;
}

pub trait CandidateWriter {
    fn covered_through(&self) -> u64;
    fn begin_record(&mut self, start: u64) -> Result<(), AppError>;
    fn folded_chunk(&mut self, folded: &str) -> Result<(), AppError>;
    fn commit_record(&mut self, end: u64, certified: bool) -> Result<(), AppError>;

    /// Publish only complete coverage. Discard the current uncommitted record.
    fn publish(self: Box<Self>, proof: IndexProof) -> Result<(), AppError>;
}
