//! The five sections every first version and full review holds, listed once for
//! `submit_review`, `save_section`, what an open full-review pass owes and the reason an
//! unfinished review gives. A note is the stored text of one section.

use super::NoteSection;

/// The notes a first version or a full review needs before it publishes, in report order.
/// Once saved, none of them can be removed, so every later version of the run holds them too.
pub(super) const REQUIRED_NOTES: [NoteSection; 5] = [
    NoteSection::Summary,
    NoteSection::Rationale,
    NoteSection::Flow,
    NoteSection::Changes,
    NoteSection::Verification,
];

/// Whether `section` is one of the required notes.
pub(super) fn is_required(section: NoteSection) -> bool {
    REQUIRED_NOTES.contains(&section)
}

/// The required notes missing from `saved`, in report order.
pub(super) fn missing(saved: impl IntoIterator<Item = NoteSection>) -> Vec<NoteSection> {
    let saved: Vec<_> = saved.into_iter().collect();
    REQUIRED_NOTES
        .into_iter()
        .filter(|section| !saved.contains(section))
        .collect()
}

/// `the flow section` or `the summary, flow and changes sections`, for a sentence; `None` when
/// `missing` is empty.
pub(super) fn named(missing: &[NoteSection]) -> Option<String> {
    let (last, rest) = missing.split_last()?;
    Some(match rest {
        [] => format!("the {last} section"),
        _ => {
            let rest: Vec<String> = rest.iter().map(ToString::to_string).collect();
            format!("the {} and {last} sections", rest.join(", "))
        }
    })
}
