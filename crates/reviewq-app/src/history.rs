//! Visible native history for one verified lead.

use reviewq_domain::identity::{NativeInputId, NativeTurnId};

#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct NativePosition {
    pub cursor: Option<String>,
    pub ascending: bool,
    pub skip_turn: Option<NativeTurnId>,
    pub within: Option<TurnWindow>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct TurnWindow {
    pub turn: NativeTurnId,
    pub index: usize,
    pub forward: bool,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum VisibleKind {
    User,
    Assistant,
}

#[derive(Clone, Debug)]
pub struct VisibleEntry {
    pub id: NativeInputId,
    pub turn: NativeTurnId,
    pub at_ms: Option<i64>,
    pub kind: VisibleKind,
    pub text: String,
}

#[derive(Debug)]
pub struct NativeHistoryPage {
    pub generation: String,
    pub entries: Vec<VisibleEntry>,
    pub older: Option<NativePosition>,
    pub newer: Option<NativePosition>,
    pub within: Option<(NativePosition, NativeTurnId, usize)>,
}

impl NativeHistoryPage {
    pub fn older_after_trim(&self, count: usize) -> Option<NativePosition> {
        if count == 0 {
            return self.older.clone();
        }
        let (position, turn, start) = self.within.as_ref()?;
        let mut position = position.clone();
        position.within = Some(TurnWindow {
            turn: turn.clone(),
            index: start + count,
            forward: false,
        });
        Some(position)
    }
}
