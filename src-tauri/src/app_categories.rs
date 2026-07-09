//! Frontmost-app category mapping for the AI polish pass: the same dictation
//! should come out conversational in Slack and as complete sentences in Mail.
//! The table is data + a unit test — extend it with one-line PRs, never guess
//! at runtime. Browsers stay `Other` on purpose: a browser tab's purpose is
//! unknowable without page context.

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AppCategory {
    Messaging,
    Email,
    Docs,
    Code,
    Terminal,
    Other,
}

impl AppCategory {
    /// Stable wire/setting id — matches the polish endpoint contract and the
    /// `polish_tones` setting keys.
    pub fn id(self) -> &'static str {
        match self {
            AppCategory::Messaging => "messaging",
            AppCategory::Email => "email",
            AppCategory::Docs => "docs",
            AppCategory::Code => "code",
            AppCategory::Terminal => "terminal",
            AppCategory::Other => "other",
        }
    }
}

/// Every entry is a bundle-id **prefix** (JetBrains ships many ids under
/// `com.jetbrains.`); exact ids are just prefixes that happen to be complete.
const CATEGORY_TABLE: &[(&str, AppCategory)] = &[
    // messaging
    ("com.tinyspeck.slackmacgap", AppCategory::Messaging),
    ("com.apple.MobileSMS", AppCategory::Messaging),
    ("net.whatsapp.WhatsApp", AppCategory::Messaging),
    ("com.hnc.Discord", AppCategory::Messaging),
    ("org.telegram.desktop", AppCategory::Messaging),
    ("ru.keepcoder.Telegram", AppCategory::Messaging),
    ("com.microsoft.teams2", AppCategory::Messaging),
    ("us.zoom.xos", AppCategory::Messaging),
    // email
    ("com.apple.mail", AppCategory::Email),
    ("com.microsoft.Outlook", AppCategory::Email),
    ("com.superhuman.electron", AppCategory::Email),
    ("com.readdle.SparkDesktop", AppCategory::Email),
    ("com.mimestream.Mimestream", AppCategory::Email),
    // docs
    ("com.apple.iWork.Pages", AppCategory::Docs),
    ("com.microsoft.Word", AppCategory::Docs),
    ("com.apple.Notes", AppCategory::Docs),
    ("md.obsidian", AppCategory::Docs),
    ("notion.id", AppCategory::Docs),
    ("com.literatureandlatte.scrivener3", AppCategory::Docs),
    // code
    ("com.microsoft.VSCode", AppCategory::Code),
    ("com.todesktop.230313mzl4w4u92", AppCategory::Code), // Cursor
    ("com.jetbrains.", AppCategory::Code),
    ("dev.zed.Zed", AppCategory::Code),
    ("com.sublimetext.4", AppCategory::Code),
    ("com.apple.dt.Xcode", AppCategory::Code),
    // terminal — polish is always skipped for these (rewriting shell
    // commands is actively harmful); the category is never shown as a
    // tone row in settings.
    ("com.apple.Terminal", AppCategory::Terminal),
    ("com.googlecode.iterm2", AppCategory::Terminal),
    ("dev.warp.Warp-Stable", AppCategory::Terminal),
    ("com.github.wez.wezterm", AppCategory::Terminal),
    ("net.kovidgoyal.kitty", AppCategory::Terminal),
    ("com.mitchellh.ghostty", AppCategory::Terminal),
    ("co.zeit.hyper", AppCategory::Terminal),
];

pub fn categorize(bundle_id: &str) -> AppCategory {
    CATEGORY_TABLE
        .iter()
        .find(|(prefix, _)| bundle_id.starts_with(prefix))
        .map(|(_, category)| *category)
        .unwrap_or(AppCategory::Other)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn categorize_matrix() {
        assert_eq!(categorize("com.tinyspeck.slackmacgap"), AppCategory::Messaging);
        assert_eq!(categorize("com.apple.mail"), AppCategory::Email);
        assert_eq!(categorize("com.apple.Notes"), AppCategory::Docs);
        assert_eq!(categorize("com.microsoft.VSCode"), AppCategory::Code);
        // JetBrains prefix covers every IDE id under it.
        assert_eq!(categorize("com.jetbrains.intellij"), AppCategory::Code);
        assert_eq!(categorize("com.jetbrains.rustrover"), AppCategory::Code);
    }

    #[test]
    fn every_terminal_id_maps_to_terminal() {
        for bundle_id in [
            "com.apple.Terminal",
            "com.googlecode.iterm2",
            "dev.warp.Warp-Stable",
            "com.github.wez.wezterm",
            "net.kovidgoyal.kitty",
            "com.mitchellh.ghostty",
            "co.zeit.hyper",
        ] {
            assert_eq!(categorize(bundle_id), AppCategory::Terminal, "{bundle_id}");
        }
    }

    #[test]
    fn unknown_apps_and_browsers_are_other() {
        assert_eq!(categorize("com.example.unknown"), AppCategory::Other);
        assert_eq!(categorize("com.apple.Safari"), AppCategory::Other);
        assert_eq!(categorize("com.google.Chrome"), AppCategory::Other);
        assert_eq!(categorize(""), AppCategory::Other);
    }

    #[test]
    fn category_ids_are_the_wire_contract_strings() {
        assert_eq!(AppCategory::Messaging.id(), "messaging");
        assert_eq!(AppCategory::Email.id(), "email");
        assert_eq!(AppCategory::Docs.id(), "docs");
        assert_eq!(AppCategory::Code.id(), "code");
        assert_eq!(AppCategory::Terminal.id(), "terminal");
        assert_eq!(AppCategory::Other.id(), "other");
    }
}
