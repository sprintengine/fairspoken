//! The polish input contract: what one dictation's user turn looks like.
//!
//! Every prompt variant that takes tags (the instructed prompt in
//! `local_models.rs`, and any model trained on these tags) builds its user turn
//! here and nowhere else, so the layout cannot drift between them. The full
//! contract, with examples, is in docs/polish-input.md; change both together.
//!
//! The user turn is the optional tags, in this order and each on its own line,
//! then the transcript, a blank line and the anchor:
//!
//! ```text
//! <spelling>term, term</spelling>
//! <format>email|chat|document|notes|code|plain</format>
//! <tone>casual|neutral|formal</tone>
//! <transcript>…</transcript>
//!
//! Output only the cleaned transcript.
//! ```
//!
//! A tag that carries no information is omitted: no spelling terms, format
//! `plain`, tone `neutral`. A dictation with none of them is byte-identical to
//! the turn before format and tone existed. The system prompt and worked
//! examples never vary per dictation (tone used to be appended to the system
//! prompt; it is a tag now), so the runtime's prompt cache always covers them.

use serde::{Deserialize, Serialize};

/// The contract, restated right after the transcript where a small model
/// weights it most.
pub const ANCHOR: &str = "Output only the cleaned transcript.";

/// Opening tags a model must never echo back. `validate_output` rejects any
/// output containing one.
pub const ECHO_MARKERS: &[&str] = &["<transcript", "<spelling", "<format", "<tone"];

/// How the destination wants the spoken words laid out. Formatting only
/// restructures what was said; it never adds words.
#[derive(Clone, Copy, Debug, Default, Deserialize, Eq, Hash, PartialEq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum Format {
    /// Greeting and sign-off on their own lines, paragraphs at topic shifts.
    Email,
    /// Short, no added structure.
    Chat,
    /// Full sentences and paragraphs.
    Document,
    /// Enumerations may become bullet lists.
    Notes,
    /// Identifiers and symbols exactly as spoken, no prose reformatting.
    Code,
    /// The cleanup with no layout opinion — the behaviour before formats.
    #[default]
    Plain,
}

impl Format {
    pub const ALL: [Format; 6] = [
        Format::Email,
        Format::Chat,
        Format::Document,
        Format::Notes,
        Format::Code,
        Format::Plain,
    ];

    /// The tag value, the cloud request value and the stored label.
    pub fn id(self) -> &'static str {
        match self {
            Format::Email => "email",
            Format::Chat => "chat",
            Format::Document => "document",
            Format::Notes => "notes",
            Format::Code => "code",
            Format::Plain => "plain",
        }
    }

    pub fn from_id(id: &str) -> Option<Format> {
        Format::ALL.into_iter().find(|format| format.id() == id)
    }
}

/// The register the speaker wants, from the per-category `polish_tones`
/// setting (`default` is `Neutral`; `off` never reaches a prompt).
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub enum Tone {
    Casual,
    #[default]
    Neutral,
    Formal,
}

impl Tone {
    pub fn id(self) -> &'static str {
        match self {
            Tone::Casual => "casual",
            Tone::Neutral => "neutral",
            Tone::Formal => "formal",
        }
    }

    /// Maps a `polish_tones` value. Unknown values are neutral.
    pub fn from_setting(value: &str) -> Tone {
        match value {
            "casual" => Tone::Casual,
            "formal" => Tone::Formal,
            _ => Tone::Neutral,
        }
    }
}

/// Everything about one dictation's destination that a prompt may see. Never
/// window titles, URLs or surrounding text: small models repeat any lead-in
/// they are shown, so context reaches them only as these compact labels.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct Layout {
    pub format: Format,
    pub tone: Tone,
}

/// Renders the user turn for `transcript` (see the module docs).
pub fn user_turn(transcript: &str, spelling: &[String], layout: Layout) -> String {
    let mut content = String::new();
    if !spelling.is_empty() {
        content.push_str(&format!("<spelling>{}</spelling>\n", spelling.join(", ")));
    }
    if layout.format != Format::Plain {
        content.push_str(&format!("<format>{}</format>\n", layout.format.id()));
    }
    if layout.tone != Tone::Neutral {
        content.push_str(&format!("<tone>{}</tone>\n", layout.tone.id()));
    }
    content.push_str(&format!("<transcript>{transcript}</transcript>\n\n{ANCHOR}"));
    content
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_untagged_turn_is_the_pre_format_layout() {
        assert_eq!(
            user_turn("hello there", &[], Layout::default()),
            "<transcript>hello there</transcript>\n\nOutput only the cleaned transcript."
        );
    }

    #[test]
    fn tags_come_in_contract_order_each_on_its_own_line() {
        let turn = user_turn(
            "hi mary",
            &["Railway".into(), "Vercel".into()],
            Layout {
                format: Format::Email,
                tone: Tone::Formal,
            },
        );
        assert_eq!(
            turn,
            "<spelling>Railway, Vercel</spelling>\n<format>email</format>\n<tone>formal</tone>\n<transcript>hi mary</transcript>\n\nOutput only the cleaned transcript."
        );
    }

    #[test]
    fn uninformative_tags_are_omitted() {
        let tone_only = user_turn(
            "ok",
            &[],
            Layout {
                format: Format::Plain,
                tone: Tone::Casual,
            },
        );
        assert_eq!(tone_only, "<tone>casual</tone>\n<transcript>ok</transcript>\n\nOutput only the cleaned transcript.");
        let format_only = user_turn(
            "ok",
            &[],
            Layout {
                format: Format::Chat,
                tone: Tone::Neutral,
            },
        );
        assert!(!format_only.contains("<tone"));
        assert!(format_only.starts_with("<format>chat</format>\n<transcript>"));
    }

    #[test]
    fn labels_round_trip_and_tones_map_from_settings() {
        for format in Format::ALL {
            assert_eq!(Format::from_id(format.id()), Some(format));
            assert_eq!(
                serde_json::to_value(format).unwrap(),
                serde_json::json!(format.id())
            );
        }
        assert_eq!(Format::from_id("Email"), None);
        assert_eq!(Tone::from_setting("default"), Tone::Neutral);
        assert_eq!(Tone::from_setting("casual"), Tone::Casual);
        assert_eq!(Tone::from_setting("formal"), Tone::Formal);
        assert_eq!(Tone::from_setting("anything"), Tone::Neutral);
    }

    #[test]
    fn every_tag_in_a_turn_is_an_echo_marker() {
        let turn = user_turn(
            "x",
            &["T".into()],
            Layout {
                format: Format::Notes,
                tone: Tone::Casual,
            },
        );
        for line in turn.lines().filter(|line| line.starts_with('<')) {
            let open = &line[..line.find('>').unwrap()];
            assert!(ECHO_MARKERS.contains(&open), "{open}");
        }
    }
}
