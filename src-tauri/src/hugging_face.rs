//! Public Hub metadata discovery. The UI keeps only models this app can run.
use serde::Serialize;
use serde_json::Value;
use std::{
    collections::VecDeque,
    io::Read,
    sync::{Mutex, OnceLock},
    time::{Duration, Instant},
};

const ENDPOINT: &str = "https://huggingface.co/api/models";
const MAX_BYTES: u64 = 2 * 1024 * 1024;
const CACHE_TTL: Duration = Duration::from_secs(5 * 60);
const CACHE_CAPACITY: usize = 32;
const ASR: &str = "automatic-speech-recognition";

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SearchResults {
    pub models: Vec<HubModel>,
    pub cached: bool,
}

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct HubModel {
    pub id: String,
    pub author: String,
    pub name: String,
    pub downloads: u64,
    pub likes: u64,
    pub pipeline_tag: Option<String>,
    pub library_name: Option<String>,
    pub languages: Vec<String>,
    pub license: Option<String>,
    pub description: Option<String>,
    pub gated: bool,
    pub source: String,
}

#[derive(Default)]
struct Cache(VecDeque<(String, Instant, Vec<HubModel>)>);
impl Cache {
    fn get(&mut self, query: &str, now: Instant) -> Option<Vec<HubModel>> {
        self.0
            .retain(|(_, at, _)| now.saturating_duration_since(*at) < CACHE_TTL);
        let index = self.0.iter().position(|(key, _, _)| key == query)?;
        let entry = self.0.remove(index)?;
        let models = entry.2.clone();
        self.0.push_back(entry);
        Some(models)
    }
    fn insert(&mut self, query: String, models: Vec<HubModel>, now: Instant) {
        self.0
            .retain(|(key, at, _)| key != &query && now.saturating_duration_since(*at) < CACHE_TTL);
        self.0.push_back((query, now, models));
        while self.0.len() > CACHE_CAPACITY {
            self.0.pop_front();
        }
    }
}
static CACHE: OnceLock<Mutex<Cache>> = OnceLock::new();

fn validate_query(query: &str) -> Result<&str, String> {
    if query.chars().count() > 120 {
        return Err("Search must be 120 characters or fewer.".into());
    }
    if query.chars().any(char::is_control) {
        return Err("Search must not contain control characters.".into());
    }
    Ok(query.trim())
}

fn repo_part(part: &str) -> bool {
    !part.is_empty()
        && part != "."
        && part != ".."
        && part
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || "-_.".contains(c))
}

fn author_search(query: &str) -> Option<(&str, &str)> {
    let (author, name) = query.split_once('/')?;
    if query.bytes().filter(|b| *b == b'/').count() == 1 && repo_part(author) && repo_part(name) {
        Some((author, name))
    } else {
        None
    }
}

fn request_url(query: &str) -> Result<reqwest::Url, String> {
    let mut url = reqwest::Url::parse(ENDPOINT)
        .map_err(|_| "Could not prepare Hugging Face search.".to_string())?;
    let mut params = url.query_pairs_mut();
    params.extend_pairs([
        ("pipeline_tag", ASR),
        ("sort", "downloads"),
        ("direction", "-1"),
        ("limit", "30"),
        ("full", "true"),
        ("cardData", "true"),
    ]);
    if let Some((author, name)) = author_search(query) {
        params.append_pair("author", author);
        params.append_pair("search", name);
    } else if !query.is_empty() {
        params.append_pair("search", query);
    }
    drop(params);
    Ok(url)
}

/// Blocking network work; call from Tauri's blocking worker pool.
pub fn search(query: &str) -> Result<SearchResults, String> {
    let query = validate_query(query)?;
    let cache = CACHE.get_or_init(|| Mutex::new(Cache::default()));
    if let Some(models) = cache
        .lock()
        .map_err(|_| "Model search cache is unavailable.".to_string())?
        .get(query, Instant::now())
    {
        return Ok(SearchResults {
            models,
            cached: true,
        });
    }
    // Fixed HTTPS origin, no redirects and no credentials, even for gated repositories.
    let client = reqwest::blocking::Client::builder()
        .connect_timeout(Duration::from_secs(5))
        .timeout(Duration::from_secs(15))
        .redirect(reqwest::redirect::Policy::none())
        .user_agent("Fairspoken/0.1 model-discovery")
        .build()
        .map_err(|_| "Could not initialize Hugging Face search.".to_string())?;
    let response = client.get(request_url(query)?).send().map_err(|error| {
        if error.is_timeout() {
            "Hugging Face search timed out. Please try again."
        } else {
            "Could not reach Hugging Face. Check your internet connection and try again."
        }
        .to_string()
    })?;
    if response.status() == reqwest::StatusCode::TOO_MANY_REQUESTS {
        return Err(
            "Hugging Face is limiting searches. Please wait a minute and try again.".into(),
        );
    }
    if !response.status().is_success() {
        return Err(
            "Hugging Face search is temporarily unavailable. Please try again later.".into(),
        );
    }
    if response
        .content_length()
        .is_some_and(|length| length > MAX_BYTES)
    {
        return Err("Hugging Face returned too much metadata. Try a more specific search.".into());
    }
    let mut bytes = Vec::new();
    response
        .take(MAX_BYTES + 1)
        .read_to_end(&mut bytes)
        .map_err(|_| {
            "Could not read Hugging Face results. Check your connection and try again.".to_string()
        })?;
    let mut models = parse_models(&bytes)?;
    rank_models(&mut models, query);
    cache
        .lock()
        .map_err(|_| "Model search cache is unavailable.".to_string())?
        .insert(query.to_owned(), models.clone(), Instant::now());
    Ok(SearchResults {
        models,
        cached: false,
    })
}

fn text(value: &Value) -> Option<String> {
    value
        .as_str()
        .filter(|text| !text.is_empty() && text.len() <= 512)
        .map(str::to_owned)
}

fn description(card: &Value) -> Option<String> {
    let raw = text(&card["short_description"]).or_else(|| text(&card["description"]))?;
    let sentence = raw.split(['.', '\n']).next().unwrap_or(&raw).trim();
    let clipped: String = sentence.chars().take(160).collect();
    if clipped.is_empty() {
        None
    } else {
        Some(clipped)
    }
}

fn rank_key(model: &HubModel, query: &str) -> u8 {
    let needle = query.to_ascii_lowercase();
    let id = model.id.to_ascii_lowercase();
    let name = model.name.to_ascii_lowercase();
    if id == needle || name == needle {
        0
    } else if id.starts_with(&needle)
        || name.starts_with(&needle)
        || id.rsplit_once('/').is_some_and(|(_, rest)| rest == needle)
    {
        1
    } else {
        2
    }
}

fn rank_models(models: &mut [HubModel], query: &str) {
    if query.is_empty() {
        return;
    }
    models.sort_by(|a, b| {
        rank_key(a, query)
            .cmp(&rank_key(b, query))
            .then(b.downloads.cmp(&a.downloads))
            .then(a.id.cmp(&b.id))
    });
}

fn parse_models(bytes: &[u8]) -> Result<Vec<HubModel>, String> {
    if bytes.len() as u64 > MAX_BYTES {
        return Err("Hugging Face returned too much metadata. Try a more specific search.".into());
    }
    let value: Value = serde_json::from_slice(bytes)
        .map_err(|_| "Hugging Face returned invalid model metadata.".to_string())?;
    let rows = value
        .as_array()
        .ok_or_else(|| "Hugging Face returned invalid model metadata.".to_string())?;
    let mut models = Vec::new();
    for row in rows {
        let Some(id) = text(&row["id"]) else { continue };
        // IDs are used in model-page links by the UI. Keep them repository-shaped.
        if id.split('/').count() > 2
            || id
                .split('/')
                .any(|part| part.is_empty() || part == "." || part == "..")
            || !id
                .chars()
                .all(|c| c.is_ascii_alphanumeric() || "-_./".contains(c))
        {
            continue;
        }
        if row["pipeline_tag"].as_str() != Some(ASR) || row["private"].as_bool() == Some(true) {
            continue;
        }
        if models.iter().any(|model: &HubModel| model.id == id) {
            continue;
        }
        let card = &row["cardData"];
        let languages = match &card["language"] {
            Value::String(_) => text(&card["language"]).into_iter().collect(),
            Value::Array(values) => values.iter().filter_map(text).take(64).collect(),
            _ => Vec::new(),
        };
        let license = text(&card["license"]).or_else(|| {
            row["tags"]
                .as_array()?
                .iter()
                .filter_map(Value::as_str)
                .find_map(|tag| {
                    tag.strip_prefix("license:")
                        .filter(|license| !license.is_empty() && license.len() <= 512)
                        .map(str::to_owned)
                })
        });
        let (author, name) = id.split_once('/').map_or_else(
            || (id.clone(), id.clone()),
            |(author, name)| (author.to_owned(), name.to_owned()),
        );
        models.push(HubModel {
            id,
            author,
            name,
            downloads: row["downloads"].as_u64().unwrap_or(0),
            likes: row["likes"].as_u64().unwrap_or(0),
            pipeline_tag: Some(ASR.to_owned()),
            library_name: text(&row["library_name"]),
            languages,
            license,
            description: description(card),
            gated: match &row["gated"] {
                Value::Bool(value) => *value,
                Value::String(value) => value != "false" && !value.is_empty(),
                _ => false,
            },
            source: "huggingface".to_owned(),
        });
        if models.len() == 30 {
            break;
        }
    }
    Ok(models)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn query_validation_and_url_keep_fixed_origin() {
        assert!(validate_query(&"é".repeat(120)).is_ok());
        assert!(validate_query(&"x".repeat(121)).is_err());
        assert!(validate_query("hello\nworld").is_err());
        assert_eq!(validate_query(" whisper ").unwrap(), "whisper");
        let query = "https://evil.example/?limit=999&search=x";
        let url = request_url(query).unwrap();
        assert_eq!(url.host_str(), Some("huggingface.co"));
        let pairs: Vec<_> = url.query_pairs().collect();
        assert_eq!(pairs.iter().filter(|(key, _)| key == "limit").count(), 1);
        assert!(pairs
            .iter()
            .any(|(key, value)| key == "search" && value == query));
        assert!(!pairs.iter().any(|(key, _)| key == "author"));
        assert!(!request_url("")
            .unwrap()
            .query_pairs()
            .any(|(key, _)| key == "search" || key == "author"));
        let split = request_url("openai/whisper-tiny").unwrap();
        let split_pairs: Vec<_> = split.query_pairs().collect();
        assert!(split_pairs
            .iter()
            .any(|(key, value)| key == "author" && value == "openai"));
        assert!(split_pairs
            .iter()
            .any(|(key, value)| key == "search" && value == "whisper-tiny"));
        assert!(author_search("openai/").is_none());
        assert!(author_search("org/a/b").is_none());
    }
    #[test]
    fn parses_optional_metadata_and_rejects_unrelated_or_unsafe_rows() {
        let models = parse_models(br#"[
          {"id":"org/speech","pipeline_tag":"automatic-speech-recognition","downloads":42,"gated":"manual","cardData":{"language":["en","fr",8],"license":"mit","short_description":"A tiny speech model. More later."}},
          {"id":"org/speech","pipeline_tag":"automatic-speech-recognition"},
          {"id":"org/other","pipeline_tag":"text-generation"},
          {"id":"../bad","pipeline_tag":"automatic-speech-recognition"},
          {"id":"org/private","pipeline_tag":"automatic-speech-recognition","private":true},
          {"id":"org/minimal","pipeline_tag":"automatic-speech-recognition","downloads":-2,"likes":"bad","gated":false,"cardData":{"language":"ja","description":"First line\nSecond"},"tags":["license:apache-2.0"]},
          null
        ]"#).unwrap();
        assert_eq!(models.len(), 2);
        assert_eq!(models[0].author, "org");
        assert_eq!(models[0].name, "speech");
        assert_eq!(models[0].description.as_deref(), Some("A tiny speech model"));
        assert!(models[0].gated);
        assert_eq!(models[0].languages, ["en", "fr"]);
        assert_eq!(models[0].downloads, 42);
        assert_eq!(models[1].downloads, 0);
        assert_eq!(models[1].languages, ["ja"]);
        assert_eq!(models[1].license.as_deref(), Some("apache-2.0"));
        assert_eq!(models[1].description.as_deref(), Some("First line"));
        assert!(!models[1].gated);
        let long = "é".repeat(200);
        let oversize = parse_models(format!(
            r#"[{{"id":"org/long","pipeline_tag":"automatic-speech-recognition","cardData":{{"description":"{long}"}}}}]"#
        ).as_bytes()).unwrap();
        assert_eq!(oversize[0].description.as_ref().unwrap().chars().count(), 160);
        let mut ranked = parse_models(br#"[
          {"id":"other/whisper-tiny","pipeline_tag":"automatic-speech-recognition","downloads":99},
          {"id":"openai/whisper-tiny","pipeline_tag":"automatic-speech-recognition","downloads":1}
        ]"#).unwrap();
        rank_models(&mut ranked, "openai/whisper-tiny");
        assert_eq!(ranked[0].id, "openai/whisper-tiny");
    }
    #[test]
    fn malformed_and_oversize_responses_fail() {
        assert!(parse_models(b"{}").is_err());
        assert!(parse_models(b"not json").is_err());
        assert!(parse_models(&vec![b' '; MAX_BYTES as usize + 1]).is_err());
        assert!(parse_models(b"[]").unwrap().is_empty());
    }
    #[test]
    fn cache_expires_and_evicts_least_recently_used() {
        let mut cache = Cache::default();
        let now = Instant::now();
        for i in 0..CACHE_CAPACITY {
            cache.insert(i.to_string(), vec![], now);
        }
        assert!(cache.get("0", now).is_some());
        cache.insert("extra".into(), vec![], now);
        assert!(cache.get("1", now).is_none());
        assert!(cache.get("0", now).is_some());
        assert_eq!(cache.0.len(), CACHE_CAPACITY);
        assert!(cache.get("0", now + CACHE_TTL).is_none());
        assert!(cache.0.is_empty());
    }
}
