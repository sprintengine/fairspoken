//! Passive, session-scoped transcription preview beside the insertion caret.
use std::sync::atomic::{AtomicU64, Ordering};
use tauri::{AppHandle, Manager};

pub const WINDOW_LABEL: &str = "cursor-preview";
static GENERATION: AtomicU64 = AtomicU64::new(0);
#[cfg(target_os = "macos")]
static AX_LOOKUP_ACTIVE: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

#[derive(Clone, Copy, Debug)]
struct Rect {
    x: f64,
    y: f64,
    width: f64,
    height: f64,
}

/// All coordinates are points with a top-left origin. Prefer below the caret,
/// flip above near the bottom edge, then clamp to the monitor's usable area.
fn position(anchor: Rect, area: Rect, width: f64, height: f64) -> (f64, f64) {
    let gap = 8.0;
    let min_x = area.x + gap;
    let min_y = area.y + gap;
    let max_x = (area.x + area.width - width - gap).max(min_x);
    let max_y = (area.y + area.height - height - gap).max(min_y);
    let below = anchor.y + anchor.height + gap;
    let y = if below > max_y {
        anchor.y - height - gap
    } else {
        below
    };
    (
        (anchor.x + anchor.width + gap).clamp(min_x, max_x),
        y.clamp(min_y, max_y),
    )
}

/// Captures one anchor per recording. A generation guard inside the actual UI
/// closure prevents a late Accessibility response from reviving a hidden box.
pub fn show(app: &AppHandle) {
    let generation = GENERATION.fetch_add(1, Ordering::SeqCst) + 1;
    let app = app.clone();
    std::thread::spawn(move || {
        #[cfg(target_os = "macos")]
        let caret = {
            let (tx, rx) = std::sync::mpsc::sync_channel(1);
            if AX_LOOKUP_ACTIVE
                .compare_exchange(false, true, Ordering::SeqCst, Ordering::SeqCst)
                .is_ok()
            {
                std::thread::spawn(move || {
                    struct LookupGuard;
                    impl Drop for LookupGuard {
                        fn drop(&mut self) {
                            AX_LOOKUP_ACTIVE.store(false, Ordering::SeqCst);
                        }
                    }
                    let _guard = LookupGuard;
                    let _ = tx.send(crate::macos_ax::focused_caret_bounds());
                });
            } else {
                // One unresponsive application must not accumulate workers.
                drop(tx);
            }
            // AX servers can fail to honor their IPC timeout. The UI still
            // appears promptly at the mouse, without waiting on that process.
            rx.recv_timeout(std::time::Duration::from_millis(300))
                .ok()
                .flatten()
        };
        let ui_app = app.clone();
        let _ = app.run_on_main_thread(move || {
            if GENERATION.load(Ordering::SeqCst) != generation {
                return;
            }
            let Some(window) = ui_app.get_webview_window(WINDOW_LABEL) else {
                return;
            };
            // Both are required: a transparent webview still intercepts clicks.
            let _ = window.set_focusable(false);
            let _ = window.set_ignore_cursor_events(true);
            #[cfg(target_os = "macos")]
            show_macos(&window, caret);
            #[cfg(not(target_os = "macos"))]
            show_fallback(&window);
        });
    });
}

pub fn hide(app: &AppHandle) {
    let generation = GENERATION.fetch_add(1, Ordering::SeqCst) + 1;
    let ui_app = app.clone();
    let _ = app.run_on_main_thread(move || {
        if GENERATION.load(Ordering::SeqCst) != generation {
            return;
        }
        if let Some(window) = ui_app.get_webview_window(WINDOW_LABEL) {
            let _ = window.hide();
        }
    });
}

#[cfg(target_os = "macos")]
fn show_macos(window: &tauri::WebviewWindow, caret: Option<crate::macos_ax::CaretBounds>) {
    use objc2::MainThreadMarker;
    use objc2_app_kit::{NSEvent, NSScreen, NSWindow};
    use objc2_foundation::NSPoint;
    let Some(mtm) = MainThreadMarker::new() else {
        return;
    };
    let screens = NSScreen::screens(mtm);
    let Some(primary) = screens.firstObject() else {
        return;
    };
    // AX uses the primary display's top-left; AppKit uses its bottom-left.
    // Stay in native points throughout: scaling global screen origins by a
    // monitor's own DPI gives incorrect coordinates with mixed-DPI displays.
    let primary_top = primary.frame().origin.y + primary.frame().size.height;
    let mouse = NSEvent::mouseLocation();
    let fallback = Rect {
        x: mouse.x,
        y: primary_top - mouse.y,
        width: 0.0,
        height: 0.0,
    };
    let mut anchor = caret
        .map(|c| Rect {
            x: c.x,
            y: c.y,
            width: c.width,
            height: c.height,
        })
        .unwrap_or(fallback);
    let screen_for = |a: Rect| {
        screens.iter().find(|screen| {
            let frame = screen.frame();
            let y = primary_top - a.y;
            a.x >= frame.origin.x
                && a.x < frame.origin.x + frame.size.width
                && y >= frame.origin.y
                && y <= frame.origin.y + frame.size.height
        })
    };
    let screen = screen_for(anchor).or_else(|| {
        anchor = fallback;
        screen_for(anchor)
    });
    let Some(screen) = screen else {
        return;
    };
    let visible = screen.visibleFrame();
    let area = Rect {
        x: visible.origin.x,
        y: primary_top - visible.origin.y - visible.size.height,
        width: visible.size.width,
        height: visible.size.height,
    };
    let Ok(raw) = window.ns_window() else {
        return;
    };
    let native = unsafe { &*(raw as *const NSWindow) };
    let size = native.frame().size;
    let (x, y) = position(anchor, area, size.width, size.height);
    native.setFrameOrigin(NSPoint::new(x, primary_top - y - size.height));
    // Unlike makeKeyAndOrderFront / application activation, this only changes
    // visibility and leaves the insertion target's keyboard focus untouched.
    native.orderFrontRegardless();
}

#[cfg(not(target_os = "macos"))]
fn show_fallback(window: &tauri::WebviewWindow) {
    let Ok(cursor) = window.cursor_position() else {
        return;
    };
    let Ok(Some(monitor)) = window.monitor_from_point(cursor.x, cursor.y) else {
        return;
    };
    let area = monitor.work_area();
    let Ok(size) = window.outer_size() else {
        return;
    };
    let (x, y) = position(
        Rect {
            x: cursor.x,
            y: cursor.y,
            width: 0.0,
            height: 0.0,
        },
        Rect {
            x: area.position.x as f64,
            y: area.position.y as f64,
            width: area.size.width as f64,
            height: area.size.height as f64,
        },
        size.width as f64,
        size.height as f64,
    );
    let _ = window.set_position(tauri::PhysicalPosition::new(x as i32, y as i32));
    let _ = window.show();
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn caret_box_flips_and_clamps_on_negative_origin_monitor() {
        let area = Rect {
            x: -1920.0,
            y: -200.0,
            width: 1920.0,
            height: 1080.0,
        };
        let anchor = Rect {
            x: -10.0,
            y: 850.0,
            width: 0.0,
            height: 20.0,
        };
        assert_eq!(position(anchor, area, 360.0, 152.0), (-368.0, 690.0));
    }
    #[test]
    fn caret_box_sits_below_and_to_the_right() {
        let area = Rect {
            x: 0.0,
            y: 24.0,
            width: 1440.0,
            height: 820.0,
        };
        let anchor = Rect {
            x: 100.0,
            y: 100.0,
            width: 1.0,
            height: 18.0,
        };
        assert_eq!(position(anchor, area, 360.0, 152.0), (109.0, 126.0));
    }
    #[test]
    fn tiny_work_area_does_not_panic() {
        let area = Rect {
            x: 0.0,
            y: 0.0,
            width: 100.0,
            height: 100.0,
        };
        assert_eq!(position(area, area, 360.0, 152.0), (8.0, 8.0));
    }
}
