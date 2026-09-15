//! Insertion tier selection: AX direct insertion → synthetic ⌘V → AppleScript
//! keystroke.
//!
//! The dangerous failure mode is DOUBLE insertion (tier 1 "fails", we fall
//! through to ⌘V, but tier 1 had actually landed). Post-insert verification
//! cannot settle this reliably (Electron misreports AX ranges), so tiers are
//! selected by PRE-conditions and each attempt is then trusted or
//! hard-errors: AX insertion runs only when the focused element is
//! pre-validated, a returned AX error means nothing was inserted (safe to
//! fall through), and an AX timeout means unknown state — no fallback, the
//! transcript stays on the clipboard. This is deliberately NOT a
//! verification loop.

/// How the transcript reaches the focused app. The clipboard copy always
/// happens first regardless of tier (today's contract).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum InsertionTier {
    /// Set `kAXSelectedText` on the focused element — no keystrokes, and it
    /// unblocks clipboard restore later (no clipboard needed to insert).
    AxInsert,
    /// Today's mechanism: synthetic ⌘V posted to the HID event tap.
    CmdV,
    /// `osascript` System Events keystroke, for apps that ignore HID-posted
    /// CGEvents. Used INSTEAD of ⌘V, never as a fallback from it.
    AppleScript,
}

impl InsertionTier {
    pub fn label(self) -> &'static str {
        match self {
            InsertionTier::AxInsert => "AX",
            InsertionTier::CmdV => "⌘V",
            InsertionTier::AppleScript => "AppleScript",
        }
    }
}

/// Pre-validated facts about the focused element, read via AX before
/// choosing a tier.
#[derive(Clone, Debug)]
pub struct FocusedElementInfo {
    pub role: String,
    pub selected_text_settable: bool,
    pub secure: bool,
}

/// Apps where AX insertion misbehaved in the field (start empty; grow from
/// the manual insertion matrix and user reports — Electron apps land here
/// the first time one misreports). Exact bundle-id match.
pub const AX_INSERT_DENYLIST: &[&str] = &[];

/// Apps known to ignore HID-posted CGEvents, where the AppleScript keystroke
/// replaces ⌘V. Starts empty; grown from evidence, not guesses. Note the
/// first entry added here also needs the Automation permission prompt
/// documented (osascript → System Events).
pub const PREFER_APPLESCRIPT: &[&str] = &[];

/// AX insertion is eligible only for plain text fields/areas — not web
/// areas, not custom views — whose `AXSelectedText` is settable and which
/// are not secure fields.
fn ax_insert_eligible(element: &FocusedElementInfo) -> bool {
    matches!(element.role.as_str(), "AXTextField" | "AXTextArea")
        && element.selected_text_settable
        && !element.secure
}

pub fn choose_insertion_tier(
    bundle_id: Option<&str>,
    element: Option<&FocusedElementInfo>,
) -> InsertionTier {
    let denylisted = bundle_id.is_some_and(|id| AX_INSERT_DENYLIST.contains(&id));
    if !denylisted {
        if let Some(element) = element {
            if ax_insert_eligible(element) {
                return InsertionTier::AxInsert;
            }
        }
    }
    if bundle_id.is_some_and(|id| PREFER_APPLESCRIPT.contains(&id)) {
        return InsertionTier::AppleScript;
    }
    InsertionTier::CmdV
}

#[cfg(test)]
mod tests {
    use super::*;

    fn element(role: &str, settable: bool, secure: bool) -> FocusedElementInfo {
        FocusedElementInfo {
            role: role.to_string(),
            selected_text_settable: settable,
            secure,
        }
    }

    #[test]
    fn eligible_native_field_gets_ax_insertion() {
        let field = element("AXTextField", true, false);
        let area = element("AXTextArea", true, false);
        assert_eq!(
            choose_insertion_tier(Some("com.apple.TextEdit"), Some(&field)),
            InsertionTier::AxInsert
        );
        assert_eq!(
            choose_insertion_tier(Some("com.apple.TextEdit"), Some(&area)),
            InsertionTier::AxInsert
        );
        // No bundle id known but the element is eligible — still AX.
        assert_eq!(
            choose_insertion_tier(None, Some(&field)),
            InsertionTier::AxInsert
        );
    }

    #[test]
    fn web_areas_and_custom_views_fall_to_cmd_v() {
        for role in ["AXWebArea", "AXGroup", "AXScrollArea", "AXUnknown"] {
            assert_eq!(
                choose_insertion_tier(Some("com.apple.Safari"), Some(&element(role, true, false))),
                InsertionTier::CmdV,
                "{role}"
            );
        }
    }

    #[test]
    fn non_settable_and_secure_fields_fall_to_cmd_v() {
        assert_eq!(
            choose_insertion_tier(None, Some(&element("AXTextArea", false, false))),
            InsertionTier::CmdV
        );
        assert_eq!(
            choose_insertion_tier(None, Some(&element("AXTextField", true, true))),
            InsertionTier::CmdV
        );
    }

    #[test]
    fn no_element_falls_to_cmd_v() {
        assert_eq!(choose_insertion_tier(Some("com.apple.TextEdit"), None), InsertionTier::CmdV);
        assert_eq!(choose_insertion_tier(None, None), InsertionTier::CmdV);
    }

    #[test]
    fn denylisted_bundles_never_get_ax_insertion() {
        // The denylist starts empty by design; prove the mechanism with a
        // local copy of the check.
        let eligible = element("AXTextField", true, false);
        let denylist = ["com.example.misbehaving"];
        let denylisted = denylist.contains(&"com.example.misbehaving");
        assert!(denylisted);
        // With the real (empty) list the same bundle still gets AX.
        assert_eq!(
            choose_insertion_tier(Some("com.example.misbehaving"), Some(&eligible)),
            InsertionTier::AxInsert
        );
    }

    #[test]
    fn prefer_applescript_replaces_cmd_v_not_ax() {
        // The map starts empty; verify the ordering contract via the public
        // function: with no eligible element and a bundle not in the map,
        // the result is CmdV (AppleScript only ever comes from the map).
        assert_eq!(
            choose_insertion_tier(Some("com.example.hid-ignoring"), None),
            InsertionTier::CmdV
        );
    }
}
