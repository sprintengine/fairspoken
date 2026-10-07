//! The format decision: how the destination wants this dictation laid out
//! (`polish_input::Format`), decided at recording start while the user speaks.
//!
//! Inputs are read once from the focused app (`FocusContext`, macOS
//! Accessibility): the window title, the focused field's role and labels, and
//! for browsers the page URL cut to scheme, host and path. None of it is ever
//! persisted or shown to a polish model; the model gets only the label.
//!
//! The decision, in order (docs/polish-input.md has the full rule table):
//! 1. the user's own mapping for this site or app (`FormatSource::User`);
//! 2. the deterministic rules below (`FormatSource::Rule`);
//! 3. a label the optional classifier learned for this site or app before
//!    (`FormatSource::Llm`);
//! 4. otherwise `plain` (`FormatSource::Default`), and when the user enabled
//!    it, the classifier runs once for the site or app (`format_classifier`).
//!
//! A single-line field (a subject line, a search box) never gets a layout
//! with line breaks, whatever the site.

use crate::app_categories::{categorize, AppCategory};
use crate::note_debug::Trace;
use crate::polish_input::Format;
use serde::{Deserialize, Serialize};
use std::fs;
use std::path::PathBuf;
use std::sync::{Arc, Condvar, Mutex};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

/// What the focused app showed at recording start. Session-only.
#[derive(Clone, Debug, Default)]
pub struct FocusContext {
    pub bundle_id: String,
    pub app_name: String,
    pub window_title: Option<String>,
    pub field: FieldInfo,
    /// `scheme://host/path` of the page, browsers only (`sanitize_url`).
    pub url: Option<String>,
}

/// The focused element's accessibility labels (never its value).
#[derive(Clone, Debug, Default)]
pub struct FieldInfo {
    pub role: Option<String>,
    pub subrole: Option<String>,
    pub role_description: Option<String>,
    pub placeholder: Option<String>,
    pub description: Option<String>,
    pub help: Option<String>,
}

impl FieldInfo {
    /// The field's human-readable labels, for rules and the classifier.
    pub fn label(&self) -> String {
        [
            &self.placeholder,
            &self.description,
            &self.help,
            &self.role_description,
        ]
        .iter()
        .filter_map(|text| text.as_deref())
        .map(str::trim)
        .filter(|text| !text.is_empty())
        .collect::<Vec<_>>()
        .join(" · ")
    }

    fn single_line(&self) -> bool {
        matches!(
            self.role.as_deref(),
            Some("AXTextField" | "AXComboBox" | "AXSearchField")
        ) || self.subrole.as_deref() == Some("AXSearchField")
    }
}

/// Keeps only `scheme://host/path` of an http(s) URL: no query string, no
/// fragment, no credentials, no port. Anything else (`file:`, `about:`,
/// internal browser pages) is `None`.
pub fn sanitize_url(raw: &str) -> Option<String> {
    let url = reqwest::Url::parse(raw.trim()).ok()?;
    if !matches!(url.scheme(), "http" | "https") {
        return None;
    }
    let host = url.host_str()?.to_ascii_lowercase();
    Some(format!("{}://{host}{}", url.scheme(), url.path()))
}

/// Host of a sanitized URL without a leading `www.`.
pub fn url_host(url: &str) -> Option<String> {
    let url = reqwest::Url::parse(url).ok()?;
    let host = url.host_str()?.to_ascii_lowercase();
    Some(host.strip_prefix("www.").unwrap_or(&host).to_string())
}

fn url_path(url: &str) -> String {
    reqwest::Url::parse(url)
        .map(|url| url.path().to_string())
        .unwrap_or_default()
}

/// Browsers by bundle-id prefix: their own category says nothing about the
/// page, so they are decided by URL and title.
const BROWSERS: &[&str] = &[
    "com.apple.Safari",
    "com.google.Chrome",
    "company.thebrowser.",
    "com.microsoft.edgemac",
    "com.brave.Browser",
    "org.mozilla.firefox",
    "org.mozilla.nightly",
    "org.chromium.Chromium",
    "com.vivaldi.Vivaldi",
    "com.operasoftware.Opera",
    "com.kagi.kagimacOS",
    "app.zen-browser.zen",
];

pub fn is_browser(bundle_id: &str) -> bool {
    BROWSERS.iter().any(|prefix| bundle_id.starts_with(prefix))
}

/// Native apps whose format is not their category's (by bundle-id prefix).
/// Notes apps are `notes`, not `document`: short entries and lists are their
/// norm. Notion and the word processors stay `document`.
const APP_RULES: &[(&str, Format)] = &[
    ("com.apple.Notes", Format::Notes),
    ("md.obsidian", Format::Notes),
    ("net.shinyfrog.bear", Format::Notes),
    ("com.logseq.logseq", Format::Notes),
    ("com.agiletortoise.Drafts-OSX", Format::Notes),
    ("com.evernote.Evernote", Format::Notes),
    ("com.microsoft.onenote.mac", Format::Notes),
    ("com.apple.TextEdit", Format::Document),
    ("com.ulyssesapp.mac", Format::Document),
    ("pro.writer.mac", Format::Document),
    ("com.anthropic.claudefordesktop", Format::Plain),
    ("com.openai.chat", Format::Plain),
    ("org.whispersystems.signal-desktop", Format::Chat),
    ("com.facebook.archon", Format::Chat),
    ("com.microsoft.teams", Format::Chat),
    ("it.bloop.airmail2", Format::Email),
    ("com.freron.MailMate", Format::Email),
    ("org.mozilla.thunderbird", Format::Email),
    ("com.readdle.smartemail-Mac", Format::Email),
];

/// One site rule: the host (and, with `subdomains`, any host under it), an
/// optional path prefix, and the format.
struct SiteRule {
    host: &'static str,
    subdomains: bool,
    path: &'static str,
    format: Format,
}

const fn site(host: &'static str, path: &'static str, format: Format) -> SiteRule {
    SiteRule {
        host,
        subdomains: true,
        path,
        format,
    }
}

const fn exact(host: &'static str, format: Format) -> SiteRule {
    SiteRule {
        host,
        subdomains: false,
        path: "",
        format,
    }
}

/// First match wins, so a path-specific rule precedes its host's catch-all.
const SITE_RULES: &[SiteRule] = &[
    // email (Gmail's Chat lives under the same host)
    site("mail.google.com", "/chat", Format::Chat),
    site("mail.google.com", "", Format::Email),
    site("outlook.office.com", "", Format::Email),
    site("outlook.office365.com", "", Format::Email),
    site("outlook.live.com", "", Format::Email),
    site("outlook.cloud.microsoft", "", Format::Email),
    site("mail.yahoo.com", "", Format::Email),
    site("app.fastmail.com", "", Format::Email),
    site("mail.proton.me", "", Format::Email),
    site("app.hey.com", "", Format::Email),
    site("mail.zoho.com", "", Format::Email),
    site("mail.superhuman.com", "", Format::Email),
    // documents and notes
    site("docs.google.com", "/document", Format::Document),
    site("docs.google.com", "", Format::Plain),
    site("notion.so", "", Format::Document),
    site("notion.site", "", Format::Document),
    site("coda.io", "", Format::Document),
    site("quip.com", "", Format::Document),
    site("paper.dropbox.com", "", Format::Document),
    site("keep.google.com", "", Format::Notes),
    site("evernote.com", "", Format::Notes),
    site("workflowy.com", "", Format::Notes),
    site("roamresearch.com", "", Format::Notes),
    // chat
    site("slack.com", "", Format::Chat),
    site("teams.microsoft.com", "", Format::Chat),
    site("teams.live.com", "", Format::Chat),
    site("teams.cloud.microsoft", "", Format::Chat),
    site("discord.com", "", Format::Chat),
    site("web.whatsapp.com", "", Format::Chat),
    site("web.telegram.org", "", Format::Chat),
    site("messenger.com", "", Format::Chat),
    site("chat.google.com", "", Format::Chat),
    site("messages.google.com", "", Format::Chat),
    site("app.element.io", "", Format::Chat),
    site("linkedin.com", "/messaging", Format::Chat),
    site("x.com", "/messages", Format::Chat),
    site("instagram.com", "/direct", Format::Chat),
    // code
    site("github.com", "", Format::Code),
    site("gist.github.com", "", Format::Code),
    site("github.dev", "", Format::Code),
    site("gitlab.com", "", Format::Code),
    site("bitbucket.org", "", Format::Code),
    site("vscode.dev", "", Format::Code),
    site("replit.com", "", Format::Code),
    site("codesandbox.io", "", Format::Code),
    site("stackoverflow.com", "", Format::Code),
    // chatbots and search: a prompt is not a message to lay out
    site("claude.ai", "", Format::Plain),
    site("chatgpt.com", "", Format::Plain),
    site("chat.openai.com", "", Format::Plain),
    site("gemini.google.com", "", Format::Plain),
    site("perplexity.ai", "", Format::Plain),
    site("copilot.microsoft.com", "", Format::Plain),
    site("poe.com", "", Format::Plain),
    site("chat.mistral.ai", "", Format::Plain),
    site("chat.deepseek.com", "", Format::Plain),
    site("grok.com", "", Format::Plain),
    exact("google.com", Format::Plain),
    exact("bing.com", Format::Plain),
    exact("duckduckgo.com", Format::Plain),
];

fn site_rule(host: &str, path: &str) -> Option<Format> {
    SITE_RULES
        .iter()
        .find(|rule| {
            let host_matches = host == rule.host
                || (rule.subdomains && host.ends_with(&format!(".{}", rule.host)));
            host_matches && path.starts_with(rule.path)
        })
        .map(|rule| rule.format)
}

/// Pieces of the titles web apps give their tabs ("Inbox (3) - me@example.com
/// - Gmail"), for a browser whose URL could not be read and for unknown apps.
const TITLE_RULES: &[(&str, Format)] = &[
    (" - gmail", Format::Email),
    (" - outlook", Format::Email),
    ("| outlook", Format::Email),
    (" - google docs", Format::Document),
    (" | notion", Format::Document),
    (" - notion", Format::Document),
    (" - slack", Format::Chat),
    (" | slack", Format::Chat),
    (" | microsoft teams", Format::Chat),
    (" - discord", Format::Chat),
    (" | discord", Format::Chat),
    ("whatsapp", Format::Chat),
    (" · github", Format::Code),
    (" - claude", Format::Plain),
    (" | chatgpt", Format::Plain),
    ("chatgpt", Format::Plain),
];

/// Subject prefixes of a reply or forward, in the languages mail clients use.
const REPLY_PREFIXES: &[&str] = &["re:", "aw:", "sv:", "antw:", "fwd:", "fw:", "wg:", "tr:"];

fn title_rule(title: &str) -> Option<(Format, &'static str)> {
    let title = title.trim().to_lowercase();
    if REPLY_PREFIXES.iter().any(|prefix| title.starts_with(prefix)) {
        return Some((Format::Email, "reply subject in window title"));
    }
    TITLE_RULES
        .iter()
        .find(|(needle, _)| title.contains(needle))
        .map(|(_, format)| (*format, "window title"))
}

/// A field labelled as a message composer, for apps no other rule knows.
fn field_rule(field: &FieldInfo) -> Option<(Format, &'static str)> {
    let label = field.label().to_lowercase();
    if label.contains("message body") || label.contains("email body") {
        return Some((Format::Email, "email body field"));
    }
    if ["type a message", "send a message", "write a message", "message #", "message @"]
        .iter()
        .any(|needle| label.contains(needle))
    {
        return Some((Format::Chat, "message field"));
    }
    None
}

fn says_reply(focus: &FocusContext) -> bool {
    let title = focus.window_title.as_deref().unwrap_or("").trim().to_lowercase();
    REPLY_PREFIXES.iter().any(|prefix| title.starts_with(prefix))
        || focus.field.label().to_lowercase().contains("reply")
}

/// What the rules alone say about a focus.
#[derive(Clone, Debug, PartialEq)]
pub enum RuleOutcome {
    Decided { format: Format, rule: &'static str },
    /// No rule applies. `key` names what a learned or classified label could
    /// be stored under (`site:<host>` or `app:<bundle id>`); `None` when
    /// there is nothing stable to key on (a browser with no readable URL).
    Inconclusive { key: Option<String> },
}

/// The key a mapping for this focus is stored under: the site for a browser
/// page, the bundle id for anything else.
pub fn mapping_key(focus: &FocusContext) -> Option<String> {
    if is_browser(&focus.bundle_id) {
        return focus
            .url
            .as_deref()
            .and_then(url_host)
            .map(|host| format!("site:{host}"));
    }
    (!focus.bundle_id.trim().is_empty()).then(|| format!("app:{}", focus.bundle_id))
}

pub fn decide_by_rules(focus: &FocusContext) -> RuleOutcome {
    let key = mapping_key(focus);
    let decided = |format, rule| RuleOutcome::Decided { format, rule };
    if is_browser(&focus.bundle_id) {
        if let Some(url) = focus.url.as_deref() {
            if let Some(format) = url_host(url).and_then(|host| site_rule(&host, &url_path(url))) {
                return decided(format, "site");
            }
        }
    } else {
        if let Some((_, format)) = APP_RULES
            .iter()
            .find(|(prefix, _)| focus.bundle_id.starts_with(prefix))
        {
            return decided(*format, "app");
        }
        let format = match categorize(&focus.bundle_id) {
            AppCategory::Email => Some(Format::Email),
            AppCategory::Messaging => Some(Format::Chat),
            AppCategory::Docs => Some(Format::Document),
            AppCategory::Code | AppCategory::Terminal => Some(Format::Code),
            AppCategory::Other => None,
        };
        if let Some(format) = format {
            return decided(format, "app category");
        }
    }
    if let Some((format, rule)) = focus.window_title.as_deref().and_then(title_rule) {
        return decided(format, rule);
    }
    if let Some((format, rule)) = field_rule(&focus.field) {
        return decided(format, rule);
    }
    RuleOutcome::Inconclusive { key }
}

/// Who decided the format.
#[derive(Clone, Copy, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum FormatSource {
    /// A deterministic rule.
    Rule,
    /// The classifier, now or on an earlier dictation (cached).
    Llm,
    /// The user's own mapping in Settings.
    User,
    /// Nothing applied: `plain`.
    #[default]
    Default,
}

/// The format chosen for one dictation, and why. Stored with the dictation in
/// transcript history so it can be inspected.
#[derive(Clone, Debug, Default, Deserialize, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct FormatDecision {
    pub format: Format,
    pub source: FormatSource,
    /// The destination is a reply (email subject "Re:", a reply composer).
    /// Kept for the record; the prompt only gets `email`.
    pub reply: bool,
    /// The rule or reason, for the debug view.
    pub rule: String,
    /// The app the decision was made for, so a dictation pasted somewhere
    /// else falls back to `plain`. Not serialized: it is not needed to
    /// inspect a decision.
    #[serde(skip)]
    pub bundle_id: String,
}

/// How long streaming polish waits, before its first pass, for a classifier
/// to answer: the classifier starts `CLASSIFY_AFTER` into the recording and
/// has `CLASSIFIER_TIMEOUT`. Only a dictation into a site or app seen for the
/// first time, with the classifier turned on, waits at all.
pub const FIRST_PASS_WAIT: Duration = Duration::from_millis(
    CLASSIFY_AFTER.as_millis() as u64 + CLASSIFIER_TIMEOUT.as_millis() as u64 + 500,
);

/// Starts the format decision for a recording. Polish off needs none.
pub fn begin_recording(
    app: &tauri::AppHandle,
    settings: &crate::settings::Settings,
    memory: Arc<Mutex<FormatMemory>>,
    trace: Option<Trace>,
) -> Arc<FormatSession> {
    if !settings.polish_enabled {
        return FormatSession::settled(FormatDecision::plain("polish is off"));
    }
    let classifier = crate::format_classifier::for_settings(app, settings);
    FormatSession::start(capture_focus, memory, classifier, trace)
}

/// Ends the recording's format session (a classifier that has not started
/// never will) and returns the decision as it stands. `plain` with no session.
pub fn end_recording(slot: &Mutex<Option<Arc<FormatSession>>>) -> FormatDecision {
    let session = slot.lock().ok().and_then(|session| session.clone());
    session
        .map(|session| {
            session.stop();
            session.current()
        })
        .unwrap_or_default()
}

/// The session the current recording started, if any.
pub fn current_session(slot: &Mutex<Option<Arc<FormatSession>>>) -> Option<Arc<FormatSession>> {
    slot.lock().ok().and_then(|session| session.clone())
}

#[cfg(target_os = "macos")]
fn capture_focus() -> Option<FocusContext> {
    crate::with_ax_timeout(crate::macos_ax::focus_context).flatten()
}

/// Other platforms have no focus read: every dictation is `plain`.
#[cfg(not(target_os = "macos"))]
fn capture_focus() -> Option<FocusContext> {
    None
}

/// Settings: the learned and user-set mappings.
#[tauri::command]
pub fn get_format_mappings(services: tauri::State<'_, crate::AppServices>) -> Result<Vec<LearnedFormat>, String> {
    Ok(services.format_memory.lock().map_err(|e| e.to_string())?.list())
}

/// Settings: the user sets (or changes) the format for a site or app.
#[tauri::command]
pub fn set_format_mapping(
    key: String,
    format: Format,
    services: tauri::State<'_, crate::AppServices>,
) -> Result<Vec<LearnedFormat>, String> {
    let mut memory = services.format_memory.lock().map_err(|e| e.to_string())?;
    memory.set_by_user(&key, format)?;
    Ok(memory.list())
}

/// Settings: forget a mapping; the rules (or the classifier) decide again.
#[tauri::command]
pub fn remove_format_mapping(
    key: String,
    services: tauri::State<'_, crate::AppServices>,
) -> Result<Vec<LearnedFormat>, String> {
    let mut memory = services.format_memory.lock().map_err(|e| e.to_string())?;
    memory.remove(&key)?;
    Ok(memory.list())
}

impl FormatDecision {
    fn plain(rule: &str) -> Self {
        Self {
            rule: rule.into(),
            ..Self::default()
        }
    }

    /// The decision for pasting into `bundle_id`: `plain` when the user
    /// switched apps after the decision was made.
    pub fn for_target(&self, bundle_id: &str) -> FormatDecision {
        if self.bundle_id == bundle_id || self.format == Format::Plain {
            return self.clone();
        }
        FormatDecision {
            format: Format::Plain,
            source: FormatSource::Default,
            reply: false,
            rule: "pasted into a different app".into(),
            bundle_id: bundle_id.into(),
        }
    }
}

/// What the classifier is told: the app name, window title, site host and
/// field labels, nothing else.
#[derive(Clone, Debug, PartialEq)]
pub struct ClassifierInput {
    pub key: String,
    pub app_name: String,
    pub window_title: String,
    pub host: String,
    pub field: String,
}

fn classifier_input(focus: &FocusContext, key: String) -> ClassifierInput {
    let cap = |text: &str, max: usize| text.chars().take(max).collect::<String>();
    ClassifierInput {
        key,
        app_name: cap(&focus.app_name, 60),
        window_title: cap(focus.window_title.as_deref().unwrap_or(""), 120),
        host: focus.url.as_deref().and_then(url_host).unwrap_or_default(),
        field: cap(&focus.field.label(), 80),
    }
}

/// A single-line field keeps no line breaks, so a layout that adds them is
/// reduced to `plain` there.
fn fit_to_field(mut decision: FormatDecision, focus: &FocusContext) -> FormatDecision {
    let multiline = matches!(decision.format, Format::Email | Format::Document | Format::Notes);
    if multiline && focus.field.single_line() {
        decision.rule = format!("single-line field (was {} by {})", decision.format.id(), decision.rule);
        decision.format = Format::Plain;
        decision.source = FormatSource::Rule;
    }
    decision
}

/// Decides without the classifier. Returns what the classifier should be
/// asked when nothing else applied and there is a key to remember it under.
pub fn decide(
    focus: Option<&FocusContext>,
    memory: &FormatMemory,
) -> (FormatDecision, Option<ClassifierInput>) {
    let Some(focus) = focus else {
        return (FormatDecision::plain("no focus context"), None);
    };
    let key = mapping_key(focus);
    let with = |format, source, rule: &str| FormatDecision {
        format,
        source,
        reply: format == Format::Email && says_reply(focus),
        rule: rule.into(),
        bundle_id: focus.bundle_id.clone(),
    };
    if let Some(entry) = key.as_deref().and_then(|key| memory.get(key)) {
        if entry.source == LearnedSource::User {
            return (fit_to_field(with(entry.format, FormatSource::User, "your mapping"), focus), None);
        }
    }
    match decide_by_rules(focus) {
        RuleOutcome::Decided { format, rule } => {
            (fit_to_field(with(format, FormatSource::Rule, rule), focus), None)
        }
        RuleOutcome::Inconclusive { key } => {
            if let Some(entry) = key.as_deref().and_then(|key| memory.get(key)) {
                let decision = with(entry.format, FormatSource::Llm, "learned");
                return (fit_to_field(decision, focus), None);
            }
            let decision = with(Format::Plain, FormatSource::Default, "no rule matched");
            (decision, key.map(|key| classifier_input(focus, key)))
        }
    }
}

/// Who stored a mapping.
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum LearnedSource {
    Llm,
    User,
}

/// One remembered site or app. Only the key and the label are stored, never
/// a title or a URL path.
#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct LearnedFormat {
    pub key: String,
    pub format: Format,
    pub source: LearnedSource,
    pub updated_at: u64,
}

const MAX_MAPPINGS: usize = 500;

/// Learned and user-set mappings, `format-memory.json` next to settings.
pub struct FormatMemory {
    entries: Vec<LearnedFormat>,
    path: Option<PathBuf>,
}

impl Default for FormatMemory {
    fn default() -> Self {
        Self::load(default_memory_path())
    }
}

impl FormatMemory {
    pub fn load(path: PathBuf) -> Self {
        let entries = fs::read_to_string(&path)
            .ok()
            .and_then(|raw| serde_json::from_str::<Vec<LearnedFormat>>(&raw).ok())
            .unwrap_or_default()
            .into_iter()
            .filter(|entry| normalize_key(&entry.key).as_deref() == Some(entry.key.as_str()))
            .collect();
        Self {
            entries,
            path: Some(path),
        }
    }

    #[cfg(test)]
    pub fn in_memory() -> Self {
        Self {
            entries: Vec::new(),
            path: None,
        }
    }

    pub fn list(&self) -> Vec<LearnedFormat> {
        let mut entries = self.entries.clone();
        entries.sort_by(|a, b| a.key.cmp(&b.key));
        entries
    }

    pub fn get(&self, key: &str) -> Option<&LearnedFormat> {
        self.entries.iter().find(|entry| entry.key == key)
    }

    /// Stores a classifier label unless the user already chose one.
    pub fn learn(&mut self, key: &str, format: Format) -> Result<(), String> {
        if self.get(key).is_some_and(|entry| entry.source == LearnedSource::User) {
            return Ok(());
        }
        self.put(key, format, LearnedSource::Llm)
    }

    /// The user's choice, which the classifier never overwrites.
    pub fn set_by_user(&mut self, key: &str, format: Format) -> Result<(), String> {
        self.put(key, format, LearnedSource::User)
    }

    pub fn remove(&mut self, key: &str) -> Result<(), String> {
        self.entries.retain(|entry| entry.key != key);
        self.save()
    }

    fn put(&mut self, key: &str, format: Format, source: LearnedSource) -> Result<(), String> {
        let key = normalize_key(key).ok_or("Enter a website like example.com or an app bundle id")?;
        self.entries.retain(|entry| entry.key != key);
        self.entries.push(LearnedFormat {
            key,
            format,
            source,
            updated_at: SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .map(|d| d.as_millis() as u64)
                .unwrap_or_default(),
        });
        if self.entries.len() > MAX_MAPPINGS {
            // Drop the oldest classifier label first; user choices stay.
            if let Some(index) = self
                .entries
                .iter()
                .enumerate()
                .filter(|(_, entry)| entry.source == LearnedSource::Llm)
                .min_by_key(|(_, entry)| entry.updated_at)
                .map(|(index, _)| index)
            {
                self.entries.remove(index);
            }
        }
        self.save()
    }

    fn save(&self) -> Result<(), String> {
        let Some(path) = &self.path else {
            return Ok(());
        };
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent).map_err(|e| format!("Failed to save format mappings: {e}"))?;
        }
        let payload = serde_json::to_string_pretty(&self.entries).map_err(|e| e.to_string())?;
        fs::write(path, payload).map_err(|e| format!("Failed to save format mappings: {e}"))
    }
}

/// `site:<host>` (lowercase, no `www.`) or `app:<bundle id>`. A bare host or
/// URL typed in Settings becomes a site key.
pub fn normalize_key(key: &str) -> Option<String> {
    let key = key.trim();
    if let Some(bundle) = key.strip_prefix("app:") {
        let bundle = bundle.trim();
        let valid = !bundle.is_empty()
            && bundle.len() <= 200
            && bundle.chars().all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '-' | '_'));
        return valid.then(|| format!("app:{bundle}"));
    }
    let site = key.strip_prefix("site:").unwrap_or(key).trim();
    let with_scheme = if site.contains("://") {
        site.to_string()
    } else {
        format!("https://{site}")
    };
    let host = url_host(&with_scheme)?;
    let valid = host.contains('.')
        && host.len() <= 253
        && host.chars().all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '-'));
    valid.then(|| format!("site:{host}"))
}

fn default_memory_path() -> PathBuf {
    if let Some(path) = crate::app_dirs::env_var_os("FAIRSPOKEN_FORMAT_MEMORY_PATH") {
        return PathBuf::from(path);
    }
    crate::app_dirs::config_dir()
        .map(|dir| dir.join("format-memory.json"))
        .unwrap_or_else(|| PathBuf::from("format-memory.json"))
}

/// The classifier only runs once the recording has lasted this long, so a
/// short dictation never pays for it.
pub const CLASSIFY_AFTER: Duration = Duration::from_secs(2);
/// The classifier's whole budget, waiting for the runtime included.
pub const CLASSIFIER_TIMEOUT: Duration = Duration::from_millis(1500);
/// How long the commit path waits for the focus read and the rules. The AX
/// read itself is bounded by `with_ax_timeout`.
const CAPTURE_WAIT: Duration = Duration::from_millis(300);

/// Asks a model for one label, given a deadline. `Err` means unavailable
/// (timed out, not loaded, no endpoint); an unparseable answer is an `Err`
/// too, so it is never remembered.
pub type Classifier = Box<dyn FnOnce(&ClassifierInput, Instant) -> Result<Format, String> + Send>;

#[derive(Default)]
struct SessionState {
    decision: FormatDecision,
    /// The focus was read and the rules applied.
    captured: bool,
    /// Nothing will change the decision any more.
    settled: bool,
    /// The recording ended.
    stopped: bool,
}

/// The format decision for one recording, filled in by a background thread
/// so recording start never waits for it.
pub struct FormatSession {
    state: Mutex<SessionState>,
    changed: Condvar,
}

impl FormatSession {
    fn new() -> Arc<Self> {
        Arc::new(Self {
            state: Mutex::new(SessionState::default()),
            changed: Condvar::new(),
        })
    }

    /// A session that is already settled, for when polish is off.
    pub fn settled(decision: FormatDecision) -> Arc<Self> {
        let session = Self::new();
        session.update(|state| {
            state.decision = decision;
            state.captured = true;
            state.settled = true;
        });
        session
    }

    fn update(&self, change: impl FnOnce(&mut SessionState)) {
        if let Ok(mut state) = self.state.lock() {
            change(&mut state);
        }
        self.changed.notify_all();
    }

    fn wait_for(&self, timeout: Duration, done: impl Fn(&SessionState) -> bool) -> FormatDecision {
        let deadline = Instant::now() + timeout;
        let Ok(mut state) = self.state.lock() else {
            return FormatDecision::default();
        };
        while !done(&state) {
            let left = deadline.saturating_duration_since(Instant::now());
            if left.is_zero() {
                break;
            }
            match self.changed.wait_timeout(state, left) {
                Ok((next, _)) => state = next,
                Err(_) => return FormatDecision::default(),
            }
        }
        state.decision.clone()
    }

    /// The decision as it stands, once the rules ran (bounded).
    pub fn current(&self) -> FormatDecision {
        self.wait_for(CAPTURE_WAIT, |state| state.captured)
    }

    /// Waits, up to `timeout`, until a classifier still running has answered.
    /// Streaming polish calls this before its first pass, so the passes all
    /// use one format.
    pub fn wait_settled(&self, timeout: Duration) -> FormatDecision {
        self.wait_for(timeout, |state| state.settled)
    }

    /// The recording ended: a classifier that has not started never will.
    pub fn stop(&self) {
        self.update(|state| state.stopped = true);
    }

    /// Sleeps until `after` has passed since `started`, or the recording
    /// stops. True when the recording is still going.
    fn still_recording_after(&self, started: Instant, after: Duration) -> bool {
        let deadline = started + after;
        let Ok(mut state) = self.state.lock() else {
            return false;
        };
        while !state.stopped {
            let left = deadline.saturating_duration_since(Instant::now());
            if left.is_zero() {
                return true;
            }
            match self.changed.wait_timeout(state, left) {
                Ok((next, _)) => state = next,
                Err(_) => return false,
            }
        }
        false
    }

    /// Starts deciding: reads the focus (`capture`, already bounded by the
    /// caller), applies the rules and, when one is supplied and nothing else
    /// applied, runs the classifier once the recording passes
    /// `CLASSIFY_AFTER`.
    pub fn start(
        capture: impl FnOnce() -> Option<FocusContext> + Send + 'static,
        memory: Arc<Mutex<FormatMemory>>,
        classifier: Option<Classifier>,
        trace: Option<Trace>,
    ) -> Arc<Self> {
        let session = Self::new();
        let worker = session.clone();
        let started = Instant::now();
        let spawned = std::thread::Builder::new()
            .name("format-decision".into())
            .spawn(move || worker.run(started, capture, memory, classifier, trace));
        if spawned.is_err() {
            session.update(|state| {
                state.decision = FormatDecision::plain("format thread unavailable");
                state.captured = true;
                state.settled = true;
            });
        }
        session
    }

    fn run(
        &self,
        started: Instant,
        capture: impl FnOnce() -> Option<FocusContext>,
        memory: Arc<Mutex<FormatMemory>>,
        classifier: Option<Classifier>,
        trace: Option<Trace>,
    ) {
        let focus = capture();
        let (decision, candidate) = match memory.lock() {
            Ok(memory) => decide(focus.as_ref(), &memory),
            Err(_) => (FormatDecision::plain("format mappings unavailable"), None),
        };
        let candidate = candidate.filter(|_| classifier.is_some());
        let record = |decision: &FormatDecision, classifier: &str| {
            if let Some(trace) = &trace {
                // The key (a host or bundle id) and labels only: no title
                // and no URL path, even in opt-in debug capture.
                trace.event("format-decision", serde_json::json!({
                    "decision": decision,
                    "key": focus.as_ref().and_then(mapping_key),
                    "fieldRole": focus.as_ref().and_then(|f| f.field.role.clone()),
                    "hasWindowTitle": focus.as_ref().is_some_and(|f| f.window_title.is_some()),
                    "hasUrl": focus.as_ref().is_some_and(|f| f.url.is_some()),
                    "classifier": classifier,
                }));
            }
        };
        let settled = candidate.is_none();
        self.update(|state| {
            state.decision = decision.clone();
            state.captured = true;
            state.settled = settled;
        });
        let (Some(input), Some(classify)) = (candidate, classifier) else {
            record(&decision, "not needed");
            return;
        };
        if !self.still_recording_after(started, CLASSIFY_AFTER) {
            record(&decision, "skipped: recording too short");
            self.update(|state| state.settled = true);
            return;
        }
        let deadline = Instant::now() + CLASSIFIER_TIMEOUT;
        let decision = match classify(&input, deadline) {
            Ok(format) => {
                if let Ok(mut memory) = memory.lock() {
                    let _ = memory.learn(&input.key, format);
                }
                let decision = FormatDecision {
                    format,
                    source: FormatSource::Llm,
                    rule: "classified".into(),
                    reply: format == Format::Email && focus.as_ref().is_some_and(says_reply),
                    ..decision
                };
                let decision = match &focus {
                    Some(focus) => fit_to_field(decision, focus),
                    None => decision,
                };
                record(&decision, "ran");
                decision
            }
            Err(reason) => {
                let decision = FormatDecision {
                    rule: format!("classifier unavailable: {reason}"),
                    ..decision
                };
                record(&decision, "failed");
                decision
            }
        };
        self.update(|state| {
            state.decision = decision;
            state.settled = true;
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn browser(url: &str) -> FocusContext {
        FocusContext {
            bundle_id: "com.google.Chrome".into(),
            app_name: "Google Chrome".into(),
            url: sanitize_url(url),
            field: FieldInfo {
                role: Some("AXTextArea".into()),
                ..FieldInfo::default()
            },
            ..FocusContext::default()
        }
    }

    fn app(bundle_id: &str) -> FocusContext {
        FocusContext {
            bundle_id: bundle_id.into(),
            app_name: "App".into(),
            field: FieldInfo {
                role: Some("AXTextArea".into()),
                ..FieldInfo::default()
            },
            ..FocusContext::default()
        }
    }

    fn format_of(focus: &FocusContext) -> Format {
        decide(Some(focus), &FormatMemory::in_memory()).0.format
    }

    #[test]
    fn urls_keep_only_scheme_host_and_path() {
        assert_eq!(
            sanitize_url("https://user:pw@Mail.Google.com:443/mail/u/0/?compose=new#inbox/FMfcg").as_deref(),
            Some("https://mail.google.com/mail/u/0/")
        );
        assert_eq!(
            sanitize_url("http://example.com/a/b?q=secret").as_deref(),
            Some("http://example.com/a/b")
        );
        for url in ["file:///Users/me/doc.txt", "about:blank", "chrome://settings", "not a url"] {
            assert_eq!(sanitize_url(url), None, "{url}");
        }
        assert_eq!(url_host("https://www.notion.so/page").as_deref(), Some("notion.so"));
    }

    #[test]
    fn site_rules_cover_the_common_destinations() {
        for (url, format) in [
            ("https://mail.google.com/mail/u/0/#inbox", Format::Email),
            ("https://mail.google.com/chat/u/0/#chat/space/x", Format::Chat),
            ("https://outlook.office.com/mail/", Format::Email),
            ("https://outlook.live.com/mail/0/", Format::Email),
            ("https://docs.google.com/document/d/abc/edit", Format::Document),
            ("https://docs.google.com/spreadsheets/d/abc/edit", Format::Plain),
            ("https://www.notion.so/team/Page-123", Format::Document),
            ("https://acme.slack.com/archives/C1", Format::Chat),
            ("https://app.slack.com/client/T1/C1", Format::Chat),
            ("https://teams.microsoft.com/v2/", Format::Chat),
            ("https://discord.com/channels/1/2", Format::Chat),
            ("https://web.whatsapp.com/", Format::Chat),
            ("https://github.com/owner/repo/pull/12", Format::Code),
            ("https://github.com/owner/repo/issues/3", Format::Code),
            ("https://claude.ai/new", Format::Plain),
            ("https://chatgpt.com/", Format::Plain),
            ("https://www.linkedin.com/messaging/thread/1", Format::Chat),
        ] {
            assert_eq!(format_of(&browser(url)), format, "{url}");
            assert_eq!(decide(Some(&browser(url)), &FormatMemory::in_memory()).0.source, FormatSource::Rule);
        }
    }

    #[test]
    fn native_apps_follow_their_category_with_notes_apps_as_notes() {
        for (bundle, format) in [
            ("com.apple.mail", Format::Email),
            ("com.microsoft.Outlook", Format::Email),
            ("com.superhuman.electron", Format::Email),
            ("com.readdle.SparkDesktop", Format::Email),
            ("com.tinyspeck.slackmacgap", Format::Chat),
            ("com.apple.MobileSMS", Format::Chat),
            ("com.microsoft.Word", Format::Document),
            ("com.apple.iWork.Pages", Format::Document),
            ("notion.id", Format::Document),
            ("md.obsidian", Format::Notes),
            ("com.apple.Notes", Format::Notes),
            ("com.microsoft.VSCode", Format::Code),
            ("com.jetbrains.rustrover", Format::Code),
            ("com.anthropic.claudefordesktop", Format::Plain),
        ] {
            assert_eq!(format_of(&app(bundle)), format, "{bundle}");
        }
    }

    #[test]
    fn titles_and_fields_decide_what_the_url_cannot() {
        // Firefox with no readable URL: the tab title still says Gmail.
        let mut firefox = browser("about:blank");
        firefox.bundle_id = "org.mozilla.firefox".into();
        firefox.window_title = Some("Inbox (3) - me@example.com - Gmail".into());
        assert_eq!(format_of(&firefox), Format::Email);
        firefox.window_title = Some("general (Channel) - Acme - Slack".into());
        assert_eq!(format_of(&firefox), Format::Chat);
        firefox.window_title = Some("Something else".into());
        assert_eq!(
            decide_by_rules(&firefox),
            RuleOutcome::Inconclusive { key: None }
        );

        // A reply subject is an email reply, in any app.
        let mut unknown = app("com.example.Mailer");
        unknown.window_title = Some("Re: Lunch on Friday".into());
        let decision = decide(Some(&unknown), &FormatMemory::in_memory()).0;
        assert_eq!((decision.format, decision.reply), (Format::Email, true));

        // A reply composer in Gmail flags the reply too.
        let mut gmail = browser("https://mail.google.com/mail/u/0/#inbox/abc");
        gmail.field.description = Some("Message Body".into());
        gmail.field.help = Some("Reply".into());
        let decision = decide(Some(&gmail), &FormatMemory::in_memory()).0;
        assert_eq!((decision.format, decision.reply), (Format::Email, true));

        let mut chat = app("com.example.Chat");
        chat.field.placeholder = Some("Type a message".into());
        assert_eq!(format_of(&chat), Format::Chat);
    }

    #[test]
    fn a_single_line_field_never_gets_line_breaks() {
        let mut subject = app("com.apple.mail");
        subject.field.role = Some("AXTextField".into());
        let decision = decide(Some(&subject), &FormatMemory::in_memory()).0;
        assert_eq!(decision.format, Format::Plain);
        assert!(decision.rule.starts_with("single-line field"));
        // Chat has no line breaks to lose.
        let mut slack = app("com.tinyspeck.slackmacgap");
        slack.field.role = Some("AXTextField".into());
        assert_eq!(format_of(&slack), Format::Chat);
    }

    #[test]
    fn unknown_apps_and_sites_are_plain_and_ask_the_classifier() {
        let (decision, candidate) = decide(
            Some(&FocusContext {
                window_title: Some("Quarterly plan".into()),
                ..browser("https://intranet.example.com/wiki/page?id=7")
            }),
            &FormatMemory::in_memory(),
        );
        assert_eq!((decision.format, decision.source), (Format::Plain, FormatSource::Default));
        let candidate = candidate.expect("classifier input");
        assert_eq!(candidate.key, "site:intranet.example.com");
        assert_eq!(candidate.host, "intranet.example.com");
        assert_eq!(candidate.window_title, "Quarterly plan");

        let (_, candidate) = decide(Some(&app("com.example.Tool")), &FormatMemory::in_memory());
        assert_eq!(candidate.unwrap().key, "app:com.example.Tool");

        assert_eq!(decide(None, &FormatMemory::in_memory()), (FormatDecision::plain("no focus context"), None));
    }

    #[test]
    fn mappings_users_set_win_and_learned_ones_fill_in() {
        let mut memory = FormatMemory::in_memory();
        let wiki = browser("https://intranet.example.com/wiki");
        memory.learn("site:intranet.example.com", Format::Document).unwrap();
        let (decision, candidate) = decide(Some(&wiki), &memory);
        assert_eq!((decision.format, decision.source), (Format::Document, FormatSource::Llm));
        assert!(candidate.is_none(), "classified once");

        // The user overrides even a rule.
        memory.set_by_user("notion.so", Format::Notes).unwrap();
        let notion = browser("https://www.notion.so/page");
        let decision = decide(Some(&notion), &memory).0;
        assert_eq!((decision.format, decision.source), (Format::Notes, FormatSource::User));
        // And the classifier never overwrites the user.
        memory.learn("site:notion.so", Format::Chat).unwrap();
        assert_eq!(memory.get("site:notion.so").unwrap().format, Format::Notes);

        memory.remove("site:notion.so").unwrap();
        assert_eq!(decide(Some(&notion), &memory).0.source, FormatSource::Rule);
    }

    #[test]
    fn keys_normalize_from_what_a_user_types() {
        assert_eq!(normalize_key("Example.com").as_deref(), Some("site:example.com"));
        assert_eq!(normalize_key("https://www.example.com/path?q=1").as_deref(), Some("site:example.com"));
        assert_eq!(normalize_key("site:wiki.example.com").as_deref(), Some("site:wiki.example.com"));
        assert_eq!(normalize_key("app:com.example.Tool").as_deref(), Some("app:com.example.Tool"));
        for bad in ["", "localhost", "app:", "app:bad id", "not a host"] {
            assert_eq!(normalize_key(bad), None, "{bad}");
        }
    }

    #[test]
    fn memory_persists_only_keys_and_labels() {
        let path = std::env::temp_dir().join(format!("format-memory-{}.json", uuid::Uuid::new_v4()));
        let mut memory = FormatMemory::load(path.clone());
        memory.learn("site:intranet.example.com", Format::Document).unwrap();
        memory.set_by_user("app:com.example.Tool", Format::Chat).unwrap();
        let raw = fs::read_to_string(&path).unwrap();
        let stored: Vec<serde_json::Value> = serde_json::from_str(&raw).unwrap();
        for entry in &stored {
            let mut keys: Vec<_> = entry.as_object().unwrap().keys().cloned().collect();
            keys.sort();
            assert_eq!(keys, ["format", "key", "source", "updatedAt"]);
        }
        let reloaded = FormatMemory::load(path.clone());
        assert_eq!(reloaded.list(), memory.list());
        fs::remove_file(path).unwrap();
    }

    #[test]
    fn decisions_apply_only_to_the_app_they_were_made_for() {
        let decision = decide(Some(&app("com.apple.mail")), &FormatMemory::in_memory()).0;
        assert_eq!(decision.for_target("com.apple.mail"), decision);
        let elsewhere = decision.for_target("com.tinyspeck.slackmacgap");
        assert_eq!(elsewhere.format, Format::Plain);
        assert_eq!(elsewhere.rule, "pasted into a different app");
    }

    fn memory() -> Arc<Mutex<FormatMemory>> {
        Arc::new(Mutex::new(FormatMemory::in_memory()))
    }

    #[test]
    fn a_session_decides_by_rule_without_a_classifier() {
        let session = FormatSession::start(|| Some(app("com.apple.mail")), memory(), None, None);
        let decision = session.wait_settled(Duration::from_secs(2));
        assert_eq!((decision.format, decision.source), (Format::Email, FormatSource::Rule));
    }

    #[test]
    fn the_classifier_waits_for_a_long_enough_recording_and_is_remembered() {
        let memory = memory();
        let classifier: Classifier = Box::new(|input, _| {
            assert_eq!(input.key, "app:com.example.Tool");
            Ok(Format::Document)
        });
        let session = FormatSession::start(|| Some(app("com.example.Tool")), memory.clone(), Some(classifier), None);
        // Before CLASSIFY_AFTER the decision is plain and unsettled.
        assert_eq!(session.current().format, Format::Plain);
        let decision = session.wait_settled(CLASSIFY_AFTER + Duration::from_secs(2));
        assert_eq!((decision.format, decision.source), (Format::Document, FormatSource::Llm));
        assert_eq!(memory.lock().unwrap().get("app:com.example.Tool").unwrap().format, Format::Document);
    }

    #[test]
    fn a_short_recording_never_runs_the_classifier() {
        let memory = memory();
        let classifier: Classifier = Box::new(|_, _| panic!("must not run"));
        let session = FormatSession::start(|| Some(app("com.example.Tool")), memory.clone(), Some(classifier), None);
        assert_eq!(session.current().format, Format::Plain);
        session.stop();
        let decision = session.wait_settled(Duration::from_secs(1));
        assert_eq!(decision.format, Format::Plain);
        assert!(memory.lock().unwrap().list().is_empty());
    }

    #[test]
    fn an_unavailable_classifier_falls_back_to_plain_and_is_not_remembered() {
        let memory = memory();
        let classifier: Classifier = Box::new(|_, _| Err("timed out".into()));
        let session = FormatSession::start(|| Some(app("com.example.Tool")), memory.clone(), Some(classifier), None);
        let decision = session.wait_settled(CLASSIFY_AFTER + Duration::from_secs(2));
        assert_eq!(decision.format, Format::Plain);
        assert!(decision.rule.contains("timed out"));
        assert!(memory.lock().unwrap().list().is_empty());
    }
}
