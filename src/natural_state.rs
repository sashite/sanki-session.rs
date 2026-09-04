//! The natural state of events at the cutoff (kind `3425` §Natural state of
//! events at the cutoff; Kernel — Sanki §II.3–II.5).
//!
//! To evaluate a session the kernel replays its play order from its first
//! half-move, selecting the canonical Ply for each successive slot under the
//! **forgiving-premove** rule ([`crate::selection`]) and applying it through the
//! engine. The replay is a single pass that is at once the chain builder and the
//! legality authority — a slot's selection depends on each candidate's legality
//! in the *replayed* position, so the two cannot be separated.
//!
//! For each play-order position (`(signer, step)` under Sanki's strict
//! alternation), the candidates are the Plies for that slot whose canonical
//! timing lies in `[t₀, cutoff]` — `t₀` the session start (a Ply timed before
//! t₀ is invalid, kind `3423` §Time accounting, and never enters a slot), the
//! `cutoff` the instant the state is evaluated at (a Conclusion's own canonical
//! timing, so a player cannot race a Conclusion by playing after it — kind
//! `3425` §Implications, *no post-conclusion racing*). Canonical timing is the
//! designated timestamper's attestation in attested mode, or the event's own
//! `created_at` when self-timed — an event whose acceptance by a designated
//! timing relay is not established is pending and must not be offered to the
//! replay ([`crate::event`]).
//!
//! The slot's **boundary** `T` is the maximum canonical timing among the
//! preceding half-moves (`t₀` for the first slot) — never rewound by a selected
//! premove — and [`select_candidate`] resolves the candidates against it:
//!
//! - **applied** — the selected Ply is applied to the board: the *latest* legal
//!   premove (anterior, timed before `T`), else the *earliest* legal live move
//!   (informed, timed at/after `T`). An illegal candidate — premove or live — is
//!   skipped, never sanctioned;
//! - **unfilled** — no candidate is legal in either window: the chain stops, still
//!   ongoing.
//!
//! Identical candidates — same content, same `draw` flag — are one candidate
//! for the cap, represented in each window by the one its scan reaches first
//! (the latest-timed anterior, the earliest-timed informed): the collapse
//! never changes which move is selected, it only stops re-signed retries from
//! consuming cap slots. Identity is the raw `content` string: two encodings
//! of one move are two candidates, as the specification's "same content"
//! says. Legality is probed **lazily** through [`select_candidate`]'s
//! callback, on the capped windows only (≤ 2K full-rule probes per slot, `K`
//! being the session's `candidate_cap`).
//!
//! Applying a selected Ply through the engine ([`step`]) also surfaces a
//! rule-system ending (checkmate, …) or a played-Ply timeout, which terminates
//! the chain. The replay therefore yields a **terminal verdict** (rule-system
//! ending / timeout, at the canonical timing of the Ply that caused it), a
//! still-**ongoing** end position for the post-chain resolution
//! ([`crate::verdict`]), or — on a broken internal invariant, never on a
//! well-formed position — an **inconsistency**, reported rather than resolved.
//! There is no `illegalmove` termination — an illegal Ply is skipped, never a
//! loss.
//!
//! The cutoff is an input: [`crate::verdict::cutoff_of`] resolves a
//! Conclusion's canonical timing (attested mode: its attestation by the
//! designated timestamper, without which the Conclusion is *pending* — kind
//! `3425` §Until the Conclusion has canonical timing; self-timed: its own
//! `created_at`), and a caller probing "the state now" passes the instant
//! directly. Given a cutoff the replay is total.

use crate::event::{Attestation, EventId, Ply};
use crate::selection::{select_candidate, Candidate, Selection};
use crate::session::SessionParams;
use crate::timing::canonical_timing;
use crate::verdict::Verdict;
use sashite_sanki_engine::domain::half_move::Move;
use sashite_sanki_engine::domain::time::Timestamp;
use sashite_sanki_engine::engine::validate;
use sashite_sanki_engine::kernel::state::SessionState;
use sashite_sanki_engine::kernel::step::{step, StepResult};
use std::collections::btree_map::Entry;
use std::collections::BTreeMap;

/// A Ply selected as canonical for its slot, paired with its canonical timing
/// ([`canonical_timing`]: the attestation's `created_at` in attested mode, or
/// the Ply's own `created_at` when self-timed).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CanonicalPly<'a> {
    /// The canonical Ply.
    pub ply: &'a Ply,
    /// The canonical timing of the Ply.
    pub at: Timestamp,
}

/// How the replayed chain ends.
#[derive(Debug, Clone)]
pub enum ChainEnd {
    /// The chain reached a terminal verdict during replay — a rule-system
    /// ending or a played-Ply timeout — at the given canonical timing.
    /// Post-chain resolution does not apply.
    Terminal {
        /// The verdict the replay reached.
        verdict: Verdict,
        /// The canonical timing of the Ply that reached it (Statuses — Sanki
        /// §Verdict resolution, step 1). A terminating premove carries an
        /// anterior timing, so `at` may precede the preceding half-move's; the
        /// instant the terminal position *existed* is `max(T, at)`, the
        /// state's clock anchor.
        at: Timestamp,
    },
    /// The chain replayed to a still-ongoing position: post-chain resolution
    /// (draw acceptance, abandonment timeout, residual resignation) decides the
    /// verdict on this state. Boxed — a [`SessionState`] dwarfs the terminal
    /// variant, so the box keeps the enum small.
    Ongoing(Box<SessionState>),
    /// The replay hit a broken internal invariant — a candidate the legality
    /// probe accepted that the kernel step then rejected, a selected candidate
    /// missing from its own slot, an engine ending without a terminated
    /// verdict. Unreachable on a well-formed position (the `validate`/`step`
    /// agreement is pinned across every variant pairing), and reported rather
    /// than swallowed: **no verdict is defined** for this state, and a consumer
    /// must treat the session as unresolved instead of deriving a wrong
    /// resignation or timeout from a truncated chain.
    Inconsistent,
}

/// The natural state: the selected canonical Ply chain, the cutoff it was
/// computed against, and how the chain ended.
#[derive(Debug, Clone)]
pub struct NaturalState<'a> {
    /// The selected canonical Plies, `chain[i]` being the Ply at play-order
    /// position `i + 1`. A skipped illegal candidate is **not** included (it is not
    /// a played half-move); a terminating *applied* Ply (a mating move, …) **is**.
    pub chain: Vec<CanonicalPly<'a>>,
    /// The cutoff the state was evaluated at — a Conclusion's canonical timing
    /// ([`crate::verdict::cutoff_of`]), or any instant a caller probed.
    pub cutoff: Timestamp,
    /// How the chain ended (terminal verdict or ongoing end position).
    pub end: ChainEnd,
}

impl NaturalState<'_> {
    /// The first play-order position **not** filled by an applied Ply — the
    /// position a continuation would occupy. With a chain of `k` half-moves,
    /// this is `k + 1`.
    #[inline]
    #[must_use]
    pub fn next_half_move(&self) -> u32 {
        let played = u32::try_from(self.chain.len()).unwrap_or(u32::MAX);
        played.saturating_add(1)
    }

    /// Whether the chain is empty (no applied Ply from step 1).
    #[inline]
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.chain.is_empty()
    }
}

/// Whether `mv` is a legal half-move in `state`'s position, under the full
/// rule system — `engine::validate`, which since engine 0.4 enforces ōgi
/// uchifuzume exactly as the kernel's `step` path does. The two must agree
/// **exactly**: [`select_candidate`] only ever returns a candidate this probe
/// called legal, so a move `validate` accepted and [`step`] then rejected
/// would fall into the defensive `StepResult::Illegal` seam below and silently
/// leave a played game unfinished. The agreement is pinned across all nine
/// variant pairings, and across the move shapes only some variants have, by the
/// `is_legal_matches_the_kernel_step_oracle` test below. Legality is a
/// position question resolved **before** the clock — a legal-but-timed-out move
/// is still legal here — and probing it clones no state.
fn is_legal(state: &SessionState, mv: &Move) -> bool {
    validate(state.position(), mv).is_ok()
}

/// A slot candidate paired with its source Ply (so the selection can be mapped
/// back to the played event) and its content parsed once — `None` when the
/// content is not a well-formed half-move, which makes the candidate illegal
/// (skipped, never a loss) while still counting against the cap.
struct SlotCandidate<'a> {
    ply: &'a Ply,
    candidate: Candidate<EventId>,
    mv: Option<Move>,
}

/// Computes the natural state of `plies`/`attestations` for the session at
/// `cutoff` — a Conclusion's canonical timing ([`crate::verdict::cutoff_of`]),
/// or any instant a caller wants the state at. Total: every input has a natural
/// state.
#[must_use]
pub fn natural_state<'a>(
    params: &SessionParams,
    plies: &'a [Ply],
    attestations: &'a [Attestation],
    cutoff: Timestamp,
) -> NaturalState<'a> {
    let timestamper = params.timestamper();
    let session = params.session();
    let start = params.start(); // t₀: the lower bound and the first slot's anchor.

    // The boundary `T` of each slot is the kernel state's own clock anchor —
    // t₀ at the start, then the maximum canonical timing of the selected
    // Plies (`SessionState::last_attestation`, which never rewinds): one
    // value, read by the selection and charged by the clock alike.
    let mut chain: Vec<CanonicalPly<'a>> = Vec::new();
    let mut state = params.initial_state();
    let mut half_move: u32 = 1;

    let end = loop {
        let signer = params.player_at(half_move);
        let step_no = params.step_at(half_move);

        // Candidates for this slot: canonically timed within [t₀, cutoff] (a
        // pre-t₀ Ply is invalid and never enters — kind 3423 §Time accounting).
        let timed: Vec<SlotCandidate<'a>> = plies
            .iter()
            .filter(|ply| ply.session == session && ply.signer == signer && ply.step == step_no)
            .filter_map(|ply| {
                let at = canonical_timing(attestations, ply.id, ply.created_at, timestamper)?;
                (at >= start && at <= cutoff).then(|| SlotCandidate {
                    ply,
                    candidate: Candidate {
                        id: ply.id,
                        created_at: at,
                    },
                    mv: Move::parse(&ply.content).ok(),
                })
            })
            .collect();

        // Identical candidates — same content, same `draw` flag — are one
        // candidate for the cap and are represented, in each window, by the
        // one the window's scan reaches first: the latest-timed anterior
        // (largest (timing, id)), the earliest-timed informed (smallest). The
        // key carries the window — `Candidate::is_anterior`, the split the
        // selection itself filters on — so a pair straddling the boundary is
        // two candidates, one per window, whatever order the events arrive
        // in. Identical candidates have identical legality, so the collapse
        // never changes which candidate a window's scan reaches first — its
        // one effect is that re-signed retries cannot push a legal candidate
        // past the cap (Move Encoding — Sanki §Slot candidates and selection;
        // Kernel — Sanki §II.3). A re-premove back to an earlier content is a
        // change of mind and supersedes, exactly as if the twin did not exist.
        let boundary = state.last_attestation();
        let mut representatives: BTreeMap<(&str, bool, bool), SlotCandidate<'a>> = BTreeMap::new();
        for entrant in timed {
            let anterior = entrant.candidate.is_anterior(boundary);
            let key = (entrant.ply.content.as_str(), entrant.ply.draw, anterior);
            match representatives.entry(key) {
                Entry::Vacant(vacant) => {
                    vacant.insert(entrant);
                }
                Entry::Occupied(mut occupied) => {
                    let held = &occupied.get().candidate;
                    let contender = &entrant.candidate;
                    let held_key = (held.created_at, held.id);
                    let contender_key = (contender.created_at, contender.id);
                    let replace = if anterior {
                        contender_key > held_key
                    } else {
                        contender_key < held_key
                    };
                    if replace {
                        occupied.insert(entrant);
                    }
                }
            }
        }
        let slot: Vec<SlotCandidate<'a>> = representatives.into_values().collect();

        let candidates: Vec<Candidate<EventId>> = slot.iter().map(|sc| sc.candidate).collect();

        // The legality probe: consulted lazily by the selection, on the capped
        // windows only — the ≤ 2K normative bound. An unparseable content is
        // illegal.
        let probe = |id: &EventId| {
            slot.iter()
                .find(|sc| sc.ply.id == *id)
                .and_then(|sc| sc.mv.as_ref())
                .is_some_and(|mv| is_legal(&state, mv))
        };

        match select_candidate(boundary, &candidates, params.candidate_cap(), probe) {
            // No candidate is legal in either window: the chain stops, still ongoing.
            Selection::Unfilled => break ChainEnd::Ongoing(Box::new(state)),

            // A candidate fills the slot: apply it and advance (or terminate on a
            // rule-system ending / timeout the application surfaces).
            Selection::Applied(chosen) => {
                let at = chosen.created_at;
                // Invariant: the selected candidate is one of this slot's, and the
                // probe called it legal, so its content parsed. Either failing is
                // a broken internal invariant — reported, never silently resolved.
                let Some((ply, mv)) = slot
                    .iter()
                    .find(|sc| sc.ply.id == chosen.id)
                    .and_then(|sc| Some((sc.ply, sc.mv.as_ref()?)))
                else {
                    break ChainEnd::Inconsistent;
                };

                // The probe validated the candidate, so a rejection here is a
                // broken internal invariant — unreachable on a well-formed
                // position (`is_legal_matches_the_kernel_step_oracle`).
                let (outcome, next) = match step(state, mv, at) {
                    StepResult::Illegal { .. } => break ChainEnd::Inconsistent,
                    StepResult::Advanced { outcome, next } => (outcome, next),
                };
                chain.push(CanonicalPly { ply, at });
                match next {
                    Some(successor) => {
                        // The successor's clock anchor is `max(T, at)`: the
                        // selection boundary NEVER rewinds — an applied premove
                        // carries an ANTERIOR timing, and anchoring the next slot
                        // on it would (a) misclassify the next slot's blind
                        // candidates as informed and (b) bill the next mover for
                        // time before the position was theirs to answer
                        // (time-accounting §Elapsed time; pinned by the shared
                        // conformance vector `scenario.premove-anchor-never-rewinds`).
                        state = successor;
                        half_move = half_move.saturating_add(1);
                    }
                    // The engine ends a session only with a terminated verdict.
                    None => match Verdict::from_engine(outcome.verdict) {
                        Some(verdict) => break ChainEnd::Terminal { verdict, at },
                        None => break ChainEnd::Inconsistent,
                    },
                }
            }
        }
    };

    NaturalState { chain, cutoff, end }
}

#[cfg(test)]
mod tests {
    #![allow(
        clippy::unwrap_used,
        clippy::expect_used,
        clippy::panic,
        clippy::indexing_slicing
    )]

    use super::{natural_state, ChainEnd};
    use crate::event::{Attestation, EventId, Ply, PublicKey};
    use crate::session::{Seats, SessionParams};
    use sashite_sanki_engine::domain::status::Status;
    use sashite_sanki_engine::domain::time::{Duration, Timestamp};
    use sashite_sanki_engine::domain::time_control::{Period, TimeControl};
    use sashite_sanki_engine::position::Position;

    const FIRST: u8 = 10;
    const SECOND: u8 = 20;
    const TIMESTAMPER: u8 = 99;
    const SESSION: u8 = 50;
    const CONCLUSION: u8 = 170;

    // A chess rook-and-king endgame: white Rook a1, white King e1, black King e8.
    // White to move. Gives a stock of legal moves for the chain tests.
    const ROOK_KING: &str = "4k^3/8/8/8/8/8/8/R3K^3 / W/w";

    fn pk(byte: u8) -> PublicKey {
        PublicKey::from_bytes([byte; 32])
    }

    fn eid(byte: u8) -> EventId {
        EventId::from_bytes([byte; 32])
    }

    fn ts(secs: i64) -> Timestamp {
        Timestamp::from_unix(secs)
    }

    fn ply(id: u8, signer: u8, step: u32, content: &str) -> Ply {
        ply_at(id, signer, step, content, 0)
    }

    // A ply with an explicit created_at — its canonical timing when self-timed
    // (as accepted by a designated timing relay, the caller's precondition). In
    // the attested tests below, created_at is ignored, so `ply` seeds 0.
    fn ply_at(id: u8, signer: u8, step: u32, content: &str, created_at: i64) -> Ply {
        Ply::new(
            eid(id),
            pk(signer),
            eid(SESSION),
            step,
            false,
            content.to_owned(),
            ts(created_at),
        )
    }

    fn att(id: u8, attests: u8, at: i64) -> Attestation {
        Attestation::new(eid(id), pk(TIMESTAMPER), eid(attests), ts(at))
    }

    fn params_feen(feen: &str) -> SessionParams {
        let period = Period::new(Duration::from_secs(600), None, None).expect("valid period");
        SessionParams::new(
            eid(SESSION),
            Some(pk(TIMESTAMPER)),
            Seats::new(pk(FIRST), pk(SECOND)).expect("distinct"),
            TimeControl::new(period, Vec::new()),
            Position::parse(feen).expect("valid FEEN"),
            ts(0),
        )
        .expect("first to move")
    }

    fn params() -> SessionParams {
        params_feen(ROOK_KING)
    }

    fn params_self_timed() -> SessionParams {
        let period = Period::new(Duration::from_secs(600), None, None).expect("valid period");
        SessionParams::new(
            eid(SESSION),
            None, // self-timed: no timestamper designated
            Seats::new(pk(FIRST), pk(SECOND)).expect("distinct"),
            TimeControl::new(period, Vec::new()),
            Position::parse(ROOK_KING).expect("valid FEEN"),
            ts(0),
        )
        .expect("first to move")
    }

    /// The cutoff the attested fixtures fix: the attestation of the
    /// (notional) Conclusion `CONCLUSION` among `atts`.
    fn cutoff(atts: &[Attestation]) -> Timestamp {
        atts.iter()
            .find(|a| a.attests == eid(CONCLUSION))
            .map(|a| a.created_at)
            .expect("the fixture attests the conclusion")
    }

    fn cutoff_att(at: i64) -> Attestation {
        att(171, CONCLUSION, at)
    }

    // Legal moves in the ROOK_KING line.
    const RA1A4: &str = "[\"a1\",\"a4\",null]"; // first, step 1
    const KE8E7: &str = "[\"e8\",\"e7\",null]"; // second, step 1
    const RA4A5: &str = "[\"a4\",\"a5\",null]"; // first, step 2

    #[test]
    fn complete_consecutive_chain() {
        let plies = [
            ply(1, FIRST, 1, RA1A4),
            ply(2, SECOND, 1, KE8E7),
            ply(3, FIRST, 2, RA4A5),
        ];
        let atts = [
            att(101, 1, 100),
            att(102, 2, 200),
            att(103, 3, 300),
            cutoff_att(1000),
        ];
        let ns = natural_state(&params(), &plies, &atts, cutoff(&atts));
        assert_eq!(ns.chain.len(), 3);
        assert_eq!(ns.next_half_move(), 4);
        assert_eq!(*ns.chain[0].ply.id.as_bytes(), [1; 32]);
        assert_eq!(*ns.chain[2].ply.id.as_bytes(), [3; 32]);
        assert!(matches!(ns.end, ChainEnd::Ongoing(_)));
    }

    #[test]
    fn self_timed_chain_uses_event_created_at() {
        // No timestamper and no attestations: the chain is assembled from the plies'
        // own created_at, and the cutoff from the Conclusion's own.
        let plies = [
            ply_at(1, FIRST, 1, RA1A4, 100),
            ply_at(2, SECOND, 1, KE8E7, 200),
            ply_at(3, FIRST, 2, RA4A5, 300),
        ];
        let no_atts: Vec<Attestation> = Vec::new();
        let ns = natural_state(&params_self_timed(), &plies, &no_atts, ts(1000));
        assert_eq!(ns.chain.len(), 3);
        assert_eq!(ns.next_half_move(), 4);
        assert_eq!(*ns.chain[0].ply.id.as_bytes(), [1; 32]);
        assert_eq!(ns.chain[0].at, ts(100));
        assert!(matches!(ns.end, ChainEnd::Ongoing(_)));
    }

    #[test]
    fn self_timed_cutoff_excludes_a_later_ply() {
        // The Conclusion's own created_at is the cutoff: a ply created after it is excluded.
        let plies = [
            ply_at(1, FIRST, 1, RA1A4, 100),
            ply_at(2, SECOND, 1, KE8E7, 500),
        ];
        let no_atts: Vec<Attestation> = Vec::new();
        let ns = natural_state(&params_self_timed(), &plies, &no_atts, ts(300));
        assert_eq!(ns.chain.len(), 1); // ply 2 (created_at 500 > cutoff 300) is excluded
    }

    #[test]
    fn cutoff_inclusivity() {
        // A Ply attested exactly at the cutoff is included (the `≤` condition).
        let plies = [ply(1, FIRST, 1, RA1A4)];
        let atts = [att(101, 1, 1000), cutoff_att(1000)];
        let ns = natural_state(&params(), &plies, &atts, cutoff(&atts));
        assert_eq!(ns.chain.len(), 1);
    }

    #[test]
    fn cutoff_excludes_a_later_ply() {
        // Position 3 attested after the cutoff: excluded, the chain stops at 2.
        let plies = [
            ply(1, FIRST, 1, RA1A4),
            ply(2, SECOND, 1, KE8E7),
            ply(3, FIRST, 2, RA4A5),
        ];
        let atts = [
            att(101, 1, 100),
            att(102, 2, 200),
            att(103, 3, 2000),
            cutoff_att(1000),
        ];
        let ns = natural_state(&params(), &plies, &atts, cutoff(&atts));
        assert_eq!(ns.chain.len(), 2);
        assert_eq!(ns.next_half_move(), 3);
    }

    #[test]
    fn opponent_slot_cannot_be_filled() {
        // `first` premoves their own step 2 while `second` never plays step 1:
        // position 2 expects (second, step 1) — `first`'s extra Ply is a
        // future-slot Ply and cannot fill it. The chain stops at 1.
        let plies = [ply(1, FIRST, 1, RA1A4), ply(2, FIRST, 2, RA4A5)];
        let atts = [att(101, 1, 100), att(102, 2, 200), cutoff_att(1000)];
        let ns = natural_state(&params(), &plies, &atts, cutoff(&atts));
        assert_eq!(ns.chain.len(), 1);
        assert!(matches!(ns.end, ChainEnd::Ongoing(_)));
    }

    #[test]
    fn pending_ply_breaks_the_chain() {
        // (second, step 1) present but not attested: pending, excluded → chain of 1.
        let plies = [ply(1, FIRST, 1, RA1A4), ply(2, SECOND, 1, KE8E7)];
        let atts = [att(101, 1, 100), cutoff_att(1000)];
        let ns = natural_state(&params(), &plies, &atts, cutoff(&atts));
        assert_eq!(ns.chain.len(), 1);
    }

    #[test]
    fn gap_in_play_order_stops_the_chain() {
        let plies = [ply(1, FIRST, 1, RA1A4)];
        let atts = [att(101, 1, 100), cutoff_att(1000)];
        let ns = natural_state(&params(), &plies, &atts, cutoff(&atts));
        assert_eq!(ns.chain.len(), 1);
        assert_eq!(ns.next_half_move(), 2);
        assert!(!ns.is_empty());
    }

    #[test]
    fn deep_premove_activates_by_chain_progression() {
        // `first` publishes step 1 (informed @100) and step 2 (a premove @110,
        // attested before second's reply @200 — anterior to its slot 3); `second`
        // answers step 1 @200. The interleaved chain consumes all three, the
        // step-2 premove applying as a forgiving anterior selection.
        let plies = [
            ply(1, FIRST, 1, RA1A4),
            ply(3, FIRST, 2, RA4A5),
            ply(2, SECOND, 1, KE8E7),
        ];
        let atts = [
            att(101, 1, 100),
            att(103, 3, 110), // premove attested before second's reply
            att(102, 2, 200),
            cutoff_att(1000),
        ];
        let ns = natural_state(&params(), &plies, &atts, cutoff(&atts));
        assert_eq!(ns.chain.len(), 3);
        assert_eq!(*ns.chain[2].ply.id.as_bytes(), [3; 32]);
    }

    #[test]
    fn re_premove_correction_supersedes_illegal_premove() {
        // `first` plays Ra1-a4 informed @200. `second` premoved two candidates for
        // slot 2, both anterior (before 200): an illegal Ke8-e6 (older @50) and a
        // newer legal Ke8-e7 (@60). The anterior window binds the LATEST legal
        // premove: the illegal older one is skipped and the newer legal correction
        // fills the slot — chain of 2.
        let plies = [
            ply(1, FIRST, 1, RA1A4),
            ply(2, SECOND, 1, "[\"e8\",\"e6\",null]"), // illegal (king moves two), older @50
            ply(3, SECOND, 1, KE8E7),                  // legal, newer @60 -> wins
        ];
        let atts = [
            att(101, 1, 200),
            att(102, 2, 50),
            att(103, 3, 60),
            cutoff_att(1000),
        ];
        let ns = natural_state(&params(), &plies, &atts, cutoff(&atts));
        assert_eq!(ns.chain.len(), 2);
        assert_eq!(*ns.chain[1].ply.id.as_bytes(), [3; 32]);
        assert!(matches!(ns.end, ChainEnd::Ongoing(_)));
    }

    #[test]
    fn informed_illegal_is_skipped_leaving_ongoing() {
        // `first` plays Ra1-a4 @100 (applied); `second` then plays an informed
        // illegal move (Ke8-e6 @200, ≥ boundary 100). Under the two-window rule it is
        // skipped (no `illegalmove`), leaving the slot unfilled and the chain ongoing.
        let plies = [
            ply(1, FIRST, 1, RA1A4),
            ply(2, SECOND, 1, "[\"e8\",\"e6\",null]"),
        ];
        let atts = [att(101, 1, 100), att(102, 2, 200), cutoff_att(1000)];
        let ns = natural_state(&params(), &plies, &atts, cutoff(&atts));
        assert_eq!(ns.chain.len(), 1); // the illegal live move is skipped, not in the chain
        assert!(matches!(ns.end, ChainEnd::Ongoing(_)));
    }

    #[test]
    fn mating_move_terminates_the_chain() {
        // Ra1-a8 mates the walled-in black King: a rule-system ending surfaced by
        // applying the move.
        let plies = [ply(1, FIRST, 1, "[\"a1\",\"a8\",null]")];
        let atts = [att(101, 1, 100), cutoff_att(1000)];
        let p = params_feen("7k^/6pp/8/8/8/8/8/R3K^3 / W/w");
        let ns = natural_state(&p, &plies, &atts, cutoff(&atts));
        assert_eq!(ns.chain.len(), 1); // the mating move is part of the chain
        match ns.end {
            ChainEnd::Terminal { verdict, at } => {
                assert_eq!(verdict.status(), Status::Checkmate);
                assert_eq!(at, ts(100));
            }
            ChainEnd::Ongoing(_) | ChainEnd::Inconsistent => {
                panic!("expected a checkmate termination")
            }
        }
    }

    #[test]
    fn empty_chain_if_no_first_ply() {
        let plies: [Ply; 0] = [];
        let atts = [cutoff_att(1000)];
        let ns = natural_state(&params(), &plies, &atts, cutoff(&atts));
        assert!(ns.is_empty());
        assert_eq!(ns.next_half_move(), 1);
        assert!(matches!(ns.end, ChainEnd::Ongoing(_)));
    }

    #[test]
    fn identical_candidates_collapse_to_the_one_the_window_reaches_first() {
        // Two identical premoves for (second, step 1) — @50 id 2 and a re-sign
        // @60 id 3: one candidate for the cap, represented by the one the
        // anterior scan (newest-first) reaches first, id 3 @60 — exactly what
        // the selection would pick with no collapse at all. The collapse
        // never changes the selected move, and a premove's timing charges no
        // clock and moves no anchor.
        let plies = [
            ply(1, FIRST, 1, RA1A4),
            ply(2, SECOND, 1, KE8E7),
            ply(3, SECOND, 1, KE8E7),
        ];
        let atts = [
            att(101, 1, 200),
            att(102, 2, 50),
            att(103, 3, 60),
            cutoff_att(1000),
        ];
        let ns = natural_state(&params(), &plies, &atts, cutoff(&atts));
        assert_eq!(ns.chain.len(), 2);
        assert_eq!(*ns.chain[1].ply.id.as_bytes(), [3; 32]);
        assert_eq!(ns.chain[1].at, ts(60));

        // In the informed window the earliest wins, with or without the
        // collapse: the same pair timed after the boundary selects id 2.
        let atts = [
            att(101, 1, 20),
            att(102, 2, 50),
            att(103, 3, 60),
            cutoff_att(1000),
        ];
        let ns = natural_state(&params(), &plies, &atts, cutoff(&atts));
        assert_eq!(*ns.chain[1].ply.id.as_bytes(), [2; 32]);
    }

    #[test]
    fn a_re_premove_back_to_an_earlier_content_supersedes() {
        // second premoves Ke8-e7 @50, changes their mind to Ke8-d8 @60, and
        // changes it back to Ke8-e7 @70 — all anterior to first's move @200.
        // The most recent intent is Ke8-e7. A collapse keyed on content that
        // kept the EARLIEST duplicate (@50) would hand the slot to Ke8-d8 @60:
        // the collapse must never change the selection.
        let plies = [
            ply(1, FIRST, 1, RA1A4),
            ply(2, SECOND, 1, KE8E7),
            ply(3, SECOND, 1, "[\"e8\",\"d8\",null]"),
            ply(4, SECOND, 1, KE8E7),
        ];
        let atts = [
            att(101, 1, 200),
            att(102, 2, 50),
            att(103, 3, 60),
            att(104, 4, 70),
            cutoff_att(1000),
        ];
        let ns = natural_state(&params(), &plies, &atts, cutoff(&atts));
        assert_eq!(ns.chain.len(), 2);
        assert_eq!(*ns.chain[1].ply.id.as_bytes(), [4; 32]);
        assert_eq!(ns.chain[1].ply.content, KE8E7);
    }

    #[test]
    fn a_pair_straddling_the_boundary_is_two_candidates() {
        // The same content published once anterior (@50) and once informed
        // (@250, after first's move @200): two candidates, one per window,
        // whatever order the events arrive in. The anterior one wins (premoves
        // are scanned first); were it buried past the cap by newer premoves,
        // the informed twin would still be there to take the slot.
        let plies = [
            ply(1, FIRST, 1, RA1A4),
            ply(2, SECOND, 1, KE8E7),
            ply(3, SECOND, 1, KE8E7),
        ];
        let atts = [
            att(101, 1, 200),
            att(102, 2, 50),
            att(103, 3, 250),
            cutoff_att(1000),
        ];
        for order in [[0_usize, 1, 2], [0, 2, 1], [2, 1, 0]] {
            let permuted: Vec<Ply> = order.iter().map(|&i| plies[i].clone()).collect();
            let ns = natural_state(&params(), &permuted, &atts, cutoff(&atts));
            assert_eq!(*ns.chain[1].ply.id.as_bytes(), [2; 32], "order {order:?}");
            assert_eq!(ns.chain[1].at, ts(50));
        }
        // Buried: eight newer premoves of eight DISTINCT illegal contents
        // (identical ones would collapse) fill the anterior cap ahead of the
        // @50 twin; the anterior window yields nothing within its cap, and
        // the informed twin — a separate candidate — takes the slot.
        let illegal = [
            "[\"e8\",\"e6\",null]",
            "[\"e8\",\"e5\",null]",
            "[\"e8\",\"e4\",null]",
            "[\"e8\",\"e3\",null]",
            "[\"e8\",\"a8\",null]",
            "[\"e8\",\"h8\",null]",
            "[\"e8\",\"b5\",null]",
            "[\"e8\",\"c6\",null]",
        ];
        let mut flood: Vec<Ply> = plies.to_vec();
        let mut flood_atts = atts.to_vec();
        for (i, content) in illegal.iter().enumerate() {
            let id = 10_u8.saturating_add(u8::try_from(i).expect("small"));
            flood.push(ply(id, SECOND, 1, content));
            flood_atts.push(att(
                100_u8.saturating_add(id),
                id,
                60 + i64::try_from(i).expect("small"),
            ));
        }
        let ns = natural_state(&params(), &flood, &flood_atts, cutoff(&flood_atts));
        assert_eq!(*ns.chain[1].ply.id.as_bytes(), [3; 32], "the informed twin");
        assert_eq!(ns.chain[1].at, ts(250));
    }

    #[test]
    fn pre_t0_candidates_are_ignored() {
        // A Ply timed before t₀ is invalid (kind 3423 §Time accounting) and
        // never enters its slot — deciders' confirmation of 2026-07-19.
        let plies = [ply(1, FIRST, 1, RA1A4)];
        let atts = [att(101, 1, -5), cutoff_att(1000)];
        let ns = natural_state(&params(), &plies, &atts, cutoff(&atts));
        assert!(ns.is_empty());
        assert!(matches!(ns.end, ChainEnd::Ongoing(_)));
    }

    #[test]
    fn played_ply_timeout_terminates_the_chain() {
        // A legal ply timed beyond the mover's 600 s budget (@700): the replay
        // surfaces the played-Ply timeout as the terminal conclusion, anchored
        // at that ply's canonical timing.
        use sashite_sanki_engine::domain::status::Status;

        let plies = [ply(1, FIRST, 1, RA1A4)];
        let atts = [att(101, 1, 700), cutoff_att(1000)];
        let ns = natural_state(&params(), &plies, &atts, cutoff(&atts));
        assert_eq!(ns.chain.len(), 1);
        match ns.end {
            ChainEnd::Terminal { verdict, at } => {
                assert_eq!(verdict.status(), Status::Timeout);
                assert_eq!(at, ts(700));
            }
            ChainEnd::Ongoing(_) | ChainEnd::Inconsistent => {
                panic!("expected a played-ply timeout")
            }
        }
    }

    #[test]
    fn a_founding_position_with_second_to_move_is_refused_at_construction() {
        // The replay maps play-order position 1 to `(first, step 1)` under
        // Sanki's strict alternation (kind `3423` §Step semantics and play
        // order), so the founding position of kind `3422` must have `first` on
        // move — as the position the rule-system document prescribes does.
        // A position with `second` on move — a real one, the chess/ōgi start
        // after 1.g2-g4 — would make slot 1 unfillable for good and charge
        // `second`'s abandonment from t₀ while their genuine Ply waits for a
        // slot that never comes. `SessionParams::new` refuses it, so no such
        // session is ever evaluated.
        let period = Period::new(Duration::from_secs(600), None, None).expect("valid period");
        let refused = SessionParams::new(
            eid(SESSION),
            Some(pk(TIMESTAMPER)),
            Seats::new(pk(FIRST), pk(SECOND)).expect("distinct"),
            TimeControl::new(period, Vec::new()),
            Position::parse(
                "-rnbik^bn-r/+f+f+f+f+f+f+f+f/8/8/6P1/8/+P+P+P+P+P+P1+P/-RNBQK^BN-R / j/W",
            )
            .expect("valid FEEN"),
            ts(0),
        );
        assert!(refused.is_none());
    }

    /// The probe as the replay applies it to a raw content: parsed once, an
    /// unparseable content being illegal.
    fn is_legal_content(
        state: &sashite_sanki_engine::kernel::state::SessionState,
        content: &str,
    ) -> bool {
        sashite_sanki_engine::domain::half_move::Move::parse(content)
            .is_ok_and(|mv| super::is_legal(state, &mv))
    }

    #[test]
    fn is_legal_matches_the_kernel_step_oracle() {
        use sashite_sanki_engine::domain::half_move::Move;
        use sashite_sanki_engine::domain::time_control::{Period, TimeControl};
        use sashite_sanki_engine::kernel::state::SessionState;
        use sashite_sanki_engine::kernel::step::step;

        // The kernel-step oracle: since engine 0.5 an illegal ply is a
        // `StepResult::Illegal` rejection. The validate-based `is_legal` must
        // agree on every legality class — the façade/kernel alignment this
        // crate relies on. A disagreement is not academic: `select_candidate`
        // only ever returns a candidate the probe called legal, so a candidate
        // `validate` accepts and `step` rejects would fall into
        // [`natural_state`]'s defensive `StepResult::Illegal` seam and silently
        // turn a played game into an unfinished one.
        //
        // The table below covers **all nine variant pairings** of the Sanki
        // suite (kind `3422` fixes the per-player variants through the initial
        // position's styles) and every move shape only some of them have:
        // castling in chess, ōgi and xiongqi alike (deciders' ruling
        // 2026-07-27, `rules-of-ogi.md` / `rules-of-xiongqi.md` §Castling);
        // the chess Pawn's diagonal en passant, the xiongqi Soldier's
        // **sideways** en passant past the river, and the ōgi Fu's refusal of
        // both; promotion with and without an actor ("Move Encoding — Sanki"
        // §Actor); ōgi drops — uchifuzume included, against an ōgi King, a
        // chess King and a xiongqi General — and the inert cross-variant tray
        // a chess or xiongqi capturer keeps, which is droppable by neither
        // side. Every position was reached by legal play from the published
        // per-variant starting positions, and every expected legality below
        // was observed from the engine before being written down.
        let oracle = |state: &SessionState, content: &str| {
            let Ok(mv) = Move::parse(content) else {
                return false;
            };
            !matches!(
                step(state.clone(), &mv, ts(30)),
                sashite_sanki_engine::kernel::step::StepResult::Illegal { .. }
            )
        };

        let state = |feen: &str, secs: u64| {
            let period = Period::new(Duration::from_secs(secs), None, None).expect("valid period");
            SessionState::start(
                Position::parse(feen).expect("valid FEEN"),
                TimeControl::new(period, Vec::new()),
                ts(0),
            )
        };

        // `(position, clock bank, content, expected legality, label)`.
        let cases: &[(&str, u64, &str, bool, &str)] = &[
            (
                "4k^3/8/8/8/8/8/8/R3K^3 / W/w",
                600,
                "[\"a1\",\"a4\",null]",
                true,
                "W/w rook move",
            ),
            (
                "4k^3/8/8/8/8/8/8/R3K^3 / W/w",
                600,
                "[\"a1\",\"b3\",null]",
                false,
                "W/w unreachable destination",
            ),
            (
                "4k^3/8/8/8/8/8/8/R3K^3 / W/w",
                600,
                "[\"e8\",\"e7\",null]",
                false,
                "W/w opponent's piece",
            ),
            (
                "4k^3/8/8/8/8/8/8/R3K^3 / W/w",
                600,
                "[\"h4\",\"h5\",null]",
                false,
                "W/w empty source",
            ),
            (
                "4k^3/8/8/8/8/8/8/R3K^3 / W/w",
                600,
                "not a ply",
                false,
                "W/w unparseable content",
            ),
            (
                "4k^3/8/8/8/8/8/8/R3K^3 / W/w",
                5,
                "[\"a1\",\"a4\",null]",
                true,
                "W/w legal but out of time",
            ),
            (
                "+r3k^1n1/3+pb+p2/b3p2r/1pp3P1/3nP1Pp/2PK^3P/RB+P+PB3/2Q3R1 2pq/2NP w/W",
                600,
                "[\"e8\",\"c8\",null]",
                true,
                "W/w castling queenside",
            ),
            (
                "+r3k^1n1/3+pb+p2/b3p2r/1pp3P1/3nP1Pp/2PK^3P/RB+P+PB3/2Q3R1 2pq/2NP w/W",
                600,
                "[\"e8\",\"g8\",null]",
                false,
                "W/w castling kingside without a rook",
            ),
            (
                "-r1b1k^1n1/1+p1+pb+p1r/4p3/2p3p1/3nPP-Pp/q1NK^3P/R+P+P+PB3/2BQ2R1 p/NP w/W",
                600,
                "[\"h4\",\"g3\",null]",
                true,
                "W/w pawn takes en passant",
            ),
            (
                "5k^2/3P1+pr1/2P5/6b1/p5Pp/2r5/6R1/2n2K^2 5pbnq/5P2B2NQR W/w",
                600,
                "[\"d7\",\"d8\",\"queen\"]",
                true,
                "W/w promotion naming an actor",
            ),
            (
                "5k^2/3P1+pr1/2P5/6b1/p5Pp/2r5/6R1/2n2K^2 5pbnq/5P2B2NQR W/w",
                600,
                "[\"d7\",\"d8\",null]",
                false,
                "W/w promotion without an actor",
            ),
            (
                "5k^2/3P1+pr1/2P5/6b1/p5Pp/2r5/6R1/2n2K^2 5pbnq/5P2B2NQR W/w",
                600,
                "[\"d7\",\"d8\",\"fu\"]",
                false,
                "W/w promotion naming an ogi actor",
            ),
            (
                "5k^2/3P1+pr1/2P5/6b1/p5Pp/2r5/6R1/2n2K^2 5pbnq/5P2B2NQR W/w",
                600,
                "[null,\"d5\",\"queen\"]",
                false,
                "W/w drop by a chess side",
            ),
            (
                "-rnb1k^bn-r/1+f2+f+f+f+f/f1f1i3/3f4/2P1P3/N5P1/+P+P1+P1+P1+P/-R1BQK^BN-R / W/j",
                600,
                "[\"c4\",\"d5\",null]",
                true,
                "W/j chess captures an ogi Fu",
            ),
            (
                "-rnb1k^bn-r/1+f1+f+f+f+f+f/f1f1i3/8/2P1P3/N5P1/+P+P1+P1+P1+P/-R1BQK^BN-R / j/W",
                600,
                "[\"e6\",\"c4\",null]",
                true,
                "W/j ogi captures a chess Pawn",
            ),
            (
                "rnb2br1/+f1ik^+f+f+f+f/8/1fff1P2/4n3/1N4P1/+P+P+P+P2B+P/-RNBQK^2+R /f W/j",
                600,
                "[\"e1\",\"g1\",null]",
                true,
                "W/j chess castles in a cross-variant session",
            ),
            (
                "-rnb1k^2+r/1+f+f+f+f+fb1/5i2/f6f/3P4/P4PPP/2+P1+P3/-RN1QK^BN-R fn/2f j/W",
                600,
                "[\"e8\",\"g8\",null]",
                true,
                "W/j ogi castles in a cross-variant session",
            ),
            (
                "3R4/3f+f2+f/5f2/5P-fP/Nf1k^4/3P4/+P2+P1K^2/RNB4b 7f2n2rbi/ W/j",
                600,
                "[\"f5\",\"g6\",null]",
                true,
                "W/j chess Pawn takes an ogi Fu en passant",
            ),
            (
                "1nb2bnr/r+fik^+f+f+f+f/f2P4/2f2P2/8/N5P1/+P+P1+PN2+P/-R1BQK^B1-R f/f j/W",
                600,
                "[null,\"d3\",\"fu\"]",
                true,
                "W/j ogi drops against a chess opponent",
            ),
            (
                "-rnb1k^b1-r/1+f+f+f+f+f1+f/4i3/P5f1/4n3/N6P/+P1+P+P+P+P+P1/1RBQK^BN-R f/ W/j",
                600,
                "[null,\"d4\",\"fu\"]",
                false,
                "W/j chess side drops its own inert tray",
            ),
            (
                "-rnb1k^b1-r/1R+f+f+f+f1+f/4i3/P5f1/4n3/N6P/+P1+P+P+P+P+P1/2BQK^BN-R 2f/ j/W",
                600,
                "[null,\"d4\",\"fu\"]",
                false,
                "W/j ogi side drops the OPPONENT's inert tray",
            ),
            (
                "1nb1ibnr/r+f1k^P+f1+f/f2P2f1/8/2f5/N5P1/+P+P1+PN2+P/-R1BQK^B1-R 2f/f W/j",
                600,
                "[\"e7\",\"f8\",\"queen\"]",
                true,
                "W/j chess promotes in a cross-variant session",
            ),
            (
                "1nb1iQn1/r+fk^2+f2/f5f1/4f3/2f2Q1f/N5P1/+P+P1f3+P/-R1B1K^BN-R 2fbr/f j/W",
                600,
                "[\"d2\",\"d1\",null]",
                true,
                "W/j ogi promotes without an actor",
            ),
            (
                "7k^/8/5N2/8/8/8/8/4K^1R1 F/ J/j",
                600,
                "[null,\"h7\",\"fu\"]",
                false,
                "J/j mating Fu drop (uchifuzume)",
            ),
            (
                "7k^/8/5N2/8/8/8/8/4K^1R1 F/ J/j",
                600,
                "[null,\"h6\",\"fu\"]",
                true,
                "J/j quiet Fu drop",
            ),
            (
                "7k^/8/5N2/8/8/8/8/4K^1R1 F/ J/j",
                600,
                "[null,\"h8\",\"fu\"]",
                false,
                "J/j drop on an occupied square",
            ),
            (
                "-rn2k^bn-r/1b2+f1+f+f/2ff1fN1/1f4B1/f7/FFIF2FF/2+F1+F+F1R/+R3K^B2 I/n J/j",
                600,
                "[\"e1\",\"c1\",null]",
                true,
                "J/j ogi castles",
            ),
            (
                "rnb2k^1r/1+f1+fn2+f/f1f2f2/2F3f1/FF2fN1F/1I1F2b1/3K^+F+F+F1/1RB2B1R I/n j/J",
                600,
                "[null,\"a1\",\"knight\"]",
                true,
                "J/j ogi drops a Knight",
            ),
            (
                "rnb4r/1+fF2rk^+f/f2f1f1n/i3n1fF/FF2f1n1/I2F4/4+F+F+Fb/R1BK^1B2 F/ J/j",
                600,
                "[\"c7\",\"c8\",null]",
                true,
                "J/j ogi promotes without an actor",
            ),
            (
                "7k^/8/5N2/8/8/8/8/4K^1R1 F/ J/w",
                600,
                "[null,\"h7\",\"fu\"]",
                false,
                "J/w mating Fu drop against a chess King",
            ),
            (
                "7k^/8/5N2/8/8/8/8/4K^1R1 F/ J/w",
                600,
                "[null,\"g2\",\"fu\"]",
                true,
                "J/w quiet Fu drop against a chess King",
            ),
            (
                "1r2k^3/8/8/8/8/2n5/8/K^7 /f j/W",
                600,
                "[null,\"a2\",\"fu\"]",
                false,
                "J/w mating Fu drop by a second-side dropper",
            ),
            (
                "1r2k^3/8/8/8/8/2n5/8/K^7 /f j/W",
                600,
                "[null,\"d4\",\"fu\"]",
                true,
                "J/w quiet Fu drop by a second-side dropper",
            ),
            (
                "-r1bqk^bn-r/+p+p+p+p+p+p+p1/n6p/8/2F5/2I5/+F+F1+F+F+F+F+F/-RNB1K^BN-R / J/w",
                600,
                "[\"c3\",\"g7\",null]",
                true,
                "J/w ogi Princess captures a chess Pawn",
            ),
            (
                "-r1bq-k^bn-r/+p+p+p+p+p+pI1/n6p/8/2F5/8/+F+F1+F+F+F+F+F/-RNB1K^BN-R F/ w/J",
                600,
                "[\"f8\",\"g7\",null]",
                true,
                "J/w chess Bishop captures an ogi Princess",
            ),
            (
                "1n1k^1Bnr/1+p2+p2+p/r1p2p2/pN1p4/4q1I1/F1FF4/1+F2N1+F+F/+R3K^B1-R 3F/2F J/w",
                600,
                "[\"e1\",\"c1\",null]",
                true,
                "J/w ogi castles queenside",
            ),
            (
                "-rnb1k^2+r/1+p1+p+p+p1+p/p6n/2F3q1/8/2bFF2N/+F2B1+F+F+F/-RN2K^BR1 F/2FI w/J",
                600,
                "[\"e8\",\"g8\",null]",
                true,
                "J/w chess castles kingside",
            ),
            (
                "r1bnqbn1/+p1+p+p+p1+p1/4k^2r/2p5/5-Fp1/F1FF4/1+F2+F2+F/-RN2K^BF-R /BFIN w/J",
                600,
                "[\"g4\",\"f3\",null]",
                true,
                "J/w chess Pawn takes an ogi Fu en passant",
            ),
            (
                "-rnbqk^bn-r/+p1+p+p+p+p+p+p/F7/1p6/8/8/1+F+F+F+F+F+F+F/-RNBIK^BN-R / J/w",
                600,
                "[\"a6\",\"b6\",null]",
                false,
                "J/w ogi Fu never takes en passant (sideways)",
            ),
            (
                "-rnbqk^bn-r/+p1+p+p+p+p+p+p/F7/1p6/8/8/1+F+F+F+F+F+F+F/-RNBIK^BN-R / J/w",
                600,
                "[\"a6\",\"b7\",null]",
                false,
                "J/w ogi Fu never takes en passant (diagonally)",
            ),
            (
                "-rnbqk^bn-r/+p1+p+p+p+p+p+p/F7/1p6/8/8/1+F+F+F+F+F+F+F/-RNBIK^BN-R / J/w",
                600,
                "[\"a6\",\"a7\",null]",
                true,
                "J/w ogi Fu captures straight ahead",
            ),
            (
                "-rnbeg^bn-r/1+s1+s+s+s+s+s/s1s5/8/2F5/2I5/+F+F1+F+F+F+F+F/-RNB1K^BN-R / J/c",
                600,
                "[\"c3\",\"g7\",null]",
                true,
                "J/c ogi Princess captures a xiongqi Soldier",
            ),
            (
                "-rnbe-g^bn-r/1+s1+s+s+sI+s/s1s5/8/2F5/8/+F+F1+F+F+F+F+F/-RNB1K^BN-R F/ c/J",
                600,
                "[\"f8\",\"g7\",null]",
                true,
                "J/c xiongqi Bear captures an ogi Princess",
            ),
            (
                "rnb2rg^1/1+s1+s+s2+s/2s2ssb/s7/4FN2/2FF4/+F+F1N1+F+F+F/+R3K^BR1 2F/BI J/c",
                600,
                "[\"e1\",\"c1\",null]",
                true,
                "J/c ogi castles queenside",
            ),
            (
                "-r1beg^2+r/1+s1+s+s+s1+s/n1s4n/s1F5/4F2F/1FN5/+Fb1+F1+F+F1/-R1B1K^BN-R F/I c/J",
                600,
                "[\"e8\",\"g8\",null]",
                true,
                "J/c xiongqi General castles kingside",
            ),
            (
                "1rbe1rg^1/1+s1+s+s+s1+s/2s4n/s1n5/4F2F/1FN5/+Fb1+F1+F+FR/-R1B1K^BN1 F/FI J/c",
                600,
                "[null,\"c2\",\"fu\"]",
                true,
                "J/c ogi drops against a xiongqi opponent",
            ),
            (
                "3T1g^2/3F4/2s1s3/rF3n1s/2s3-F1/5F1s/1I2B2R/1K^6 /11F2NBR c/J",
                600,
                "[\"h3\",\"g3\",null]",
                true,
                "J/c xiongqi Soldier takes an ogi Fu sideways en passant",
            ),
            (
                "7g^/8/5N2/8/8/8/8/4K^1R1 F/ J/c",
                600,
                "[null,\"h7\",\"fu\"]",
                false,
                "J/c mating Fu drop against a xiongqi General",
            ),
            (
                "7g^/8/5N2/8/8/8/8/4K^1R1 F/ J/c",
                600,
                "[null,\"h6\",\"fu\"]",
                true,
                "J/c quiet Fu drop against a xiongqi General",
            ),
            (
                "1n1eg^bn-r/rb6/s4sss/1ssss3/2SS2S1/E6N/+S+S3+SB+S/-RNB1G^2+R /S C/c",
                600,
                "[\"e1\",\"g1\",null]",
                true,
                "C/c xiongqi General castles kingside",
            ),
            (
                "1b1eg^2r/3b4/r1sss3/5SB1/1s1-S2Ss/2s1S2R/+S1+S2+SG^1/RN2EB2 2n2s/NS c/C",
                600,
                "[\"c3\",\"d3\",null]",
                true,
                "C/c Soldier takes sideways en passant past the river",
            ),
            (
                "1r6/1n1S2g^+s/1ss1ss2/rS6/1B3s2/3S4/1NR1G^1Be/6NR 2b2sn/5SE C/c",
                600,
                "[\"d7\",\"d8\",\"chariot\"]",
                true,
                "C/c xiongqi promotion naming an actor",
            ),
            (
                "1r6/1n1S2g^+s/1ss1ss2/rS6/1B3s2/3S4/1NR1G^1Be/6NR 2b2sn/5SE C/c",
                600,
                "[\"d7\",\"d8\",\"queen\"]",
                false,
                "C/c xiongqi promotion naming a chess actor",
            ),
            (
                "1r6/1n1S2g^+s/1ss1ss2/rS6/1B3s2/3S4/1NR1G^1Be/6NR 2b2sn/5SE C/c",
                600,
                "[null,\"d4\",\"chariot\"]",
                false,
                "C/c drop by a xiongqi side",
            ),
            (
                "-r1bqk^b1-r/+p+p+p+p+p+p+p1/n4n1p/8/2S1S3/8/+S+S1+S1+S+S+S/-RNBEG^BN-R / w/C",
                600,
                "[\"f6\",\"e4\",null]",
                true,
                "C/w chess Knight captures a xiongqi Soldier",
            ),
            (
                "-r1bqk^b1-r/+p+p+p+p+p+p1n/n6p/2S3p1/4S3/8/+S+S1+S1+S+S+S/-RNBEG^BN-R / C/w",
                600,
                "[\"f1\",\"a6\",null]",
                true,
                "C/w xiongqi Bear captures a chess Knight",
            ),
            (
                "-rnb1k^2+r/2+p+pb+p1n/pE3q1p/1BS1p3/4S1p1/1S3SS1/+SB1+S3+S/-RN2G^1N-R p/ w/C",
                600,
                "[\"e8\",\"g8\",null]",
                true,
                "C/w chess castles in a cross-variant session",
            ),
            (
                "rqb5/N1+p1nk^2/1p6/2npR1S1/8/2S1E3/3+S+S+S2/+R3G^B2 5pbr/3SBN C/w",
                600,
                "[\"e1\",\"c1\",null]",
                true,
                "C/w xiongqi General castles queenside",
            ),
            (
                "2b5/2+p4k^/1p6/5nS1/q2-SpS2/1BS1S3/2G^5/r2R4 5pbnr/3S2NBER w/C",
                600,
                "[\"e4\",\"d3\",null]",
                true,
                "C/w chess Pawn takes a xiongqi Soldier en passant",
            ),
            (
                "-rnbqk^bn-r/+p1+p+p+p+p+p+p/S7/1-p6/8/8/1+S+S+S+S+S+S+S/-RNBEG^BN-R / C/w",
                600,
                "[\"a6\",\"b6\",null]",
                true,
                "C/w Soldier takes a chess Pawn sideways en passant",
            ),
            (
                "-rnbqk^bn-r/+p1+p+p+p+p+p+p/S7/1-p6/8/8/1+S+S+S+S+S+S+S/-RNBEG^BN-R / C/w",
                600,
                "[\"a6\",\"b7\",null]",
                false,
                "C/w Soldier never captures diagonally",
            ),
            (
                "-rnb1k^bn-r/1+f1+f+f+f+f+f/f1f1i3/8/2S1S3/8/+S+S1+S1+S+S+S/-RNBEG^BN-R / j/C",
                600,
                "[\"e6\",\"c4\",null]",
                true,
                "C/j ogi Princess captures a xiongqi Soldier",
            ),
            (
                "-rnb1k^bn-r/1+f2+f+f+f+f/f1f5/2Sf1i2/4S3/8/+S+S1+S1+S+S+S/-RNBEG^BN-R / C/j",
                600,
                "[\"f1\",\"a6\",null]",
                true,
                "C/j xiongqi Bear captures an ogi Fu",
            ),
            (
                "-rnb1k^b1-r/+f1+f4+f/R2ff3/1f2E1f1/2N1SfBS/8/1+S+S2+S1R/2i1G^1N1 3fn/f C/j",
                600,
                "[\"e1\",\"c1\",null]",
                true,
                "C/j xiongqi General captures at Chariot range, not castling",
            ),
            (
                "1rb1k^2+r/1+f+f+f1+f+f+f/4B3/2bnf3/f1N5/3nS1S1/R+S3+S1+S/3EBG^NR fi/2f j/C",
                600,
                "[\"e8\",\"g8\",null]",
                true,
                "C/j ogi castles kingside",
            ),
            (
                "rnb2b1r/1+f1k^+f+f+f+f/f1f2n2/3SS3/1S6/2N3S1/+S2+S1+S1+S/-R1BEG^Bi-R f/f j/C",
                600,
                "[null,\"d3\",\"fu\"]",
                true,
                "C/j ogi drops against a xiongqi opponent",
            ),
            (
                "r4Br1/1f+f5/f1S4f/3-ffffS/S1S2nk^1/3SS3/8/R2E1G^N1 4f2bin/f C/j",
                600,
                "[\"c6\",\"d6\",null]",
                true,
                "C/j Soldier takes an ogi Fu sideways en passant",
            ),
            (
                "r4Br1/2S5/ff5f/3ffffn/S1S3k^1/3SS3/8/R1E2G^N1 5f2bin/2f C/j",
                600,
                "[\"c7\",\"c8\",\"chariot\"]",
                true,
                "C/j xiongqi promotes in a cross-variant session",
            ),
            (
                "rf1fk^f2/4f2r/f6B/S1S5/6f1/2NG^1SSS/2f5/b5R1 8f2nbi/ j/C",
                600,
                "[\"c2\",\"c1\",null]",
                true,
                "C/j ogi promotes without an actor",
            ),
        ];

        for (feen, secs, content, expected, label) in cases {
            let s = state(feen, *secs);
            assert_eq!(
                Position::parse(feen).expect("valid FEEN").to_feen(),
                *feen,
                "{label}: the fixture FEEN is not canonical"
            );
            assert_eq!(
                is_legal_content(&s, content),
                oracle(&s, content),
                "probe/oracle divergence on {label}: {content} in {feen} ({secs} s bank)"
            );
            assert_eq!(
                is_legal_content(&s, content),
                *expected,
                "legality class drifted on {label}: {content} in {feen}"
            );
        }
    }

    /// The nine variant pairings, each from the published per-variant starting
    /// positions of the Sanki suite: the second player's two home ranks over the
    /// first player's two, under the pairing's `<first>/<second>` SIN styles. The
    /// chess/ōgi entry is byte-identical to the engine's own `MIXED_START`
    /// fixture.
    const PAIRING_STARTS: [&str; 9] = [
        "-rnbqk^bn-r/+p+p+p+p+p+p+p+p/8/8/8/8/+P+P+P+P+P+P+P+P/-RNBQK^BN-R / W/w",
        "-rnbik^bn-r/+f+f+f+f+f+f+f+f/8/8/8/8/+P+P+P+P+P+P+P+P/-RNBQK^BN-R / W/j",
        "-rnbeg^bn-r/+s+s+s+s+s+s+s+s/8/8/8/8/+P+P+P+P+P+P+P+P/-RNBQK^BN-R / W/c",
        "-rnbqk^bn-r/+p+p+p+p+p+p+p+p/8/8/8/8/+F+F+F+F+F+F+F+F/-RNBIK^BN-R / J/w",
        "-rnbik^bn-r/+f+f+f+f+f+f+f+f/8/8/8/8/+F+F+F+F+F+F+F+F/-RNBIK^BN-R / J/j",
        "-rnbeg^bn-r/+s+s+s+s+s+s+s+s/8/8/8/8/+F+F+F+F+F+F+F+F/-RNBIK^BN-R / J/c",
        "-rnbqk^bn-r/+p+p+p+p+p+p+p+p/8/8/8/8/+S+S+S+S+S+S+S+S/-RNBEG^BN-R / C/w",
        "-rnbik^bn-r/+f+f+f+f+f+f+f+f/8/8/8/8/+S+S+S+S+S+S+S+S/-RNBEG^BN-R / C/j",
        "-rnbeg^bn-r/+s+s+s+s+s+s+s+s/8/8/8/8/+S+S+S+S+S+S+S+S/-RNBEG^BN-R / C/c",
    ];

    #[test]
    #[ignore = "exhaustive: 486_852 probe pairs over 20 positions, ~2.6 s in a debug build — seven times the rest of the suite. Run with `cargo test -- --ignored`."]
    fn is_legal_matches_the_kernel_step_oracle_exhaustively() {
        use sashite_sanki_engine::domain::half_move::Move;
        use sashite_sanki_engine::domain::square::Square;
        use sashite_sanki_engine::domain::time_control::{Period, TimeControl};
        use sashite_sanki_engine::kernel::state::SessionState;
        use sashite_sanki_engine::kernel::step::{step, StepResult};

        // The `is_legal` / `step` seam, hunted rather than sampled: over each
        // position below, EVERY well-formed content is put to both sides —
        // every ordered pair of distinct squares with a null actor, the same
        // with each of the three variants' actor vocabularies ("Move Encoding
        // — Sanki" §Actor), and every drop of every actor on every square.
        // `validate` resolves legality only, while `step` additionally applies
        // and canonicalizes the resolved effect, so a divergence here would be
        // an `IllegalReason::Malformed` — the broken-invariant seam
        // [`natural_state`] degrades through.
        //
        // This is the committed, deterministic residue of a wider search: the
        // same differential run in release mode over 10,534 positions drawn
        // from capture-biased random self-play across all nine pairings also
        // found no divergence.
        const ACTORS: [&str; 12] = [
            // The three vocabularies, plus names no variant knows.
            "queen", "rook", "bishop", "knight", "fu", "princess", "chariot", "bear", "empress",
            "king", "soldier", "zz",
        ];
        const FILES: [char; 8] = ['a', 'b', 'c', 'd', 'e', 'f', 'g', 'h'];
        const RANKS: [char; 8] = ['1', '2', '3', '4', '5', '6', '7', '8'];
        let name = |square: Square| {
            format!(
                "{}{}",
                FILES[usize::from(square.file())],
                RANKS[usize::from(square.rank())]
            )
        };

        // The nine pairing starts, plus cross-variant midgames carrying hands,
        // castling rights, en-passant markers and inert trays.
        let mut positions: Vec<&str> = PAIRING_STARTS.to_vec();
        positions.extend_from_slice(&[
            "-rnb1k^b1-r/1R+f+f+f+f1+f/4i3/P5f1/4n3/N6P/+P1+P+P+P+P+P1/2BQK^BN-R 2f/ j/W",
            "1nb2bnr/r+fik^+f+f+f+f/f2P4/2f2P2/8/N5P1/+P+P1+PN2+P/-R1BQK^B1-R f/f j/W",
            "3R4/3f+f2+f/5f2/5P-fP/Nf1k^4/3P4/+P2+P1K^2/RNB4b 7f2n2rbi/ W/j",
            "-rnb1k^2+r/1+f+f+f+f+fb1/5i2/f6f/3P4/P4PPP/2+P1+P3/-RN1QK^BN-R fn/2f j/W",
            "7k^/8/5N2/8/8/8/8/4K^1R1 F/ J/w",
            "7g^/8/5N2/8/8/8/8/4K^1R1 F/ J/c",
            "1rbe1rg^1/1+s1+s+s+s1+s/2s4n/s1n5/4F2F/1FN5/+Fb1+F1+F+FR/-R1B1K^BN1 F/FI J/c",
            "r4Br1/1f+f5/f1S4f/3-ffffS/S1S2nk^1/3SS3/8/R2E1G^N1 4f2bin/f C/j",
            "1b1eg^2r/3b4/r1sss3/5SB1/1s1-S2Ss/2s1S2R/+S1+S2+SG^1/RN2EB2 2n2s/NS c/C",
            "2b5/2+p4k^/1p6/5nS1/q2-SpS2/1BS1S3/2G^5/r2R4 5pbnr/3S2NBER w/C",
            "rqb5/N1+p1nk^2/1p6/2npR1S1/8/2S1E3/3+S+S+S2/+R3G^B2 5pbr/3SBN C/w",
        ]);

        let mut probes: u64 = 0;
        for feen in &positions {
            let position = Position::parse(feen).expect("valid FEEN");
            assert_eq!(position.to_feen(), *feen, "non-canonical fixture {feen}");
            let period = Period::new(Duration::from_secs(600), None, None).expect("valid period");
            let state = SessionState::start(position, TimeControl::new(period, Vec::new()), ts(0));

            let mut check = |content: &str| {
                probes = probes.saturating_add(1);
                let oracle = match Move::parse(content) {
                    Ok(mv) => {
                        !matches!(step(state.clone(), &mv, ts(30)), StepResult::Illegal { .. })
                    }
                    Err(_) => false,
                };
                assert_eq!(
                    is_legal_content(&state, content),
                    oracle,
                    "probe/oracle divergence on {content} in {feen}"
                );
            };

            for from in Square::all() {
                let occupied = state.position().piece_at(from).is_some();
                for to in Square::all() {
                    if from == to {
                        continue;
                    }
                    check(&format!("[\"{}\",\"{}\",null]", name(from), name(to)));
                    if occupied {
                        for actor in ACTORS {
                            check(&format!(
                                "[\"{}\",\"{}\",\"{actor}\"]",
                                name(from),
                                name(to)
                            ));
                        }
                    }
                }
            }
            for to in Square::all() {
                for actor in ACTORS {
                    check(&format!("[null,\"{}\",\"{actor}\"]", name(to)));
                }
            }
        }
        assert_eq!(
            probes, 486_852,
            "the sweep no longer covers the expected cross-product"
        );
    }

    /// The collapse of identical candidates is keyed on the content AND the
    /// `draw` flag: an offer is part of what a Ply says, so a flagged twin
    /// and a flagless one are two candidates (Move Encoding — Sanki §Slot
    /// candidates and selection). Since each window keeps the candidate its
    /// scan reaches first, the flag's part in the identity can never change
    /// *which* candidate is selected — that twin would represent the pair
    /// either way. Its one observable effect is on the **cap**: a pair that
    /// differs only by the flag consumes two of the `K` window slots. That is
    /// the case pinned below, discriminatingly; the two leading cases are the
    /// selection-level ones the shared corpus carries.
    #[test]
    fn the_draw_flag_is_part_of_the_identity_of_a_candidate() {
        // Boundary T for second's slot is first's timing (500), so all of
        // second's candidates below are ANTERIOR premoves.
        let offer = |id: u8, content: &str| {
            Ply::new(
                eid(id),
                pk(SECOND),
                eid(SESSION),
                1,
                true,
                content.to_owned(),
                ts(0),
            )
        };
        let atts = [
            att(101, 1, 500),
            att(103, 3, 100),
            att(104, 4, 200),
            att(171, CONCLUSION, 1000),
        ];
        let p = params();

        // Same content, the later one carrying an offer: the latest legal
        // premove — the offer — takes the slot (as it would with no flag in
        // the key: the collapse keeps the scan's first).
        let with_offer = [
            ply(1, FIRST, 1, RA1A4),
            ply(3, SECOND, 1, KE8E7),
            offer(4, KE8E7),
        ];
        let natural = natural_state(&p, &with_offer, &atts, cutoff(&atts));
        assert_eq!(natural.chain.len(), 2);
        let tail = natural.chain.last().expect("a tail");
        assert_eq!(tail.ply.id, eid(4));
        assert!(tail.ply.draw, "the offer stands");

        // Same content, both flagless: one candidate, represented by the
        // latest (the scan's first) — the selection is unchanged either way.
        let twins = [
            ply(1, FIRST, 1, RA1A4),
            ply(3, SECOND, 1, KE8E7),
            ply(4, SECOND, 1, KE8E7),
        ];
        let natural = natural_state(&p, &twins, &atts, cutoff(&atts));
        let tail = natural.chain.last().expect("a tail");
        assert_eq!(tail.ply.id, eid(4));
        assert!(!tail.ply.draw);

        // THE DISCRIMINATING CASE — the cap. K = 3; four anterior premoves,
        // newest first: Y @130 (illegal), X+draw @120 (illegal), X @110
        // (illegal), Z @100 (legal). With the flag in the key, X and X+draw
        // are two candidates and the window is {Y, X+draw, X} — three illegal
        // moves, the slot unfilled, Z never probed. Were the flag ignored, X
        // and X+draw would collapse to one candidate (X+draw, the scan's
        // first) and the window {Y, X+draw, Z} would reach Z.
        const X: &str = "[\"e8\",\"e6\",null]"; // illegal: the king moves two
        const Y: &str = "[\"e8\",\"e5\",null]"; // illegal
        let capped = [
            ply(1, FIRST, 1, RA1A4),
            ply(2, SECOND, 1, KE8E7), // Z, legal
            ply(3, SECOND, 1, X),
            offer(4, X),
            ply(5, SECOND, 1, Y),
        ];
        let capped_atts = [
            att(101, 1, 500),
            att(102, 2, 100),
            att(103, 3, 110),
            att(104, 4, 120),
            att(105, 5, 130),
            att(171, CONCLUSION, 1000),
        ];
        let three = core::num::NonZeroUsize::new(3).expect("non-zero");
        let natural = natural_state(
            &p.clone().with_candidate_cap(three),
            &capped,
            &capped_atts,
            cutoff(&capped_atts),
        );
        assert_eq!(
            natural.chain.len(),
            1,
            "the flagged and flagless X are two candidates, and fill the cap"
        );
        // One more slot of cap and Z is reached: the cap, not legality, is
        // what excluded it.
        let four = core::num::NonZeroUsize::new(4).expect("non-zero");
        let natural = natural_state(
            &p.clone().with_candidate_cap(four),
            &capped,
            &capped_atts,
            cutoff(&capped_atts),
        );
        assert_eq!(natural.chain.len(), 2);
        assert_eq!(natural.chain[1].ply.id, eid(2));
    }

    #[test]
    fn the_cap_the_session_carries_bounds_the_windows() {
        // first plays @100; second premoved Ke8-e7 @50 (legal) and then
        // Ke8-e5 @60 (illegal) — both anterior. Under the reference cap the
        // anterior scan skips the illegal newer premove and reaches the legal
        // older one: chain of 2. Under a session whose document carries K = 1
        // the newest premove alone fills the window, and the slot stays
        // unfilled: chain of 1. The cap in force is the session's, not the
        // constant's (kind `3422` `rules`; Move Encoding — Sanki §Bounding a
        // slot's candidates).
        let plies = [
            ply(1, FIRST, 1, RA1A4),
            ply(2, SECOND, 1, KE8E7),
            ply(3, SECOND, 1, "[\"e8\",\"e5\",null]"),
        ];
        let atts = [
            att(101, 1, 100),
            att(102, 2, 50),
            att(103, 3, 60),
            cutoff_att(1000),
        ];
        let reference = natural_state(&params(), &plies, &atts, cutoff(&atts));
        assert_eq!(reference.chain.len(), 2);
        assert_eq!(reference.chain[1].ply.id, eid(2));

        let one = core::num::NonZeroUsize::new(1).expect("non-zero");
        let tight = params().with_candidate_cap(one);
        let natural = natural_state(&tight, &plies, &atts, cutoff(&atts));
        assert_eq!(
            natural.chain.len(),
            1,
            "K = 1 admits the newest premove only"
        );
        assert!(matches!(natural.end, ChainEnd::Ongoing(_)));
    }

    #[test]
    fn identical_twins_timed_at_the_boundary_are_informed() {
        // first plays @100, which is the boundary T of second's slot. second's
        // two identical replies are both timed exactly AT 100: informed (at
        // or after T), so the earliest wins and the tie falls to the smaller
        // id, 2 — the informed scan's first, which is the twin the collapse
        // must keep. A collapse that classified `<= T` as anterior would keep
        // the larger id, 3, and diverge from the selection (and from the
        // client) on which event is canonical.
        let plies = [
            ply(1, FIRST, 1, RA1A4),
            ply(2, SECOND, 1, KE8E7),
            ply(3, SECOND, 1, KE8E7),
        ];
        let atts = [
            att(101, 1, 100),
            att(102, 2, 100),
            att(103, 3, 100),
            cutoff_att(1000),
        ];
        for order in [[0_usize, 1, 2], [0, 2, 1], [2, 1, 0], [1, 2, 0]] {
            let permuted: Vec<Ply> = order.iter().map(|&i| plies[i].clone()).collect();
            let ns = natural_state(&params(), &permuted, &atts, cutoff(&atts));
            assert_eq!(ns.chain.len(), 2, "order {order:?}");
            assert_eq!(ns.chain[1].ply.id, eid(2), "order {order:?}");
        }
        // One second earlier the pair is anterior, and the LATEST — the larger
        // id at equal timing — represents it instead.
        let anterior = [
            att(101, 1, 100),
            att(102, 2, 99),
            att(103, 3, 99),
            cutoff_att(1000),
        ];
        let ns = natural_state(&params(), &plies, &anterior, cutoff(&anterior));
        assert_eq!(ns.chain[1].ply.id, eid(3));
    }

    #[test]
    fn a_ply_timed_exactly_at_t0_is_a_candidate() {
        // The candidate window is `[t₀, cutoff]`, closed at both ends: a Ply
        // canonically timed AT t₀ is valid (kind `3423` §Time accounting rules
        // out timings *before* t₀) and, the first slot's boundary being t₀,
        // informed. One second earlier it never enters the slot.
        let period = Period::new(Duration::from_secs(600), None, None).expect("valid period");
        let scheduled = SessionParams::new(
            eid(SESSION),
            Some(pk(TIMESTAMPER)),
            Seats::new(pk(FIRST), pk(SECOND)).expect("distinct"),
            TimeControl::new(period, Vec::new()),
            Position::parse(ROOK_KING).expect("valid FEEN"),
            ts(1000),
        )
        .expect("first to move");
        let plies = [ply(1, FIRST, 1, RA1A4)];

        let at_start = [att(101, 1, 1000), cutoff_att(2000)];
        let ns = natural_state(&scheduled, &plies, &at_start, cutoff(&at_start));
        assert_eq!(ns.chain.len(), 1);
        assert_eq!(ns.chain[0].at, ts(1000));

        let too_early = [att(101, 1, 999), cutoff_att(2000)];
        let ns = natural_state(&scheduled, &plies, &too_early, cutoff(&too_early));
        assert!(ns.is_empty());
    }
}
