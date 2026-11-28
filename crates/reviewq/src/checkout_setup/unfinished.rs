//! The one sentence for checkout setup or cleanup that left something for the user: why each
//! part stays, what the user does about it, then the step that finishes the command.

use std::path::Path;

use super::Failure;

/// What setup or cleanup left, for one checkout or several, and the step that then finishes
/// the command, such as "run the same `reviewq checkout setup` command again".
#[derive(Debug)]
pub(crate) struct Unfinished {
    /// What the user removes, one entry each, where what ReviewQ wrote was changed by hand.
    edited: Vec<String>,
    /// A sentence each for anything else, with its cause and, where the user acts, the step.
    others: Vec<Other>,
    again: String,
}

#[derive(Debug)]
struct Other {
    sentence: String,
    /// The sentence names a step for the user, rather than only a cause to fix.
    step: bool,
}

impl Unfinished {
    pub(super) fn new(again: &str) -> Self {
        Self {
            edited: Vec::new(),
            others: Vec::new(),
            again: again.to_owned(),
        }
    }

    pub(super) fn is_empty(&self) -> bool {
        self.edited.is_empty() && self.others.is_empty()
    }

    /// An entry changed by hand, with what the user removes, such as "remove the
    /// [mcp_servers.reviewq] table from <file>".
    pub(super) fn edited(&mut self, removal: String) {
        self.edited.push(removal);
    }

    /// A sentence of its own that names a step for the user.
    pub(super) fn step(&mut self, sentence: String) {
        self.others.push(Other {
            sentence,
            step: true,
        });
    }

    /// A file setup left as it is: `cause` says why, in a clause about the file such as "it is
    /// not valid JSON", and `step` what the user changes there.
    pub(super) fn unchanged(&mut self, file: &Path, cause: &str, step: &str) {
        self.step(format!(
            "ReviewQ setup left {} unchanged because {cause}. To finish, {step}.",
            file.display()
        ));
    }

    /// A failure at `file`, with its cause, for the user to fix.
    pub(super) fn failed(&mut self, action: &str, file: &Path, error: &Failure) {
        self.others.push(Other {
            sentence: format!(
                "ReviewQ could not {action} {}: {}.",
                file.display(),
                error.to_string().trim_end_matches('.')
            ),
            step: false,
        });
    }

    /// Adds what another part left. A failure that is no `Unfinished` is said with its cause.
    pub(super) fn absorb(&mut self, error: Failure, common: &Path) {
        match error.downcast::<Self>() {
            Ok(other) => {
                self.edited.extend(other.edited);
                self.others.extend(other.others);
            }
            Err(error) => self.failed("finish its setup for the Git folder", common, &error),
        }
    }

    /// Ok when nothing is left, otherwise this as the error.
    pub(super) fn result(self) -> Result<(), Failure> {
        if self.is_empty() {
            Ok(())
        } else {
            Err(Box::new(self))
        }
    }

    /// The sentence after `lead`, within `limit` bytes when one is given. Parts that do not fit
    /// are counted instead, so the closing step stays unless `lead` alone nearly fills it.
    pub(crate) fn render(&self, lead: &str, limit: Option<usize>) -> String {
        let total = self.edited.len() + self.others.len();
        let Some(limit) = limit else {
            return self.compose(lead, total, 0);
        };
        let fitting = (0..=total)
            .rev()
            .map(|shown| self.compose(lead, shown, total - shown))
            .find(|text| text.len() <= limit);
        fitting.unwrap_or_else(|| clip(self.compose(lead, 0, total), limit))
    }

    fn compose(&self, lead: &str, shown: usize, hidden: usize) -> String {
        let edited = &self.edited[..shown.min(self.edited.len())];
        let others = &self.others[..shown.saturating_sub(self.edited.len())];
        let mut sentences = Vec::new();
        if !edited.is_empty() {
            sentences.push(format!(
                "ReviewQ left its management setup in place where it was changed by hand. To finish, {}.",
                edited.join("; ")
            ));
        }
        sentences.extend(others.iter().map(|other| other.sentence.clone()));
        if hidden > 0 {
            sentences.push(if hidden == 1 {
                "1 more entry is left; ReviewQ names it once these are done.".to_owned()
            } else {
                format!("{hidden} more entries are left; ReviewQ names them once these are done.")
            });
        }
        // After only failures the user's step is to fix what each one names.
        let step = !self.edited.is_empty() || self.others.iter().any(|other| other.step);
        sentences.push(if step {
            format!("Then {}.", self.again)
        } else {
            format!("Once that is fixed, {}.", self.again)
        });
        format!("{lead}{}", sentences.join(" "))
            .chars()
            .filter(|ch| !ch.is_control())
            .collect()
    }
}

impl std::fmt::Display for Unfinished {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(&self.render("", None))
    }
}

impl std::error::Error for Unfinished {}

/// `text` cut to at most `limit` bytes at a character boundary, ending in "…" when cut.
pub(crate) fn clip(mut text: String, limit: usize) -> String {
    if text.len() > limit {
        let room = limit.saturating_sub('…'.len_utf8());
        let cut = (0..=room).rev().find(|at| text.is_char_boundary(*at));
        text.truncate(cut.unwrap_or(0));
        text.push('…');
    }
    text
}

#[cfg(test)]
mod tests {
    use super::Unfinished;

    /// The service stores at most 1,024 bytes: parts that do not fit are counted, and the
    /// closing step stays last.
    #[test]
    fn render_counts_what_does_not_fit() {
        let mut left = Unfinished::new("ReviewQ tries again on its own");
        for index in 0..40 {
            left.edited(format!(
                "remove the lines of entry {index} from /checkout/{index}/file"
            ));
        }
        let text = left.render("/checkout: ", Some(1024));
        assert!(text.len() <= 1024, "{} bytes", text.len());
        let shown = text.matches("remove the lines").count();
        assert!(shown > 0 && shown < 40);
        assert!(text.contains(&format!("{} more entries are left", 40 - shown)));
        assert!(text.ends_with("Then ReviewQ tries again on its own."));
        assert_eq!(
            left.render("", None).matches("remove the lines").count(),
            40
        );
    }
}
