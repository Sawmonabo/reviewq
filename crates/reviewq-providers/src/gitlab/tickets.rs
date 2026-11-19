use std::collections::HashSet;

use aho_corasick::AhoCorasick;
use reviewq_app::access::AppError;
use reviewq_app::sources::{MAX_PUBLICATION_REVIEWS, MatchedReview};
use reviewq_app::workspace::ConfiguredProject;
use reviewq_domain::identity::TicketKey;

use super::{GitLab, lookup::key_boundaries};

const PAGE_LIMIT: u32 = 2_000;

impl GitLab {
    pub(super) async fn match_ticket_page_set(
        &self,
        project: ConfiguredProject,
        keys: Vec<TicketKey>,
        open: bool,
    ) -> Result<Vec<MatchedReview>, AppError> {
        if project.origin != self.identity || keys.is_empty() {
            return Err(AppError::InvalidRequest);
        }
        let matcher = AhoCorasick::builder()
            .ascii_case_insensitive(true)
            .build(keys.iter().map(TicketKey::as_str))
            .map_err(|_| AppError::PayloadTooLarge)?;
        let id = project.project_id;
        let metadata = self.project(id).await?;
        let mut seen = HashSet::new();
        let mut matches = Vec::new();
        let state = if open { "opened" } else { "all" };
        for page in 1..=PAGE_LIMIT {
            let (rows, more) = self.merge_requests(id, page, state).await?;
            for row in rows {
                if !seen.insert(row.iid) {
                    return Err(AppError::Conflict);
                }
                if row.state != "opened" && open {
                    return Err(AppError::Conflict);
                }
                if row.state == "opened" && !open {
                    continue;
                }
                let mut matching = HashSet::new();
                for field in [&row.title, &row.source_branch] {
                    matching.extend(
                        matcher
                            .find_overlapping_iter(field)
                            .filter(|item| key_boundaries(field, item.start(), item.end()))
                            .map(|item| item.pattern().as_usize()),
                    );
                }
                if !matching.is_empty() {
                    if matches.len() == MAX_PUBLICATION_REVIEWS {
                        return Err(AppError::PayloadTooLarge);
                    }
                    matches.push(MatchedReview {
                        review: self.review(id, &metadata.path_with_namespace, row)?,
                        keys: matching
                            .into_iter()
                            .map(|index| keys[index].clone())
                            .collect(),
                    });
                }
            }
            if !more {
                return Ok(matches);
            }
        }
        Err(AppError::PayloadTooLarge)
    }
}
