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
//! - `scenarios.json` — full sessions, driven through [`natural_state`] and
//!   [`verdict_at`]: a founding position, plies with their canonical timings
//!   (and, since v9, their `draw` flags), and a cutoff. The asserted **selected
//!   chain** is the consensus property — the TypeScript client replays the same
//!   `scenarios.json` and must select the same chain, so the kernel cannot
//!   finalise a chain the client would not. Since v4 (ADR-0010) each vector also
//!   pins the **termination**: the replay must end `Terminal` with the expected
//!   status on the chain's last ply (the background draws — insufficiency,
//!   repetition, the move limit — truncate it), or still be `Ongoing` when
//!   `expectedTermination` is null. Since v9 a vector may also carry an
//!   `invoker` and an `expectedVerdict` (`{ status, result: { first, second } }`):
//!   the **post-chain resolution** — draw acceptance, abandonment timeout,
//!   residual resignation, in that order — is then pinned too, so the two
//!   implementations cannot drift on the verdict either; and a `candidateCap`
//!   (the session's `K`, the reference document's 8 when absent), so the cap
//!   is exercised as the session parameter it is.
//!
//! The TypeScript client runs both files. Both corpora are vendored with the
//! crate (`cargo package` ships them), so a missing file is a failure, not a
//! skipped test.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing,
    clippy::arithmetic_side_effects
)]

use std::num::NonZeroUsize;
use std::path::PathBuf;

use sashite_sanki_engine::domain::side::Side;
use sashite_sanki_engine::domain::time::{Duration, Timestamp};
use sashite_sanki_engine::domain::time_control::{Period, TimeControl};
use sashite_sanki_engine::position::Position;
use sashite_sanki_session::event::{Attestation, EventId, Ply, PublicKey};
use sashite_sanki_session::natural_state::{natural_state, ChainEnd};
use sashite_sanki_session::selection::{select_candidate, Candidate, Selection, CANDIDATE_CAP};
use sashite_sanki_session::session::{Seats, SessionParams};
use sashite_sanki_session::verdict::verdict_at;

#[derive(serde::Deserialize)]
struct Corpus {
    vectors: Vec<SelectionVector>,
}

#[derive(serde::Deserialize)]
struct SelectionVector {
    id: String,
    boundary: i64,
    cap: NonZeroUsize,
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

/// Reads a vendored corpus file, or fails: the corpora ship with the crate.
fn read_corpus(name: &str) -> String {
    let path = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("tests/conformance")
        .join(name);
    std::fs::read_to_string(&path)
        .unwrap_or_else(|error| panic!("conformance corpus {} unreadable: {error}", path.display()))
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
    let corpus: Corpus = serde_json::from_str(&read_corpus("selection.json"))
        .expect("conformance/selection.json: invalid JSON");
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
    /// The concluding side (v9): `first` or `second`. Present iff
    /// `expectedVerdict` is.
    #[serde(default)]
    invoker: Option<String>,
    /// The verdict the kernel yields at the cutoff for `invoker` (v9): the
    /// status and the two players' scores.
    #[serde(rename = "expectedVerdict", default)]
    expected_verdict: Option<ScenarioVerdict>,
    /// The session's slot selection cap `K` (v9). Absent -> the reference
    /// document's value.
    #[serde(rename = "candidateCap", default)]
    candidate_cap: Option<NonZeroUsize>,
}

#[derive(serde::Deserialize)]
struct ScenarioTermination {
    status: String,
}

#[derive(serde::Deserialize)]
struct ScenarioVerdict {
    status: String,
    result: ScenarioScores,
}

#[derive(serde::Deserialize)]
struct ScenarioScores {
    first: u8,
    second: u8,
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
    /// The `draw` flag (v9): a standing offer when the Ply is the chain's tail.
    #[serde(default)]
    draw: bool,
}

const FIRST: u8 = 10;
const SECOND: u8 = 20;
const TIMESTAMPER: u8 = 99;

fn pk(byte: u8) -> PublicKey {
    PublicKey::from_bytes([byte; 32])
}

/// Pack a short ASCII id into a 32-byte EventId (zero-padded). Injective for the
/// distinct ASCII ids the corpus uses, and reversible by [`str_from_eid`]. The
/// byte order of the packed ids is the lexicographic order of the ASCII ids,
/// which is the order the TypeScript client compares hex ids in — so the
/// race-tiebreak vectors (`race-equal-timing-*`, same canonical timing,
/// distinct ids) select the same event in both implementations.
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
    let corpus: ScenarioCorpus = serde_json::from_str(&read_corpus("scenarios.json"))
        .expect("conformance/scenarios.json: invalid JSON");
    assert!(!corpus.vectors.is_empty(), "the corpus has no vectors");

    let session = eid_from_str("session");
    let seats = Seats::new(pk(FIRST), pk(SECOND)).expect("distinct players");

    for scenario in &corpus.vectors {
        let params = SessionParams::new(
            session,
            Some(pk(TIMESTAMPER)),
            seats,
            scenario_time_control(&scenario.time_control),
            Position::parse(&scenario.position).expect("valid FEEN"),
            Timestamp::from_unix(scenario.t0),
        )
        .expect("first to move")
        .with_candidate_cap(scenario.candidate_cap.unwrap_or(CANDIDATE_CAP));

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
                    ply.draw,
                    content,
                    // Attested here, so the ply's own created_at is ignored; seed it with
                    // the attested time for consistency.
                    Timestamp::from_unix(ply.timed_at),
                )
            })
            .collect();

        let attestations: Vec<Attestation> = scenario
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

        // The cutoff the chain is computed against; the chain and its
        // termination are pinned by every vector, the post-chain verdict by
        // those carrying an `invoker`.
        let natural = natural_state(
            &params,
            &plies,
            &attestations,
            Timestamp::from_unix(scenario.cutoff),
        );
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
        let actual_termination = match &natural.end {
            ChainEnd::Terminal { verdict, .. } => Some(verdict.status().to_string()),
            ChainEnd::Ongoing(_) => None,
            ChainEnd::Inconsistent => panic!("scenario {}: inconsistent replay", scenario.id),
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

        // v9: the post-chain resolution, pinned by an invoker and a verdict.
        match (&scenario.invoker, &scenario.expected_verdict) {
            (None, None) => {}
            (Some(invoker), Some(expected)) => {
                let side = match invoker.as_str() {
                    "first" => Side::First,
                    "second" => Side::Second,
                    other => panic!("scenario {}: unknown invoker {other:?}", scenario.id),
                };
                let result = verdict_at(
                    &params,
                    &plies,
                    &attestations,
                    side,
                    Timestamp::from_unix(scenario.cutoff),
                )
                .unwrap_or_else(|_| panic!("scenario {}: inconsistent replay", scenario.id));
                assert_eq!(
                    result.status().to_string(),
                    expected.status,
                    "scenario {}: verdict status mismatch",
                    scenario.id
                );
                assert_eq!(
                    (result.score(Side::First), result.score(Side::Second)),
                    (expected.result.first, expected.result.second),
                    "scenario {}: verdict scores mismatch",
                    scenario.id
                );
            }
            _ => panic!(
                "scenario {}: `invoker` and `expectedVerdict` come together",
                scenario.id
            ),
        }
    }
}
