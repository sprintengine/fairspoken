use crate::settings::Settings;

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct TranscriptPostProcessResult {
    pub text: String,
    pub corrections_applied: usize,
}

pub fn apply_transcript_post_processing(
    transcript: &str,
    settings: &Settings,
) -> TranscriptPostProcessResult {
    if !settings.post_process {
        return TranscriptPostProcessResult {
            text: transcript.to_string(),
            corrections_applied: 0,
        };
    }

    // The model-free cleanup goes first, whether or not a polish pass ran: it
    // is the whole cleanup when polish is off, and a pass that tripped a guard
    // contributes its raw tail.
    let mut text = crate::transcript_cleanup::tidy(transcript, &settings.language);
    let mut corrections_applied = 0;
    // A scoped (learned) correction only fixes the speech models that made
    // the mishearing; another model may hear that phrase correctly.
    let speech_model = settings.speech_model_id();
    for correction in &settings.transcript_corrections {
        if !correction.enabled
            || !(correction.models.is_empty() || correction.models.contains(&speech_model))
        {
            continue;
        }

        let (next_text, count) = replace_phrase(
            &text,
            correction.from.trim(),
            correction.to.trim(),
            correction.case_sensitive,
            correction.whole_phrase,
        );
        text = next_text;
        corrections_applied += count;
    }

    // Snippets expand a dictated trigger phrase into longer text.
    for snippet in &settings.snippets {
        if !snippet.enabled {
            continue;
        }
        let (next_text, count) = replace_phrase(
            &text,
            snippet.trigger.trim(),
            &snippet.expansion,
            false,
            true,
        );
        text = next_text;
        corrections_applied += count;
    }

    TranscriptPostProcessResult {
        text,
        corrections_applied,
    }
}

fn replace_phrase(
    text: &str,
    from: &str,
    to: &str,
    case_sensitive: bool,
    whole_phrase: bool,
) -> (String, usize) {
    if text.is_empty() || from.is_empty() {
        return (text.to_string(), 0);
    }

    let haystack = if case_sensitive {
        text.to_string()
    } else {
        text.to_ascii_lowercase()
    };
    let needle = if case_sensitive {
        from.to_string()
    } else {
        from.to_ascii_lowercase()
    };

    let mut output = String::with_capacity(text.len());
    let mut search_cursor = 0;
    let mut emit_cursor = 0;
    let mut count = 0;
    while let Some(relative_index) = haystack[search_cursor..].find(&needle) {
        let index = search_cursor + relative_index;
        let end = index + from.len();
        if !text.is_char_boundary(index)
            || !text.is_char_boundary(end)
            || (whole_phrase && !is_phrase_boundary(text, index, end))
        {
            search_cursor = text[index..]
                .char_indices()
                .nth(1)
                .map(|(offset, _)| index + offset)
                .unwrap_or(text.len());
            continue;
        }

        output.push_str(&text[emit_cursor..index]);
        output.push_str(to);
        emit_cursor = end;
        search_cursor = end;
        count += 1;
    }

    if count == 0 {
        return (text.to_string(), 0);
    }

    output.push_str(&text[emit_cursor..]);
    (output, count)
}

fn is_phrase_boundary(text: &str, start: usize, end: usize) -> bool {
    let before = text[..start].chars().next_back();
    let after = text[end..].chars().next();
    !before.map(is_word_char).unwrap_or(false) && !after.map(is_word_char).unwrap_or(false)
}

fn is_word_char(ch: char) -> bool {
    ch.is_alphanumeric() || ch == '_'
}

#[cfg(test)]
mod tests {
    use super::apply_transcript_post_processing;
    use crate::settings::{Settings, Snippet, TranscriptCorrection};

    #[test]
    fn applies_case_insensitive_phrase_correction() {
        let settings = settings_with_corrections(vec![TranscriptCorrection {
            enabled: true,
            from: "fair spoken".to_string(),
            to: "Fairspoken".to_string(),
            case_sensitive: false,
            whole_phrase: true,
            ..TranscriptCorrection::default()
        }]);

        let result = apply_transcript_post_processing("Open fair spoken settings.", &settings);

        assert_eq!(result.text, "Open Fairspoken settings.");
        assert_eq!(result.corrections_applied, 1);
    }

    #[test]
    fn whole_phrase_correction_does_not_replace_inside_words() {
        let settings = settings_with_corrections(vec![TranscriptCorrection {
            enabled: true,
            from: "app".to_string(),
            to: "application".to_string(),
            case_sensitive: false,
            whole_phrase: true,
            ..TranscriptCorrection::default()
        }]);

        let result = apply_transcript_post_processing("The app maps happen.", &settings);

        assert_eq!(result.text, "The application maps happen.");
        assert_eq!(result.corrections_applied, 1);
    }

    #[test]
    fn strips_filler_noises_without_counting_them_as_corrections() {
        let settings = Settings {
            language: "en".to_string(),
            ..Settings::default()
        };

        let result = apply_transcript_post_processing(
            "Um, let's get rid of channels, at least for now. Mm-hmm.",
            &settings,
        );

        assert_eq!(result.text, "Let's get rid of channels, at least for now.");
        assert_eq!(result.corrections_applied, 0);
    }

    #[test]
    fn disabled_post_processing_skips_corrections() {
        let mut settings = settings_with_corrections(vec![TranscriptCorrection {
            enabled: true,
            from: "toury".to_string(),
            to: "Tauri".to_string(),
            case_sensitive: false,
            whole_phrase: true,
            ..TranscriptCorrection::default()
        }]);
        settings.post_process = false;

        let result = apply_transcript_post_processing("toury app", &settings);

        assert_eq!(result.text, "toury app");
        assert_eq!(result.corrections_applied, 0);
    }

    #[test]
    fn model_scoped_correction_only_applies_to_its_speech_models() {
        let mut settings = settings_with_corrections(vec![TranscriptCorrection {
            from: "cloud code".to_string(),
            to: "Claude Code".to_string(),
            models: vec!["parakeet-tdt-0.6b-v2".to_string()],
            ..TranscriptCorrection::default()
        }]);
        settings.model = crate::models::SttModel::ParakeetV2;
        assert_eq!(
            apply_transcript_post_processing("Ask cloud code.", &settings).text,
            "Ask Claude Code."
        );

        settings.model = crate::models::SttModel::Parakeet;
        assert_eq!(
            apply_transcript_post_processing("Ask cloud code.", &settings).text,
            "Ask cloud code."
        );
    }

    #[test]
    fn expands_snippet_trigger_phrase() {
        let settings = Settings {
            snippets: vec![Snippet {
                enabled: true,
                trigger: "my email".to_string(),
                expansion: "jane@example.com".to_string(),
            }],
            ..Settings::default()
        };

        let result = apply_transcript_post_processing("Send it to my email please.", &settings);

        assert_eq!(result.text, "Send it to jane@example.com please.");
        assert_eq!(result.corrections_applied, 1);
    }

    #[test]
    fn disabled_snippet_is_not_expanded() {
        let settings = Settings {
            snippets: vec![Snippet {
                enabled: false,
                trigger: "my email".to_string(),
                expansion: "jane@example.com".to_string(),
            }],
            ..Settings::default()
        };

        let result = apply_transcript_post_processing("Send it to my email please.", &settings);

        assert_eq!(result.text, "Send it to my email please.");
        assert_eq!(result.corrections_applied, 0);
    }

    fn settings_with_corrections(corrections: Vec<TranscriptCorrection>) -> Settings {
        Settings {
            transcript_corrections: corrections,
            ..Settings::default()
        }
    }
}
