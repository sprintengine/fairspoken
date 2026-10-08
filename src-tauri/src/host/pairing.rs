//! Discovery and pairing (PROTOCOL.md "Discovery and pairing"): the
//! unauthenticated `GET /v1/hello` identity a scanning client probes, and
//! `POST /v1/pair`, which trades the operator's pairing password for the host
//! token. The password is only ever compared here and written to the host
//! config file; no route, log line or event carries it.

use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use base64::Engine;
use serde::{Deserialize, Deserializer, Serialize};
use sha2::{Digest, Sha256};
use std::collections::{HashMap, VecDeque};
use std::time::{Duration, Instant};

pub(super) const PAIRING_PASSWORD_ENV: &str = "FAIRSPOKEN_HOST_PAIRING_PASSWORD";
pub(super) const HOST_NAME_ENV: &str = "FAIRSPOKEN_HOST_NAME";
pub(super) const MIN_PAIRING_PASSWORD_CHARS: usize = 6;
pub(super) const MAX_PAIRING_PASSWORD_CHARS: usize = 128;
const MAX_CLIENT_NAME_CHARS: usize = 64;
const MAX_HOST_NAME_CHARS: usize = 64;
const FALLBACK_HOST_NAME: &str = "Fairspoken host";
const HELLO_SERVICE: &str = "fairspoken-host";
/// Bumps only on incompatible discovery or pairing changes.
const HELLO_PROTOCOL: u32 = 1;

const RATE_WINDOW: Duration = Duration::from_secs(10 * 60);
const MAX_FAILURES_PER_ADDRESS: usize = 5;
const MAX_FAILURES_OVERALL: usize = 20;

/// Who may use the host and how a new client gets its token. The effective
/// values come from the environment when set (the env wins over the config
/// file); the `saved_*` copies are what the config file holds, so an env
/// token or password is never written to disk on the operator's behalf.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(super) struct HostAuth {
    /// The token every authenticated route requires; `None` leaves the host open.
    pub(super) token: Option<String>,
    pub(super) saved_token: Option<String>,
    pub(super) pairing_password: Option<String>,
    pub(super) saved_pairing_password: Option<String>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum AuthMode {
    None,
    Password,
    Token,
}

impl AuthMode {
    pub(super) fn as_str(self) -> &'static str {
        match self {
            Self::None => "none",
            Self::Password => "password",
            Self::Token => "token",
        }
    }
}

impl HostAuth {
    /// Startup auth from the environment and the config file. A pairing
    /// password on a host without a token generates one into `saved_token`,
    /// because pairing must hand the client a token; the bool reports that
    /// so the caller persists it before serving.
    pub(super) fn resolve(
        env_token: Option<String>,
        saved_token: Option<String>,
        env_password: Option<String>,
        saved_password: Option<String>,
    ) -> Result<(Self, bool), String> {
        let saved_token = non_blank(saved_token);
        let saved_pairing_password = non_empty(saved_password);
        let env_password = non_empty(env_password);
        if let Some(password) = &env_password {
            validate_pairing_password(password)
                .map_err(|err| format!("{PAIRING_PASSWORD_ENV}: {err}"))?;
        }
        if let Some(password) = &saved_pairing_password {
            validate_pairing_password(password)
                .map_err(|err| format!("pairingPassword in the host config: {err}"))?;
        }
        let mut auth = Self {
            token: non_blank(env_token).or_else(|| saved_token.clone()),
            saved_token,
            pairing_password: env_password.or_else(|| saved_pairing_password.clone()),
            saved_pairing_password,
        };
        let generated = auth.ensure_token_for_pairing()?;
        Ok((auth, generated))
    }

    pub(super) fn mode(&self) -> AuthMode {
        match (&self.token, &self.pairing_password) {
            (None, _) => AuthMode::None,
            (Some(_), Some(_)) => AuthMode::Password,
            (Some(_), None) => AuthMode::Token,
        }
    }

    pub(super) fn pairing_enabled(&self) -> bool {
        self.mode() == AuthMode::Password
    }

    /// Sets (or with `None` clears) the pairing password, as
    /// `POST /v1/config` and `--set-pairing-password` do. Returns whether a
    /// token had to be generated. The password must already be validated.
    pub(super) fn set_pairing_password(
        &mut self,
        password: Option<String>,
    ) -> Result<bool, String> {
        let password = non_empty(password);
        let mut next = self.clone();
        next.pairing_password = password.clone();
        next.saved_pairing_password = password;
        let generated = next.ensure_token_for_pairing()?;
        *self = next;
        Ok(generated)
    }

    fn ensure_token_for_pairing(&mut self) -> Result<bool, String> {
        if self.pairing_password.is_none() || self.token.is_some() {
            return Ok(false);
        }
        let token = generate_token()?;
        self.token = Some(token.clone());
        self.saved_token = Some(token);
        Ok(true)
    }
}

fn non_empty(value: Option<String>) -> Option<String> {
    value.filter(|value| !value.is_empty())
}

/// Tokens are compared as given, but a whitespace-only one means "no token",
/// matching how the host has always treated an empty `FAIRSPOKEN_HOST_TOKEN`.
fn non_blank(value: Option<String>) -> Option<String> {
    value.filter(|value| !value.trim().is_empty())
}

pub(super) fn validate_pairing_password(password: &str) -> Result<(), String> {
    let chars = password.chars().count();
    if !(MIN_PAIRING_PASSWORD_CHARS..=MAX_PAIRING_PASSWORD_CHARS).contains(&chars) {
        return Err(format!(
            "pairingPassword must be {MIN_PAIRING_PASSWORD_CHARS} to {MAX_PAIRING_PASSWORD_CHARS} characters"
        ));
    }
    Ok(())
}

/// 32 random bytes, base64url without padding.
pub(super) fn generate_token() -> Result<String, String> {
    let mut bytes = [0u8; 32];
    getrandom::fill(&mut bytes).map_err(|err| format!("Failed to generate a host token: {err}"))?;
    Ok(URL_SAFE_NO_PAD.encode(bytes))
}

/// Compares fixed-length digests so neither the content nor the length of
/// the stored password leaks through timing.
fn passwords_match(given: &str, expected: &str) -> bool {
    super::constant_time_eq(
        &Sha256::digest(given.as_bytes()),
        &Sha256::digest(expected.as_bytes()),
    )
}

/// `name` in `/v1/hello`: `FAIRSPOKEN_HOST_NAME`, else `name` in the host
/// config, else the machine's own name.
pub(super) fn resolve_host_name(env_name: Option<String>, saved_name: Option<&str>) -> String {
    let clean = |raw: &str| -> Option<String> {
        let name: String = raw
            .trim()
            .chars()
            .filter(|c| !c.is_control())
            .take(MAX_HOST_NAME_CHARS)
            .collect();
        Some(name.trim().to_string()).filter(|name| !name.is_empty())
    };
    env_name
        .as_deref()
        .and_then(clean)
        .or_else(|| saved_name.and_then(clean))
        .or_else(|| crate::machine::computer_name().as_deref().and_then(clean))
        .unwrap_or_else(|| FALLBACK_HOST_NAME.to_string())
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub(super) struct Hello<'a> {
    service: &'static str,
    protocol: u32,
    name: &'a str,
    server_version: &'static str,
    auth: &'static str,
}

impl<'a> Hello<'a> {
    pub(super) fn new(name: &'a str, server_version: &'static str, auth: AuthMode) -> Self {
        Self {
            service: HELLO_SERVICE,
            protocol: HELLO_PROTOCOL,
            name,
            server_version,
            auth: auth.as_str(),
        }
    }
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub(super) struct PairRequest {
    pub(super) password: String,
    #[serde(default)]
    pub(super) client_name: Option<String>,
}

impl PairRequest {
    /// The informational client name: trimmed, without control characters,
    /// at most 64 characters, `None` when nothing is left.
    pub(super) fn client_name(&self) -> Option<String> {
        let name: String = self
            .client_name
            .as_deref()?
            .trim()
            .chars()
            .filter(|c| !c.is_control())
            .take(MAX_CLIENT_NAME_CHARS)
            .collect();
        Some(name.trim().to_string()).filter(|name| !name.is_empty())
    }
}

#[derive(Debug, PartialEq, Eq)]
pub(super) enum PairOutcome {
    /// `token` is `None` on a host without one: there is nothing to pair.
    Paired {
        token: Option<String>,
    },
    WrongPassword,
    Disabled,
    RateLimited {
        retry_after_seconds: u64,
    },
}

/// Decides one `POST /v1/pair` attempt from `address`.
pub(super) fn attempt_pairing(
    auth: &HostAuth,
    limiter: &mut PairingLimiter,
    address: &str,
    password: &str,
    now: Instant,
) -> PairOutcome {
    let Some(token) = auth.token.as_ref() else {
        return PairOutcome::Paired { token: None };
    };
    let Some(expected) = auth.pairing_password.as_deref() else {
        return PairOutcome::Disabled;
    };
    if let Err(retry_after_seconds) = limiter.check(address, now) {
        return PairOutcome::RateLimited {
            retry_after_seconds,
        };
    }
    if passwords_match(password, expected) {
        PairOutcome::Paired {
            token: Some(token.clone()),
        }
    } else {
        limiter.record_failure(address, now);
        PairOutcome::WrongPassword
    }
}

/// Failed pairing attempts in the last 10 minutes, per client address and
/// overall (the overall cap bounds guessing from spoofed or many addresses).
/// In memory only; a restart forgets it.
#[derive(Default)]
pub(super) struct PairingLimiter {
    by_address: HashMap<String, VecDeque<Instant>>,
    overall: VecDeque<Instant>,
}

impl PairingLimiter {
    /// `Err(seconds)` until the window has room again for `address`. Blocked
    /// attempts aren't failures, so they never extend the wait.
    fn check(&mut self, address: &str, now: Instant) -> Result<(), u64> {
        self.prune(now);
        let wait_for = |failures: &VecDeque<Instant>, limit: usize| {
            (failures.len() >= limit).then(|| {
                let oldest_that_counts = failures[failures.len() - limit];
                RATE_WINDOW.saturating_sub(now.saturating_duration_since(oldest_that_counts))
            })
        };
        let wait = [
            self.by_address
                .get(address)
                .and_then(|failures| wait_for(failures, MAX_FAILURES_PER_ADDRESS)),
            wait_for(&self.overall, MAX_FAILURES_OVERALL),
        ]
        .into_iter()
        .flatten()
        .max();
        match wait {
            Some(wait) => Err(wait.as_secs_f64().ceil().max(1.0) as u64),
            None => Ok(()),
        }
    }

    fn record_failure(&mut self, address: &str, now: Instant) {
        self.by_address
            .entry(address.to_string())
            .or_default()
            .push_back(now);
        self.overall.push_back(now);
    }

    /// Drops failures older than the window, so the map holds at most the
    /// addresses that failed recently (no more than the overall cap).
    fn prune(&mut self, now: Instant) {
        let expired = |at: &Instant| now.saturating_duration_since(*at) >= RATE_WINDOW;
        while self.overall.front().is_some_and(expired) {
            self.overall.pop_front();
        }
        self.by_address.retain(|_, failures| {
            while failures.front().is_some_and(expired) {
                failures.pop_front();
            }
            !failures.is_empty()
        });
    }
}

/// `pairingPassword` in `POST /v1/config`: absent leaves pairing alone,
/// `null` or `""` turns it off, a string sets it.
pub(super) fn deserialize_password_change<'de, D>(
    deserializer: D,
) -> Result<Option<Option<String>>, D::Error>
where
    D: Deserializer<'de>,
{
    Option::<String>::deserialize(deserializer).map(Some)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn password_host() -> HostAuth {
        HostAuth::resolve(
            Some("the-token".to_string()),
            None,
            Some("123456".to_string()),
            None,
        )
        .unwrap()
        .0
    }

    #[test]
    fn auth_mode_follows_token_and_password() {
        assert_eq!(HostAuth::default().mode(), AuthMode::None);
        let token_only = HostAuth::resolve(Some("t".to_string()), None, None, None)
            .unwrap()
            .0;
        assert_eq!(token_only.mode(), AuthMode::Token);
        assert!(!token_only.pairing_enabled());
        assert_eq!(password_host().mode(), AuthMode::Password);
        assert!(password_host().pairing_enabled());
        // A blank token is no token, as before pairing existed.
        let blank = HostAuth::resolve(Some("  ".to_string()), None, None, None)
            .unwrap()
            .0;
        assert_eq!(blank.mode(), AuthMode::None);
    }

    #[test]
    fn env_wins_over_the_config_file_and_is_never_saved() {
        let (auth, generated) = HostAuth::resolve(
            Some("env-token".to_string()),
            Some("file-token".to_string()),
            Some("env-password".to_string()),
            Some("file-password".to_string()),
        )
        .unwrap();
        assert!(!generated);
        assert_eq!(auth.token.as_deref(), Some("env-token"));
        assert_eq!(auth.saved_token.as_deref(), Some("file-token"));
        assert_eq!(auth.pairing_password.as_deref(), Some("env-password"));
        assert_eq!(
            auth.saved_pairing_password.as_deref(),
            Some("file-password")
        );

        let (from_file, _) = HostAuth::resolve(
            None,
            Some("file-token".to_string()),
            Some(String::new()),
            Some("file-password".to_string()),
        )
        .unwrap();
        assert_eq!(from_file.token.as_deref(), Some("file-token"));
        assert_eq!(from_file.pairing_password.as_deref(), Some("file-password"));
    }

    #[test]
    fn a_password_without_a_token_generates_and_saves_one() {
        let (auth, generated) =
            HostAuth::resolve(None, None, Some("letmein".to_string()), None).unwrap();
        assert!(generated);
        let token = auth.token.clone().expect("generated token");
        assert_eq!(auth.saved_token.as_deref(), Some(token.as_str()));
        // 32 bytes of base64url without padding.
        assert_eq!(token.len(), 43);
        assert!(token
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_'));
        assert_ne!(generate_token().unwrap(), generate_token().unwrap());

        // Setting one at runtime does the same; clearing keeps the token.
        let mut open = HostAuth::default();
        assert!(open
            .set_pairing_password(Some("654321".to_string()))
            .unwrap());
        assert_eq!(open.mode(), AuthMode::Password);
        let token = open.token.clone();
        assert!(!open.set_pairing_password(None).unwrap());
        assert_eq!(open.mode(), AuthMode::Token);
        assert_eq!(open.token, token);
        assert_eq!(open.saved_pairing_password, None);
        // "" turns pairing off like null.
        let mut host = password_host();
        host.set_pairing_password(Some(String::new())).unwrap();
        assert!(!host.pairing_enabled());
    }

    #[test]
    fn password_validation_counts_characters() {
        assert!(validate_pairing_password("123456").is_ok());
        assert!(validate_pairing_password("12345").is_err());
        assert!(validate_pairing_password(&"x".repeat(128)).is_ok());
        assert!(validate_pairing_password(&"x".repeat(129)).is_err());
        // Six characters, more than six bytes.
        assert!(validate_pairing_password("éééééé").is_ok());
        let err = HostAuth::resolve(None, None, Some("short".to_string()), None).unwrap_err();
        assert!(err.contains(PAIRING_PASSWORD_ENV));
        assert!(
            !err.contains("short"),
            "the error must not echo the password"
        );
        assert!(HostAuth::resolve(None, None, None, Some("tiny".to_string())).is_err());
    }

    #[test]
    fn pairing_answers_each_host_state() {
        let mut limiter = PairingLimiter::default();
        let now = Instant::now();
        assert_eq!(
            attempt_pairing(&HostAuth::default(), &mut limiter, "a", "x", now),
            PairOutcome::Paired { token: None }
        );
        let token_only = HostAuth::resolve(Some("t".to_string()), None, None, None)
            .unwrap()
            .0;
        assert_eq!(
            attempt_pairing(&token_only, &mut limiter, "a", "x", now),
            PairOutcome::Disabled
        );
        let host = password_host();
        assert_eq!(
            attempt_pairing(&host, &mut limiter, "a", "123456", now),
            PairOutcome::Paired {
                token: Some("the-token".to_string())
            }
        );
        assert_eq!(
            attempt_pairing(&host, &mut limiter, "a", "1234567", now),
            PairOutcome::WrongPassword
        );
        assert!(passwords_match("same", "same"));
        assert!(!passwords_match("same", "Same"));
        assert!(!passwords_match("", "same"));
    }

    #[test]
    fn five_failures_block_an_address_even_for_the_right_password() {
        let host = password_host();
        let mut limiter = PairingLimiter::default();
        let start = Instant::now();
        for _ in 0..MAX_FAILURES_PER_ADDRESS {
            assert_eq!(
                attempt_pairing(&host, &mut limiter, "100.64.0.9", "nope!!", start),
                PairOutcome::WrongPassword
            );
        }
        let later = start + Duration::from_secs(60);
        assert_eq!(
            attempt_pairing(&host, &mut limiter, "100.64.0.9", "123456", later),
            PairOutcome::RateLimited {
                retry_after_seconds: 540
            }
        );
        // Another address is unaffected, and its success doesn't count.
        assert!(matches!(
            attempt_pairing(&host, &mut limiter, "100.64.0.10", "123456", later),
            PairOutcome::Paired { .. }
        ));
        // Once the window has room again the right password works.
        let after = start + RATE_WINDOW;
        assert!(matches!(
            attempt_pairing(&host, &mut limiter, "100.64.0.9", "123456", after),
            PairOutcome::Paired { .. }
        ));
        assert!(limiter.by_address.is_empty());
    }

    #[test]
    fn twenty_failures_across_addresses_block_everyone() {
        let host = password_host();
        let mut limiter = PairingLimiter::default();
        let start = Instant::now();
        for i in 0..MAX_FAILURES_OVERALL {
            let at = start + Duration::from_secs(i as u64);
            assert_eq!(
                attempt_pairing(&host, &mut limiter, &format!("10.0.0.{i}"), "wrong!", at),
                PairOutcome::WrongPassword
            );
        }
        let now = start + Duration::from_secs(30);
        assert_eq!(
            attempt_pairing(&host, &mut limiter, "10.9.9.9", "123456", now),
            PairOutcome::RateLimited {
                retry_after_seconds: 570
            }
        );
        // The oldest failure expiring makes room for one more attempt.
        assert!(matches!(
            attempt_pairing(
                &host,
                &mut limiter,
                "10.9.9.9",
                "123456",
                start + RATE_WINDOW
            ),
            PairOutcome::Paired { .. }
        ));
    }

    #[test]
    fn pair_requests_reject_unknown_fields_and_tidy_the_client_name() {
        let parsed: PairRequest = serde_json::from_str(
            r#"{"password":"123456","clientName":"  Conal's MacBook\u0007  "}"#,
        )
        .unwrap();
        assert_eq!(parsed.client_name().as_deref(), Some("Conal's MacBook"));
        let long: PairRequest = serde_json::from_value(serde_json::json!({
            "password": "123456", "clientName": "x".repeat(100)
        }))
        .unwrap();
        assert_eq!(long.client_name().unwrap().len(), MAX_CLIENT_NAME_CHARS);
        let blank: PairRequest =
            serde_json::from_str(r#"{"password":"123456","clientName":"   "}"#).unwrap();
        assert_eq!(blank.client_name(), None);
        assert!(serde_json::from_str::<PairRequest>(r#"{"password":"1","token":"x"}"#).is_err());
        assert!(serde_json::from_str::<PairRequest>(r#"{"clientName":"x"}"#).is_err());
    }

    #[test]
    fn hello_has_exactly_the_protocol_fields() {
        let hello =
            serde_json::to_value(Hello::new("Studio Mac", "0.4.0", AuthMode::Password)).unwrap();
        assert_eq!(
            hello,
            serde_json::json!({"service": "fairspoken-host", "protocol": 1,
                "name": "Studio Mac", "serverVersion": "0.4.0", "auth": "password"})
        );
    }

    #[test]
    fn host_name_prefers_env_then_config_then_the_machine() {
        assert_eq!(
            resolve_host_name(Some(" Studio Mac ".to_string()), Some("Saved")),
            "Studio Mac"
        );
        assert_eq!(
            resolve_host_name(Some("  ".to_string()), Some("Saved")),
            "Saved"
        );
        let fallback = resolve_host_name(None, Some(""));
        assert!(!fallback.is_empty());
        assert!(fallback.chars().count() <= MAX_HOST_NAME_CHARS);
    }
}
