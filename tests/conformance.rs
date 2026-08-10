//! Shared conformance vectors — selection and full-session scenarios
//! (Move Encoding — Sanki §Slot candidates and selection).
//!
//! Two vendored corpora (copies of the shared set at `web-specs.md/nostr/conformance`):
//!
//! - `selection.json` — the pure per-slot rule, driven through [`select_candidate`].
//!   Each vector candidate's `legal` is a given, supplied to the selection as
//!   its legality probe, so the file pins only the *selection algorithm*: the
//!   two windows (anterior latest-legal / informed earliest-legal) split at the
//!   `boundary`, and the per-window cap `K`.
//! - `scenarios.json` — full sessions, driven through [`natural_state`]: a founding
//!   position, plies with their canonical-attestation timings, and a cutoff. The
//!   asserted **selected chain** is the consensus property — the TypeScript client
//!   replays the same `scenarios.json` through `forgivingPlyChain` and must select
//!   the same chain, so the arbiter cannot finalise a chain the client would not.
//!   Since v4 (ADR-0010) each vector also pins the **termination**: the replay must
//!   conclude `Terminal` with the expected status on the chain's last ply (the
//!   background draws — insufficiency, repetition, the move limit — truncate it),
//!   or still be `Ongoing` when `expectedTermination` is null.
//!
//! The TypeScript client runs both files, so the two implementations cannot drift on
//! which Ply is canonical.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing,
    clippy::arithmetic_side_effects
)]

use std::path::PathBuf;

use sashite_sanki_arbiter::event::{AdjudicationRequest, Attestation, EventId, Ply, PublicKey};
use sashite_sanki_arbiter::natural_state::{natural_state, Conclusion};
use sashite_sanki_arbiter::selection::{select_candidate, Candidate, Selection};
use sashite_sanki_arbiter::session::SessionParams;
use sashite_sanki_engine::domain::outcome::Verdict;
use sashite_sanki_engine::domain::time::{Duration, Timestamp};
use sashite_sanki_engine::domain::time_control::{Period, TimeControl};
use sashite_sanki_engine::position::Position;

#[derive(serde::Deserialize)]
struct Corpus {
    vectors: Vec<SelectionVector>,
}

#[derive(serde::Deserialize)]
struct SelectionVector {
    id: String,
    boundary: i64,
    cap: usize,
    candidates: Vec<CandidateVector>,
    expected: Expected,
}

#[derive(serde::Deserialize)]
struct CandidateVector {
    id: String,
    #[serde(rename = "createdAt")]
    created_at: i64,
    legal: bool,
}

#[derive(serde::Deserialize)]
struct Expected {
    result: String,
    selected: Option<String>,
}

/// The vendored selection corpus.
fn corpus_path() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/conformance/selection.json")
}

/// The `(result, selected)` pair a [`Selection`] maps to, in the corpus' encoding.
fn outcome(selection: &Selection<'_, String>) -> (&'static str, Option<String>) {
    match selection {
        Selection::Applied(candidate) => ("applied", Some(candidate.id.clone())),
        Selection::Unfilled => ("unfilled", None),
    }
}

#[test]
fn selection_conformance() {
    let path = corpus_path();
    let Ok(contents) = std::fs::read_to_string(&path) else {
        eprintln!(
            "conformance corpus absent ({}) — test skipped.",
            path.display()
        );
        return;
    };
    let corpus: Corpus =
        serde_json::from_str(&contents).expect("conformance/selection.json: invalid JSON");
    assert!(!corpus.vectors.is_empty(), "the corpus has no vectors");

    for vector in &corpus.vectors {
        let candidates: Vec<Candidate<String>> = vector
            .candidates
            .iter()
            .map(|candidate| Candidate {
                id: candidate.id.clone(),
                created_at: Timestamp::from_unix(candidate.created_at),
            })
            .collect();
        let legality: std::collections::BTreeMap<&str, bool> = vector
            .candidates
            .iter()
            .map(|candidate| (candidate.id.as_str(), candidate.legal))
            .collect();
        let probe = |id: &String| legality.get(id.as_str()).copied().unwrap_or(false);

        let boundary = Timestamp::from_unix(vector.boundary);
        let (result, selected) =
            outcome(&select_candidate(boundary, &candidates, vector.cap, probe));

        assert_eq!(
            result,
            vector.expected.result.as_str(),
            "vector {}: result mismatch",
            vector.id
        );
        assert_eq!(
            selected, vector.expected.selected,
            "vector {}: selected candidate mismatch",
            vector.id
        );
    }
}

#[derive(serde::Deserialize)]
struct ScenarioCorpus {
    vectors: Vec<ScenarioVector>,
}

#[derive(serde::Deserialize)]
struct ScenarioVector {
    id: String,
    position: String,
    t0: i64,
    cutoff: i64,
    /// The session's time control (v5): `[duration, increment, plies]` period
    /// triples in kind-3420 order. Absent -> a neutral control that never flags,
    /// so the vector pins selection only.
    #[serde(rename = "timeControl", default)]
    time_control: Option<Vec<PeriodTriple>>,
    plies: Vec<ScenarioPly>,
    #[serde(rename = "expectedChain")]
    expected_chain: Vec<String>,
    /// The natural termination at the chain's tip (v4, ADR-0010): `{ status }`, or
    /// null / absent for a still-ongoing end position.
    #[serde(rename = "expectedTermination", default)]
    expected_termination: Option<ScenarioTermination>,
}

#[derive(serde::Deserialize)]
struct ScenarioTermination {
    status: String,
}

/// A v5 `timeControl` period: `[duration, increment, plies]` (kind-3420 order).
type PeriodTriple = (u64, Option<u64>, Option<u32>);

#[derive(serde::Deserialize)]
struct ScenarioPly {
    id: String,
    seat: String,
    step: u32,
    #[serde(rename = "move")]
    mv: serde_json::Value,
    #[serde(rename = "timedAt")]
    timed_at: i64,
}

const FIRST: u8 = 10;
const SECOND: u8 = 20;
const TIMESTAMPER: u8 = 99;
const ARBITER: u8 = 2;

fn pk(byte: u8) -> PublicKey {
    PublicKey::from_bytes([byte; 32])
}

/// Pack a short ASCII id into a 32-byte EventId (zero-padded). Injective for the
/// distinct ASCII ids the corpus uses, and reversible by [`str_from_eid`] — the
/// scenarios avoid `created_at` ties, so the resulting byte order never affects
/// selection (which would otherwise need to match the TS lexicographic tiebreak).
fn eid_from_str(s: &str) -> EventId {
    let mut bytes = [0_u8; 32];
    for (i, b) in s.bytes().take(32).enumerate() {
        bytes[i] = b;
    }
    EventId::from_bytes(bytes)
}

fn str_from_eid(id: &EventId) -> String {
    let bytes = id.as_bytes();
    let end = bytes.iter().position(|&b| b == 0).unwrap_or(bytes.len());
    String::from_utf8_lossy(&bytes[..end]).into_owned()
}

/// A time control generous enough never to flag, so the chain reflects only the
/// selection rule (the scenarios pin chain composition, not the clock).
fn neutral_time_control() -> TimeControl {
    let period = Period::new(Duration::from_secs(3_600), None, None).expect("valid period");
    TimeControl::new(period, Vec::new())
}

/// The scenario's time control (v5 `timeControl` triples), or the neutral one.
fn scenario_time_control(spec: &Option<Vec<PeriodTriple>>) -> TimeControl {
    let Some(triples) = spec else {
        return neutral_time_control();
    };
    let mut periods = triples.iter().map(|(duration, increment, plies)| {
        Period::new(
            Duration::from_secs(*duration),
            increment.map(Duration::from_secs),
            *plies,
        )
        .expect("valid scenario period")
    });
    let first = periods.next().expect("non-empty scenario timeControl");
    TimeControl::new(first, periods.collect())
}

#[test]
fn scenario_conformance() {
    let path = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/conformance/scenarios.json");
    let Ok(contents) = std::fs::read_to_string(&path) else {
        eprintln!(
            "conformance corpus absent ({}) — test skipped.",
            path.display()
        );
        return;
    };
    let corpus: ScenarioCorpus =
        serde_json::from_str(&contents).expect("conformance/scenarios.json: invalid JSON");
    assert!(!corpus.vectors.is_empty(), "the corpus has no vectors");

    let session = eid_from_str("session");
    let request_id = eid_from_str("request");

    for scenario in &corpus.vectors {
        let params = SessionParams::new(
            session,
            pk(ARBITER),
            Some(pk(TIMESTAMPER)),
            pk(FIRST),
            pk(SECOND),
            scenario_time_control(&scenario.time_control),
            Position::parse(&scenario.position).expect("valid FEEN"),
            Timestamp::from_unix(scenario.t0),
        );

        let plies: Vec<Ply> = scenario
            .plies
            .iter()
            .map(|ply| {
                let signer = if ply.seat == "first" { FIRST } else { SECOND };
                let content = serde_json::to_string(&ply.mv).expect("serialize move");
                Ply::new(
                    eid_from_str(&ply.id),
                    pk(signer),
                    session,
                    ply.step,
                    false,
                    content,
                    // Attested here, so the ply's own created_at is ignored; seed it with
                    // the attested time for consistency.
                    Timestamp::from_unix(ply.timed_at),
                )
            })
            .collect();

        let mut attestations: Vec<Attestation> = scenario
            .plies
            .iter()
            .map(|ply| {
                Attestation::new(
                    eid_from_str(&format!("att-{}", ply.id)),
                    pk(TIMESTAMPER),
                    eid_from_str(&ply.id),
                    Timestamp::from_unix(ply.timed_at),
                )
            })
            .collect();
        // The Request's canonical attestation sets the cutoff the chain is computed against.
        attestations.push(Attestation::new(
            eid_from_str("att-request"),
            pk(TIMESTAMPER),
            request_id,
            Timestamp::from_unix(scenario.cutoff),
        ));

        let request = AdjudicationRequest::new(
            request_id,
            pk(FIRST),
            session,
            pk(ARBITER),
            Timestamp::from_unix(scenario.cutoff),
        );

        let natural =
            natural_state(&params, &plies, &attestations, &request).expect("attested request");
        let chain: Vec<String> = natural
            .chain
            .iter()
            .map(|selected| str_from_eid(&selected.ply.id))
            .collect();

        assert_eq!(
            chain, scenario.expected_chain,
            "scenario {}: selected chain mismatch",
            scenario.id
        );

        // The replay's conclusion must match the pinned termination: a terminal
        // verdict with the expected status, or a still-ongoing end position.
        let actual_termination = match &natural.conclusion {
            Conclusion::Terminal(Verdict::Terminated { status, .. }, _) => Some(status.to_string()),
            Conclusion::Terminal(Verdict::Ongoing, _) | Conclusion::Ongoing(_) => None,
        };
        let expected_termination = scenario
            .expected_termination
            .as_ref()
            .map(|termination| termination.status.clone());
        assert_eq!(
            actual_termination, expected_termination,
            "scenario {}: termination mismatch",
            scenario.id
        );
    }
}
