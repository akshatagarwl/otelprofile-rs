//! Heap profiles from `otelprofile::alloc::Sampled`, through `start()` to a
//! receiver, in a binary whose global allocator is the wrapper.

#[path = "../src/testing.rs"]
mod testing;

use std::alloc::System;
use std::time::Duration;

use otelprofile::alloc::Sampled;
use otelprofile::{Config, ProfileType, SpanLink, span};
use testing::{MockReceiver, decode};

const RATE: usize = 64 * 1024;

#[global_allocator]
static ALLOC: Sampled<System> = Sampled::with_rate(System, RATE);

#[inline(never)]
fn retain_in_the_sampled_test(chunks: usize, size: usize) -> Vec<Vec<u8>> {
    (0..chunks).map(|i| vec![i as u8; size]).collect()
}

#[inline(never)]
fn churn_in_the_sampled_test(chunks: usize, size: usize) {
    for i in 0..chunks {
        std::hint::black_box(vec![i as u8; size]);
    }
}

fn attributed(profile: &testing::DecodedProfile, function: &str) -> (i64, Vec<Option<String>>) {
    let samples: Vec<_> = profile
        .samples
        .iter()
        .filter(|s| s.frames.iter().any(|f| f.contains(function)))
        .collect();
    (
        samples.iter().map(|s| s.value).sum(),
        samples.iter().map(|s| s.link.clone()).collect(),
    )
}

#[test]
fn sampled_heap_profiles_estimate_retained_and_churned_memory_with_span_links() {
    let receiver = MockReceiver::start(vec![]);
    let profiler = otelprofile::start(Config {
        endpoint: receiver.url.clone(),
        profile_types: vec![ProfileType::Heap],
        interval: Duration::from_secs(60),
        ..Config::default()
    })
    .unwrap();

    let link = SpanLink {
        trace_id: [0xab; 16],
        span_id: [0xcd; 8],
    };
    let kept = {
        let _span = span::enter(link);
        retain_in_the_sampled_test(1024, 64 * 1024)
    };
    churn_in_the_sampled_test(512, 64 * 1024);
    drop(profiler);
    drop(kept);

    let requests = receiver.requests();
    let profiles: Vec<_> = requests
        .iter()
        .flat_map(|r| decode(&r.body).profiles)
        .collect();
    let find = |name: &str| {
        profiles
            .iter()
            .find(|p| p.sample_type.0 == name)
            .unwrap_or_else(|| panic!("no {name}"))
    };
    let mib = 1024.0 * 1024.0;

    let (retained, links) = attributed(find("inuse_space"), "retain_in_the_sampled_test");
    eprintln!(
        "inuse_space under retain: {:.1} MiB (64 expected)",
        retained as f64 / mib
    );
    assert!(
        (0.8..1.2).contains(&(retained as f64 / (64.0 * mib))),
        "{retained}"
    );
    assert!(
        links
            .iter()
            .all(|l| l.as_deref() == Some(&format!("{}/{}", "ab".repeat(16), "cd".repeat(8))[..])),
        "retained memory links to the span: {links:?}"
    );

    let (churned_inuse, _) = attributed(find("inuse_space"), "churn_in_the_sampled_test");
    assert_eq!(churned_inuse, 0, "freed memory is not in use");
    let (churned, links) = attributed(find("alloc_space"), "churn_in_the_sampled_test");
    eprintln!(
        "alloc_space under churn: {:.1} MiB (32 expected)",
        churned as f64 / mib
    );
    assert!(
        (0.8..1.2).contains(&(churned as f64 / (32.0 * mib))),
        "{churned}"
    );
    assert!(links.iter().all(Option::is_none));

    let (objects, _) = attributed(find("inuse_objects"), "retain_in_the_sampled_test");
    eprintln!("inuse_objects under retain: {objects} (1024 expected)");
    assert!((800..1250).contains(&objects), "{objects}");
}
