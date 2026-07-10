//! macOS Accessibility reads for context awareness: the focused element's
//! caret context (Phase A/C) and the on-screen vocabulary harvest (Phase B).
//! No OCR, no screenshots — the AX tree only, under the same Accessibility
//! permission the paste path already holds.
//!
//! Privacy invariants (backlog/context-awareness-ax.md):
//! - reads are local and session-only; nothing is persisted;
//! - secure fields (`AXSecureTextField`) are never read or descended into;
//! - password managers are skipped entirely by bundle id;
//! - every entry point is wrapped in a caller-side thread timeout because AX
//!   calls are cross-process IPC and can hang.

use crate::ax_context::{
    collect_context_text, extract_candidate_terms, CaretContext, ContextNode, WalkBudget,
    SECURE_FIELD_ROLE,
};
use core_foundation::base::{CFRange, CFType, TCFType};
use core_foundation::string::{CFString, CFStringRef};
use std::ffi::c_void;

type AXUIElementRef = *const c_void;
type RawCFTypeRef = *const c_void;

#[link(name = "ApplicationServices", kind = "framework")]
extern "C" {
    fn AXUIElementCreateSystemWide() -> AXUIElementRef;
    fn AXUIElementCreateApplication(pid: i32) -> AXUIElementRef;
    fn AXUIElementCopyAttributeValue(
        element: AXUIElementRef,
        attribute: CFStringRef,
        value: *mut RawCFTypeRef,
    ) -> i32;
    fn AXValueGetValue(value: RawCFTypeRef, value_type: u32, out: *mut c_void) -> bool;
    fn AXUIElementIsAttributeSettable(
        element: AXUIElementRef,
        attribute: CFStringRef,
        settable: *mut bool,
    ) -> i32;
    fn AXUIElementSetAttributeValue(
        element: AXUIElementRef,
        attribute: CFStringRef,
        value: RawCFTypeRef,
    ) -> i32;
}

const K_AX_VALUE_TYPE_CFRANGE: u32 = 4;
/// Cap for any single text attribute read during the harvest walk — web
/// areas can carry megabytes in one AXValue.
const TEXT_ATTRIBUTE_CAP_BYTES: usize = 2048;
/// Phase A/C read: how much text before the caret is kept.
const CARET_BEFORE_CHARS: usize = 500;

/// No AX reads at all inside password managers.
pub const PASSWORD_MANAGER_PREFIXES: &[&str] = &[
    "com.1password.",
    "com.agilebits.",
    "com.bitwarden.",
    "com.lastpass.",
    "org.keepassxc.",
];

pub fn is_password_manager(bundle_id: &str) -> bool {
    PASSWORD_MANAGER_PREFIXES
        .iter()
        .any(|prefix| bundle_id.starts_with(prefix))
}

/// A retained AX element (CFType keeps the ref alive for the wrapper's life).
pub struct AxElement(CFType);

impl AxElement {
    fn element_ref(&self) -> AXUIElementRef {
        self.0.as_CFTypeRef()
    }

    fn copy_attribute(&self, name: &str) -> Option<CFType> {
        copy_attribute(self.element_ref(), name)
    }

    fn string_attribute(&self, name: &str) -> Option<String> {
        let value = self.copy_attribute(name)?;
        let string = value.downcast::<CFString>()?.to_string();
        Some(truncate_at_char_boundary(string, TEXT_ATTRIBUTE_CAP_BYTES))
    }
}

fn truncate_at_char_boundary(mut text: String, max_bytes: usize) -> String {
    if text.len() <= max_bytes {
        return text;
    }
    let mut cut = max_bytes;
    while cut > 0 && !text.is_char_boundary(cut) {
        cut -= 1;
    }
    text.truncate(cut);
    text
}

fn copy_attribute(element: AXUIElementRef, name: &str) -> Option<CFType> {
    if element.is_null() {
        return None;
    }
    let attribute = CFString::new(name);
    let mut value: RawCFTypeRef = std::ptr::null();
    let err = unsafe {
        AXUIElementCopyAttributeValue(element, attribute.as_concrete_TypeRef(), &mut value)
    };
    if err != 0 || value.is_null() {
        return None;
    }
    Some(unsafe { CFType::wrap_under_create_rule(value) })
}

fn system_wide_element() -> Option<AxElement> {
    let raw = unsafe { AXUIElementCreateSystemWide() };
    if raw.is_null() {
        return None;
    }
    Some(AxElement(unsafe { CFType::wrap_under_create_rule(raw) }))
}

fn application_element(pid: i32) -> Option<AxElement> {
    let raw = unsafe { AXUIElementCreateApplication(pid) };
    if raw.is_null() {
        return None;
    }
    Some(AxElement(unsafe { CFType::wrap_under_create_rule(raw) }))
}

fn focused_element() -> Option<AxElement> {
    system_wide_element()?
        .copy_attribute("AXFocusedUIElement")
        .map(AxElement)
}

impl ContextNode for AxElement {
    fn role(&self) -> Option<String> {
        self.string_attribute("AXRole")
    }

    fn texts(&self) -> Vec<String> {
        ["AXValue", "AXTitle", "AXDescription"]
            .iter()
            .filter_map(|attribute| self.string_attribute(attribute))
            .collect()
    }

    fn children(&self) -> Vec<AxElement> {
        use core_foundation::array::{CFArray, CFArrayRef};
        let Some(value) = self.copy_attribute("AXChildren") else {
            return Vec::new();
        };
        // Generic CFArray has no ConcreteCFType impl, so downcast by hand:
        // check the type id, then re-wrap under the get rule (adds a retain;
        // `value` keeps its own).
        if value.type_of() != CFArray::<CFType>::type_id() {
            return Vec::new();
        }
        let array = unsafe {
            CFArray::<CFType>::wrap_under_get_rule(value.as_CFTypeRef() as CFArrayRef)
        };
        array
            .iter()
            .map(|child| AxElement(child.clone()))
            .collect()
    }
}

/// The frontmost app's pid and bundle id, for the harvest root and the
/// password-manager exclusion.
fn frontmost_pid_and_bundle() -> Option<(i32, String)> {
    let workspace = objc2_app_kit::NSWorkspace::sharedWorkspace();
    let app = workspace.frontmostApplication()?;
    let bundle_id = app.bundleIdentifier()?.to_string();
    Some((app.processIdentifier(), bundle_id))
}

/// Phase B: collect candidate vocabulary terms from the focused element and
/// the frontmost window's AX tree. Returns `None` when reads are skipped
/// entirely (password manager frontmost / no frontmost app).
pub fn harvest_screen_vocabulary() -> Option<Vec<String>> {
    let (pid, bundle_id) = frontmost_pid_and_bundle()?;
    if is_password_manager(&bundle_id) {
        return None;
    }

    // The focused element goes first so its terms win the extraction cap
    // (the walker and extractor are both order-preserving).
    let mut roots = Vec::new();
    if let Some(focused) = focused_element() {
        roots.push(focused);
    }
    if let Some(window) = application_element(pid).and_then(|app| {
        app.copy_attribute("AXFocusedWindow").map(AxElement)
    }) {
        roots.push(window);
    }
    if roots.is_empty() {
        return Some(Vec::new());
    }

    let texts = collect_context_text(roots, &WalkBudget::standard());
    Some(extract_candidate_terms(&texts))
}

/// Raw focused-element facts for the Phase-0 spike command.
pub struct FocusedElementDebug {
    pub bundle_id: String,
    pub role: Option<String>,
    pub value_chars: Option<usize>,
    pub selected_range: Option<(isize, isize)>,
}

pub fn focused_element_debug() -> Option<FocusedElementDebug> {
    let (_, bundle_id) = frontmost_pid_and_bundle()?;
    if is_password_manager(&bundle_id) {
        return Some(FocusedElementDebug {
            bundle_id,
            role: Some("(skipped: password manager)".to_string()),
            value_chars: None,
            selected_range: None,
        });
    }
    let focused = focused_element()?;
    let role = focused.role();
    let value_chars = focused
        .copy_attribute("AXValue")
        .and_then(|v| v.downcast::<CFString>())
        .map(|s| s.to_string().chars().count());
    let selected_range = focused
        .copy_attribute("AXSelectedTextRange")
        .and_then(|value| read_cf_range(&value))
        .map(|range| (range.location, range.length));
    Some(FocusedElementDebug {
        bundle_id,
        role,
        value_chars,
        selected_range,
    })
}

fn read_cf_range(value: &CFType) -> Option<CFRange> {
    let mut range = CFRange {
        location: 0,
        length: 0,
    };
    let ok = unsafe {
        AXValueGetValue(
            value.as_CFTypeRef(),
            K_AX_VALUE_TYPE_CFRANGE,
            &mut range as *mut CFRange as *mut c_void,
        )
    };
    (ok && range.location >= 0 && range.length >= 0).then_some(range)
}

/// Pre-validation facts for the three-tier insertion's tier choice: role,
/// whether `AXSelectedText` is settable, and whether the field is secure.
pub fn focused_element_info() -> Option<crate::insertion::FocusedElementInfo> {
    let focused = focused_element()?;
    let role = focused.role()?;
    let secure = role == SECURE_FIELD_ROLE;
    let mut settable = false;
    let attribute = CFString::new("AXSelectedText");
    let err = unsafe {
        AXUIElementIsAttributeSettable(
            focused.element_ref(),
            attribute.as_concrete_TypeRef(),
            &mut settable,
        )
    };
    Some(crate::insertion::FocusedElementInfo {
        role,
        selected_text_settable: err == 0 && settable,
        secure,
    })
}

/// Tier 1 insertion: set `AXSelectedText` on the focused element, inserting
/// at the caret (replacing any selection — same semantics as paste) without
/// touching keystrokes or the clipboard. A returned error code means nothing
/// was inserted, so the caller may safely fall through to ⌘V.
pub fn ax_insert_text(text: &str) -> Result<(), i32> {
    let focused = focused_element().ok_or(-1)?;
    if focused.role().as_deref() == Some(SECURE_FIELD_ROLE) {
        return Err(-2);
    }
    let attribute = CFString::new("AXSelectedText");
    let value = CFString::new(text);
    let err = unsafe {
        AXUIElementSetAttributeValue(
            focused.element_ref(),
            attribute.as_concrete_TypeRef(),
            value.as_CFTypeRef(),
        )
    };
    if err == 0 {
        Ok(())
    } else {
        Err(err)
    }
}

/// Phase A/C: read the focused element's text around the caret. Returns
/// `None` on anything ambiguous — no focused text element, a secure field, a
/// password manager, or an unreadable selection range — so the caller falls
/// back to today's exact paste behavior.
pub fn focused_caret_context() -> Option<CaretContext> {
    let (_, bundle_id) = frontmost_pid_and_bundle()?;
    if is_password_manager(&bundle_id) {
        return None;
    }

    let focused = focused_element()?;
    if focused.role().as_deref() == Some(SECURE_FIELD_ROLE) {
        return None;
    }

    let value = focused
        .copy_attribute("AXValue")?
        .downcast::<CFString>()?
        .to_string();
    let range = read_cf_range(&focused.copy_attribute("AXSelectedTextRange")?)?;

    // AXSelectedTextRange is measured in UTF-16 code units (CFString/NSString
    // indices); split there, then decode.
    let utf16: Vec<u16> = value.encode_utf16().collect();
    let caret = (range.location as usize).min(utf16.len());
    // An insertion replaces the selection, so "after" starts at selection end.
    let selection_end = caret.saturating_add(range.length as usize).min(utf16.len());

    let before_full = String::from_utf16_lossy(&utf16[..caret]);
    let before: String = {
        let chars: Vec<char> = before_full.chars().collect();
        let start = chars.len().saturating_sub(CARET_BEFORE_CHARS);
        chars[start..].iter().collect()
    };
    let after_char = String::from_utf16_lossy(
        &utf16[selection_end..(selection_end + 2).min(utf16.len())],
    )
    .chars()
    .next();

    Some(CaretContext { before, after_char })
}
