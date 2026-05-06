#[derive(Default)]
pub struct ClipboardService;

impl ClipboardService {
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
