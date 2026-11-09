//! Turns `Config` plus the standard `OTEL_*` environment into typed settings.
//! Explicit fields win over the environment. Everything is validated here so
//! the rest of the crate trusts its inputs.

use std::time::Duration;

use crate::export::{Compression, DEFAULT_MAX_REQUEST_BYTES, ExportSettings, PROFILES_PATH, Retry};

/// A configuration value that cannot be used.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ConfigError(pub(crate) String);

impl std::fmt::Display for ConfigError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

impl std::error::Error for ConfigError {}

/// Reads one environment variable; empty values count as unset.
pub(crate) type Env<'a> = &'a dyn Fn(&str) -> Option<String>;

pub(crate) fn process_env(name: &str) -> Option<String> {
    std::env::var(name).ok().filter(|v| !v.trim().is_empty())
}

/// The first of `names` that is set.
fn first(env: Env<'_>, names: &[&str]) -> Option<String> {
    names.iter().find_map(|name| env(name))
}

pub(crate) fn export_settings(
    endpoint: &str,
    headers: &[(String, String)],
    env: Env<'_>,
) -> Result<ExportSettings, ConfigError> {
    let url = if !endpoint.trim().is_empty() {
        profiles_url(endpoint, false)?
    } else if let Some(full) = env("OTEL_EXPORTER_OTLP_PROFILES_ENDPOINT") {
        profiles_url(&full, false)?
    } else if let Some(base) = env("OTEL_EXPORTER_OTLP_ENDPOINT") {
        profiles_url(&base, true)?
    } else {
        format!("http://localhost:4318{PROFILES_PATH}")
    };

    let mut merged = match first(
        env,
        &[
            "OTEL_EXPORTER_OTLP_PROFILES_HEADERS",
            "OTEL_EXPORTER_OTLP_HEADERS",
        ],
    ) {
        Some(raw) => parse_headers(&raw)?,
        None => Vec::new(),
    };
    for (key, value) in headers {
        merged.retain(|(k, _)| !k.eq_ignore_ascii_case(key));
        merged.push((key.clone(), value.clone()));
    }

    let timeout = match first(
        env,
        &[
            "OTEL_EXPORTER_OTLP_PROFILES_TIMEOUT",
            "OTEL_EXPORTER_OTLP_TIMEOUT",
        ],
    ) {
        Some(ms) => match ms.trim().parse::<u64>() {
            Ok(ms) if ms > 0 => Duration::from_millis(ms),
            _ => {
                return Err(ConfigError(format!(
                    "OTLP timeout must be milliseconds > 0, got {ms:?}"
                )));
            }
        },
        None => Duration::from_secs(10),
    };

    let compression = match first(
        env,
        &[
            "OTEL_EXPORTER_OTLP_PROFILES_COMPRESSION",
            "OTEL_EXPORTER_OTLP_COMPRESSION",
        ],
    ) {
        None => Compression::None,
        Some(v) if v.trim().eq_ignore_ascii_case("gzip") => Compression::Gzip,
        Some(v) if v.trim().eq_ignore_ascii_case("none") => Compression::None,
        Some(v) => {
            return Err(ConfigError(format!(
                "OTLP compression must be gzip or none, got {v:?}"
            )));
        }
    };

    Ok(ExportSettings {
        url,
        headers: merged,
        timeout,
        compression,
        retry: Retry::default(),
        max_request_bytes: DEFAULT_MAX_REQUEST_BYTES,
    })
}

/// A full profiles URL from a base URL (`append` always adds the profiles
/// path, as for `OTEL_EXPORTER_OTLP_ENDPOINT`) or from a URL that may already
/// carry its path.
fn profiles_url(raw: &str, append: bool) -> Result<String, ConfigError> {
    let raw = raw.trim();
    let rest = raw
        .strip_prefix("http://")
        .or_else(|| raw.strip_prefix("https://"))
        .ok_or_else(|| {
            ConfigError(format!(
                "OTLP endpoint must start with http:// or https://, got {raw:?}"
            ))
        })?;
    let (host, path) = rest.split_at(rest.find('/').unwrap_or(rest.len()));
    if host.is_empty() {
        return Err(ConfigError(format!("OTLP endpoint has no host: {raw:?}")));
    }
    let scheme = &raw[..raw.len() - rest.len()];
    let path = path.trim_end_matches('/');
    Ok(if append || path.is_empty() {
        format!("{scheme}{host}{path}{PROFILES_PATH}")
    } else {
        format!("{scheme}{host}{path}")
    })
}

/// `key=value,key2=value2`, with percent-encoded keys and values.
pub(crate) fn parse_headers(raw: &str) -> Result<Vec<(String, String)>, ConfigError> {
    parse_pairs(raw, "OTLP header")
}

pub(crate) fn parse_pairs(raw: &str, what: &str) -> Result<Vec<(String, String)>, ConfigError> {
    raw.split(',')
        .map(str::trim)
        .filter(|part| !part.is_empty())
        .map(|part| {
            let bad = || ConfigError(format!("invalid {what} {part:?}"));
            let (key, value) = part.split_once('=').ok_or_else(bad)?;
            let key = percent_decode(key.trim()).ok_or_else(bad)?;
            let value = percent_decode(value.trim()).ok_or_else(bad)?;
            if key.is_empty() {
                return Err(bad());
            }
            Ok((key, value))
        })
        .collect()
}

fn percent_decode(s: &str) -> Option<String> {
    let bytes = s.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'%' {
            let hex = s.get(i + 1..i + 3)?;
            out.push(u8::from_str_radix(hex, 16).ok()?);
            i += 3;
        } else {
            out.push(bytes[i]);
            i += 1;
        }
    }
    String::from_utf8(out).ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn env_of(pairs: &'static [(&'static str, &'static str)]) -> impl Fn(&str) -> Option<String> {
        move |name| {
            pairs
                .iter()
                .find(|(k, _)| *k == name)
                .map(|(_, v)| v.to_string())
        }
    }

    fn url(endpoint: &str, env: &'static [(&'static str, &'static str)]) -> String {
        export_settings(endpoint, &[], &env_of(env)).unwrap().url
    }

    #[test]
    fn the_endpoint_comes_from_config_then_profiles_env_then_base_env() {
        assert_eq!(url("", &[]), "http://localhost:4318/v1development/profiles");
        assert_eq!(
            url("http://c:4318", &[]),
            "http://c:4318/v1development/profiles"
        );
        assert_eq!(url("https://c/custom/path/", &[]), "https://c/custom/path");
        assert_eq!(
            url(
                "",
                &[("OTEL_EXPORTER_OTLP_ENDPOINT", "http://base:4318/otlp/")]
            ),
            "http://base:4318/otlp/v1development/profiles"
        );
        let both: &[(&str, &str)] = &[
            ("OTEL_EXPORTER_OTLP_ENDPOINT", "http://base:4318"),
            ("OTEL_EXPORTER_OTLP_PROFILES_ENDPOINT", "http://full:9999/p"),
        ];
        assert_eq!(url("", both), "http://full:9999/p");
        assert_eq!(
            url("http://explicit", both),
            "http://explicit/v1development/profiles"
        );
    }

    #[test]
    fn explicit_headers_override_env_headers() {
        let env = env_of(&[(
            "OTEL_EXPORTER_OTLP_HEADERS",
            "authorization=Bearer%20a,x-team=core",
        )]);
        let settings =
            export_settings("", &[("Authorization".into(), "Bearer b".into())], &env).unwrap();
        assert_eq!(
            settings.headers,
            [
                ("x-team".to_string(), "core".to_string()),
                ("Authorization".into(), "Bearer b".into())
            ]
        );
    }

    #[test]
    fn timeout_and_compression_follow_the_otlp_spec_names() {
        let env = env_of(&[
            ("OTEL_EXPORTER_OTLP_PROFILES_TIMEOUT", "2500"),
            ("OTEL_EXPORTER_OTLP_COMPRESSION", "gzip"),
        ]);
        let settings = export_settings("", &[], &env).unwrap();
        assert_eq!(settings.timeout, Duration::from_millis(2500));
        assert_eq!(settings.compression, Compression::Gzip);
        let defaults = export_settings("", &[], &env_of(&[])).unwrap();
        assert_eq!(defaults.timeout, Duration::from_secs(10));
        assert_eq!(defaults.compression, Compression::None);
    }

    #[test]
    fn bad_values_are_errors_not_silent_defaults() {
        for env in [
            env_of(&[("OTEL_EXPORTER_OTLP_ENDPOINT", "localhost:4318")]),
            env_of(&[("OTEL_EXPORTER_OTLP_TIMEOUT", "10s")]),
            env_of(&[("OTEL_EXPORTER_OTLP_COMPRESSION", "zstd")]),
            env_of(&[("OTEL_EXPORTER_OTLP_HEADERS", "novalue")]),
        ] {
            assert!(export_settings("", &[], &env).is_err());
        }
    }
}
