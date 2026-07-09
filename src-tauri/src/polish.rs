//! Client side of the AI polish pass: sends the raw transcript to the cloud
//! Worker's `/v1/polish` between ASR and the user's deterministic
//! post-processing rules. Polish failure is never a dictation failure — any
//! error, timeout, or suspicious response falls back to the raw transcript
//! and the pipeline continues exactly as if polish were off.

use crate::settings::{cloud_url, Settings};
use reqwest::blocking::Client;
use reqwest::header::{HeaderMap, HeaderValue, AUTHORIZATION};
use serde::{Deserialize, Serialize};
use std::time::Duration;

/// Matches the Worker's POLISH_MAX_CHARS; longer dictations skip polish
/// (long-form is where LLM latency and mangling risk are worst).
pub const POLISH_MAX_CHARS: usize = 4000;

/// Hard total budget for the polish HTTP call (connect + response). The
/// dictation already succeeded; polish may only add a bounded wait.
const POLISH_TIMEOUT: Duration = Duration::from_secs(2);

/// Rewriting shell commands is actively harmful — never polish into a
/// terminal. Generalized into app categories by `backlog/app-aware-styles.md`.
pub const TERMINAL_BUNDLE_IDS: &[&str] = &[
    "com.apple.Terminal",
    "com.googlecode.iterm2",
    "dev.warp.Warp-Stable",
    "com.github.wez.wezterm",
    "net.kovidgoyal.kitty",
    "com.mitchellh.ghostty",
    "co.zeit.hyper",
];

pub fn is_terminal_app(bundle_id: &str) -> bool {
    TERMINAL_BUNDLE_IDS.contains(&bundle_id)
}

/// The app the transcript will be pasted into (platform-neutral mirror of
/// `macos_input::FrontmostApp`).
#[derive(Clone, Debug)]
pub struct PolishTargetApp {
    pub bundle_id: String,
    pub name: String,
}

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct PolishAppContext {
    bundle_id: String,
    app_name: String,
    category: String,
    tone: String,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct PolishRequest<'a> {
    text: &'a str,
    #[serde(skip_serializing_if = "Option::is_none")]
    language: Option<&'a str>,
    #[serde(skip_serializing_if = "Option::is_none")]
    app_context: Option<PolishAppContext>,
    #[serde(skip_serializing_if = "Option::is_none")]
    surrounding_text: Option<&'a str>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    vocabulary: Vec<String>,
}

#[derive(Clone, Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct PolishResponse {
    text: String,
    changed: bool,
    model: String,
    duration_ms: u64,
}

#[derive(Clone, Debug)]
pub struct PolishOutcome {
    pub text: String,
    pub model: String,
    pub duration_ms: u64,
}

/// What the polish stage decided, so the pipeline can log honestly without
/// ever failing the dictation.
#[derive(Clone, Debug)]
pub enum PolishDecision {
    /// Polish is off — behavior must stay byte-identical to before the
    /// feature existed, including no extra events.
    Disabled,
    /// Pre-call condition — no request was made. The reason is event copy.
    Skipped(&'static str),
    /// The call succeeded and changed the text.
    Polished(PolishOutcome),
    /// The call succeeded but the text is unchanged (includes Worker
    /// guardrail trips, which return the raw text with changed=false).
    Unchanged { duration_ms: u64 },
    /// The call failed — timeout, network, non-2xx, bad body.
    Failed(String),
}

pub fn maybe_polish(
    raw_transcript: &str,
    settings: &Settings,
    target_app: Option<&PolishTargetApp>,
) -> PolishDecision {
    if !settings.polish_enabled {
        return PolishDecision::Disabled;
    }
    if settings.cloud_auth_token.trim().is_empty() {
        return PolishDecision::Skipped("no MultiVoice Cloud token");
    }
    if raw_transcript.trim().is_empty() {
        return PolishDecision::Skipped("empty transcript");
    }
    if raw_transcript.chars().count() > POLISH_MAX_CHARS {
        return PolishDecision::Skipped("transcript too long");
    }
    if let Some(app) = target_app {
        if is_terminal_app(&app.bundle_id) {
            return PolishDecision::Skipped("terminal app frontmost");
        }
    }

    match polish_transcript(raw_transcript, settings, target_app) {
        Ok(response) => {
            let text = response.text.trim();
            if !response.changed || text.is_empty() || text == raw_transcript.trim() {
                PolishDecision::Unchanged {
                    duration_ms: response.duration_ms,
                }
            } else {
                PolishDecision::Polished(PolishOutcome {
                    text: text.to_string(),
                    model: response.model,
                    duration_ms: response.duration_ms,
                })
            }
        }
        Err(err) => PolishDecision::Failed(err),
    }
}

fn polish_transcript(
    raw_transcript: &str,
    settings: &Settings,
    target_app: Option<&PolishTargetApp>,
) -> Result<PolishResponse, String> {
    let base_url = reqwest::Url::parse(&cloud_url())
        .map_err(|err| format!("invalid cloud URL: {err}"))?;
    let url = base_url
        .join("v1/polish")
        .map_err(|err| format!("invalid polish URL: {err}"))?;

    let mut headers = HeaderMap::new();
    headers.insert(
        AUTHORIZATION,
        HeaderValue::from_str(&format!("Bearer {}", settings.cloud_auth_token.trim()))
            .map_err(|err| format!("invalid cloud token: {err}"))?,
    );

    let language = match settings.language.trim() {
        "" | "auto" => None,
        language => Some(language),
    };
    let request = PolishRequest {
        text: raw_transcript,
        language,
        app_context: target_app.map(|app| PolishAppContext {
            bundle_id: app.bundle_id.clone(),
            app_name: app.name.clone(),
            // Real category/tone mapping arrives with backlog/app-aware-styles.md.
            category: "other".to_string(),
            tone: "default".to_string(),
        }),
        surrounding_text: None,
        vocabulary: settings.vocabulary_hints.clone(),
    };

    let client = Client::builder()
        .timeout(POLISH_TIMEOUT)
        .build()
        .map_err(|err| format!("polish client failed: {err}"))?;
    let response = client
        .post(url)
        .headers(headers)
        .json(&request)
        .send()
        .map_err(|err| format!("polish request failed: {err}"))?;
    if !response.status().is_success() {
        return Err(format!("polish returned {}", response.status()));
    }
    response
        .json::<PolishResponse>()
        .map_err(|err| format!("polish response was invalid: {err}"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::settings::Settings;

    fn polish_settings() -> Settings {
        Settings {
            polish_enabled: true,
            cloud_auth_token: "jwt".to_string(),
            ..Settings::default()
        }
    }

    fn app(bundle_id: &str) -> PolishTargetApp {
        PolishTargetApp {
            bundle_id: bundle_id.to_string(),
            name: "App".to_string(),
        }
    }

    #[test]
    fn request_serializes_to_the_worker_contract() {
        let request = PolishRequest {
            text: "um hello",
            language: Some("en"),
            app_context: Some(PolishAppContext {
                bundle_id: "com.tinyspeck.slackmacgap".to_string(),
                app_name: "Slack".to_string(),
                category: "messaging".to_string(),
                tone: "default".to_string(),
            }),
            surrounding_text: Some("earlier text"),
            vocabulary: vec!["Tauri".to_string()],
        };

        let json = serde_json::to_value(&request).expect("serialize");
        assert_eq!(json["text"], "um hello");
        assert_eq!(json["language"], "en");
        assert_eq!(json["appContext"]["bundleId"], "com.tinyspeck.slackmacgap");
        assert_eq!(json["appContext"]["appName"], "Slack");
        assert_eq!(json["appContext"]["category"], "messaging");
        assert_eq!(json["appContext"]["tone"], "default");
        assert_eq!(json["surroundingText"], "earlier text");
        assert_eq!(json["vocabulary"][0], "Tauri");
    }

    #[test]
    fn optional_request_fields_are_omitted_not_null() {
        let request = PolishRequest {
            text: "hello",
            language: None,
            app_context: None,
            surrounding_text: None,
            vocabulary: Vec::new(),
        };

        let json = serde_json::to_value(&request).expect("serialize");
        let object = json.as_object().expect("object");
        assert_eq!(object.keys().collect::<Vec<_>>(), vec!["text"]);
    }

    #[test]
    fn response_deserializes_from_the_worker_shape() {
        // Copied from the Worker contract (cloud/worker/README.md).
        let body = r#"{
            "text": "cleaned transcript",
            "changed": true,
            "model": "@cf/meta/llama-3.1-8b-instruct-fast",
            "durationMs": 412
        }"#;

        let response: PolishResponse = serde_json::from_str(body).expect("deserialize");
        assert_eq!(response.text, "cleaned transcript");
        assert!(response.changed);
        assert_eq!(response.model, "@cf/meta/llama-3.1-8b-instruct-fast");
        assert_eq!(response.duration_ms, 412);
    }

    #[test]
    fn maybe_polish_skips_before_any_network_call() {
        let raw = "um hello world";

        assert!(matches!(
            maybe_polish(raw, &Settings::default(), None),
            PolishDecision::Disabled
        ));

        let no_token = Settings {
            polish_enabled: true,
            ..Settings::default()
        };
        assert!(matches!(
            maybe_polish(raw, &no_token, None),
            PolishDecision::Skipped("no MultiVoice Cloud token")
        ));

        assert!(matches!(
            maybe_polish("   ", &polish_settings(), None),
            PolishDecision::Skipped("empty transcript")
        ));

        let long = "a".repeat(POLISH_MAX_CHARS + 1);
        assert!(matches!(
            maybe_polish(&long, &polish_settings(), None),
            PolishDecision::Skipped("transcript too long")
        ));

        assert!(matches!(
            maybe_polish(raw, &polish_settings(), Some(&app("com.googlecode.iterm2"))),
            PolishDecision::Skipped("terminal app frontmost")
        ));
    }

    #[test]
    fn terminal_denylist_covers_every_listed_terminal() {
        for bundle_id in TERMINAL_BUNDLE_IDS {
            assert!(is_terminal_app(bundle_id), "{bundle_id} should be a terminal");
        }
        assert!(!is_terminal_app("com.apple.Notes"));
        assert!(!is_terminal_app("com.tinyspeck.slackmacgap"));
    }
}
