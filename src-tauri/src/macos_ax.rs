//! macOS Accessibility reads for context awareness: the focused element's
//! caret context (Phase A/C) and the on-screen vocabulary harvest (Phase B).
//! No OCR, no screenshots — the AX tree only, under the same Accessibility
//! permission the paste path already holds.
//!
//! Privacy invariants:
//! - reads are local and session-only; nothing is persisted;
//! - secure fields (`AXSecureTextField`, as role or subrole) are never read
//!   or descended into;
//! - password managers are skipped entirely by bundle id;
//! - every entry point is wrapped in a caller-side thread timeout because AX
//!   calls are cross-process IPC and can hang.

use crate::ax_context::{
    collect_context_text, extract_candidate_terms, is_secure_role, CaretContext, ContextNode,
    WalkBudget,
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
    fn AXValueCreate(value_type: u32, value: *const c_void) -> RawCFTypeRef;
    fn AXUIElementSetMessagingTimeout(element: AXUIElementRef, timeout: f32) -> i32;
    fn AXUIElementCopyParameterizedAttributeValue(
        element: AXUIElementRef,
        attribute: CFStringRef,
        parameter: RawCFTypeRef,
        value: *mut RawCFTypeRef,
    ) -> i32;
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

/// Global screen points, top-left origin (the AX/Quartz coordinate system).
#[derive(Clone, Copy, Debug)]
pub struct CaretBounds {
    pub x: f64,
    pub y: f64,
    pub width: f64,
    pub height: f64,
}

/// Geometry only: never reads the text value. Run off the main thread because
/// Accessibility is cross-process IPC. Unsupported controls return no anchor.
pub fn focused_caret_bounds() -> Option<CaretBounds> {
    let (pid, bundle) = frontmost_pid_and_bundle()?;
    if is_password_manager(&bundle) {
        return None;
    }
    // Setting the system-wide element's timeout changes the process default;
    // scope this geometry lookup's timeout to the target application instead.
    let application = application_element(pid)?;
    unsafe { AXUIElementSetMessagingTimeout(application.element_ref(), 0.1) };
    let focused = AxElement(application.copy_attribute("AXFocusedUIElement")?);
    unsafe { AXUIElementSetMessagingTimeout(focused.element_ref(), 0.1) };
    if is_secure_field(&focused) {
        return None;
    }
    let range = focused.copy_attribute("AXSelectedTextRange")?;
    // A selected span has no unambiguous insertion caret; use mouse fallback.
    if read_cf_range(&range)?.length != 0 {
        return None;
    }
    let attribute = CFString::new("AXBoundsForRange");
    let mut raw = std::ptr::null();
    let error = unsafe {
        AXUIElementCopyParameterizedAttributeValue(
            focused.element_ref(),
            attribute.as_concrete_TypeRef(),
            range.as_CFTypeRef(),
            &mut raw,
        )
    };
    if error != 0 || raw.is_null() {
        return None;
    }
    let value = unsafe { CFType::wrap_under_create_rule(raw) };
    let mut rect = core_graphics::geometry::CGRect::default();
    let valid =
        unsafe { AXValueGetValue(value.as_CFTypeRef(), 3, &mut rect as *mut _ as *mut c_void) };
    let bounds = CaretBounds {
        x: rect.origin.x,
        y: rect.origin.y,
        width: rect.size.width,
        height: rect.size.height,
    };
    (valid
        && [bounds.x, bounds.y, bounds.width, bounds.height]
            .iter()
            .all(|v| v.is_finite())
        && bounds.width >= 0.0
        && bounds.height > 0.0)
        .then_some(bounds)
}
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

    fn subrole(&self) -> Option<String> {
        self.string_attribute("AXSubrole")
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
        let array =
            unsafe { CFArray::<CFType>::wrap_under_get_rule(value.as_CFTypeRef() as CFArrayRef) };
        array.iter().map(|child| AxElement(child.clone())).collect()
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

pub fn harvest_screen_context() -> Option<(Vec<String>, Vec<String>)> {
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
    if let Some(window) = application_element(pid)
        .and_then(|app| app.copy_attribute("AXFocusedWindow").map(AxElement))
    {
        roots.push(window);
    }
    if roots.is_empty() {
        return Some((Vec::new(), Vec::new()));
    }

    let texts = collect_context_text(roots, &WalkBudget::standard());
    let terms = extract_candidate_terms(&texts);
    Some((texts, terms))
}

/// Longest window title or field label kept for the format decision.
const FOCUS_TEXT_CHARS: usize = 200;
/// How far up from the focused element to look for the page's web area.
const WEB_AREA_ANCESTORS: usize = 40;
/// Elements searched for a web area when focus is outside the page (the
/// address bar, a toolbar).
const WEB_AREA_SEARCH_NODES: usize = 120;

fn capped(text: String) -> Option<String> {
    let text: String = text.trim().chars().take(FOCUS_TEXT_CHARS).collect();
    (!text.is_empty()).then_some(text)
}

impl AxElement {
    fn short_string(&self, name: &str) -> Option<String> {
        self.string_attribute(name).and_then(capped)
    }

    fn parent(&self) -> Option<AxElement> {
        self.copy_attribute("AXParent").map(AxElement)
    }

    /// A URL attribute, which AX may hand over as a CFURL or a string.
    fn url_attribute(&self, name: &str) -> Option<String> {
        use core_foundation::url::CFURL;
        let value = self.copy_attribute(name)?;
        if let Some(url) = value.downcast::<CFURL>() {
            return Some(url.get_string().to_string());
        }
        value.downcast::<CFString>().map(|s| s.to_string())
    }
}

/// The page URL of a browser window, sanitized to scheme, host and path:
/// the web area holding the focused element, else the window's document,
/// else the first web area in a small search of the window.
fn browser_url(focused: Option<&AxElement>, window: Option<&AxElement>) -> Option<String> {
    use crate::format_context::sanitize_url;
    let web_area_url = |element: &AxElement| {
        (element.role().as_deref() == Some("AXWebArea"))
            .then(|| element.url_attribute("AXURL"))
            .flatten()
            .and_then(|url| sanitize_url(&url))
    };
    if let Some(focused) = focused {
        let mut node = Some(AxElement(focused.0.clone()));
        for _ in 0..WEB_AREA_ANCESTORS {
            let Some(current) = node else { break };
            if let Some(url) = web_area_url(&current) {
                return Some(url);
            }
            node = current.parent();
        }
    }
    let window = window?;
    if let Some(url) = window.url_attribute("AXDocument").and_then(|url| sanitize_url(&url)) {
        return Some(url);
    }
    let mut queue = std::collections::VecDeque::from([AxElement(window.0.clone())]);
    let mut visited = 0;
    while let Some(node) = queue.pop_front() {
        visited += 1;
        if visited > WEB_AREA_SEARCH_NODES {
            break;
        }
        if let Some(url) = web_area_url(&node) {
            return Some(url);
        }
        if is_secure_field(&node) {
            continue;
        }
        queue.extend(node.children());
    }
    None
}

/// The format decision's read at recording start: the app's name, the
/// focused window's title, the focused field's role and labels (never its
/// value) and, in a browser, the page URL cut to scheme, host and path.
/// Session-only; never persisted. `None` in a password manager.
pub fn focus_context() -> Option<crate::format_context::FocusContext> {
    use crate::format_context::{is_browser, FieldInfo, FocusContext};
    let workspace = objc2_app_kit::NSWorkspace::sharedWorkspace();
    let app = workspace.frontmostApplication()?;
    let bundle_id = app.bundleIdentifier()?.to_string();
    if is_password_manager(&bundle_id) {
        return None;
    }
    let app_name = app
        .localizedName()
        .map(|name| name.to_string())
        .unwrap_or_default();
    let application = application_element(app.processIdentifier())?;
    unsafe { AXUIElementSetMessagingTimeout(application.element_ref(), 0.1) };
    let window = application
        .copy_attribute("AXFocusedWindow")
        .map(AxElement);
    let window_title = window.as_ref().and_then(|w| w.short_string("AXTitle"));
    let focused = application
        .copy_attribute("AXFocusedUIElement")
        .map(AxElement);
    let field = match &focused {
        Some(focused) => {
            let role = focused.role();
            let subrole = focused.short_string("AXSubrole");
            if is_secure_role(role.as_deref(), subrole.as_deref()) {
                FieldInfo {
                    role,
                    subrole,
                    ..FieldInfo::default()
                }
            } else {
                FieldInfo {
                    role,
                    subrole,
                    role_description: focused.short_string("AXRoleDescription"),
                    placeholder: focused.short_string("AXPlaceholderValue"),
                    description: focused.short_string("AXDescription"),
                    help: focused.short_string("AXHelp"),
                }
            }
        }
        None => FieldInfo::default(),
    };
    let url = if is_browser(&bundle_id) {
        browser_url(focused.as_ref(), window.as_ref())
    } else {
        None
    };
    Some(FocusContext {
        bundle_id,
        app_name,
        window_title,
        field,
        url,
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
    element_info(&focused_element()?)
}

fn element_info(focused: &AxElement) -> Option<crate::insertion::FocusedElementInfo> {
    let role = focused.role()?;
    let secure = is_secure_role(Some(&role), focused.subrole().as_deref());
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
    if is_secure_field(&focused) {
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
    if is_secure_field(&focused) {
        return None;
    }
    caret_context(&focused)
}

/// The caret read for an element already cleared of the exclusions.
fn caret_context(focused: &AxElement) -> Option<CaretContext> {
    let range = read_cf_range(&focused.copy_attribute("AXSelectedTextRange")?)?;
    let (mut before_units, after_units) = read_around_selection(focused, &range)?;
    // A window that starts inside a surrogate pair: drop the orphaned half.
    if before_units
        .first()
        .is_some_and(|unit| (0xDC00..=0xDFFF).contains(unit))
    {
        before_units.remove(0);
    }

    let before_full = String::from_utf16_lossy(&before_units);
    let before: String = {
        let chars: Vec<char> = before_full.chars().collect();
        let start = chars.len().saturating_sub(CARET_BEFORE_CHARS);
        chars[start..].iter().collect()
    };
    let after_char = String::from_utf16_lossy(&after_units).chars().next();

    Some(CaretContext { before, after_char })
}

/// UTF-16 code units fetched before the caret: `CARET_BEFORE_CHARS` chars
/// are at most two code units each.
const CARET_BEFORE_UTF16: usize = CARET_BEFORE_CHARS * 2;

/// The UTF-16 text just before the selection (up to `CARET_BEFORE_UTF16`
/// units) and the two units just after it. Prefers `AXStringForRange` so only
/// the window around the caret crosses the process boundary; falls back to a
/// capped `AXValue` read for elements without the parameterized attribute.
/// AXSelectedTextRange is measured in UTF-16 code units (CFString/NSString
/// indices), so the split happens there before decoding.
fn read_around_selection(element: &AxElement, selection: &CFRange) -> Option<(Vec<u16>, Vec<u16>)> {
    let caret = selection.location as usize;
    // An insertion replaces the selection, so "after" starts at selection end.
    let selection_end = caret.saturating_add(selection.length as usize);
    if let Some(total) = number_of_characters(element) {
        let caret = caret.min(total);
        let selection_end = selection_end.min(total);
        let start = caret.saturating_sub(CARET_BEFORE_UTF16);
        let before = string_for_range(element, start, caret - start);
        let after = string_for_range(element, selection_end, (total - selection_end).min(2));
        if let (Some(before), Some(after)) = (before, after) {
            return Some((before.encode_utf16().collect(), after.encode_utf16().collect()));
        }
    }
    let utf16: Vec<u16> = read_capped_value(element)?.encode_utf16().collect();
    let caret = caret.min(utf16.len());
    let selection_end = selection_end.min(utf16.len());
    let after_end = (selection_end + 2).min(utf16.len());
    Some((
        utf16[caret.saturating_sub(CARET_BEFORE_UTF16)..caret].to_vec(),
        utf16[selection_end..after_end].to_vec(),
    ))
}

/// `AXStringForRange` over `[location, location + length)` in UTF-16 units.
fn string_for_range(element: &AxElement, location: usize, length: usize) -> Option<String> {
    if length == 0 {
        return Some(String::new());
    }
    let range = CFRange {
        location: location as isize,
        length: length as isize,
    };
    let raw_range = unsafe {
        AXValueCreate(
            K_AX_VALUE_TYPE_CFRANGE,
            &range as *const CFRange as *const c_void,
        )
    };
    if raw_range.is_null() {
        return None;
    }
    let range_value = unsafe { CFType::wrap_under_create_rule(raw_range) };
    let attribute = CFString::new("AXStringForRange");
    let mut raw = std::ptr::null();
    let error = unsafe {
        AXUIElementCopyParameterizedAttributeValue(
            element.element_ref(),
            attribute.as_concrete_TypeRef(),
            range_value.as_CFTypeRef(),
            &mut raw,
        )
    };
    if error != 0 || raw.is_null() {
        return None;
    }
    let value = unsafe { CFType::wrap_under_create_rule(raw) };
    value.downcast::<CFString>().map(|s| s.to_string())
}

/// Fields longer than this (UTF-16 code units) are not watched for edits:
/// re-reading a whole long document is too slow for the AX timeout, and the
/// fields people dictate into are far smaller.
pub const WATCH_VALUE_CAP_UTF16: usize = 100_000;

/// A retained element the edit watcher reads again after focus moves on.
pub struct WatchedElement(AxElement);

// SAFETY: an AXUIElementRef is an immutable CF object. Retain/release are
// thread-safe and AX messaging may happen from any thread, which is what the
// timeout helper's worker threads need.
unsafe impl Send for WatchedElement {}

impl Clone for WatchedElement {
    fn clone(&self) -> Self {
        WatchedElement(AxElement(self.0 .0.clone()))
    }
}

/// The focused text field just before a dictation is inserted into it.
pub struct WatchedField {
    pub element: WatchedElement,
    pub pid: i32,
    pub bundle_id: String,
    pub value: String,
    /// `AXSelectedTextRange`, in UTF-16 code units.
    pub selection_start: usize,
    pub selection_length: usize,
}

/// Reads the focused field's text and selection for the edit watcher. Refuses
/// password managers, secure fields (by role or subrole), fields with no
/// plain-text value, and values over `WATCH_VALUE_CAP_UTF16`.
pub fn snapshot_focused_field() -> Result<WatchedField, &'static str> {
    let (pid, bundle_id) = frontmost_pid_and_bundle().ok_or("no frontmost app")?;
    if is_password_manager(&bundle_id) {
        return Err("password manager");
    }
    let application = application_element(pid).ok_or("no application element")?;
    unsafe { AXUIElementSetMessagingTimeout(application.element_ref(), 0.1) };
    let focused = AxElement(
        application
            .copy_attribute("AXFocusedUIElement")
            .ok_or("no focused element")?,
    );
    unsafe { AXUIElementSetMessagingTimeout(focused.element_ref(), 0.1) };
    if is_secure_field(&focused) {
        return Err("secure field");
    }
    field_snapshot(focused, pid, bundle_id)
}

/// The edit watcher's snapshot of an element already cleared of the
/// exclusions.
fn field_snapshot(
    focused: AxElement,
    pid: i32,
    bundle_id: String,
) -> Result<WatchedField, &'static str> {
    let value = read_capped_value(&focused).ok_or("no readable text, or too long")?;
    let range = focused
        .copy_attribute("AXSelectedTextRange")
        .and_then(|range| read_cf_range(&range))
        .ok_or("no selection range")?;
    Ok(WatchedField {
        element: WatchedElement(focused),
        pid,
        bundle_id,
        value,
        selection_start: range.location as usize,
        selection_length: range.length as usize,
    })
}

/// What the commit path reads from the focused element, in one pass that
/// starts at release and overlaps the final transcription: the caret context,
/// the edit watcher's field snapshot and the insertion facts. Each is `None`
/// when not asked for or unavailable, under the same exclusions as the single
/// reads (password managers and secure fields give no text; the insertion
/// facts are read as `focused_element_info` reads them).
pub struct FocusedRead {
    /// The frontmost app when read, so a caller can tell the user moved on.
    pub pid: i32,
    pub caret: Option<CaretContext>,
    pub field: Option<WatchedField>,
    pub info: Option<crate::insertion::FocusedElementInfo>,
}

pub fn read_focused(want_caret: bool, want_field: bool, want_info: bool) -> Option<FocusedRead> {
    let (pid, bundle_id) = frontmost_pid_and_bundle()?;
    let application = application_element(pid)?;
    unsafe { AXUIElementSetMessagingTimeout(application.element_ref(), 0.1) };
    let focused = AxElement(application.copy_attribute("AXFocusedUIElement")?);
    unsafe { AXUIElementSetMessagingTimeout(focused.element_ref(), 0.1) };
    let info = if want_info { element_info(&focused) } else { None };
    let readable = (want_caret || want_field)
        && !is_password_manager(&bundle_id)
        && !info.as_ref().map_or_else(|| is_secure_field(&focused), |info| info.secure);
    let caret = if readable && want_caret { caret_context(&focused) } else { None };
    let field = if readable && want_field {
        field_snapshot(focused, pid, bundle_id).ok()
    } else {
        None
    };
    Some(FocusedRead {
        pid,
        caret,
        field,
        info,
    })
}

/// The watched field's current text, under the same exclusions.
pub fn read_watched_value(element: &WatchedElement) -> Option<String> {
    if is_secure_field(&element.0) {
        return None;
    }
    read_capped_value(&element.0)
}

/// Whether `element` still has keyboard focus in the frontmost app.
pub fn is_focused(element: &WatchedElement) -> Option<bool> {
    let (pid, _) = frontmost_pid_and_bundle()?;
    let focused = application_element(pid)?.copy_attribute("AXFocusedUIElement")?;
    Some(focused == element.0 .0)
}

pub fn frontmost_pid() -> Option<i32> {
    frontmost_pid_and_bundle().map(|(pid, _)| pid)
}

/// Secure by role or subrole: `NSSecureTextField` and browser password
/// inputs report role `AXTextField` with subrole `AXSecureTextField`.
fn is_secure_field(element: &AxElement) -> bool {
    is_secure_role(element.role().as_deref(), element.subrole().as_deref())
}

/// `AXValue` as a string, checking `AXNumberOfCharacters` first so a huge
/// value is never copied across the process boundary.
fn read_capped_value(element: &AxElement) -> Option<String> {
    use core_foundation::number::CFNumber;
    let length = element
        .copy_attribute("AXNumberOfCharacters")
        .and_then(|value| value.downcast::<CFNumber>())
        .and_then(|number| number.to_i64());
    if length.is_some_and(|length| length < 0 || length as usize > WATCH_VALUE_CAP_UTF16) {
        return None;
    }
    let value = element
        .copy_attribute("AXValue")?
        .downcast::<CFString>()?
        .to_string();
    (value.encode_utf16().count() <= WATCH_VALUE_CAP_UTF16).then_some(value)
}

/// `AXNumberOfCharacters` (UTF-16 code units), when the element reports it.
fn number_of_characters(element: &AxElement) -> Option<usize> {
    use core_foundation::number::CFNumber;
    element
        .copy_attribute("AXNumberOfCharacters")
        .and_then(|value| value.downcast::<CFNumber>())
        .and_then(|number| number.to_i64())
        .and_then(|length| usize::try_from(length).ok())
}
