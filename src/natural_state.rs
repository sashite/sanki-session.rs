//! The natural state of events at adjudication (kind `6425` §Natural state).
//!
//! When the arbiter rules, it replays the session's play order from its first
//! half-move, selecting the canonical Ply for each successive slot under the
//! **forgiving-premove** rule ([`crate::selection`]) and applying it through the
//! engine. The replay is a single pass that is at once the chain builder and the
//! legality authority — a slot's selection depends on each candidate's legality
//! in the *replayed* position, so the two cannot be separated.
//!
//! For each play-order position (`(signer, step)` under Sanki's strict
//! alternation), the candidates are the Plies for that slot whose canonical
//! timing lies in `[t₀, cutoff]` — `t₀` the session start (a Ply timed before
//! t₀ is invalid, kind `6423` §Time accounting, and never enters a slot —
//! deciders' confirmation of 2026-07-19), the `cutoff` the
//! triggering Request's canonical timing (so a player cannot race the arbiter by
//! playing after invoking). Identical-content re-submissions are idempotent
//! retries, not alternatives: per content, only the **race-canonical
//! representative** (smallest canonical timing, then smallest event id — kind
//! `6423` §Race resolution) enters the two-window selection, so duplicates
//! neither shift the selected timing nor consume cap slots. Legality is probed
//! **lazily** through [`select_candidate`]'s callback, on the capped windows
//! only (≤ 2K full-rule probes per slot). Canonical timing is the designated timestamper's
//! attestation in attested mode, or the event's own relay-enforced `created_at`
//! when self-timed. The slot's **anchor** is the predecessor half-move's canonical
//! timing (`t₀` for the first slot), and [`select_candidate`] resolves the
//! candidates against it (the boundary `T`):
//!
//! - **applied** — the selected Ply is applied to the board: the *latest* legal
//!   premove (anterior, timed before `T`), else the *earliest* legal live move
//!   (informed, timed at/after `T`). An illegal candidate — premove or live — is
//!   skipped, never sanctioned;
//! - **unfilled** — no candidate is legal in either window: the chain stops, still
//!   ongoing.
//!
//! Applying a selected Ply through the engine ([`step`]) also surfaces a
//! rule-system ending (checkmate, …) or a played-Ply timeout, which terminates the
//! chain. The replay therefore yields either a **terminal verdict** (rule-system
//! ending / timeout, with the attestation time that caused it) or a still-**ongoing**
//! end position for the post-chain resolution ([`crate::verdict`]). There is no
//! `illegalmove` termination — an illegal Ply is skipped, never a loss.
//!
//! In attested mode, if the Request is not yet canonically attested the cutoff is
//! undefined and the natural state cannot be computed ([`natural_state`] returns
//! `None`); self-timed, the request's own `created_at` is always a defined cutoff.

use crate::event::{AdjudicationRequest, Attestation, EventId, Ply};
use crate::race_resolution::{canonical_timing, CanonicalPly};
use crate::selection::{select_candidate, Candidate, Selection, CANDIDATE_CAP};
use crate::session::SessionParams;
use sashite_sanki_engine::domain::half_move::Move;
use sashite_sanki_engine::domain::outcome::Verdict;
use sashite_sanki_engine::domain::time::Timestamp;
use sashite_sanki_engine::engine::validate;
use sashite_sanki_engine::kernel::state::SessionState;
use sashite_sanki_engine::kernel::step::{step, StepResult};
use std::collections::btree_map::Entry;
use std::collections::BTreeMap;

/// How the replayed chain ends.
#[derive(Debug, Clone)]
pub enum Conclusion {
    /// The chain reached a terminal verdict during replay — a rule-system ending
    /// or a played-Ply timeout — at the given attestation time. Post-chain
    /// resolution does not apply.
    Terminal(Verdict, Timestamp),
    /// The chain replayed to a still-ongoing position: post-chain resolution
    /// (draw acceptance, abandonment timeout, residual resignation) decides the
    /// verdict on this state. Boxed — a [`SessionState`] dwarfs the terminal
    /// variant, so the box keeps the enum small.
    Ongoing(Box<SessionState>),
}

/// The natural state: the selected canonical Ply chain, the cutoff it was
/// computed against, and how the chain concluded.
#[derive(Debug, Clone)]
pub struct NaturalState<'a> {
    /// The selected canonical Plies, `chain[i]` being the Ply at play-order
    /// position `i + 1`. A skipped illegal candidate is **not** included (it is not
    /// a played half-move); a terminating *applied* Ply (a mating move, …) **is**.
    pub chain: Vec<CanonicalPly<'a>>,
    /// The cutoff: the triggering Request's canonical timing.
    pub cutoff: Timestamp,
    /// How the chain concluded (terminal verdict or ongoing end position).
    pub conclusion: Conclusion,
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

/// Whether `content` is a legal half-move in `state`'s position, under the full
/// rule system — `engine::validate`, which since engine 0.4 enforces ōgi
/// uchifuzume exactly as the kernel's `step` path does (the agreement is pinned
/// by the `is_legal_matches_the_kernel_step_oracle` test below). Legality is a
/// position question resolved **before** the clock — a legal-but-timed-out move
/// is still legal here — and probing it clones no state. An unparseable content
/// is illegal.
fn is_legal(state: &SessionState, content: &str) -> bool {
    let Ok(mv) = Move::parse(content) else {
        return false;
    };
    validate(state.position(), &mv).is_ok()
}

/// A slot candidate paired with its source Ply (so the selection can be mapped
/// back to the played event).
struct SlotCandidate<'a> {
    ply: &'a Ply,
    candidate: Candidate<EventId>,
}

/// Computes the natural state of `plies`/`attestations` for the session, cut off
/// at the canonical attestation timing of `request`.
///
/// Returns `None` if `request` has no canonical timing (attested mode: no
/// attestation from the designated timestamper yet — the cutoff is undefined and
/// the arbiter must wait). Self-timed, the cutoff is the request's own
/// `created_at`, always defined.
#[must_use]
pub fn natural_state<'a>(
    params: &SessionParams,
    plies: &'a [Ply],
    attestations: &'a [Attestation],
    request: &AdjudicationRequest,
) -> Option<NaturalState<'a>> {
    let timestamper = params.timestamper();
    let session = params.session();
    let start = params.anchor(); // t₀: the lower bound and the first slot's anchor.

    // The cutoff: the Request's authoritative timing. Undefined ⇒ cannot rule.
    let cutoff = canonical_timing(attestations, request.id, request.created_at, timestamper)?;

    let mut chain: Vec<CanonicalPly<'a>> = Vec::new();
    let mut state = params.initial_state();
    let mut anchor = start;
    let mut half_move: u32 = 1;

    let conclusion = loop {
        let signer = params.player_at(half_move);
        let step_no = params.step_at(half_move);

        // Candidates for this slot: canonically timed within [t₀, cutoff] (a
        // pre-t₀ Ply is invalid and never enters — kind 6423 §Time accounting).
        let timed: Vec<SlotCandidate<'a>> = plies
            .iter()
            .filter(|ply| ply.session == session && ply.signer == signer && ply.step == step_no)
            .filter_map(|ply| {
                let at = canonical_timing(attestations, ply.id, ply.created_at, timestamper)?;
                (at >= start && at <= cutoff).then_some(SlotCandidate {
                    ply,
                    candidate: Candidate {
                        id: ply.id,
                        created_at: at,
                    },
                })
            })
            .collect();

        // Identical-content re-submissions collapse to their race-canonical
        // representative — smallest (canonical timing, event id) — before the
        // two-window rule (Move Encoding — Sanki §Slot candidates and
        // selection; kind 6423 §Race resolution).
        let mut representatives: BTreeMap<&str, SlotCandidate<'a>> = BTreeMap::new();
        for entrant in timed {
            match representatives.entry(entrant.ply.content.as_str()) {
                Entry::Vacant(vacant) => {
                    vacant.insert(entrant);
                }
                Entry::Occupied(mut occupied) => {
                    let held = &occupied.get().candidate;
                    let contender = &entrant.candidate;
                    if (contender.created_at, contender.id) < (held.created_at, held.id) {
                        occupied.insert(entrant);
                    }
                }
            }
        }
        let slot: Vec<SlotCandidate<'a>> = representatives.into_values().collect();

        let candidates: Vec<Candidate<EventId>> = slot.iter().map(|sc| sc.candidate).collect();

        // The legality probe: consulted lazily by the selection, on the capped
        // windows only — the ≤ 2K normative bound.
        let probe = |id: &EventId| {
            slot.iter()
                .find(|sc| sc.ply.id == *id)
                .is_some_and(|sc| is_legal(&state, &sc.ply.content))
        };

        match select_candidate(anchor, &candidates, CANDIDATE_CAP, probe) {
            // No candidate is legal in either window: the chain stops, still ongoing.
            Selection::Unfilled => break Conclusion::Ongoing(Box::new(state)),

            // A candidate fills the slot: apply it and advance (or terminate on a
            // rule-system ending / timeout the application surfaces).
            Selection::Applied(chosen) => {
                let at = chosen.created_at;
                let Some(ply) = slot
                    .iter()
                    .find(|sc| sc.ply.id == chosen.id)
                    .map(|sc| sc.ply)
                else {
                    // Unreachable: the selected candidate is one of this slot's
                    // candidates. Degrade safely to an ongoing chain end.
                    break Conclusion::Ongoing(Box::new(state));
                };

                // Selection guarantees legality, so the content parses; a defensive
                // failure stops the chain safely (an illegal Ply is never a loss).
                let Ok(mv) = Move::parse(&ply.content) else {
                    break Conclusion::Ongoing(Box::new(state));
                };

                // The probe validated the candidate, so a rejection here is a
                // broken internal invariant — unreachable on a well-formed
                // position. The rejection hands the state back untouched:
                // degrade to an ongoing end (an illegal Ply is never a loss).
                let (outcome, next) = match step(state, &mv, at) {
                    StepResult::Illegal { state, .. } => {
                        break Conclusion::Ongoing(Box::new(state))
                    }
                    StepResult::Advanced { outcome, next } => (outcome, next),
                };
                chain.push(CanonicalPly { ply, at });
                match next {
                    Some(successor) => {
                        state = successor;
                        // The selection boundary NEVER rewinds: an applied premove
                        // carries an ANTERIOR timing, and anchoring the next slot on
                        // it would (a) misclassify the next slot's blind candidates
                        // as informed and (b) — through the kernel clock, which holds
                        // its own monotonic anchor — disagree with the time actually
                        // chargeable. The anchor is the moment the position became
                        // answerable: the max of the timings so far (time-accounting
                        // §Elapsed time; pinned by the shared conformance vector
                        // `scenario.premove-anchor-never-rewinds`).
                        anchor = anchor.max(at);
                        half_move = half_move.saturating_add(1);
                    }
                    None => break Conclusion::Terminal(outcome.verdict, at),
                }
            }
        }
    };

    Some(NaturalState {
        chain,
        cutoff,
        conclusion,
    })
}

#[cfg(test)]
mod tests {
    #![allow(
        clippy::unwrap_used,
        clippy::expect_used,
        clippy::panic,
        clippy::indexing_slicing
    )]

    use super::{natural_state, Conclusion};
    use crate::event::{AdjudicationRequest, Attestation, EventId, Ply, PublicKey};
    use crate::session::SessionParams;
    use sashite_sanki_engine::domain::outcome::Verdict;
    use sashite_sanki_engine::domain::status::Status;
    use sashite_sanki_engine::domain::time::{Duration, Timestamp};
    use sashite_sanki_engine::domain::time_control::{Period, TimeControl};
    use sashite_sanki_engine::position::Position;

    const FIRST: u8 = 10;
    const SECOND: u8 = 20;
    const TIMESTAMPER: u8 = 99;
    const SESSION: u8 = 50;
    const REQUEST: u8 = 170;

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

    // A ply with an explicit relay-enforced created_at — its canonical timing when
    // self-timed. In the attested tests below, created_at is ignored, so `ply` seeds 0.
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
            pk(2),
            Some(pk(TIMESTAMPER)),
            pk(FIRST),
            pk(SECOND),
            TimeControl::new(period, Vec::new()),
            Position::parse(feen).expect("valid FEEN"),
            ts(0),
        )
    }

    fn params() -> SessionParams {
        params_feen(ROOK_KING)
    }

    fn params_self_timed() -> SessionParams {
        let period = Period::new(Duration::from_secs(600), None, None).expect("valid period");
        SessionParams::new(
            eid(SESSION),
            pk(2),
            None, // self-timed: no timestamper designated
            pk(FIRST),
            pk(SECOND),
            TimeControl::new(period, Vec::new()),
            Position::parse(ROOK_KING).expect("valid FEEN"),
            ts(0),
        )
    }

    fn request() -> AdjudicationRequest {
        AdjudicationRequest::new(eid(REQUEST), pk(FIRST), eid(SESSION), pk(2), ts(0))
    }

    fn cutoff_att(at: i64) -> Attestation {
        att(171, REQUEST, at)
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
        let ns = natural_state(&params(), &plies, &atts, &request()).expect("attested request");
        assert_eq!(ns.chain.len(), 3);
        assert_eq!(ns.next_half_move(), 4);
        assert_eq!(*ns.chain[0].ply.id.as_bytes(), [1; 32]);
        assert_eq!(*ns.chain[2].ply.id.as_bytes(), [3; 32]);
        assert!(matches!(ns.conclusion, Conclusion::Ongoing(_)));
    }

    #[test]
    fn self_timed_chain_uses_event_created_at() {
        // No timestamper and no attestations: the chain is assembled from the plies'
        // own relay-enforced created_at, and the cutoff from the request's own.
        let plies = [
            ply_at(1, FIRST, 1, RA1A4, 100),
            ply_at(2, SECOND, 1, KE8E7, 200),
            ply_at(3, FIRST, 2, RA4A5, 300),
        ];
        let no_atts: Vec<Attestation> = Vec::new();
        let request =
            AdjudicationRequest::new(eid(REQUEST), pk(FIRST), eid(SESSION), pk(2), ts(1000));
        let ns = natural_state(&params_self_timed(), &plies, &no_atts, &request)
            .expect("self-timed request has canonical timing");
        assert_eq!(ns.chain.len(), 3);
        assert_eq!(ns.next_half_move(), 4);
        assert_eq!(*ns.chain[0].ply.id.as_bytes(), [1; 32]);
        assert_eq!(ns.chain[0].at, ts(100));
        assert!(matches!(ns.conclusion, Conclusion::Ongoing(_)));
    }

    #[test]
    fn self_timed_cutoff_excludes_a_later_ply() {
        // The request's own created_at is the cutoff: a ply created after it is excluded.
        let plies = [
            ply_at(1, FIRST, 1, RA1A4, 100),
            ply_at(2, SECOND, 1, KE8E7, 500),
        ];
        let no_atts: Vec<Attestation> = Vec::new();
        let request =
            AdjudicationRequest::new(eid(REQUEST), pk(FIRST), eid(SESSION), pk(2), ts(300));
        let ns = natural_state(&params_self_timed(), &plies, &no_atts, &request).expect("request");
        assert_eq!(ns.chain.len(), 1); // ply 2 (created_at 500 > cutoff 300) is excluded
    }

    #[test]
    fn cutoff_inclusivity() {
        // A Ply attested exactly at the cutoff is included (the `≤` condition).
        let plies = [ply(1, FIRST, 1, RA1A4)];
        let atts = [att(101, 1, 1000), cutoff_att(1000)];
        let ns = natural_state(&params(), &plies, &atts, &request()).expect("attested request");
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
        let ns = natural_state(&params(), &plies, &atts, &request()).expect("attested request");
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
        let ns = natural_state(&params(), &plies, &atts, &request()).expect("attested request");
        assert_eq!(ns.chain.len(), 1);
        assert!(matches!(ns.conclusion, Conclusion::Ongoing(_)));
    }

    #[test]
    fn pending_ply_breaks_the_chain() {
        // (second, step 1) present but not attested: pending, excluded → chain of 1.
        let plies = [ply(1, FIRST, 1, RA1A4), ply(2, SECOND, 1, KE8E7)];
        let atts = [att(101, 1, 100), cutoff_att(1000)];
        let ns = natural_state(&params(), &plies, &atts, &request()).expect("attested request");
        assert_eq!(ns.chain.len(), 1);
    }

    #[test]
    fn gap_in_play_order_stops_the_chain() {
        let plies = [ply(1, FIRST, 1, RA1A4)];
        let atts = [att(101, 1, 100), cutoff_att(1000)];
        let ns = natural_state(&params(), &plies, &atts, &request()).expect("attested request");
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
        let ns = natural_state(&params(), &plies, &atts, &request()).expect("attested request");
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
        let ns = natural_state(&params(), &plies, &atts, &request()).expect("attested request");
        assert_eq!(ns.chain.len(), 2);
        assert_eq!(*ns.chain[1].ply.id.as_bytes(), [3; 32]);
        assert!(matches!(ns.conclusion, Conclusion::Ongoing(_)));
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
        let ns = natural_state(&params(), &plies, &atts, &request()).expect("attested request");
        assert_eq!(ns.chain.len(), 1); // the illegal live move is skipped, not in the chain
        assert!(matches!(ns.conclusion, Conclusion::Ongoing(_)));
    }

    #[test]
    fn mating_move_terminates_the_chain() {
        // Ra1-a8 mates the walled-in black King: a rule-system ending surfaced by
        // applying the move.
        let plies = [ply(1, FIRST, 1, "[\"a1\",\"a8\",null]")];
        let atts = [att(101, 1, 100), cutoff_att(1000)];
        let p = params_feen("7k^/6pp/8/8/8/8/8/R3K^3 / W/w");
        let ns = natural_state(&p, &plies, &atts, &request()).expect("attested request");
        assert_eq!(ns.chain.len(), 1); // the mating move is part of the chain
        match ns.conclusion {
            Conclusion::Terminal(verdict, at) => {
                assert!(matches!(
                    verdict,
                    Verdict::Terminated {
                        status: Status::Checkmate,
                        ..
                    }
                ));
                assert_eq!(at, ts(100));
            }
            Conclusion::Ongoing(_) => panic!("expected a checkmate termination"),
        }
    }

    #[test]
    fn unattested_request_yields_none() {
        let plies = [ply(1, FIRST, 1, RA1A4)];
        let atts = [att(101, 1, 100)];
        assert!(natural_state(&params(), &plies, &atts, &request()).is_none());
    }

    #[test]
    fn empty_chain_if_no_first_ply() {
        let plies: [Ply; 0] = [];
        let atts = [cutoff_att(1000)];
        let ns = natural_state(&params(), &plies, &atts, &request()).expect("attested request");
        assert!(ns.is_empty());
        assert_eq!(ns.next_half_move(), 1);
        assert!(matches!(ns.conclusion, Conclusion::Ongoing(_)));
    }

    #[test]
    fn identical_content_duplicates_collapse_to_the_race_canonical() {
        // Two identical-content premoves for (second, step 1) — @50 id 2 and a
        // retry @60 id 3: idempotent retries, not alternatives. The
        // representative is the race-canonical (smallest timing, then id), so
        // the selected ply is id 2 @50 — its timing then anchors the next slot.
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
        let ns = natural_state(&params(), &plies, &atts, &request()).expect("attested request");
        assert_eq!(ns.chain.len(), 2);
        assert_eq!(*ns.chain[1].ply.id.as_bytes(), [2; 32]);
        assert_eq!(ns.chain[1].at, ts(50));
    }

    #[test]
    fn pre_t0_candidates_are_ignored() {
        // A Ply timed before t₀ is invalid (kind 6423 §Time accounting) and
        // never enters its slot — deciders' confirmation of 2026-07-19.
        let plies = [ply(1, FIRST, 1, RA1A4)];
        let atts = [att(101, 1, -5), cutoff_att(1000)];
        let ns = natural_state(&params(), &plies, &atts, &request()).expect("attested request");
        assert!(ns.is_empty());
        assert!(matches!(ns.conclusion, Conclusion::Ongoing(_)));
    }

    #[test]
    fn played_ply_timeout_terminates_the_chain() {
        // A legal ply timed beyond the mover's 600 s budget (@700): the replay
        // surfaces the played-Ply timeout as the terminal conclusion, anchored
        // at that ply's canonical timing.
        use sashite_sanki_engine::domain::status::Status;

        let plies = [ply(1, FIRST, 1, RA1A4)];
        let atts = [att(101, 1, 700), cutoff_att(1000)];
        let ns = natural_state(&params(), &plies, &atts, &request()).expect("attested request");
        assert_eq!(ns.chain.len(), 1);
        match ns.conclusion {
            Conclusion::Terminal(verdict, at) => {
                assert!(matches!(
                    verdict,
                    Verdict::Terminated {
                        status: Status::Timeout,
                        ..
                    }
                ));
                assert_eq!(at, ts(700));
            }
            Conclusion::Ongoing(_) => panic!("expected a played-ply timeout"),
        }
    }

    #[test]
    fn is_legal_matches_the_kernel_step_oracle() {
        use super::is_legal;
        use sashite_sanki_engine::domain::half_move::Move;
        use sashite_sanki_engine::domain::time_control::{Period, TimeControl};
        use sashite_sanki_engine::kernel::state::SessionState;
        use sashite_sanki_engine::kernel::step::step;

        // The kernel-step oracle: since engine 0.5 an illegal ply is a
        // `StepResult::Illegal` rejection. The validate-based `is_legal` must
        // agree on every legality class — the façade/kernel alignment this
        // crate relies on.
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

        const OGI_UCHIFUZUME: &str = "7k^/8/5N2/8/8/8/8/4K^1R1 F/ J/j";
        let cases: &[(&str, u64, &str)] = &[
            // Chess: a legal move, an unreachable destination, the opponent's
            // piece, an empty source, unparseable content.
            (ROOK_KING, 600, RA1A4),
            (ROOK_KING, 600, "[\"a1\",\"b3\",null]"),
            (ROOK_KING, 600, "[\"e8\",\"e7\",null]"),
            (ROOK_KING, 600, "[\"h4\",\"h5\",null]"),
            (ROOK_KING, 600, "not a ply"),
            // Ōgi: the mating Fu drop (uchifuzume — the class the engine-0.4
            // alignment brings to `validate`), a quiet legal drop, a drop on
            // an occupied square.
            (OGI_UCHIFUZUME, 600, "[null,\"h7\",\"fu\"]"),
            (OGI_UCHIFUZUME, 600, "[null,\"h6\",\"fu\"]"),
            (OGI_UCHIFUZUME, 600, "[null,\"h8\",\"fu\"]"),
            // Legality before the clock: with a 5 s bank and a 30 s ply, the
            // oracle sees a `timeout` termination — still a *legal* move.
            (ROOK_KING, 5, RA1A4),
        ];

        for (feen, secs, content) in cases {
            let s = state(feen, *secs);
            assert_eq!(
                is_legal(&s, content),
                oracle(&s, content),
                "probe/oracle divergence on {content} in {feen} ({secs} s bank)"
            );
        }
    }
}
