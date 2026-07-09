//! macOS-only input integration: insert-at-cursor via a synthesized ⌘V and
//! hold-Fn push-to-talk via a listen-only CGEventTap.
//!
//! Both paths sit behind macOS privacy permissions. Posting keyboard events
//! needs Accessibility; installing an event tap on keyboard traffic needs
//! Input Monitoring. Failures must stay observable: callers surface them as
//! backend events instead of silently falling back.

use core_foundation::base::TCFType;
use core_foundation::boolean::CFBoolean;
use core_foundation::dictionary::{CFDictionary, CFDictionaryRef};
use core_foundation::runloop::CFRunLoop;
use core_foundation::string::{CFString, CFStringRef};
use core_graphics::event::{
    CGEvent, CGEventFlags, CGEventTap, CGEventTapLocation, CGEventTapOptions, CGEventTapPlacement,
    CGEventType, CallbackResult, EventField,
};
use core_graphics::event_source::{CGEventSource, CGEventSourceStateID};
use serde::Serialize;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::thread;
use std::time::Duration;
use tauri::{AppHandle, Emitter};

/// Virtual key code for `V` on the ANSI layout, the key ⌘V paste expects.
const KEY_CODE_V: u16 = 9;
/// Virtual key code reported by `flagsChanged` events for the Fn/Globe key.
const KEY_CODE_FN: i64 = 63;

#[link(name = "ApplicationServices", kind = "framework")]
extern "C" {
    fn AXIsProcessTrustedWithOptions(options: CFDictionaryRef) -> bool;
    static kAXTrustedCheckOptionPrompt: CFStringRef;
}

/// Reports whether this process may control the computer (post key events).
/// With `prompt` set, macOS shows the one-time Accessibility approval dialog.
pub fn accessibility_trusted(prompt: bool) -> bool {
    unsafe {
        let key = CFString::wrap_under_get_rule(kAXTrustedCheckOptionPrompt);
        let options = CFDictionary::from_CFType_pairs(&[(
            key.as_CFType(),
            CFBoolean::from(prompt).as_CFType(),
        )]);
        AXIsProcessTrustedWithOptions(options.as_concrete_TypeRef())
    }
}

/// Pastes the current clipboard into the focused app by synthesizing ⌘V.
/// The transcript is already on the clipboard, so this delivers it at the
/// cursor without disturbing the copied-messages flow.
pub fn paste_clipboard_at_cursor() -> Result<(), String> {
    if !accessibility_trusted(true) {
        return Err(
            "Multivoice needs the macOS Accessibility permission to insert text at the cursor"
                .to_string(),
        );
    }

    let source = CGEventSource::new(CGEventSourceStateID::HIDSystemState)
        .map_err(|_| "Could not create a keyboard event source".to_string())?;

    let key_down = CGEvent::new_keyboard_event(source.clone(), KEY_CODE_V, true)
        .map_err(|_| "Could not create the paste key-down event".to_string())?;
    key_down.set_flags(CGEventFlags::CGEventFlagCommand);
    let key_up = CGEvent::new_keyboard_event(source, KEY_CODE_V, false)
        .map_err(|_| "Could not create the paste key-up event".to_string())?;
    key_up.set_flags(CGEventFlags::CGEventFlagCommand);

    key_down.post(CGEventTapLocation::HID);
    // Give the focused app one frame to observe the key-down before release;
    // some apps drop instantaneous synthetic presses.
    thread::sleep(Duration::from_millis(12));
    key_up.post(CGEventTapLocation::HID);
    Ok(())
}

/// The app the user is dictating into, captured at finish time so the AI
/// polish pass can match its register (and skip terminals).
#[derive(Clone, Debug)]
pub struct FrontmostApp {
    pub bundle_id: String,
    pub name: String,
}

/// Identity of the frontmost application via NSWorkspace. Needs no macOS
/// permission. Returns None when there is no frontmost app or it exposes no
/// bundle identifier (e.g. some daemon-owned windows).
pub fn frontmost_app() -> Option<FrontmostApp> {
    let workspace = objc2_app_kit::NSWorkspace::sharedWorkspace();
    let app = workspace.frontmostApplication()?;
    let bundle_id = app.bundleIdentifier()?.to_string();
    let name = app
        .localizedName()
        .map(|name| name.to_string())
        .unwrap_or_default();
    Some(FrontmostApp { bundle_id, name })
}

#[derive(Clone, Serialize)]
#[serde(rename_all = "camelCase")]
struct FnPushToTalkEvent {
    pressed: bool,
}

/// Spawns the listen-only event tap that watches the Fn/Globe modifier and
/// emits `fn-push-to-talk` to the pill window on press and release. The tap
/// stays installed for the app lifetime; `enabled` gates emission so settings
/// changes apply without reinstalling (and re-prompting for) the tap.
pub fn spawn_fn_push_to_talk_tap(app: AppHandle, enabled: Arc<AtomicBool>) {
    thread::Builder::new()
        .name("fn-push-to-talk-tap".to_string())
        .spawn(move || {
            let fn_was_down = AtomicBool::new(false);
            let emit_app = app.clone();
            let result = CGEventTap::with_enabled(
                CGEventTapLocation::HID,
                CGEventTapPlacement::HeadInsertEventTap,
                CGEventTapOptions::ListenOnly,
                vec![CGEventType::FlagsChanged],
                move |_proxy, _event_type, event| {
                    let key_code =
                        event.get_integer_value_field(EventField::KEYBOARD_EVENT_KEYCODE);
                    if key_code == KEY_CODE_FN {
                        let pressed = event
                            .get_flags()
                            .contains(CGEventFlags::CGEventFlagSecondaryFn);
                        let was_down = fn_was_down.swap(pressed, Ordering::SeqCst);
                        if pressed != was_down && enabled.load(Ordering::SeqCst) {
                            let _ = emit_app.emit("fn-push-to-talk", FnPushToTalkEvent { pressed });
                        }
                    }
                    CallbackResult::Keep
                },
                CFRunLoop::run_current,
            );

            if result.is_err() {
                crate::emit_backend_event(
                    &app,
                    "warning",
                    "Hold-Fn push-to-talk is unavailable: macOS denied the keyboard event tap. \
                     Grant Multivoice the Input Monitoring (or Accessibility) permission and relaunch.",
                );
            }
        })
        .ok();
}
