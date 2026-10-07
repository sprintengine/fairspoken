//! Client side of the AI polish pass: sends the raw transcript to the cloud
//! Worker's `/v1/polish` between ASR and the user's deterministic
//! post-processing rules. Polish failure is never a dictation failure — any
//! error, timeout, or suspicious response falls back to the raw transcript
//! and the pipeline continues exactly as if polish were off.

use crate::app_categories::{categorize, AppCategory};
use crate::polish_input::Format;
use crate::settings::{cloud_url, Settings, CLOUD_UNAVAILABLE};
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

/// Resolve the polish tone for a category from the user's `polish_tones`
/// setting; missing key = "default".
pub fn tone_for_category(settings: &Settings, category: AppCategory) -> String {
    settings
        .polish_tones
        .get(category.id())
        .cloned()
        .unwrap_or_else(|| "default".to_string())
}

/// Which `polish_tones` row applies. A recognised app keeps its own category;
/// a browser or unknown app (`Other`) takes the category of the format decided
/// for it, so a Gmail tab uses the Email tone and Slack in a browser the
/// Messaging tone.
pub fn tone_category(app: AppCategory, format: Format) -> AppCategory {
    if app != AppCategory::Other {
        return app;
    }
    match format {
        Format::Email => AppCategory::Email,
        Format::Chat => AppCategory::Messaging,
        Format::Document | Format::Notes => AppCategory::Docs,
        Format::Code => AppCategory::Code,
        Format::Plain => AppCategory::Other,
    }
}

/// The app the transcript will be pasted into (platform-neutral mirror of
/// `macos_input::FrontmostApp`), with the layout decided for it at recording
/// start (`format_context`).
#[derive(Clone, Debug)]
pub struct PolishTargetApp {
    pub bundle_id: String,
    pub name: String,
    pub format: Format,
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
    /// The `<format>` label of the local contract (docs/polish-input.md),
    /// omitted when `plain`. Optional for the Worker: one that predates it
    /// ignores the field and polishes exactly as before.
    #[serde(skip_serializing_if = "Option::is_none")]
    format: Option<&'static str>,
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

#[cfg(test)]
pub fn maybe_polish(
    raw: &str,
    settings: &Settings,
    target: Option<&PolishTargetApp>,
    surrounding: Option<&str>,
) -> PolishDecision {
    maybe_polish_traced(raw, settings, target, surrounding, None)
}
pub fn maybe_polish_traced(
    raw: &str,
    settings: &Settings,
    target: Option<&PolishTargetApp>,
    surrounding: Option<&str>,
    span: Option<&crate::note_debug::Span>,
) -> PolishDecision {
    let result = maybe_polish_impl(raw, settings, target, surrounding, span);
    if let Some(span) = span {
        span.finish(raw, &result);
    }
    result
}
fn maybe_polish_impl(
    raw_transcript: &str,
    settings: &Settings,
    target_app: Option<&PolishTargetApp>,
    surrounding_text: Option<&str>,
    span: Option<&crate::note_debug::Span>,
) -> PolishDecision {
    if !settings.polish_enabled {
        return PolishDecision::Disabled;
    }
    if settings.cloud_auth_token.trim().is_empty() {
        return PolishDecision::Skipped("no Fairspoken Cloud token");
    }
    if raw_transcript.trim().is_empty() {
        return PolishDecision::Skipped("empty transcript");
    }
    if raw_transcript.chars().count() > POLISH_MAX_CHARS {
        return PolishDecision::Skipped("transcript too long");
    }
    let category = target_app
        .map(|app| categorize(&app.bundle_id))
        .unwrap_or(AppCategory::Other);
    if category == AppCategory::Terminal {
        return PolishDecision::Skipped("terminal app frontmost");
    }
    let format = target_app.map(|app| app.format).unwrap_or_default();
    let tone = tone_for_category(settings, tone_category(category, format));
    if tone == "off" {
        return PolishDecision::Skipped("polish is off for this app type");
    }
    if cloud_url().is_none() {
        return PolishDecision::Skipped("Fairspoken Cloud is not available in this build");
    }

    // Surrounding text goes off-device only when the user opted into BOTH
    // polish and context awareness (Phase C of context-awareness-ax).
    let surrounding = if settings.context_awareness {
        surrounding_text.filter(|text| !text.trim().is_empty())
    } else {
        None
    };

    match polish_transcript(
        raw_transcript,
        settings,
        target_app,
        category,
        tone,
        surrounding,
        span,
    ) {
        Ok(response) => {
            let text = response.text.trim();
            // Whatever the model, a dictionary term nobody said is invented.
            if let Some(term) = crate::transcript_cleanup::ungrounded_vocabulary(
                raw_transcript,
                text,
                &settings.vocabulary_hints,
            ) {
                return PolishDecision::Failed(format!(
                    "polish inserted the dictionary term \"{term}\" that was not spoken"
                ));
            }
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
    category: AppCategory,
    tone: String,
    surrounding_text: Option<&str>,
    span: Option<&crate::note_debug::Span>,
) -> Result<PolishResponse, String> {
    let base_url =
        reqwest::Url::parse(&cloud_url().ok_or_else(|| CLOUD_UNAVAILABLE.to_string())?)
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
            category: category.id().to_string(),
            tone,
        }),
        surrounding_text,
        // Only the terms something in the transcript sounds like: the rest
        // cannot help, tempt the model, and need not leave the device.
        vocabulary: crate::transcript_cleanup::relevant_vocabulary(
            raw_transcript,
            &settings.vocabulary_hints,
        ),
        format: target_app
            .map(|app| app.format)
            .filter(|format| *format != Format::Plain)
            .map(Format::id),
    };

    if let Some(span) = span {
        span.event("polish-request", serde_json::json!({"provider":"cloud","body":request,"prompt":"The cloud service owns its system prompt; it is not available to this client."}));
    }
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
    let value = response
        .json::<serde_json::Value>()
        .map_err(|err| format!("polish response was invalid: {err}"))?;
    if let Some(span) = span {
        span.event("polish-response", value.clone());
    }
    serde_json::from_value(value).map_err(|err| format!("polish response was invalid: {err}"))
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
            format: Format::Plain,
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
            format: Some("email"),
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
        assert_eq!(json["format"], "email");
    }

    #[test]
    fn optional_request_fields_are_omitted_not_null() {
        let request = PolishRequest {
            text: "hello",
            language: None,
            app_context: None,
            surrounding_text: None,
            vocabulary: Vec::new(),
            format: None,
        };

        let json = serde_json::to_value(&request).expect("serialize");
        let object = json.as_object().expect("object");
        assert_eq!(object.keys().collect::<Vec<_>>(), vec!["text"]);
    }

    #[test]
    fn response_deserializes_from_the_worker_shape() {
        // Copied from the Fairspoken Cloud Worker's /v1/polish contract.
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
            maybe_polish(raw, &Settings::default(), None, None),
            PolishDecision::Disabled
        ));

        let no_token = Settings {
            polish_enabled: true,
            ..Settings::default()
        };
        assert!(matches!(
            maybe_polish(raw, &no_token, None, None),
            PolishDecision::Skipped("no Fairspoken Cloud token")
        ));

        assert!(matches!(
            maybe_polish("   ", &polish_settings(), None, None),
            PolishDecision::Skipped("empty transcript")
        ));

        let long = "a".repeat(POLISH_MAX_CHARS + 1);
        assert!(matches!(
            maybe_polish(&long, &polish_settings(), None, None),
            PolishDecision::Skipped("transcript too long")
        ));

        assert!(matches!(
            maybe_polish(
                raw,
                &polish_settings(),
                Some(&app("com.googlecode.iterm2")),
                None
            ),
            PolishDecision::Skipped("terminal app frontmost")
        ));
    }

    #[test]
    fn every_terminal_stays_skipped_after_the_category_refactor() {
        for bundle_id in [
            "com.apple.Terminal",
            "com.googlecode.iterm2",
            "dev.warp.Warp-Stable",
            "com.github.wez.wezterm",
            "net.kovidgoyal.kitty",
            "com.mitchellh.ghostty",
            "co.zeit.hyper",
        ] {
            assert!(
                matches!(
                    maybe_polish("um hello", &polish_settings(), Some(&app(bundle_id)), None),
                    PolishDecision::Skipped("terminal app frontmost")
                ),
                "{bundle_id} should skip polish"
            );
        }
    }

    #[test]
    fn tone_off_short_circuits_before_any_network_call() {
        let mut settings = polish_settings();
        settings
            .polish_tones
            .insert("docs".to_string(), "off".to_string());

        assert!(matches!(
            maybe_polish("um hello", &settings, Some(&app("com.apple.Notes")), None),
            PolishDecision::Skipped("polish is off for this app type")
        ));

        // `off` for `other` also covers dictations with no frontmost app info.
        settings
            .polish_tones
            .insert("other".to_string(), "off".to_string());
        assert!(matches!(
            maybe_polish("um hello", &settings, None, None),
            PolishDecision::Skipped("polish is off for this app type")
        ));
    }

    #[test]
    fn tone_resolution_reads_the_setting_with_default_fallback() {
        let mut settings = polish_settings();
        settings
            .polish_tones
            .insert("messaging".to_string(), "casual".to_string());

        assert_eq!(
            tone_for_category(&settings, AppCategory::Messaging),
            "casual"
        );
        assert_eq!(tone_for_category(&settings, AppCategory::Email), "default");
    }

    #[test]
    fn a_browser_takes_the_tone_of_its_decided_format() {
        assert_eq!(
            tone_category(AppCategory::Other, Format::Email),
            AppCategory::Email
        );
        assert_eq!(
            tone_category(AppCategory::Other, Format::Chat),
            AppCategory::Messaging
        );
        assert_eq!(
            tone_category(AppCategory::Other, Format::Notes),
            AppCategory::Docs
        );
        assert_eq!(
            tone_category(AppCategory::Other, Format::Plain),
            AppCategory::Other
        );
        // A recognised app keeps its own row whatever the format.
        assert_eq!(
            tone_category(AppCategory::Messaging, Format::Email),
            AppCategory::Messaging
        );

        // Email tone "off" now covers a Gmail tab too.
        let mut settings = polish_settings();
        settings
            .polish_tones
            .insert("email".to_string(), "off".to_string());
        let gmail = PolishTargetApp {
            format: Format::Email,
            ..app("com.google.Chrome")
        };
        assert!(matches!(
            maybe_polish("um hello", &settings, Some(&gmail), None),
            PolishDecision::Skipped("polish is off for this app type")
        ));
    }
}
