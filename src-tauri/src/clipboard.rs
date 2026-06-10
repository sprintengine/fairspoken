#[derive(Default)]
pub struct ClipboardService;

impl ClipboardService {
    /// Reads the current clipboard text, or `None` when the clipboard is
    /// empty or holds content we cannot capture as text (an image, copied
    /// files). Callers must treat `None` as "nothing restorable", not as an
    /// empty clipboard.
    pub fn read_text(&self) -> Option<String> {
        arboard::Clipboard::new().ok()?.get_text().ok()
    }

    pub fn write_text(&self, text: &str) -> Result<(), String> {
        let mut clipboard =
            arboard::Clipboard::new().map_err(|err| format!("Clipboard unavailable: {err}"))?;
        clipboard
            .set_text(text.to_string())
            .map_err(|err| format!("Clipboard write failed: {err}"))?;

        let written = clipboard
            .get_text()
            .map_err(|err| format!("Clipboard verification failed: {err}"))?;
        if written != text {
            return Err("Clipboard verification did not match transcript".to_string());
        }

        Ok(())
    }
}
