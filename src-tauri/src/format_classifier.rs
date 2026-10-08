//! The optional format classifier: one short model call that labels a site or
//! app no rule knows (`format_context`), made while the user is still
//! speaking and remembered per site or app so each is classified once.
//!
//! Off unless the user turns on "Detect format with AI for unknown apps". It
//! uses whichever provider polish uses: the local runtime (never loading a
//! model for it, and never waiting past its deadline for a polish pass) or
//! Fairspoken Cloud's `/v1/format`. Its input is the app name, window title,
//! site host and field labels; its output is one label. Anything else —
//! a timeout, an unloaded model, a Worker without the endpoint, an answer
//! that is not a label — is unavailable, and the dictation stays `plain`.

use crate::format_context::{Classifier, ClassifierInput};
use crate::polish_input::Format;
use crate::settings::{cloud_url, PolishProvider, Settings};
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Instant;
use tauri::{AppHandle, Manager};

const SYSTEM_PROMPT: &str = "You decide how text dictated into an app should be laid out. Reply with one word: email, chat, document, notes, code or plain.
- email: writing or replying to an email
- chat: a chat or direct message
- document: a document, article or long-form page
- notes: a note, list or outline
- code: source code, a code review or an issue comment
- plain: a search box, a form, a prompt to an AI assistant, or anything else";

/// Set once the Worker answered that it has no `/v1/format`, so later
/// dictations do not ask again until the app restarts.
static CLOUD_UNSUPPORTED: AtomicBool = AtomicBool::new(false);

fn describe(input: &ClassifierInput) -> String {
    [
        ("App", &input.app_name),
        ("Window", &input.window_title),
        ("Site", &input.host),
        ("Field", &input.field),
    ]
    .iter()
    .filter(|(_, value)| !value.trim().is_empty())
    .map(|(label, value)| format!("{label}: {}", value.trim()))
    .collect::<Vec<_>>()
    .join("\n")
}

pub fn messages(input: &ClassifierInput) -> serde_json::Value {
    serde_json::json!([
        {"role":"system", "content":SYSTEM_PROMPT},
        {"role":"user", "content":describe(input)},
    ])
}

/// The label in a model's answer: its first word, if that is a format.
pub fn parse_label(answer: &str) -> Option<Format> {
    let word: String = answer
        .trim()
        .trim_start_matches(|c: char| !c.is_alphabetic())
        .chars()
        .take_while(|c| c.is_alphabetic())
        .collect();
    Format::from_id(&word.to_lowercase())
}

/// The classifier for this recording, or `None` when the user has not turned
/// it on (or polish is off, or its provider cannot run it).
pub fn for_settings(app: &AppHandle, settings: &Settings) -> Option<Classifier> {
    if !settings.polish_enabled || !settings.format_ai_detection {
        return None;
    }
    match settings.polish_provider {
        PolishProvider::Local => {
            let model = settings.polish_model.clone();
            let app = app.clone();
            Some(Box::new(move |input: &ClassifierInput, deadline: Instant| {
                let services = app.state::<crate::AppServices>();
                let answer = services
                    .local_models
                    .complete_short(&model, messages(input), 4, deadline)?;
                // Never the answer itself: it could repeat the window title, and
                // the reason is stored with the dictation.
                parse_label(&answer).ok_or_else(|| "the answer was not a label".to_string())
            }))
        }
        PolishProvider::Cloud => {
            let token = settings.cloud_auth_token.trim().to_string();
            let base = cloud_url()?;
            if token.is_empty() || CLOUD_UNSUPPORTED.load(Ordering::SeqCst) {
                return None;
            }
            Some(Box::new(move |input: &ClassifierInput, deadline: Instant| {
                classify_with_cloud(&base, &token, input, deadline)
            }))
        }
    }
}

fn classify_with_cloud(
    base: &str,
    token: &str,
    input: &ClassifierInput,
    deadline: Instant,
) -> Result<Format, String> {
    let timeout = deadline.saturating_duration_since(Instant::now());
    if timeout.is_zero() {
        return Err("no time left".into());
    }
    let url = reqwest::Url::parse(base)
        .and_then(|base| base.join("v1/format"))
        .map_err(|e| format!("invalid cloud URL: {e}"))?;
    let response = crate::polish::shared_client()?
        .post(url)
        .timeout(timeout)
        .bearer_auth(token)
        .json(&serde_json::json!({
            "appName": input.app_name,
            "windowTitle": input.window_title,
            "host": input.host,
            "field": input.field,
        }))
        .send()
        .map_err(|e| format!("request failed: {e}"))?;
    let status = response.status();
    if matches!(status.as_u16(), 404 | 405 | 501) {
        CLOUD_UNSUPPORTED.store(true, Ordering::SeqCst);
        return Err("Fairspoken Cloud has no format endpoint".into());
    }
    if !status.is_success() {
        return Err(format!("format endpoint returned {status}"));
    }
    let body: serde_json::Value = response.json().map_err(|e| e.to_string())?;
    body["format"]
        .as_str()
        .and_then(Format::from_id)
        .ok_or_else(|| "format endpoint returned no label".into())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn answers_parse_to_a_label_or_nothing() {
        assert_eq!(parse_label("email"), Some(Format::Email));
        assert_eq!(parse_label(" Document."), Some(Format::Document));
        assert_eq!(parse_label("**chat**"), Some(Format::Chat));
        assert_eq!(parse_label("notes\n"), Some(Format::Notes));
        assert_eq!(parse_label("It looks like email"), None);
        assert_eq!(parse_label(""), None);
    }

    #[test]
    fn the_model_sees_only_the_four_inputs() {
        let input = ClassifierInput {
            key: "site:intranet.example.com".into(),
            app_name: "Google Chrome".into(),
            window_title: "Team wiki".into(),
            host: "intranet.example.com".into(),
            field: String::new(),
        };
        let messages = messages(&input);
        assert_eq!(
            messages[1]["content"],
            "App: Google Chrome\nWindow: Team wiki\nSite: intranet.example.com"
        );
        assert_eq!(messages.as_array().unwrap().len(), 2);
    }
}
