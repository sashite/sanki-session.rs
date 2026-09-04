//! Cross-variant evaluation: the nine pairings driven end to end through
//! [`kernel_result`] (Statuses — Sanki §Verdict resolution).
//!
//! Sanki is a **cross-variant** family: kind `3422` assigns each player a
//! variant through the initial position's SIN styles, so chess, ōgi and xiongqi
//! meet on one 8×8 board in nine possible pairings. Every session below is a
//! real one — each position was reached by playing legal half-moves through the
//! engine from a published per-variant starting position, and every asserted
//! chain, status and result was observed before being written down.
//!
//! The file exists because a pairing is not a cosmetic parameter. Three rules
//! only ever fire cross-variant, and each of them can decide a verdict:
//!
//! - the **capture transform** (`sashite_sanki_engine::capture`): an ōgi
//!   capturer converts the captured piece to its own case — a genuinely
//!   droppable reserve — while a chess or xiongqi capturer keeps the piece's
//!   *original* case, so its hand fills with an **inert tray** it can never
//!   drop, cased as the side it took them from;
//! - **drops** belong to the ōgi side alone, whatever the opponent plays, and
//!   uchifuzume applies whether the mated royal is an ōgi King, a chess King or
//!   a xiongqi General;
//! - **castling** exists in all three variants (deciders' ruling 2026-07-27),
//!   and the royal's path is judged against the *opponent's* movement rules.
//!
//! [`inert_tray_checkmate_binds_the_verdict`] is a **pinned regression** for the
//! `sashite-sanki-engine` 0.8.0 fix: below engine 0.8 the same session was
//! evaluated as a `resignation` **against the player who had just delivered
//! checkmate**. See that test for the recorded before/after.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing,
    clippy::arithmetic_side_effects
)]

use sashite_sanki_engine::domain::outcome::Verdict;
use sashite_sanki_engine::domain::side::Side;
use sashite_sanki_engine::domain::status::{Outcome3, Status};
use sashite_sanki_engine::domain::time::{Duration, Timestamp};
use sashite_sanki_engine::domain::time_control::{Period, TimeControl};
use sashite_sanki_engine::position::Position;
use sashite_sanki_session::event::{Attestation, Conclusion, EventId, Ply, PublicKey};
use sashite_sanki_session::natural_state::{natural_state, ChainEnd, NaturalState};
use sashite_sanki_session::session::SessionParams;
use sashite_sanki_session::verdict::{conforms, kernel_result, KernelResult};

const FIRST: u8 = 10;
const SECOND: u8 = 20;
const TIMESTAMPER: u8 = 99;
const SESSION: u8 = 50;
const CONCLUSION: u8 = 170;

/// The canonical spacing between successive half-moves. Every half-move is
/// informed (each is timed at or after its slot's boundary, the predecessor's
/// timing), so the two-window rule never reorders these chains — they pin the
/// cross-variant *rules*, not the selection, which `selection.json` owns.
const TICK: i64 = 100;

/// A generous bank: the chains below terminate on the board, never on the clock.
const BANK: u64 = 3_600;

fn pk(byte: u8) -> PublicKey {
    PublicKey::from_bytes([byte; 32])
}

fn eid(byte: u8) -> EventId {
    EventId::from_bytes([byte; 32])
}

fn ts(secs: i64) -> Timestamp {
    Timestamp::from_unix(secs)
}

/// A session under evaluation: the founding parameters plus the published
/// Plies and their canonical attestations.
struct Session {
    params: SessionParams,
    plies: Vec<Ply>,
    attestations: Vec<Attestation>,
    cutoff: i64,
}

impl Session {
    /// Builds a session from `position` and a play order of `line` contents:
    /// `line[i]` is the half-move at play-order position `i + 1`, signed by the
    /// side Sanki's strict alternation puts on move there and attested at
    /// `(i + 1) * TICK`.
    fn new(position: &str, line: &[&str]) -> Self {
        let parsed = Position::parse(position).expect("valid FEEN");
        assert_eq!(
            parsed.to_feen(),
            position,
            "the fixture FEEN is not canonical"
        );
        let period = Period::new(Duration::from_secs(BANK), None, None).expect("valid period");
        let params = SessionParams::new(
            eid(SESSION),
            Some(pk(TIMESTAMPER)),
            pk(FIRST),
            pk(SECOND),
            TimeControl::new(period, Vec::new()),
            parsed,
            ts(0),
        );

        let mut plies = Vec::with_capacity(line.len());
        let mut attestations = Vec::with_capacity(line.len() + 1);
        for (index, content) in line.iter().enumerate() {
            let half_move = u32::try_from(index).expect("small") + 1;
            let id = u8::try_from(half_move).expect("short line");
            let at = i64::from(half_move) * TICK;
            plies.push(Ply::new(
                eid(id),
                params.player_at(half_move),
                eid(SESSION),
                params.step_at(half_move),
                false,
                (*content).to_owned(),
                ts(at),
            ));
            attestations.push(Attestation::new(
                eid(100 + id),
                pk(TIMESTAMPER),
                eid(id),
                ts(at),
            ));
        }
        // The Conclusion's own canonical attestation, well after the last half-move.
        let cutoff = (i64::try_from(line.len()).expect("small") + 10) * TICK;
        attestations.push(Attestation::new(
            eid(171),
            pk(TIMESTAMPER),
            eid(CONCLUSION),
            ts(cutoff),
        ));

        Self {
            params,
            plies,
            attestations,
            cutoff,
        }
    }

    /// A Conclusion by `invoker` claiming `status`/`result`.
    fn conclusion(&self, invoker: u8, status: Status, result: Outcome3) -> Conclusion {
        Conclusion::new(
            eid(CONCLUSION),
            pk(invoker),
            eid(SESSION),
            status,
            result,
            ts(0),
        )
    }

    /// A Conclusion by `invoker` whose claim is a placeholder (not read by the
    /// replay).
    fn probe(&self, invoker: u8) -> Conclusion {
        self.conclusion(invoker, Status::Resignation, Outcome3::Draw)
    }

    /// The natural-state replay (the chain builder and legality authority).
    fn replay(&self) -> NaturalState<'_> {
        let natural = natural_state(
            &self.params,
            &self.plies,
            &self.attestations,
            &self.probe(FIRST),
        )
        .expect("the Conclusion is canonically attested");
        // The chain is computed against the Conclusion's canonical timing, and the
        // cutoff here is late enough to admit every published half-move.
        assert_eq!(natural.cutoff, ts(self.cutoff));
        natural
    }

    /// The kernel result for a Conclusion by `invoker` — and, on the way, the
    /// binding-by-correctness check: a Conclusion claiming exactly that result
    /// conforms, one claiming the flipped outcome does not.
    fn rule(&self, invoker: u8) -> KernelResult {
        let result = kernel_result(
            &self.params,
            &self.plies,
            &self.attestations,
            &self.probe(invoker),
        )
        .expect("a canonically timed Conclusion by a player always has a result");
        let right = self.conclusion(invoker, result.status(), result.result());
        assert!(conforms(
            &self.params,
            &self.plies,
            &self.attestations,
            &right
        ));
        let flipped = match result.result() {
            Outcome3::FirstWins => Outcome3::SecondWins,
            Outcome3::SecondWins => Outcome3::FirstWins,
            Outcome3::Draw => Outcome3::FirstWins,
        };
        let wrong = self.conclusion(invoker, result.status(), flipped);
        assert!(!conforms(
            &self.params,
            &self.plies,
            &self.attestations,
            &wrong
        ));
        result
    }
}

/// The replay's terminal status, or `None` for a still-ongoing end position.
fn termination(natural: &NaturalState<'_>) -> Option<Status> {
    match &natural.end {
        ChainEnd::Terminal(Verdict::Terminated { status, .. }, _) => Some(*status),
        ChainEnd::Terminal(Verdict::Ongoing, _) | ChainEnd::Ongoing(_) => None,
    }
}

/// The FEEN of a still-ongoing end position.
fn ongoing_feen(natural: &NaturalState<'_>) -> String {
    match &natural.end {
        ChainEnd::Ongoing(state) => state.position().to_feen(),
        ChainEnd::Terminal(verdict, _) => {
            panic!("expected an ongoing end position, got {verdict:?}")
        }
    }
}

/// One pairing's end-to-end case: `(label, initial position, play order,
/// invoker, expected status, expected result)`.
type Pairing<'a> = (&'a str, &'a str, &'a [&'a str], u8, Status, Outcome3);

/// The chess/ōgi pairing from the published starting positions — byte-identical
/// to the engine's own `MIXED_START` fixture.
const MIXED_START: &str = "-rnbik^bn-r/+f+f+f+f+f+f+f+f/8/8/8/8/+P+P+P+P+P+P+P+P/-RNBQK^BN-R / W/j";

/// The position after the tenth half-move of [`INERT_TRAY_GAME`]: chess (first)
/// already holds one captured ōgi Fu, cased as second and therefore inert.
const INERT_TRAY_WINDOW: &str =
    "-rnb1k^b1-r/1+f+f+f+f+f1+f/4i3/P5f1/4n3/N6P/+P1+P+P+P+P+P1/1RBQK^BN-R f/ W/j";

/// The last five half-moves of [`INERT_TRAY_GAME`]: two more captures fill
/// first's inert tray, then `Rc7xc8` mates.
const INERT_TRAY_WINDOW_LINE: [&str; 5] = [
    r#"["b1","b7",null]"#,
    r#"["e6","d4",null]"#,
    r#"["b7","c7",null]"#,
    r#"["d4","c3",null]"#,
    r#"["c7","c8",null]"#,
];

/// A complete fifteen-half-move chess-versus-ōgi game, played out from
/// [`MIXED_START`] through the engine. First's Rook eats its way along the
/// seventh rank; every captured ōgi piece keeps its own (second) case in first's
/// hand, so first ends with an inert tray of four second-cased tokens while
/// second's own hand stays empty — the shape the checkmate below turns on.
const INERT_TRAY_GAME: [&str; 15] = [
    r#"["h2","h3",null]"#,
    r#"["d8","e6",null]"#,
    r#"["b2","b4",null]"#,
    r#"["g8","f6",null]"#,
    r#"["b1","a3",null]"#,
    r#"["f6","e4",null]"#,
    r#"["a1","b1",null]"#,
    r#"["a7","a5",null]"#,
    r#"["b4","a5",null]"#,
    r#"["g7","g5",null]"#,
    r#"["b1","b7",null]"#,
    r#"["e6","d4",null]"#,
    r#"["b7","c7",null]"#,
    r#"["d4","c3",null]"#,
    r#"["c7","c8",null]"#,
];

#[test]
fn inert_tray_checkmate_binds_the_verdict() {
    // PINNED REGRESSION for `sashite-sanki-engine` 0.8.0.
    //
    // `crate::capture`'s inert-tray rule keeps a captured piece's ORIGINAL case
    // when the capturer is chess or xiongqi: the token can then never satisfy
    // `belongs_to` for the capturer, so the tray is dead material. Below engine
    // 0.8 the kernel's terminal classification built its droppable-move probe
    // from the UNION of both hands, and that same token does satisfy
    // `belongs_to` for the side it was taken from — so first's inert, ōgi-cased
    // tray read as second's own reserve, a phantom drop appeared to interpose
    // on the checking rank, and a genuine checkmate classified as `Ongoing`.
    //
    // The kernel applies each selected Ply through `kernel::step`, so the
    // misclassification reached the verdict directly. Observed on this exact
    // session, published crates, no other change:
    //
    //   engine 0.7.0  chain = 5, conclusion = Ongoing
    //                 invoker `first`  -> resignation, SecondWins
    //                 invoker `second` -> resignation, FirstWins
    //   engine 0.8.2  chain = 5, conclusion = Terminal(checkmate)
    //                 invoker `first`  -> checkmate, FirstWins
    //                 invoker `second` -> checkmate, FirstWins
    //
    // The first row is the damaging one: with the mating player as the invoker,
    // the residual-resignation fallback (Statuses — Sanki §Verdict resolution)
    // awarded the game to the side that had just been mated.
    let session = Session::new(INERT_TRAY_WINDOW, &INERT_TRAY_WINDOW_LINE);
    let natural = session.replay();
    assert_eq!(natural.chain.len(), 5);
    assert_eq!(natural.next_half_move(), 6);
    assert_eq!(termination(&natural), Some(Status::Checkmate));
    match natural.end {
        ChainEnd::Terminal(_, at) => assert_eq!(at, ts(5 * TICK)),
        ChainEnd::Ongoing(_) => panic!("expected the mate to terminate the chain"),
    }

    // The verdict is play-derived, so it does not depend on who invoked.
    for invoker in [FIRST, SECOND] {
        let result = session.rule(invoker);
        assert_eq!(result.status(), Status::Checkmate);
        assert_eq!(result.result(), Outcome3::FirstWins);
        assert_eq!(result.score(Side::First), 100);
        assert_eq!(result.score(Side::Second), 0);
    }
}

#[test]
fn inert_tray_checkmate_from_the_standard_mixed_start() {
    // The same ending, evaluated over the WHOLE game rather than a window, so
    // the inert tray is built inside the session from two empty hands.
    let session = Session::new(MIXED_START, &INERT_TRAY_GAME);
    let natural = session.replay();
    assert_eq!(natural.chain.len(), 15);
    assert_eq!(termination(&natural), Some(Status::Checkmate));

    let result = session.rule(FIRST);
    assert_eq!(result.status(), Status::Checkmate);
    assert_eq!(result.result(), Outcome3::FirstWins);
}

#[test]
fn every_pairing_is_evaluated_end_to_end() {
    // The nine pairings kind `3422` can assign, each replayed from its published
    // starting position and ruled on. Eight terminate on the board; the
    // chess-second xiongqi session (`C/w`) is still ongoing at the cutoff and
    // resolves through the residual-resignation branch instead, so both arms of
    // `verdict::resolve_play` are exercised cross-variant.
    //
    // `(label, initial position, play order, invoker, status, result)`.
    let cases: &[Pairing<'_>] = &[
        (
            "W/w chess vs chess — the Queen mates on e5",
            "-rnbqk^bn-r/+p+p+p+p+p+p+p+p/8/8/8/8/+P+P+P+P+P+P+P+P/-RNBQK^BN-R / W/w",
            &[
                r#"["e2","e3",null]"#,
                r#"["e7","e5",null]"#,
                r#"["d1","h5",null]"#,
                r#"["e8","e7",null]"#,
                r#"["h5","e5",null]"#,
            ],
            SECOND,
            Status::Checkmate,
            Outcome3::FirstWins,
        ),
        (
            "W/j chess vs ōgi — the ōgi Princess mates the chess King on h4",
            MIXED_START,
            &[
                r#"["g2","g4",null]"#,
                r#"["e7","e5",null]"#,
                r#"["f2","f3",null]"#,
                r#"["d8","h4",null]"#,
            ],
            FIRST,
            Status::Checkmate,
            Outcome3::SecondWins,
        ),
        (
            "W/c chess vs xiongqi — the chess Knight mates the General on c7",
            "-rnbeg^bn-r/+s+s+s+s+s+s+s+s/8/8/8/8/+P+P+P+P+P+P+P+P/-RNBQK^BN-R / W/c",
            &[
                r#"["b1","a3",null]"#,
                r#"["b7","b5",null]"#,
                r#"["a3","b5",null]"#,
                r#"["f7","f6",null]"#,
                r#"["b5","c7",null]"#,
            ],
            SECOND,
            Status::Checkmate,
            Outcome3::FirstWins,
        ),
        (
            "J/w ōgi vs chess — the ōgi Princess mates on g7",
            "-rnbqk^bn-r/+p+p+p+p+p+p+p+p/8/8/8/8/+F+F+F+F+F+F+F+F/-RNBIK^BN-R / J/w",
            &[
                r#"["g2","g4",null]"#,
                r#"["g7","g6",null]"#,
                r#"["d1","c3",null]"#,
                r#"["f8","g7",null]"#,
                r#"["c3","g7",null]"#,
            ],
            SECOND,
            Status::Checkmate,
            Outcome3::FirstWins,
        ),
        (
            "J/j ōgi vs ōgi — the Princess mates on g7",
            "-rnbik^bn-r/+f+f+f+f+f+f+f+f/8/8/8/8/+F+F+F+F+F+F+F+F/-RNBIK^BN-R / J/j",
            &[
                r#"["d1","c3",null]"#,
                r#"["g7","g6",null]"#,
                r#"["g2","g4",null]"#,
                r#"["f8","g7",null]"#,
                r#"["c3","g7",null]"#,
            ],
            SECOND,
            Status::Checkmate,
            Outcome3::FirstWins,
        ),
        (
            "J/c ōgi vs xiongqi — the ōgi Knight mates the General on c7",
            "-rnbeg^bn-r/+s+s+s+s+s+s+s+s/8/8/8/8/+F+F+F+F+F+F+F+F/-RNBIK^BN-R / J/c",
            &[
                r#"["b1","a3",null]"#,
                r#"["b8","c6",null]"#,
                r#"["a3","b5",null]"#,
                r#"["c6","b8",null]"#,
                r#"["b5","c7",null]"#,
            ],
            SECOND,
            Status::Checkmate,
            Outcome3::FirstWins,
        ),
        (
            "C/w xiongqi vs chess — ongoing at the cutoff, residual resignation",
            "-rnbqk^bn-r/+p+p+p+p+p+p+p+p/8/8/8/8/+S+S+S+S+S+S+S+S/-RNBEG^BN-R / C/w",
            &SIDEWAYS_EN_PASSANT_LINE,
            SECOND,
            Status::Resignation,
            Outcome3::FirstWins,
        ),
        (
            "C/j xiongqi vs ōgi — the ōgi Princess mates the General on f3",
            "-rnbik^bn-r/+f+f+f+f+f+f+f+f/8/8/8/8/+S+S+S+S+S+S+S+S/-RNBEG^BN-R / C/j",
            &[
                r#"["g1","h3",null]"#,
                r#"["d8","c6",null]"#,
                r#"["f2","f3",null]"#,
                r#"["c6","f3",null]"#,
            ],
            FIRST,
            Status::Checkmate,
            Outcome3::SecondWins,
        ),
        (
            "C/c xiongqi vs xiongqi — the Empress mates on c7",
            "-rnbeg^bn-r/+s+s+s+s+s+s+s+s/8/8/8/8/+S+S+S+S+S+S+S+S/-RNBEG^BN-R / C/c",
            &[
                r#"["d1","c3",null]"#,
                r#"["g8","f6",null]"#,
                r#"["c3","c7",null]"#,
            ],
            SECOND,
            Status::Checkmate,
            Outcome3::FirstWins,
        ),
    ];

    for (label, position, line, invoker, status, result) in cases {
        let session = Session::new(position, line);
        let natural = session.replay();
        assert_eq!(
            natural.chain.len(),
            line.len(),
            "{label}: every published half-move must join the chain"
        );
        let ruled = session.rule(*invoker);
        assert_eq!(ruled.status(), *status, "{label}: status");
        assert_eq!(ruled.result(), *result, "{label}: result");
    }
}

#[test]
fn each_variant_castles_inside_a_cross_variant_session() {
    // Castling is a King move in chess and ōgi and a General move in xiongqi
    // (deciders' ruling 2026-07-27), and its FIDE conditions 4–6 — not in check,
    // not through an attacked square, not onto one — are judged with the
    // OPPONENT's movement rules, so a cross-variant board is where the rule is
    // least like its single-variant self. Each position below was reached by
    // legal play; the resulting FEEN shows the royal and its rook-class partner
    // both moved in one half-move, and the castling markers stripped.
    let cases: &[(&str, &str, &str, &str)] = &[
        (
            "chess castles kingside against an ōgi opponent",
            "rnb2br1/+f1ik^+f+f+f+f/8/1fff1P2/4n3/1N4P1/+P+P+P+P2B+P/-RNBQK^2+R /f W/j",
            r#"["e1","g1",null]"#,
            "rnb2br1/+f1ik^+f+f+f+f/8/1fff1P2/4n3/1N4P1/+P+P+P+P2B+P/RNBQ1RK^1 /f j/W",
        ),
        (
            "ōgi castles queenside against a xiongqi opponent",
            "rnb2rg^1/1+s1+s+s2+s/2s2ssb/s7/4FN2/2FF4/+F+F1N1+F+F+F/+R3K^BR1 2F/BI J/c",
            r#"["e1","c1",null]"#,
            "rnb2rg^1/1+s1+s+s2+s/2s2ssb/s7/4FN2/2FF4/+F+F1N1+F+F+F/2K^R1BR1 2F/BI c/J",
        ),
        (
            "the xiongqi General castles queenside against a chess opponent",
            "rqb5/N1+p1nk^2/1p6/2npR1S1/8/2S1E3/3+S+S+S2/+R3G^B2 5pbr/3SBN C/w",
            r#"["e1","c1",null]"#,
            "rqb5/N1+p1nk^2/1p6/2npR1S1/8/2S1E3/3+S+S+S2/2G^R1B2 5pbr/3SBN w/C",
        ),
    ];

    for (label, position, content, expected) in cases {
        let session = Session::new(position, &[content]);
        let natural = session.replay();
        assert_eq!(
            natural.chain.len(),
            1,
            "{label}: the castling must be applied"
        );
        assert_eq!(termination(&natural), None, "{label}");
        assert_eq!(ongoing_feen(&natural), *expected, "{label}");
    }
}

#[test]
fn cross_variant_capture_feeds_the_ogi_hand_and_the_drop_is_played() {
    // ōgi (first) versus chess (second), from the published ōgi/chess start:
    //
    //   1. F d2-d4   d7-d5      the two foot soldiers meet
    //   2. F d4xd5              an ŌGI capturer converts the chess Pawn to its
    //                           OWN case: first's hand gains a droppable Fu
    //   2. …  Q d8xd5           a CHESS capturer keeps the Fu's original case:
    //                           second's hand gains an inert, first-cased token
    //   3. F*d3                 first drops the Fu it converted — the drop
    //                           mechanic belongs to the ōgi side alone, whatever
    //                           the opponent's variant
    let session = Session::new(
        "-rnbqk^bn-r/+p+p+p+p+p+p+p+p/8/8/8/8/+F+F+F+F+F+F+F+F/-RNBIK^BN-R / J/w",
        &[
            r#"["d2","d4",null]"#,
            r#"["d7","d5",null]"#,
            r#"["d4","d5",null]"#,
            r#"["d8","d5",null]"#,
            r#"[null,"d3","fu"]"#,
        ],
    );
    let natural = session.replay();
    assert_eq!(natural.chain.len(), 5, "the drop must join the chain");
    assert_eq!(termination(&natural), None);
    // The end position: the dropped Fu stands on d3, first's hand is spent, and
    // second holds the single first-cased token it can never play.
    assert_eq!(
        ongoing_feen(&natural),
        "-rnb1k^bn-r/+p+p+p1+p+p+p+p/8/3q4/8/3F4/+F+F+F1+F+F+F+F/-RNBIK^BN-R /F w/J"
    );

    // Still ongoing at the cutoff, both clocks healthy: the invocation resolves
    // as the residual resignation, against whoever invoked.
    assert_eq!(session.rule(SECOND).status(), Status::Resignation);
    assert_eq!(session.rule(SECOND).result(), Outcome3::FirstWins);
    assert_eq!(session.rule(FIRST).result(), Outcome3::SecondWins);
}

#[test]
fn cross_variant_uchifuzume_is_skipped_never_a_loss() {
    // ōgi (first) versus chess (second): first's only published half-move is a
    // Fu drop that would mate the CHESS King. Uchifuzume is a rule of the
    // dropper's variant, not the mated side's, so the drop is illegal here just
    // as it is against an ōgi King. An illegal candidate is skipped, never
    // sanctioned (Statuses — Sanki §Verdict resolution: there is no
    // `illegalmove`), so the slot stays unfilled and the chain is empty.
    let session = Session::new("7k^/8/5N2/8/8/8/8/4K^1R1 F/ J/w", &[r#"[null,"h7","fu"]"#]);
    let natural = session.replay();
    assert!(natural.is_empty());
    assert_eq!(natural.next_half_move(), 1);
    assert_eq!(termination(&natural), None);
    assert_eq!(ongoing_feen(&natural), "7k^/8/5N2/8/8/8/8/4K^1R1 F/ J/w");

    // The illegal drop costs first nothing: it is second's invocation that
    // resolves, as a residual resignation against second.
    let result = session.rule(SECOND);
    assert_eq!(result.status(), Status::Resignation);
    assert_eq!(result.result(), Outcome3::FirstWins);
}

/// Seven half-moves from the published xiongqi/chess start: the Soldier walks
/// `a2-a4-a5-a6`, crossing the river, and answers `b7-b5` with `a6xb6` — the
/// **sideways** en passant only a Soldier past the river has. The captured Pawn
/// keeps its own (second) case in first's inert tray.
const SIDEWAYS_EN_PASSANT_LINE: [&str; 7] = [
    r#"["a2","a4",null]"#,
    r#"["g8","f6",null]"#,
    r#"["a4","a5",null]"#,
    r#"["f6","g8",null]"#,
    r#"["a5","a6",null]"#,
    r#"["b7","b5",null]"#,
    r#"["a6","b6",null]"#,
];

#[test]
fn xiongqi_sideways_en_passant_is_played_in_a_cross_variant_session() {
    let session = Session::new(
        "-rnbqk^bn-r/+p+p+p+p+p+p+p+p/8/8/8/8/+S+S+S+S+S+S+S+S/-RNBEG^BN-R / C/w",
        &SIDEWAYS_EN_PASSANT_LINE,
    );
    let natural = session.replay();
    assert_eq!(
        natural.chain.len(),
        7,
        "the sideways en-passant capture must be selected and applied"
    );
    assert_eq!(termination(&natural), None);
    // b5 is empty and the Soldier stands on b6: the victim was taken on the
    // square it had skipped past, and it sits in first's hand still cased as
    // second — a xiongqi capturer keeps the original case.
    assert_eq!(
        ongoing_feen(&natural),
        "-rnbqk^bn-r/+p1+p+p+p+p+p+p/1S6/8/8/8/1+S+S+S+S+S+S+S/-RNBEG^BN-R p/ w/C"
    );

    // The ōgi mirror of the same opening: an ōgi Fu on a6 has no en passant at
    // all (neither sideways nor diagonally), so the seventh half-move is
    // illegal, is skipped, and the chain stops one short.
    let mirrored = Session::new(
        "-rnbqk^bn-r/+p+p+p+p+p+p+p+p/8/8/8/8/+F+F+F+F+F+F+F+F/-RNBIK^BN-R / J/w",
        &SIDEWAYS_EN_PASSANT_LINE,
    );
    let natural = mirrored.replay();
    assert_eq!(natural.chain.len(), 6);
    assert_eq!(natural.next_half_move(), 7);
    assert_eq!(termination(&natural), None);
}
