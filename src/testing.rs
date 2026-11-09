//! Test oracle: decode a request with the upstream `opentelemetry-proto`
//! types and resolve every index, the way a receiver would.

use std::fmt::Write as _;

use opentelemetry_proto::tonic::collector::profiles::v1development::ExportProfilesServiceRequest;
use opentelemetry_proto::tonic::common::v1::{AnyValue, any_value};
use prost::Message;

#[derive(Debug, Clone, PartialEq)]
pub(crate) struct Decoded {
    pub resource: Vec<(String, String)>,
    pub scope: (String, String),
    pub profiles: Vec<DecodedProfile>,
}

#[derive(Debug, Clone, PartialEq)]
pub(crate) struct DecodedProfile {
    pub sample_type: (String, String),
    pub period_type: (String, String),
    pub period: i64,
    pub time_unix_nano: u64,
    pub duration_nano: u64,
    pub samples: Vec<DecodedSample>,
}

#[derive(Debug, Clone, PartialEq)]
pub(crate) struct DecodedSample {
    /// Leaf first, `function file:line`.
    pub frames: Vec<String>,
    pub value: i64,
    pub attributes: Vec<(String, String)>,
    /// Hex `trace_id/span_id`.
    pub link: Option<String>,
}

fn show(value: &Option<AnyValue>) -> String {
    match value.as_ref().and_then(|v| v.value.as_ref()) {
        Some(any_value::Value::StringValue(s)) => s.clone(),
        Some(any_value::Value::IntValue(i)) => i.to_string(),
        Some(any_value::Value::BoolValue(b)) => b.to_string(),
        other => format!("{other:?}"),
    }
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().fold(String::new(), |mut s, b| {
        let _ = write!(s, "{b:02x}");
        s
    })
}

pub(crate) fn decode(body: &[u8]) -> Decoded {
    let request = ExportProfilesServiceRequest::decode(body).expect("valid protobuf");
    let dict = request.dictionary.expect("dictionary");
    let s = |i: i32| dict.string_table[i as usize].clone();
    assert_eq!(request.resource_profiles.len(), 1);
    let rp = &request.resource_profiles[0];
    let resource = rp
        .resource
        .as_ref()
        .map(|r| {
            r.attributes
                .iter()
                .map(|kv| (kv.key.clone(), show(&kv.value)))
                .collect()
        })
        .unwrap_or_default();
    assert_eq!(rp.scope_profiles.len(), 1);
    let sp = &rp.scope_profiles[0];
    let scope = sp
        .scope
        .as_ref()
        .map(|s| (s.name.clone(), s.version.clone()))
        .unwrap_or_default();
    let profiles = sp
        .profiles
        .iter()
        .map(|p| {
            let vt =
                |v: &Option<opentelemetry_proto::tonic::profiles::v1development::ValueType>| {
                    let v = v.expect("value type");
                    (s(v.type_strindex), s(v.unit_strindex))
                };
            DecodedProfile {
                sample_type: vt(&p.sample_type),
                period_type: vt(&p.period_type),
                period: p.period,
                time_unix_nano: p.time_unix_nano,
                duration_nano: p.duration_nano,
                samples: p
                    .samples
                    .iter()
                    .map(|sample| {
                        let stack = &dict.stack_table[sample.stack_index as usize];
                        let frames = stack
                            .location_indices
                            .iter()
                            .flat_map(|&l| dict.location_table[l as usize].lines.iter())
                            .map(|line| {
                                let f = &dict.function_table[line.function_index as usize];
                                format!(
                                    "{} {}:{}",
                                    s(f.name_strindex),
                                    s(f.filename_strindex),
                                    line.line
                                )
                            })
                            .collect();
                        let attributes = sample
                            .attribute_indices
                            .iter()
                            .map(|&a| {
                                let kv = &dict.attribute_table[a as usize];
                                (s(kv.key_strindex), show(&kv.value))
                            })
                            .collect();
                        let link = (sample.link_index != 0).then(|| {
                            let l = &dict.link_table[sample.link_index as usize];
                            format!("{}/{}", hex(&l.trace_id), hex(&l.span_id))
                        });
                        assert_eq!(sample.values.len(), 1, "one value per sample");
                        DecodedSample {
                            frames,
                            value: sample.values[0],
                            attributes,
                            link,
                        }
                    })
                    .collect(),
            }
        })
        .collect();
    Decoded {
        resource,
        scope,
        profiles,
    }
}

/// A stable text rendering for golden files.
pub(crate) fn render(decoded: &Decoded) -> String {
    let mut out = String::new();
    let _ = writeln!(out, "resource {:?}", decoded.resource);
    let _ = writeln!(out, "scope {:?}", decoded.scope);
    for p in &decoded.profiles {
        let _ = writeln!(
            out,
            "profile {}/{} period {} {}/{} at {} for {}",
            p.sample_type.0,
            p.sample_type.1,
            p.period,
            p.period_type.0,
            p.period_type.1,
            p.time_unix_nano,
            p.duration_nano
        );
        for sample in &p.samples {
            let _ = writeln!(
                out,
                "  {} attrs={:?} link={}",
                sample.value,
                sample.attributes,
                sample.link.as_deref().unwrap_or("-")
            );
            for frame in &sample.frames {
                let _ = writeln!(out, "    {frame}");
            }
        }
    }
    out
}

/// Runs `profcheck` with every check on. Returns `None` when it is not
/// installed, unless `OTELPROFILE_REQUIRE_PROFCHECK` is set (CI), which turns
/// a missing binary into a failure.
pub(crate) fn run_profcheck(body: &[u8]) -> Option<Result<(), String>> {
    let binary = std::env::var("PROFCHECK").unwrap_or_else(|_| "profcheck".into());
    let file = tempfile_path("pb");
    std::fs::write(&file, body).unwrap();
    let output = std::process::Command::new(&binary)
        .args(["-check-dupes", "-check-orphans", "-check-sample-shapes"])
        .arg(&file)
        .output();
    let _ = std::fs::remove_file(&file);
    match output {
        Ok(output) if output.status.success() => Some(Ok(())),
        Ok(output) => Some(Err(format!(
            "{}{}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        ))),
        Err(error) if std::env::var_os("OTELPROFILE_REQUIRE_PROFCHECK").is_some() => {
            panic!("profcheck is required but could not run ({binary}): {error}")
        }
        Err(error) => {
            eprintln!("skipping profcheck ({binary}: {error})");
            None
        }
    }
}

pub(crate) fn profcheck(body: &[u8]) {
    if let Some(Err(report)) = run_profcheck(body) {
        panic!("profcheck failed:\n{report}");
    }
}

fn tempfile_path(extension: &str) -> std::path::PathBuf {
    use std::sync::atomic::{AtomicUsize, Ordering};
    static NEXT: AtomicUsize = AtomicUsize::new(0);
    std::env::temp_dir().join(format!(
        "otelprofile-test-{}-{}.{extension}",
        std::process::id(),
        NEXT.fetch_add(1, Ordering::Relaxed)
    ))
}
