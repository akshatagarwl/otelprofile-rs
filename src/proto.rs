//! The subset of the OTLP Profiles messages this crate writes and reads
//! (opentelemetry-proto v1.10 layout, `profiles/v1development`). Field tags
//! are copied from the upstream `.proto` files; the tests decode our output
//! with the `opentelemetry-proto` crate to keep them honest.

#[derive(Clone, PartialEq, prost::Message)]
pub(crate) struct ExportProfilesServiceRequest {
    #[prost(message, repeated, tag = "1")]
    pub resource_profiles: Vec<ResourceProfiles>,
    #[prost(message, optional, tag = "2")]
    pub dictionary: Option<ProfilesDictionary>,
}

#[derive(Clone, PartialEq, prost::Message)]
pub(crate) struct ExportProfilesServiceResponse {
    #[prost(message, optional, tag = "1")]
    pub partial_success: Option<ExportProfilesPartialSuccess>,
}

#[derive(Clone, PartialEq, prost::Message)]
pub(crate) struct ExportProfilesPartialSuccess {
    #[prost(int64, tag = "1")]
    pub rejected_profiles: i64,
    #[prost(string, tag = "2")]
    pub error_message: String,
}

#[derive(Clone, PartialEq, prost::Message)]
pub(crate) struct ProfilesDictionary {
    #[prost(message, repeated, tag = "1")]
    pub mapping_table: Vec<Mapping>,
    #[prost(message, repeated, tag = "2")]
    pub location_table: Vec<Location>,
    #[prost(message, repeated, tag = "3")]
    pub function_table: Vec<Function>,
    #[prost(message, repeated, tag = "4")]
    pub link_table: Vec<Link>,
    #[prost(string, repeated, tag = "5")]
    pub string_table: Vec<String>,
    #[prost(message, repeated, tag = "6")]
    pub attribute_table: Vec<KeyValueAndUnit>,
    #[prost(message, repeated, tag = "7")]
    pub stack_table: Vec<Stack>,
}

#[derive(Clone, PartialEq, prost::Message)]
pub(crate) struct ResourceProfiles {
    #[prost(message, optional, tag = "1")]
    pub resource: Option<Resource>,
    #[prost(message, repeated, tag = "2")]
    pub scope_profiles: Vec<ScopeProfiles>,
}

#[derive(Clone, PartialEq, prost::Message)]
pub(crate) struct ScopeProfiles {
    #[prost(message, optional, tag = "1")]
    pub scope: Option<InstrumentationScope>,
    #[prost(message, repeated, tag = "2")]
    pub profiles: Vec<Profile>,
}

#[derive(Clone, PartialEq, prost::Message)]
pub(crate) struct Profile {
    #[prost(message, optional, tag = "1")]
    pub sample_type: Option<ValueType>,
    #[prost(message, repeated, tag = "2")]
    pub samples: Vec<Sample>,
    #[prost(fixed64, tag = "3")]
    pub time_unix_nano: u64,
    #[prost(uint64, tag = "4")]
    pub duration_nano: u64,
    #[prost(message, optional, tag = "5")]
    pub period_type: Option<ValueType>,
    #[prost(int64, tag = "6")]
    pub period: i64,
}

#[derive(Clone, PartialEq, prost::Message)]
pub(crate) struct Link {
    #[prost(bytes = "vec", tag = "1")]
    pub trace_id: Vec<u8>,
    #[prost(bytes = "vec", tag = "2")]
    pub span_id: Vec<u8>,
}

#[derive(Clone, Copy, PartialEq, prost::Message)]
pub(crate) struct ValueType {
    #[prost(int32, tag = "1")]
    pub type_strindex: i32,
    #[prost(int32, tag = "2")]
    pub unit_strindex: i32,
}

#[derive(Clone, PartialEq, prost::Message)]
pub(crate) struct Sample {
    #[prost(int32, tag = "1")]
    pub stack_index: i32,
    #[prost(int32, repeated, tag = "2")]
    pub attribute_indices: Vec<i32>,
    #[prost(int32, tag = "3")]
    pub link_index: i32,
    #[prost(int64, repeated, tag = "4")]
    pub values: Vec<i64>,
}

#[derive(Clone, PartialEq, prost::Message)]
pub(crate) struct Mapping {}

#[derive(Clone, PartialEq, prost::Message)]
pub(crate) struct Stack {
    #[prost(int32, repeated, tag = "1")]
    pub location_indices: Vec<i32>,
}

#[derive(Clone, PartialEq, prost::Message)]
pub(crate) struct Location {
    #[prost(int32, tag = "1")]
    pub mapping_index: i32,
    #[prost(message, repeated, tag = "3")]
    pub lines: Vec<Line>,
}

#[derive(Clone, PartialEq, prost::Message)]
pub(crate) struct Line {
    #[prost(int32, tag = "1")]
    pub function_index: i32,
    #[prost(int64, tag = "2")]
    pub line: i64,
}

#[derive(Clone, PartialEq, prost::Message)]
pub(crate) struct Function {
    #[prost(int32, tag = "1")]
    pub name_strindex: i32,
    #[prost(int32, tag = "2")]
    pub system_name_strindex: i32,
    #[prost(int32, tag = "3")]
    pub filename_strindex: i32,
}

#[derive(Clone, PartialEq, prost::Message)]
pub(crate) struct KeyValueAndUnit {
    #[prost(int32, tag = "1")]
    pub key_strindex: i32,
    #[prost(message, optional, tag = "2")]
    pub value: Option<AnyValue>,
}

#[derive(Clone, PartialEq, prost::Message)]
pub(crate) struct Resource {
    #[prost(message, repeated, tag = "1")]
    pub attributes: Vec<KeyValue>,
}

#[derive(Clone, PartialEq, prost::Message)]
pub(crate) struct InstrumentationScope {
    #[prost(string, tag = "1")]
    pub name: String,
    #[prost(string, tag = "2")]
    pub version: String,
}

#[derive(Clone, PartialEq, prost::Message)]
pub(crate) struct KeyValue {
    #[prost(string, tag = "1")]
    pub key: String,
    #[prost(message, optional, tag = "2")]
    pub value: Option<AnyValue>,
}

#[derive(Clone, PartialEq, prost::Message)]
pub(crate) struct AnyValue {
    #[prost(oneof = "any_value::Value", tags = "1, 2, 3")]
    pub value: Option<any_value::Value>,
}

pub(crate) mod any_value {
    #[derive(Clone, PartialEq, prost::Oneof)]
    pub(crate) enum Value {
        #[prost(string, tag = "1")]
        String(String),
        #[prost(bool, tag = "2")]
        Bool(bool),
        #[prost(int64, tag = "3")]
        Int(i64),
    }
}
