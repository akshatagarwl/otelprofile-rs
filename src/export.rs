//! OTLP/HTTP export: size-capped requests, optional gzip, retries that honor
//! `Retry-After`, and partial-success reporting.

use std::fmt;
use std::io::Write as _;
use std::time::{Duration, Instant, SystemTime};

use prost::Message;

use crate::encode::encode;
use crate::model::{Origin, Profile};
use crate::proto;

/// The OTLP/HTTP path for profiles while the signal is in development.
pub(crate) const PROFILES_PATH: &str = "/v1development/profiles";
/// Each uncompressed request stays under common receiver limits (the
/// Collector and sigiro default to 8 MiB).
pub(crate) const DEFAULT_MAX_REQUEST_BYTES: usize = 8 << 20;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Compression {
    None,
    Gzip,
}

/// Retries of 429, 502, 503, 504 and transport errors.
#[derive(Debug, Clone, Copy)]
pub(crate) struct Retry {
    pub initial: Duration,
    pub max_interval: Duration,
    /// No retry starts after this much time since the first attempt.
    pub max_elapsed: Duration,
}

impl Default for Retry {
    /// Gives up within 10 s, so an unreachable endpoint is reported inside
    /// one 15 s interval.
    fn default() -> Self {
        Self {
            initial: Duration::from_secs(1),
            max_interval: Duration::from_secs(5),
            max_elapsed: Duration::from_secs(10),
        }
    }
}

#[derive(Debug, Clone)]
pub(crate) struct ExportSettings {
    pub url: String,
    pub headers: Vec<(String, String)>,
    pub timeout: Duration,
    pub compression: Compression,
    pub retry: Retry,
    pub max_request_bytes: usize,
}

/// Why (part of) an export did not land.
#[derive(Debug)]
pub(crate) enum ExportError {
    Transport(String),
    Status {
        code: u16,
        body: String,
    },
    PartialSuccess {
        rejected: i64,
        message: String,
    },
    TooLarge {
        kind: &'static str,
        bytes: usize,
        cap: usize,
    },
    GaveUp {
        attempts: u32,
        last: Box<ExportError>,
    },
}

impl fmt::Display for ExportError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            ExportError::Transport(e) => write!(f, "transport error: {e}"),
            ExportError::Status { code, body } => write!(f, "HTTP {code}: {body}"),
            ExportError::PartialSuccess { rejected, message } => {
                write!(
                    f,
                    "partial success: {rejected} profiles rejected: {message}"
                )
            }
            ExportError::TooLarge { kind, bytes, cap } => write!(
                f,
                "dropped a {kind} profile: {bytes} bytes exceeds the {cap}-byte request cap"
            ),
            ExportError::GaveUp { attempts, last } => {
                write!(f, "gave up after {attempts} attempts: {last}")
            }
        }
    }
}

enum Attempt {
    Done(Result<(), ExportError>),
    Retry {
        error: ExportError,
        after: Option<Duration>,
    },
}

pub(crate) struct Exporter {
    settings: ExportSettings,
    agent: ureq::Agent,
    user_agent: String,
}

impl Exporter {
    pub(crate) fn new(settings: ExportSettings) -> Self {
        let agent = ureq::Agent::config_builder()
            .timeout_global(Some(settings.timeout))
            .http_status_as_error(false)
            .build()
            .into();
        Self {
            settings,
            agent,
            user_agent: format!("otelprofile-rust/{}", env!("CARGO_PKG_VERSION")),
        }
    }

    pub(crate) fn url(&self) -> &str {
        &self.settings.url
    }

    /// Sends `profiles` in as few requests as the size cap allows and
    /// returns every error; nothing here panics or blocks past the retry
    /// budget.
    pub(crate) fn export(&self, origin: &Origin, profiles: &[Profile]) -> Vec<ExportError> {
        let mut errors = Vec::new();
        for body in requests(
            origin,
            profiles,
            self.settings.max_request_bytes,
            &mut errors,
        ) {
            if let Err(error) = self.send(&body) {
                errors.push(error);
            }
        }
        errors
    }

    fn send(&self, raw: &[u8]) -> Result<(), ExportError> {
        let body = match self.settings.compression {
            Compression::None => raw.to_vec(),
            Compression::Gzip => gzip(raw),
        };
        let started = Instant::now();
        let mut interval = self.settings.retry.initial;
        let mut jitter = Jitter::new();
        for attempt in 1.. {
            let (error, after) = match self.attempt(&body) {
                Attempt::Done(result) => return result,
                Attempt::Retry { error, after } => (error, after),
            };
            let wait = after.unwrap_or_else(|| {
                let wait = jitter.around(interval);
                interval = (interval * 2).min(self.settings.retry.max_interval);
                wait
            });
            if started.elapsed() + wait > self.settings.retry.max_elapsed {
                return Err(ExportError::GaveUp {
                    attempts: attempt,
                    last: Box::new(error),
                });
            }
            std::thread::sleep(wait);
        }
        unreachable!("the retry loop only exits by returning")
    }

    fn attempt(&self, body: &[u8]) -> Attempt {
        let mut request = self
            .agent
            .post(&self.settings.url)
            .header("Content-Type", "application/x-protobuf")
            .header("User-Agent", &self.user_agent);
        if self.settings.compression == Compression::Gzip {
            request = request.header("Content-Encoding", "gzip");
        }
        for (key, value) in &self.settings.headers {
            request = request.header(key, value);
        }
        let mut response = match request.send(body) {
            Ok(response) => response,
            Err(error) => {
                return Attempt::Retry {
                    error: ExportError::Transport(error.to_string()),
                    after: None,
                };
            }
        };
        let code = response.status().as_u16();
        let after = response
            .headers()
            .get("retry-after")
            .and_then(|v| v.to_str().ok())
            .and_then(retry_after);
        let bytes = response
            .body_mut()
            .with_config()
            .limit(64 << 10)
            .read_to_vec()
            .unwrap_or_default();
        match code {
            200..=299 => Attempt::Done(partial_success(&bytes)),
            429 | 502 | 503 | 504 => Attempt::Retry {
                error: status_error(code, &bytes),
                after,
            },
            _ => Attempt::Done(Err(status_error(code, &bytes))),
        }
    }
}

/// Encodes `profiles` into request bodies of at most `cap` bytes, splitting
/// the batch in halves until each part fits. A single profile over the cap is
/// dropped and reported.
pub(crate) fn requests(
    origin: &Origin,
    profiles: &[Profile],
    cap: usize,
    errors: &mut Vec<ExportError>,
) -> Vec<Vec<u8>> {
    let mut out = Vec::new();
    let mut pending = vec![profiles];
    while let Some(batch) = pending.pop() {
        if batch.is_empty() {
            continue;
        }
        let body = encode(origin, batch);
        if body.len() <= cap {
            out.push(body);
        } else if let [profile] = batch {
            errors.push(ExportError::TooLarge {
                kind: profile.kind.spec().sample.0,
                bytes: body.len(),
                cap,
            });
        } else {
            let (first, second) = batch.split_at(batch.len() / 2);
            pending.push(second);
            pending.push(first);
        }
    }
    out
}

fn gzip(raw: &[u8]) -> Vec<u8> {
    let mut encoder = flate2::write::GzEncoder::new(
        Vec::with_capacity(raw.len() / 3),
        flate2::Compression::default(),
    );
    encoder
        .write_all(raw)
        .expect("writing to a Vec cannot fail");
    encoder.finish().expect("writing to a Vec cannot fail")
}

/// Reads `ExportProfilesServiceResponse.partial_success` from a 2xx body.
/// A body that is not protobuf (some receivers answer with text) is success.
fn partial_success(body: &[u8]) -> Result<(), ExportError> {
    let Ok(response) = proto::ExportProfilesServiceResponse::decode(body) else {
        return Ok(());
    };
    match response.partial_success {
        Some(p) if p.rejected_profiles > 0 || !p.error_message.is_empty() => {
            Err(ExportError::PartialSuccess {
                rejected: p.rejected_profiles,
                message: p.error_message,
            })
        }
        _ => Ok(()),
    }
}

fn status_error(code: u16, body: &[u8]) -> ExportError {
    let mut body = String::from_utf8_lossy(body).trim().to_string();
    if body.len() > 512 {
        let cut = (0..=512)
            .rev()
            .find(|&i| body.is_char_boundary(i))
            .unwrap_or(0);
        body.truncate(cut);
        body.push('…');
    }
    ExportError::Status { code, body }
}

/// `Retry-After` as delay-seconds or an HTTP date.
fn retry_after(value: &str) -> Option<Duration> {
    let value = value.trim();
    if let Ok(seconds) = value.parse::<u64>() {
        return Some(Duration::from_secs(seconds));
    }
    let at = httpdate::parse_http_date(value).ok()?;
    Some(at.duration_since(SystemTime::now()).unwrap_or_default())
}

/// Full jitter: a wait uniformly between half and all of the interval.
struct Jitter(u64);

impl Jitter {
    fn new() -> Self {
        let seed = SystemTime::now()
            .duration_since(SystemTime::UNIX_EPOCH)
            .map_or(0, |d| d.subsec_nanos() as u64);
        Self(seed | 1)
    }

    fn around(&mut self, interval: Duration) -> Duration {
        self.0 ^= self.0 << 13;
        self.0 ^= self.0 >> 7;
        self.0 ^= self.0 << 17;
        let half = interval / 2;
        half + half.mul_f64((self.0 % 1_000_000) as f64 / 1_000_000.0)
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use super::*;
    use crate::model::{Frame, Kind, Sample};
    use crate::testing::{MockReceiver, decode};

    fn settings(url: &str) -> ExportSettings {
        ExportSettings {
            url: url.into(),
            headers: vec![("authorization".into(), "Bearer t".into())],
            timeout: Duration::from_secs(2),
            compression: Compression::Gzip,
            retry: Retry {
                initial: Duration::from_millis(50),
                max_interval: Duration::from_millis(100),
                max_elapsed: Duration::from_secs(3),
            },
            max_request_bytes: DEFAULT_MAX_REQUEST_BYTES,
        }
    }

    fn origin() -> Origin {
        Origin {
            resource: vec![("service.name".into(), "svc".into())],
            scope_name: "otelprofile".into(),
            scope_version: "0".into(),
        }
    }

    fn profile(kind: Kind, functions: usize) -> Profile {
        let samples = (0..functions)
            .map(|i| Sample {
                stack: Arc::from([Frame {
                    function: format!("app::{kind:?}_function_number_{i}").into(),
                    file: "src/lib.rs".into(),
                    line: 1,
                }]),
                value: 1,
                labels: Arc::from([]),
                link: None,
            })
            .collect();
        Profile {
            kind,
            time_unix_nano: 1,
            duration_nano: 1,
            period: 1,
            samples,
        }
    }

    #[test]
    fn a_request_arrives_gzipped_with_headers_and_decodes() {
        let receiver = MockReceiver::start(vec![]);
        let errors =
            Exporter::new(settings(&receiver.url)).export(&origin(), &[profile(Kind::Cpu, 3)]);
        assert!(errors.is_empty(), "{errors:?}");
        let requests = receiver.requests();
        assert_eq!(requests.len(), 1);
        let request = &requests[0];
        assert_eq!(
            request.header("content-type"),
            Some("application/x-protobuf")
        );
        assert_eq!(request.header("content-encoding"), Some("gzip"));
        assert_eq!(request.header("authorization"), Some("Bearer t"));
        assert!(
            request
                .header("user-agent")
                .unwrap()
                .starts_with("otelprofile-rust/")
        );
        assert_eq!(decode(&request.body).profiles[0].samples.len(), 3);
    }

    #[test]
    fn retryable_statuses_wait_for_retry_after_then_succeed() {
        let receiver = MockReceiver::start(vec![(503, vec![("Retry-After", "1".into())], vec![])]);
        let errors =
            Exporter::new(settings(&receiver.url)).export(&origin(), &[profile(Kind::Cpu, 1)]);
        assert!(errors.is_empty(), "{errors:?}");
        let requests = receiver.requests();
        assert_eq!(requests.len(), 2);
        assert!(requests[1].at - requests[0].at >= Duration::from_secs(1));
    }

    #[test]
    fn client_errors_are_reported_without_retrying() {
        let receiver = MockReceiver::start(vec![(400, vec![], b"bad profile".to_vec())]);
        let errors =
            Exporter::new(settings(&receiver.url)).export(&origin(), &[profile(Kind::Cpu, 1)]);
        assert_eq!(receiver.requests().len(), 1);
        assert!(
            matches!(&errors[..], [ExportError::Status { code: 400, body }] if body == "bad profile")
        );
    }

    #[test]
    fn a_partial_success_is_reported() {
        let reply = proto::ExportProfilesServiceResponse {
            partial_success: Some(proto::ExportProfilesPartialSuccess {
                rejected_profiles: 2,
                error_message: "unknown sample type".into(),
            }),
        }
        .encode_to_vec();
        let receiver = MockReceiver::start(vec![(200, vec![], reply)]);
        let errors =
            Exporter::new(settings(&receiver.url)).export(&origin(), &[profile(Kind::Cpu, 1)]);
        assert!(matches!(
            &errors[..],
            [ExportError::PartialSuccess { rejected: 2, .. }]
        ));
    }

    #[test]
    fn an_unreachable_endpoint_gives_up_within_the_retry_budget() {
        let mut s = settings("http://127.0.0.1:9/v1development/profiles");
        s.retry.max_elapsed = Duration::from_millis(400);
        let started = Instant::now();
        let errors = Exporter::new(s).export(&origin(), &[profile(Kind::Cpu, 1)]);
        assert!(started.elapsed() < Duration::from_secs(2));
        assert!(matches!(&errors[..], [ExportError::GaveUp { attempts, .. }] if *attempts >= 2));
    }

    #[test]
    fn batches_over_the_cap_are_split_and_oversized_profiles_dropped() {
        let receiver = MockReceiver::start(vec![]);
        let small = [profile(Kind::Cpu, 40), profile(Kind::Samples, 40)];
        let one = encode(&origin(), &small[..1]).len();
        let mut s = settings(&receiver.url);
        s.max_request_bytes = one + one / 2;
        let exporter = Exporter::new(s);
        assert!(exporter.export(&origin(), &small).is_empty());
        let kinds: Vec<_> = receiver
            .requests()
            .iter()
            .map(|r| {
                decode(&r.body)
                    .profiles
                    .iter()
                    .map(|p| p.sample_type.0.clone())
                    .collect::<Vec<_>>()
            })
            .collect();
        assert_eq!(
            kinds,
            [vec!["cpu".to_string()], vec!["samples".to_string()]]
        );

        let errors = exporter.export(&origin(), &[profile(Kind::InuseSpace, 400)]);
        assert!(matches!(
            &errors[..],
            [ExportError::TooLarge {
                kind: "inuse_space",
                ..
            }]
        ));
        assert_eq!(
            receiver.requests().len(),
            2,
            "the oversized profile is not sent"
        );
    }

    #[test]
    fn retry_after_accepts_seconds_and_http_dates() {
        assert_eq!(retry_after("7"), Some(Duration::from_secs(7)));
        let later = httpdate::fmt_http_date(SystemTime::now() + Duration::from_secs(30));
        let wait = retry_after(&later).unwrap();
        assert!(wait > Duration::from_secs(28) && wait <= Duration::from_secs(30));
        assert_eq!(retry_after("soon"), None);
    }
}
