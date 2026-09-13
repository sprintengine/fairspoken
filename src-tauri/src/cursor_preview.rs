//! Nonactivating, session-scoped transcription preview beside the insertion caret.
use std::sync::atomic::{AtomicU64, Ordering};
use tauri::{AppHandle, Manager};

pub const WINDOW_LABEL: &str = "cursor-preview";
static GENERATION: AtomicU64 = AtomicU64::new(0);
static HOVER_SESSION: AtomicU64 = AtomicU64::new(0);
static CURRENT_SESSION: AtomicU64 = AtomicU64::new(0);
// 21-point lines plus 24-point padding, borders and 32-point shadow margins.
const MIN_HEIGHT: f64 = 79.0;
const MAX_HEIGHT: f64 = 205.0;
static DESIRED_SIZE: std::sync::Mutex<(u64, f64, bool)> =
    std::sync::Mutex::new((0, MIN_HEIGHT, false));

#[derive(Clone, Copy)]
struct Layout {
    session: u64,
    anchor: Rect,
    area: Rect,
    width: f64,
    scale: f64,
    primary_top: f64,
    above: bool,
}
impl Layout {
    fn frame(self, height: f64) -> Rect {
        let height = height * self.scale;
        let (x, _) = position(self.anchor, self.area, self.width, height);
        let min_y = self.area.y + 8.0;
        let max_y = (self.area.y + self.area.height - height - 8.0).max(min_y);
        let y = if self.above {
            self.anchor.y - height - 8.0
        } else {
            self.anchor.y + self.anchor.height + 8.0
        }
        .clamp(min_y, max_y);
        Rect {
            x,
            y,
            width: self.width,
            height,
        }
    }
}
thread_local! {
    static LAYOUT: std::cell::RefCell<Option<Layout>> = const { std::cell::RefCell::new(None) };
}
fn desired_height(session: u64) -> f64 {
    DESIRED_SIZE
        .lock()
        .ok()
        .filter(|size| size.0 == session)
        .map(|size| size.1)
        .unwrap_or(MIN_HEIGHT)
}
pub fn resize(app: &AppHandle, session: u64, height: f64, dark: bool) {
    if !height.is_finite() {
        return;
    }
    if let Ok(mut size) = DESIRED_SIZE.lock() {
        *size = (session, height.clamp(MIN_HEIGHT, MAX_HEIGHT), dark);
    }
    let app_clone = app.clone();
    let _ = app.run_on_main_thread(move || {
        if CURRENT_SESSION.load(Ordering::SeqCst) != session {
            return;
        }
        LAYOUT.with(|slot| {
            let Some(layout) = *slot.borrow() else {
                return;
            };
            if layout.session != session {
                return;
            }
            let height = desired_height(session);
            let frame = layout.frame(height);
            #[cfg(target_os = "macos")]
            PANEL.with(|slot| {
                if let Some(panel) = slot.borrow().as_ref() {
                    sync_panel_appearance(panel, session);
                    set_panel_frame(panel, frame, layout.primary_top);
                }
            });
            #[cfg(not(target_os = "macos"))]
            if let Some(window) = app_clone.get_webview_window(WINDOW_LABEL) {
                let _ = window.set_size(tauri::LogicalSize::new(400.0, height));
                let _ = window
                    .set_position(tauri::PhysicalPosition::new(frame.x as i32, frame.y as i32));
            }
        });
        let _ = app_clone;
    });
}

pub fn set_interacting(session: u64, active: bool) {
    if active {
        HOVER_SESSION.store(session, Ordering::SeqCst);
    } else {
        let _ = HOVER_SESSION.compare_exchange(session, 0, Ordering::SeqCst, Ordering::SeqCst);
    }
}

pub fn is_interacting(session: u64) -> bool {
    session != 0 && HOVER_SESSION.load(Ordering::SeqCst) == session
}
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
pub fn show(app: &AppHandle, session: u64) {
    CURRENT_SESSION.store(session, Ordering::SeqCst);
    HOVER_SESSION.store(0, Ordering::SeqCst);
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
            // The native panel accepts scrolling while never becoming key.
            let _ = window.set_focusable(false);
            let _ = window.set_ignore_cursor_events(false);
            #[cfg(target_os = "macos")]
            show_macos(&window, caret, session);
            #[cfg(not(target_os = "macos"))]
            show_fallback(&window, session);
        });
    });
}

pub fn hide(app: &AppHandle) {
    CURRENT_SESSION.store(0, Ordering::SeqCst);
    HOVER_SESSION.store(0, Ordering::SeqCst);
    let generation = GENERATION.fetch_add(1, Ordering::SeqCst) + 1;
    let ui_app = app.clone();
    let _ = app.run_on_main_thread(move || {
        if GENERATION.load(Ordering::SeqCst) != generation {
            return;
        }
        LAYOUT.with(|slot| *slot.borrow_mut() = None);
        #[cfg(target_os = "macos")]
        PANEL.with(|slot| {
            if let Some(panel) = slot.borrow().as_ref() {
                panel.orderOut(None);
            }
        });
        if let Some(window) = ui_app.get_webview_window(WINDOW_LABEL) {
            let _ = window.hide();
        }
    });
}

#[cfg(target_os = "macos")]
use objc2::{define_class, rc::Retained, MainThreadOnly};
#[cfg(target_os = "macos")]
use objc2_app_kit::NSPanel;

#[cfg(target_os = "macos")]
define_class!(
    #[unsafe(super(NSPanel))]
    #[thread_kind = MainThreadOnly]
    struct PreviewPanel;
    impl PreviewPanel {
        #[unsafe(method(canBecomeKeyWindow))]
        fn can_become_key(&self) -> bool { false }
        #[unsafe(method(canBecomeMainWindow))]
        fn can_become_main(&self) -> bool { false }
    }
);
#[cfg(target_os = "macos")]
thread_local! {
    static PANEL: std::cell::RefCell<Option<Retained<PreviewPanel>>> = const { std::cell::RefCell::new(None) };
}

#[cfg(target_os = "macos")]
fn native_panel(
    window: &tauri::WebviewWindow,
    mtm: objc2::MainThreadMarker,
) -> Option<Retained<PreviewPanel>> {
    use objc2_app_kit::{
        NSAutoresizingMaskOptions, NSBackingStoreType, NSColor, NSVisualEffectBlendingMode,
        NSVisualEffectMaterial, NSVisualEffectState, NSVisualEffectView, NSWindow,
        NSWindowCollectionBehavior, NSWindowOrderingMode, NSWindowStyleMask,
    };
    use objc2_foundation::{NSPoint, NSRect, NSSize};
    PANEL.with(|slot| {
        if let Some(panel) = slot.borrow().as_ref() {
            return Some(panel.clone());
        }
        let raw = window.ns_window().ok()?;
        let source = unsafe { &*(raw as *const NSWindow) };
        let content = source.contentView()?;
        let frame = source.frame();
        let panel: Retained<PreviewPanel> = unsafe {
            objc2::msg_send![PreviewPanel::alloc(mtm),
            initWithContentRect: frame,
            styleMask: NSWindowStyleMask::Borderless | NSWindowStyleMask::NonactivatingPanel,
            backing: NSBackingStoreType::Buffered,
            defer: false]
        };
        panel.setOpaque(false);
        panel.setBackgroundColor(Some(&NSColor::clearColor()));
        panel.setHasShadow(false);
        panel.setLevel(3); // NSFloatingWindowLevel
        panel.setCollectionBehavior(
            NSWindowCollectionBehavior::CanJoinAllSpaces
                | NSWindowCollectionBehavior::FullScreenAuxiliary,
        );
        panel.setFloatingPanel(true);
        panel.setHidesOnDeactivate(false);
        panel.setBecomesKeyOnlyIfNeeded(true);
        panel.setIgnoresMouseEvents(false);
        panel.setAcceptsMouseMovedEvents(true);
        // Keep Tauri's original host hidden. Reparent its content, preserving
        // the existing WKWebView, IPC bridge, and frontend listeners.
        source.orderOut(None);
        source.setContentView(None);
        panel.setContentView(Some(&content));
        let material = NSVisualEffectView::initWithFrame(
            NSVisualEffectView::alloc(mtm),
            NSRect::new(
                NSPoint::new(16.0, 16.0),
                NSSize::new(frame.size.width - 32.0, frame.size.height - 32.0),
            ),
        );
        material.setMaterial(NSVisualEffectMaterial::Popover);
        material.setBlendingMode(NSVisualEffectBlendingMode::BehindWindow);
        material.setState(NSVisualEffectState::Active);
        // Match the shared glass opacity: preserve some desktop visibility
        // instead of stacking an opaque native material beneath the CSS tint.
        material.setAlphaValue(0.8);
        material.setAutoresizingMask(
            NSAutoresizingMaskOptions::ViewWidthSizable
                | NSAutoresizingMaskOptions::ViewHeightSizable,
        );
        material.setWantsLayer(true);
        if let Some(layer) = material.layer() {
            unsafe {
                let _: () = objc2::msg_send![&*layer, setCornerRadius: 12.0_f64];
            }
            layer.setMasksToBounds(true);
        }
        content.addSubview_positioned_relativeTo(&material, NSWindowOrderingMode::Below, None);
        *slot.borrow_mut() = Some(panel.clone());
        Some(panel)
    })
}

#[cfg(target_os = "macos")]
fn show_macos(
    window: &tauri::WebviewWindow,
    caret: Option<crate::macos_ax::CaretBounds>,
    session: u64,
) {
    use objc2::MainThreadMarker;
    use objc2_app_kit::{NSEvent, NSScreen};
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
    let Some(native) = native_panel(window, mtm) else {
        return;
    };
    let layout = Layout {
        session,
        anchor,
        area,
        width: 400.0,
        scale: 1.0,
        primary_top,
        above: anchor.y + anchor.height + 8.0 + MAX_HEIGHT > area.y + area.height - 8.0,
    };
    LAYOUT.with(|slot| *slot.borrow_mut() = Some(layout));
    sync_panel_appearance(&native, session);
    set_panel_frame(&native, layout.frame(desired_height(session)), primary_top);
    // Unlike makeKeyAndOrderFront / application activation, this only changes
    // visibility and leaves the insertion target's keyboard focus untouched.
    native.orderFront(None);
}

#[cfg(target_os = "macos")]
fn sync_panel_appearance(panel: &PreviewPanel, session: u64) {
    use objc2_app_kit::{
        NSAppearance, NSAppearanceCustomization, NSAppearanceNameAqua, NSAppearanceNameDarkAqua,
    };
    let dark = DESIRED_SIZE
        .lock()
        .ok()
        .filter(|size| size.0 == session)
        .is_some_and(|size| size.2);
    let name = unsafe {
        if dark {
            NSAppearanceNameDarkAqua
        } else {
            NSAppearanceNameAqua
        }
    };
    panel.setAppearance(NSAppearance::appearanceNamed(name).as_deref());
}

#[cfg(target_os = "macos")]
fn set_panel_frame(panel: &PreviewPanel, frame: Rect, primary_top: f64) {
    use objc2_foundation::{NSPoint, NSRect, NSSize};
    panel.setFrame_display(
        NSRect::new(
            NSPoint::new(frame.x, primary_top - frame.y - frame.height),
            NSSize::new(frame.width, frame.height),
        ),
        true,
    );
}

#[cfg(not(target_os = "macos"))]
fn show_fallback(window: &tauri::WebviewWindow, session: u64) {
    let Ok(cursor) = window.cursor_position() else {
        return;
    };
    let Ok(Some(monitor)) = window.monitor_from_point(cursor.x, cursor.y) else {
        return;
    };
    let area = monitor.work_area();
    let scale = monitor.scale_factor();
    let anchor = Rect {
        x: cursor.x,
        y: cursor.y,
        width: 0.0,
        height: 0.0,
    };
    let area = Rect {
        x: area.position.x as f64,
        y: area.position.y as f64,
        width: area.size.width as f64,
        height: area.size.height as f64,
    };
    let layout = Layout {
        session,
        anchor,
        area,
        width: 400.0 * scale,
        scale,
        primary_top: 0.0,
        above: anchor.y + anchor.height + 8.0 + MAX_HEIGHT * scale > area.y + area.height - 8.0,
    };
    LAYOUT.with(|slot| *slot.borrow_mut() = Some(layout));
    let height = desired_height(session);
    let frame = layout.frame(height);
    let _ = window.set_size(tauri::LogicalSize::new(400.0, height));
    let _ = window.set_position(tauri::PhysicalPosition::new(frame.x as i32, frame.y as i32));
    let _ = window.show();
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn growth_keeps_the_same_caret_edge_and_stays_inside_the_screen() {
        let layout = Layout {
            session: 1,
            anchor: Rect {
                x: 120.0,
                y: 700.0,
                width: 1.0,
                height: 18.0,
            },
            area: Rect {
                x: 0.0,
                y: 24.0,
                width: 1440.0,
                height: 820.0,
            },
            width: 400.0,
            scale: 1.0,
            primary_top: 900.0,
            above: true,
        };
        let small = layout.frame(MIN_HEIGHT);
        let full = layout.frame(MAX_HEIGHT);
        assert_eq!(small.y + small.height, full.y + full.height);
        assert_eq!(small.x, full.x);
        assert!(full.y >= layout.area.y);
        let below = Layout {
            above: false,
            anchor: Rect {
                y: 100.0,
                ..layout.anchor
            },
            ..layout
        };
        assert_eq!(below.frame(MIN_HEIGHT).y, below.frame(MAX_HEIGHT).y);
    }
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
