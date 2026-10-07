//! Super mode evaluation harness (an ignored test; needs macOS `say` and
//! `afconvert`, the Parakeet v3 model installed, and downloads the Whisper
//! model if missing):
//!
//! ```sh
//! cargo test --release --manifest-path src-tauri/Cargo.toml --lib \
//!   super_mode_eval -- --ignored --nocapture
//! ```
//!
//! Synthesizes ~30 sentences full of names, technical terms and drug names,
//! then dictates each one through the real chunked pipeline (5 s preview
//! chunks, frames at real-time pace) three ways: Parakeet alone, Whisper
//! alone (with the dictionary as its usual prompt) and super mode. Reports
//! WER, dictionary-term accuracy and release-to-text latency.
//!
//! `FAIRSPOKEN_EVAL_WHISPER` picks the Whisper model (default `base`; keep
//! it small: Parakeet alone holds about 2.5 GB), `FAIRSPOKEN_EVAL_VOICE` the
//! `say` voice (default Samantha), `FAIRSPOKEN_EVAL_FAST=1` feeds audio as
//! fast as possible instead of in real time (latency is then meaningless),
//! `FAIRSPOKEN_EVAL_OUT` writes the per-sentence results as JSON.

use crate::audio::{AudioFrame, Recording};
use crate::models::{ModelService, SttModel, WhisperModel};
use crate::settings::{Settings, SuperModeSetting};
use crate::transcription::TranscriptionService;
use std::path::Path;
use std::process::Command;
use std::time::{Duration, Instant};

const DICTIONARY: &[&str] = &[
    "Siobhan", "Nguyen", "Saoirse", "Aoife", "Tadhg", "Krzysztof", "Ngozi", "Xiomara", "Eilidh",
    "Joaquin", "Kubernetes", "PostgreSQL", "Grafana", "Tailscale", "Terraform", "Fairspoken",
    "Supabase", "nginx", "Prometheus", "Kafka", "Redis", "GraphQL", "WebAssembly", "Vercel",
    "metoprolol", "atorvastatin", "levothyroxine", "Ozempic", "Xarelto", "amlodipine",
    "lisinopril", "gabapentin", "apixaban", "sertraline", "omeprazole", "Claude",
];

/// (sentence, the dictionary terms in it). The last few are confusers: no
/// dictionary term, but words that sound like one ("cloud" for "Claude").
const SENTENCES: &[(&str, &[&str])] = &[
    ("Please ask Siobhan to review the Kubernetes deployment before lunch.", &["Siobhan", "Kubernetes"]),
    ("Doctor Nguyen increased the metoprolol dose after the last visit.", &["Nguyen", "metoprolol"]),
    ("Saoirse moved the dashboards from Grafana to the new cluster.", &["Saoirse", "Grafana"]),
    ("The patient takes atorvastatin every night and levothyroxine in the morning.", &["atorvastatin", "levothyroxine"]),
    ("We connect the build servers to the office network with Tailscale.", &["Tailscale"]),
    ("Aoife thinks the Terraform plan will delete the old database.", &["Aoife", "Terraform"]),
    ("Ozempic and Xarelto are both on the pharmacy shortage list this week.", &["Ozempic", "Xarelto"]),
    ("Tadhg wrote the migration from PostgreSQL to the managed service.", &["Tadhg", "PostgreSQL"]),
    ("Start amlodipine at the lowest dose and recheck her blood pressure.", &["amlodipine"]),
    ("Krzysztof is giving a talk about WebAssembly at the meetup.", &["Krzysztof", "WebAssembly"]),
    ("The Prometheus alert fired because Kafka fell behind again.", &["Prometheus", "Kafka"]),
    ("Ngozi switched the session cache over to Redis.", &["Ngozi", "Redis"]),
    ("He stopped lisinopril because of the cough and started losartan.", &["lisinopril"]),
    ("Xiomara deployed the preview build to Vercel this morning.", &["Xiomara", "Vercel"]),
    ("Gabapentin can make some patients dizzy for the first few days.", &["gabapentin"]),
    ("Eilidh prefers GraphQL for the mobile client.", &["Eilidh", "GraphQL"]),
    ("Joaquin asked whether apixaban interacts with ibuprofen.", &["Joaquin", "apixaban"]),
    ("Fairspoken pastes the transcript at the cursor when you release the key.", &["Fairspoken"]),
    ("The team moved authentication over to Supabase last quarter.", &["Supabase"]),
    ("Sertraline was increased and omeprazole was stopped.", &["sertraline", "omeprazole"]),
    ("Put nginx in front of the API and cache the static files.", &["nginx"]),
    ("I need to call Siobhan and Doctor Nguyen about the referral.", &["Siobhan", "Nguyen"]),
    ("The Grafana panel shows the Redis memory climbing all afternoon.", &["Grafana", "Redis"]),
    ("Remind me to refill the levothyroxine prescription on Friday.", &["levothyroxine"]),
    ("Saoirse and Krzysztof are pairing on the Kubernetes upgrade.", &["Saoirse", "Krzysztof", "Kubernetes"]),
    ("Tailscale and Terraform both need updating before the audit.", &["Tailscale", "Terraform"]),
    ("We stored the backups in the cloud and checked them twice.", &[]),
    ("The meeting moved to the small conference room upstairs.", &[]),
    ("Can you send me the notes from the planning session yesterday.", &[]),
    ("Her readiness score was low after the long flight home.", &[]),
];

struct Utterance {
    text: &'static str,
    terms: &'static [&'static str],
    samples: Vec<i16>,
}

#[derive(Clone, serde::Serialize)]
struct RunResult {
    config: String,
    reference: String,
    hypothesis: String,
    errors: usize,
    reference_words: usize,
    terms_correct: usize,
    terms_total: usize,
    release_ms: u64,
    speech_model_ms: u64,
    super_mode: Option<crate::super_mode::SuperModeOutcome>,
}

fn words(text: &str) -> Vec<String> {
    text.split(|c: char| !(c.is_alphanumeric() || c == '\''))
        .map(|w| w.trim_matches('\'').to_lowercase())
        .filter(|w| !w.is_empty())
        .collect()
}

fn word_errors(reference: &[String], hypothesis: &[String]) -> usize {
    let mut previous: Vec<usize> = (0..=hypothesis.len()).collect();
    for (i, r) in reference.iter().enumerate() {
        let mut current = vec![i + 1];
        for (j, h) in hypothesis.iter().enumerate() {
            let cost = usize::from(r != h);
            current.push((previous[j + 1] + 1).min(current[j] + 1).min(previous[j] + cost));
        }
        previous = current;
    }
    previous[hypothesis.len()]
}

fn term_found(hypothesis: &[String], term: &str) -> bool {
    let term = words(term);
    hypothesis.windows(term.len()).any(|window| window == term.as_slice())
}

fn synthesize(dir: &Path, index: usize, text: &str, voice: &str) -> Vec<i16> {
    let aiff = dir.join(format!("{index}.aiff"));
    let wav = dir.join(format!("{index}.wav"));
    let status = Command::new("say")
        .args(["-v", voice, "-o"])
        .arg(&aiff)
        .arg(text)
        .status()
        .expect("run say");
    assert!(status.success(), "say failed for {text}");
    let status = Command::new("afconvert")
        .args(["-f", "WAVE", "-d", "LEI16@16000", "-c", "1"])
        .arg(&aiff)
        .arg(&wav)
        .status()
        .expect("run afconvert");
    assert!(status.success(), "afconvert failed for {text}");
    let mut reader = hound::WavReader::open(&wav).expect("read wav");
    assert_eq!(reader.spec().sample_rate, 16_000);
    reader.samples::<i16>().map(|s| s.expect("sample")).collect()
}

/// One dictation through the chunked pipeline, as the app runs it.
fn dictate(
    service: &mut TranscriptionService,
    settings: &Settings,
    models: &ModelService,
    samples: &[i16],
    paced: bool,
) -> (String, Duration, u64, Option<crate::super_mode::SuperModeOutcome>) {
    let (preview_tx, preview_rx) = std::sync::mpsc::channel();
    let start = service
        .start_session_traced(settings, models, Some(preview_tx), None)
        .expect("start session");
    let audio_tx = start.audio_tx.expect("streaming session");
    let mut audio = samples.to_vec();
    // The pause before the key is released.
    audio.extend(std::iter::repeat_n(0_i16, 16_000 * 3 / 10));
    for frame in audio.chunks(1_600) {
        audio_tx
            .send(AudioFrame {
                pcm_i16: frame.to_vec(),
                sample_rate: 16_000,
            })
            .expect("send frame");
        if paced {
            std::thread::sleep(Duration::from_millis(100));
        }
    }
    drop(audio_tx);
    let recording = Recording {
        pcm_i16: audio,
        sample_rate: 16_000,
        dropped_stream_frames: 0,
    };
    let released = Instant::now();
    let text = service
        .finish_session(&recording, settings, models)
        .unwrap_or_default();
    let release = released.elapsed();
    drop(preview_rx);
    (
        text,
        release,
        crate::transcription::speech_model_busy_ms(),
        service.take_super_mode_outcome(),
    )
}

fn percentile(values: &mut [u64], p: f64) -> u64 {
    values.sort_unstable();
    if values.is_empty() {
        return 0;
    }
    let rank = ((values.len() - 1) as f64 * p).round() as usize;
    values[rank]
}

#[test]
#[ignore = "needs macOS say/afconvert and the speech models; run by hand"]
fn super_mode_eval() {
    let models = ModelService::default();
    assert!(
        models.files_present(SttModel::Parakeet),
        "install Parakeet TDT 0.6B v3 first"
    );
    let whisper_id = std::env::var("FAIRSPOKEN_EVAL_WHISPER").unwrap_or_else(|_| "base".into());
    let whisper = WhisperModel::from_model_id(&whisper_id).expect("a Whisper model id");
    models
        .prepare(SttModel::Whisper(whisper))
        .expect("download the Whisper model");
    let voice = std::env::var("FAIRSPOKEN_EVAL_VOICE").unwrap_or_else(|_| "Samantha".into());
    let paced = std::env::var("FAIRSPOKEN_EVAL_FAST").is_err();

    let dir = tempfile::tempdir().expect("temp dir");
    let utterances: Vec<Utterance> = SENTENCES
        .iter()
        .enumerate()
        .map(|(index, (text, terms))| Utterance {
            text,
            terms,
            samples: synthesize(dir.path(), index, text, &voice),
        })
        .collect();
    let audio_seconds: f64 = utterances.iter().map(|u| u.samples.len() as f64 / 16_000.0).sum();
    println!(
        "{} sentences, {audio_seconds:.1} s of audio, voice {voice}, Whisper {whisper_id}, {} feed",
        utterances.len(),
        if paced { "real-time" } else { "fast" }
    );

    let dictionary: Vec<String> = DICTIONARY.iter().map(|t| t.to_string()).collect();
    let base = Settings {
        vocabulary_hints: dictionary,
        language: "en".into(),
        ..Settings::default()
    };
    let configs = [
        ("parakeet", Settings { model: SttModel::Parakeet, ..base.clone() }),
        (
            "whisper",
            Settings {
                model: SttModel::Whisper(whisper),
                ..base.clone()
            },
        ),
        (
            "super",
            Settings {
                model: SttModel::Parakeet,
                super_mode: SuperModeSetting::On,
                super_mode_model: whisper,
                ..base.clone()
            },
        ),
    ];

    let mut results: Vec<RunResult> = Vec::new();
    for (name, settings) in &configs {
        // One configuration's engines at a time keeps memory to Parakeet
        // plus one small Whisper.
        let mut service = TranscriptionService::default();
        service.preload(settings, &models).expect("load engine");
        // Warm-up dictation: loads the super mode secondary and fills caches.
        let _ = dictate(&mut service, settings, &models, &utterances[0].samples, false);
        for utterance in &utterances {
            let (text, release, busy_ms, outcome) =
                dictate(&mut service, settings, &models, &utterance.samples, paced);
            let reference = words(utterance.text);
            let hypothesis = words(&text);
            results.push(RunResult {
                config: name.to_string(),
                reference: utterance.text.to_string(),
                hypothesis: text,
                errors: word_errors(&reference, &hypothesis),
                reference_words: reference.len(),
                terms_correct: utterance.terms.iter().filter(|t| term_found(&hypothesis, t)).count(),
                terms_total: utterance.terms.len(),
                release_ms: release.as_millis() as u64,
                speech_model_ms: busy_ms,
                super_mode: outcome,
            });
        }
        service.unload();
    }

    println!("\nper sentence (where the configurations differ):");
    for (index, utterance) in utterances.iter().enumerate() {
        let row: Vec<&RunResult> = configs
            .iter()
            .map(|(name, _)| {
                results
                    .iter()
                    .filter(|r| r.config == *name)
                    .nth(index)
                    .expect("result")
            })
            .collect();
        if row.iter().all(|r| words(&r.hypothesis) == words(&row[0].hypothesis)) && row[0].errors == 0 {
            continue;
        }
        println!("  ref      {}", utterance.text);
        for r in &row {
            println!("  {:<8} {} [{} err]", r.config, r.hypothesis, r.errors);
        }
    }

    println!("\nconfig     WER     terms        release p50/p95/max ms   model ms/dictation");
    for (name, _) in &configs {
        let rows: Vec<&RunResult> = results.iter().filter(|r| r.config == *name).collect();
        let errors: usize = rows.iter().map(|r| r.errors).sum();
        let words: usize = rows.iter().map(|r| r.reference_words).sum();
        let correct: usize = rows.iter().map(|r| r.terms_correct).sum();
        let total: usize = rows.iter().map(|r| r.terms_total).sum();
        let mut release: Vec<u64> = rows.iter().map(|r| r.release_ms).collect();
        let busy = rows.iter().map(|r| r.speech_model_ms).sum::<u64>() / rows.len().max(1) as u64;
        println!(
            "{name:<9} {:>5.1}%  {correct:>2}/{total} {:>5.1}%   {:>5} / {:>5} / {:>5}         {busy}",
            100.0 * errors as f64 / words.max(1) as f64,
            100.0 * correct as f64 / total.max(1) as f64,
            percentile(&mut release, 0.5),
            percentile(&mut release, 0.95),
            percentile(&mut release, 1.0),
        );
    }

    let outcomes: Vec<&crate::super_mode::SuperModeOutcome> =
        results.iter().filter_map(|r| r.super_mode.as_ref()).collect();
    let sum = |f: fn(&crate::super_mode::SuperModeOutcome) -> usize| outcomes.iter().map(|o| f(o)).sum::<usize>();
    println!(
        "\nsuper mode: {} chunks, {} merged, {} Whisper timeouts, {} failures, {} spans won by Whisper",
        sum(|o| o.chunks),
        sum(|o| o.merged_chunks),
        sum(|o| o.secondary_timeouts),
        sum(|o| o.secondary_failures),
        sum(|o| o.secondary_wins),
    );
    for outcome in &outcomes {
        for d in &outcome.disagreements {
            if d.chosen == crate::ensemble::Side::Secondary {
                println!("  took {:?} over {:?} ({:?}, {:?})", d.secondary, d.primary, d.reason, d.term);
            }
        }
    }
    let mut reasons: std::collections::BTreeMap<String, usize> = Default::default();
    for outcome in &outcomes {
        for d in &outcome.disagreements {
            *reasons.entry(format!("{:?}->{:?}", d.reason, d.chosen)).or_default() += 1;
        }
    }
    println!("  disagreement reasons: {reasons:?}");
    let failures: std::collections::BTreeSet<&str> =
        outcomes.iter().filter_map(|o| o.reason.as_deref()).collect();
    println!("  failure reasons: {failures:?}");

    if let Ok(path) = std::env::var("FAIRSPOKEN_EVAL_OUT") {
        std::fs::write(&path, serde_json::to_string_pretty(&results).unwrap()).expect("write results");
        println!("results written to {path}");
    }
}
