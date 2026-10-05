//! Conformance against the shared fixture.
//!
//! `tests/fixtures/normalization.json` is the same oracle the Python pipeline and
//! both SDKs are held to. Reading it here is what makes the Rust normalizer
//! *verifiable* rather than merely present: the CLI and the SDKs must agree on
//! which places a query matches, and the only way to know they agree is to check
//! them against one set of vectors.
//!
//! A failing vector here is a real defect, not a fixture to be adjusted. See
//! `CONTRIBUTING.md` §5.1.

use serde::Deserialize;
use std::path::PathBuf;

use terrakit_cli::normalize::{normalize, postal_normalize, search_key_unaccented};

#[derive(Deserialize)]
struct Fixture {
    vectors: Vec<Vector>,
}

#[derive(Deserialize)]
struct Vector {
    n: u32,
    input: String,
    expected: String,
    #[serde(default)]
    covers: String,
}

fn fixture_path() -> PathBuf {
    // CARGO_MANIFEST_DIR is <repo>/cli, so the fixture is one level up.
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .expect("cli has a parent directory")
        .join("tests")
        .join("fixtures")
        .join("normalization.json")
}

fn load() -> Fixture {
    let path = fixture_path();
    let raw = std::fs::read_to_string(&path).unwrap_or_else(|e| {
        panic!(
            "cannot read the conformance fixture at {}: {e}. \
             The fixture is the spec made executable; without it the Rust \
             normalizer cannot be checked against the Python and Swift ones.",
            path.display()
        )
    });
    serde_json::from_str(&raw).expect("fixture is not valid JSON")
}

#[test]
fn every_fixture_vector_passes() {
    let fixture = load();
    let mut failures = Vec::new();

    for vector in &fixture.vectors {
        let actual = normalize(&vector.input);
        if actual != vector.expected {
            failures.push(format!(
                "  #{} {}\n      input:    {:?}\n      expected: {:?}\n      actual:   {:?}",
                vector.n, vector.covers, vector.input, vector.expected, actual
            ));
        }
    }

    assert!(
        failures.is_empty(),
        "{}/{} vectors failed:\n{}\n\n\
         Do not edit the fixture to make these pass. See CONTRIBUTING.md section 5.1.",
        failures.len(),
        fixture.vectors.len(),
        failures.join("\n")
    );
}

#[test]
fn output_is_idempotent_for_every_vector() {
    for vector in load().vectors {
        let once = normalize(&vector.input);
        assert_eq!(
            normalize(&once),
            once,
            "vector #{} is not idempotent: {:?} -> {:?}",
            vector.n,
            vector.input,
            once
        );
    }
}

#[test]
fn no_latinish_combining_marks_survive() {
    // Narrower than "no combining marks at all", which would be wrong: a Japanese
    // dakuten must survive. See `core/spec/normalization.md` §2.
    for vector in load().vectors {
        let result = normalize(&vector.input);
        let mut base_is_latinish = false;
        for ch in result.chars() {
            let cp = ch as u32;
            let is_mark = matches!(cp,
                0x0300..=0x036F | 0x0483..=0x0489 | 0x0591..=0x05BD
                | 0x064B..=0x065F | 0x093A..=0x093C | 0x3099..=0x309A
                | 0x20D0..=0x20F0);
            if is_mark && base_is_latinish {
                panic!(
                    "vector #{} kept a Latinish combining mark: {:?}",
                    vector.n, result
                );
            }
            if !is_mark {
                base_is_latinish = matches!(cp,
                    0x0041..=0x007A | 0x00C0..=0x02AF | 0x0370..=0x03FF
                    | 0x1F00..=0x1FFF | 0x0400..=0x052F);
            }
        }
    }
}

#[test]
fn postal_normalization_matches_the_documented_cases() {
    // `DATA_MODEL.md` §8. Not in the shared fixture because postal normalization
    // is pipeline-only: an SDK reads the indexed `code_norm` and never
    // normalizes a postal code at query time.
    for (raw, expected) in [
        ("SW1A 1AA", "sw1a1aa"),
        ("SW1A1AA", "sw1a1aa"),
        ("SW1A-1AA", "sw1a1aa"),
        ("07160", "07160"),
        ("07000", "07000"),
        ("  07000  ", "07000"),
        ("sw1a 1aa", "sw1a1aa"),
    ] {
        assert_eq!(postal_normalize(raw), expected, "postal_normalize({raw:?})");
    }
}

#[test]
fn search_key_strips_all_separators() {
    // Lets a single-token query match a multi-word name.
    for (raw, expected) in [
        ("Atatürk Caddesi", "ataturkcaddesi"),
        ("Ataturk Caddesi", "ataturkcaddesi"),
        ("New York", "newyork"),
        ("東京都", "東京都"),
        ("SW1A 1AA", "sw1a1aa"),
    ] {
        assert_eq!(
            search_key_unaccented(raw),
            expected,
            "search_key_unaccented({raw:?})"
        );
    }
}
