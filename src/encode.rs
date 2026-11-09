//! Profiles to one `ExportProfilesServiceRequest` with one deduplicated
//! dictionary shared by every profile in the request.

use std::collections::HashMap;
use std::sync::Arc;

use prost::Message;

use crate::model::{AttributeValue, Frame, Origin, Profile, SpanLink};
use crate::proto::{self, any_value};

/// Encode `profiles` from `origin` as protobuf bytes.
pub(crate) fn encode(origin: &Origin, profiles: &[Profile]) -> Vec<u8> {
    let mut dict = Dictionary::new();
    let profiles = profiles
        .iter()
        .map(|profile| dict.profile(profile))
        .collect();
    proto::ExportProfilesServiceRequest {
        resource_profiles: vec![proto::ResourceProfiles {
            resource: Some(proto::Resource {
                attributes: origin
                    .resource
                    .iter()
                    .map(|(key, value)| proto::KeyValue {
                        key: key.clone(),
                        value: Some(any_value(value)),
                    })
                    .collect(),
            }),
            scope_profiles: vec![proto::ScopeProfiles {
                scope: Some(proto::InstrumentationScope {
                    name: origin.scope_name.clone(),
                    version: origin.scope_version.clone(),
                }),
                profiles,
            }],
        }],
        dictionary: Some(dict.tables),
    }
    .encode_to_vec()
}

fn any_value(value: &AttributeValue) -> proto::AnyValue {
    proto::AnyValue {
        value: Some(match value {
            AttributeValue::String(s) => any_value::Value::String(s.clone()),
            AttributeValue::Int(i) => any_value::Value::Int(*i),
            AttributeValue::Bool(b) => any_value::Value::Bool(*b),
        }),
    }
}

/// The dictionary tables plus an index per table, so each distinct entry is
/// written once. Index 0 of every table is the zero value, as the spec
/// requires.
struct Dictionary {
    tables: proto::ProfilesDictionary,
    strings: HashMap<Arc<str>, i32>,
    functions: HashMap<(i32, i32), i32>,
    locations: HashMap<(i32, u32), i32>,
    stacks: HashMap<Vec<i32>, i32>,
    attributes: HashMap<(i32, Arc<str>), i32>,
    links: HashMap<SpanLink, i32>,
    stack_scratch: Vec<i32>,
}

/// Appends `entry` to `table` and returns its index.
fn push<T>(table: &mut Vec<T>, entry: T) -> i32 {
    table.push(entry);
    i32::try_from(table.len() - 1).expect("dictionary table exceeds i32::MAX entries")
}

impl Dictionary {
    fn new() -> Self {
        let tables = proto::ProfilesDictionary {
            mapping_table: vec![proto::Mapping::default()],
            location_table: vec![proto::Location::default()],
            function_table: vec![proto::Function::default()],
            link_table: vec![proto::Link::default()],
            string_table: vec![String::new()],
            attribute_table: vec![proto::KeyValueAndUnit::default()],
            stack_table: vec![proto::Stack::default()],
        };
        Self {
            tables,
            strings: HashMap::from([(Arc::from(""), 0)]),
            functions: HashMap::new(),
            locations: HashMap::new(),
            stacks: HashMap::new(),
            attributes: HashMap::new(),
            links: HashMap::new(),
            stack_scratch: Vec::new(),
        }
    }

    fn profile(&mut self, profile: &Profile) -> proto::Profile {
        let spec = profile.kind.spec();
        let samples = profile
            .samples
            .iter()
            .filter(|sample| sample.value != 0 && !sample.stack.is_empty())
            .map(|sample| proto::Sample {
                stack_index: self.stack(&sample.stack),
                attribute_indices: sample
                    .labels
                    .iter()
                    .map(|(key, value)| self.attribute(key, value))
                    .collect(),
                link_index: sample.link.map_or(0, |link| self.link(link)),
                values: vec![sample.value],
            })
            .collect();
        proto::Profile {
            sample_type: Some(self.value_type(spec.sample)),
            samples,
            time_unix_nano: profile.time_unix_nano,
            duration_nano: profile.duration_nano,
            period_type: Some(self.value_type(spec.period)),
            period: profile.period,
        }
    }

    fn string(&mut self, value: &str) -> i32 {
        if let Some(&index) = self.strings.get(value) {
            return index;
        }
        let index = push(&mut self.tables.string_table, value.to_string());
        self.strings.insert(Arc::from(value), index);
        index
    }

    fn value_type(&mut self, (kind, unit): (&str, &str)) -> proto::ValueType {
        proto::ValueType {
            type_strindex: self.string(kind),
            unit_strindex: self.string(unit),
        }
    }

    fn function(&mut self, frame: &Frame) -> i32 {
        let name = self.string(&frame.function);
        let file = self.string(&frame.file);
        *self.functions.entry((name, file)).or_insert_with(|| {
            push(
                &mut self.tables.function_table,
                proto::Function {
                    name_strindex: name,
                    system_name_strindex: name,
                    filename_strindex: file,
                },
            )
        })
    }

    fn location(&mut self, frame: &Frame) -> i32 {
        let function = self.function(frame);
        *self
            .locations
            .entry((function, frame.line))
            .or_insert_with(|| {
                push(
                    &mut self.tables.location_table,
                    proto::Location {
                        mapping_index: 0,
                        lines: vec![proto::Line {
                            function_index: function,
                            line: i64::from(frame.line),
                        }],
                    },
                )
            })
    }

    fn stack(&mut self, frames: &[Frame]) -> i32 {
        let mut locations = std::mem::take(&mut self.stack_scratch);
        locations.clear();
        for frame in frames {
            let location = self.location(frame);
            locations.push(location);
        }
        let index = match self.stacks.get(&locations) {
            Some(&index) => index,
            None => {
                let index = push(
                    &mut self.tables.stack_table,
                    proto::Stack {
                        location_indices: locations.clone(),
                    },
                );
                self.stacks.insert(locations.clone(), index);
                index
            }
        };
        self.stack_scratch = locations;
        index
    }

    fn attribute(&mut self, key: &str, value: &Arc<str>) -> i32 {
        let key = self.string(key);
        *self
            .attributes
            .entry((key, value.clone()))
            .or_insert_with(|| {
                push(
                    &mut self.tables.attribute_table,
                    proto::KeyValueAndUnit {
                        key_strindex: key,
                        value: Some(any_value(&AttributeValue::String(value.to_string()))),
                    },
                )
            })
    }

    fn link(&mut self, link: SpanLink) -> i32 {
        *self.links.entry(link).or_insert_with(|| {
            push(
                &mut self.tables.link_table,
                proto::Link {
                    trace_id: link.trace_id.to_vec(),
                    span_id: link.span_id.to_vec(),
                },
            )
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::{Kind, Labels, Sample};
    use crate::testing::{decode, profcheck, render, run_profcheck};

    fn frame(function: &str, line: u32) -> Frame {
        Frame {
            function: function.into(),
            file: "src/main.rs".into(),
            line,
        }
    }

    fn labels(pairs: &[(&str, &str)]) -> Labels {
        pairs
            .iter()
            .map(|&(k, v)| (Arc::from(k), Arc::from(v)))
            .collect()
    }

    fn origin() -> Origin {
        Origin {
            resource: vec![
                ("service.name".into(), "checkout".into()),
                ("process.pid".into(), AttributeValue::Int(42)),
            ],
            scope_name: "otelprofile".into(),
            scope_version: "0.1.0".into(),
        }
    }

    const LINK: SpanLink = SpanLink {
        trace_id: [0x11; 16],
        span_id: [0x22; 8],
    };

    fn fixture() -> Vec<Profile> {
        let handle: Arc<[Frame]> = Arc::from([
            frame("app::hash", 10),
            frame("app::handle", 20),
            frame("main", 1),
        ]);
        let parse: Arc<[Frame]> = Arc::from([
            frame("app::parse", 30),
            frame("app::handle", 21),
            frame("main", 1),
        ]);
        let sample = |stack: &Arc<[Frame]>, value, labels, link| Sample {
            stack: stack.clone(),
            value,
            labels,
            link,
        };
        let cpu = vec![
            sample(
                &handle,
                30_000_000,
                labels(&[("thread.name", "worker")]),
                Some(LINK),
            ),
            sample(
                &parse,
                10_000_000,
                labels(&[("thread.name", "worker"), ("endpoint", "/pay")]),
                None,
            ),
            sample(&parse, 0, labels(&[]), None),
            sample(&Arc::from([]), 10_000_000, labels(&[]), None),
        ];
        vec![
            Profile {
                kind: Kind::Cpu,
                time_unix_nano: 1_000,
                duration_nano: 10_000_000_000,
                period: 10_000_000,
                samples: cpu,
            },
            Profile {
                kind: Kind::InuseSpace,
                time_unix_nano: 2_000,
                duration_nano: 0,
                period: 524_288,
                samples: vec![sample(&handle, 4096, labels(&[]), None)],
            },
        ]
    }

    #[test]
    fn a_receiver_reads_back_every_stack_value_label_and_link() {
        let decoded = decode(&encode(&origin(), &fixture()));
        let golden = render(&decoded);
        let path = concat!(env!("CARGO_MANIFEST_DIR"), "/testdata/encode.golden.txt");
        if std::env::var_os("UPDATE_GOLDEN").is_some() {
            std::fs::write(path, &golden).unwrap();
        }
        assert_eq!(golden, std::fs::read_to_string(path).unwrap());

        let cpu = &decoded.profiles[0];
        assert_eq!(cpu.sample_type, ("cpu".into(), "nanoseconds".into()));
        assert_eq!(
            cpu.samples.len(),
            2,
            "zero values and empty stacks are dropped"
        );
        assert_eq!(
            cpu.samples[0].frames,
            [
                "app::hash src/main.rs:10",
                "app::handle src/main.rs:20",
                "main src/main.rs:1"
            ]
        );
        assert_eq!(
            cpu.samples[0].link.as_deref(),
            Some("11111111111111111111111111111111/2222222222222222")
        );
        assert_eq!(
            cpu.samples[1].attributes,
            [
                ("thread.name".into(), "worker".into()),
                ("endpoint".into(), "/pay".into())
            ]
        );
        let heap = &decoded.profiles[1];
        assert_eq!(heap.sample_type, ("inuse_space".into(), "bytes".into()));
        assert_eq!(heap.period_type, ("space".into(), "bytes".into()));
        assert_eq!(heap.samples[0].value, 4096);
    }

    #[test]
    fn shared_frames_are_written_once() {
        let body = encode(&origin(), &fixture());
        let request = <opentelemetry_proto::tonic::collector::profiles::v1development::ExportProfilesServiceRequest as Message>::decode(body.as_slice()).unwrap();
        let dict = request.dictionary.unwrap();
        // Zero entry + hash, handle, parse, main.
        assert_eq!(dict.function_table.len(), 5);
        // Zero entry + the two CPU stacks; the heap sample reuses the first.
        assert_eq!(dict.stack_table.len(), 3);
        assert_eq!(dict.link_table.len(), 2);
    }

    #[test]
    fn output_passes_profcheck() {
        profcheck(&encode(&origin(), &fixture()));
    }

    #[test]
    fn profcheck_rejects_a_duplicated_dictionary_entry() {
        let body = encode(&origin(), &fixture());
        let mut request = proto::ExportProfilesServiceRequest::decode(body.as_slice()).unwrap();
        let strings = &mut request.dictionary.as_mut().unwrap().string_table;
        let copy = strings[1].clone();
        strings.push(copy);
        if let Some(result) = run_profcheck(&request.encode_to_vec()) {
            assert!(result.is_err(), "profcheck accepted a duplicated string");
        }
    }
}
