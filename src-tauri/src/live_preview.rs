//! Authoritative cursor-box snapshot. Late raw/polish results cannot replace
//! newer text, finalization, or a different recording.
use serde::Serialize;

#[derive(Clone, Copy, Debug, Default, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub enum Phase {
    #[default]
    Idle,
    Recording,
    Finishing,
    Complete,
}
#[derive(Clone, Debug, Default, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Snapshot {
    pub session_id: u64,
    pub revision: u64,
    pub phase: Phase,
    pub text: String,
    pub polished: bool,
    pub remote: bool,
    /// A polish pass is in flight, so the text on screen is still moving.
    /// Drives the preview's activity glow; always false once Complete.
    pub polishing: bool,
}
impl Snapshot {
    pub fn begin(&mut self, session_id: u64, remote: bool) {
        *self = Self {
            session_id,
            phase: Phase::Recording,
            remote,
            ..Self::default()
        };
    }
    pub fn preview(&mut self, session: u64, revision: u64, text: &str, polished: bool) -> bool {
        if self.phase != Phase::Recording
            || self.session_id != session
            || revision < self.revision
            || (revision == self.revision && self.polished && !polished)
        {
            return false;
        }
        self.revision = revision;
        self.text = text.into();
        self.polished = polished;
        true
    }
    pub fn finishing(&mut self) -> u64 {
        self.phase = Phase::Finishing;
        self.session_id
    }
    /// Reports whether a polish pass is running. Only meaningful while text can
    /// still change, and reported as changed only when it actually flips so the
    /// caller can skip a redundant event.
    pub fn set_polishing(&mut self, session: u64, active: bool) -> bool {
        let active = active && matches!(self.phase, Phase::Recording | Phase::Finishing);
        if self.session_id != session || self.polishing == active {
            return false;
        }
        self.polishing = active;
        true
    }
    pub fn full_text(&mut self, session: u64, text: &str, complete: bool, polished: bool) -> bool {
        if self.session_id != session || self.phase != Phase::Finishing {
            return false;
        }
        self.text = text.into();
        self.polished = polished;
        if complete {
            self.phase = Phase::Complete;
            self.polishing = false;
        }
        true
    }
    pub fn hide(&mut self, session: u64) -> bool {
        if self.session_id != session {
            return false;
        }
        self.phase = Phase::Idle;
        self.text.clear();
        self.polishing = false;
        true
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn raw_polish_final_and_restart_never_go_backwards() {
        let mut s = Snapshot::default();
        s.begin(1, false);
        assert!(s.preview(1, 1, "hello", false));
        assert!(s.preview(1, 1, "Hello.", true));
        assert!(!s.preview(1, 1, "hello", false));
        assert!(s.preview(1, 2, "hello world", false));
        assert!(!s.preview(1, 1, "Hello.", true));
        s.finishing();
        assert!(s.set_polishing(1, true));
        assert!(!s.set_polishing(1, true), "no event when unchanged");
        assert!(!s.set_polishing(2, false), "another session cannot clear it");
        assert!(!s.preview(1, 3, "stale", true));
        assert!(s.full_text(1, "Hello world.", true, true));
        assert!(!s.polishing, "completion settles the glow");
        assert!(!s.set_polishing(1, true), "a complete preview never glows");
        s.begin(3, false);
        assert!(!s.hide(1));
        assert!(!s.full_text(1, "late", true, true));
        assert!(!s.preview(1, 4, "old", false));
        assert_eq!(s.phase, Phase::Recording);
    }
}
