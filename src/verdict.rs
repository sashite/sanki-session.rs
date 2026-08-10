//! Adjudication assembly and the top-level [`adjudicate`] orchestration.
//!
//! [`adjudicate`] turns a session's public events into the arbiter's binding
//! verdict (kind `3425`), per Statuses — Sanki §Verdict resolution: a
//! termination [`Status`] (the event's `content`) and a result distribution
//! ([`Outcome3`], the `result` tags). It composes every layer below it.
//!
//! # Verdict resolution
//!
//! Under the forgiving-premove model the verdict is **entirely play-derived** —
//! there is no separate equivocation sanction. The natural-state replay
//! ([`crate::natural_state`]) selects and applies the canonical Ply of each slot
//! and yields one of two conclusions:
//!
//! - a **terminal** verdict reached during replay — a rule-system ending
//!   (checkmate, …) or a played-Ply timeout — which is the verdict directly; or
//! - a still-**ongoing** end position, on which the invocation is resolved at the
//!   cutoff, in order: draw acceptance (`agreement`, [`crate::implicit`]);
//!   abandonment timeout (`timeout`: the on-move player's clock, ticked from the
//!   chain's last attestation — or t₀ for an empty chain — to the cutoff, has
//!   expired); otherwise **residual resignation** (`resignation`, decisive
//!   against the invoker, whatever the turn).
//!
//! An illegal candidate — premove or live — is never a cause: it is skipped during
//! selection (never a loss), so there is no `illegalmove` termination. Because
//! resignation is the residual interpretation, a conforming, canonically attested
//! Request
//! from a session player **always yields a verdict**. [`adjudicate`] returns
//! `None` only when the Request is non-conforming — it does not reference this
//! session and this arbiter, or its signer is not a session player (kind
//! `3424` §Semantic constraints, items 2–4) — or when it has no canonical
//! timing yet (the cutoff is undefined).
//!
//! Several Requests may coexist, and the choice of which to rule on fixes the
//! cutoff, hence the verdict. [`select_request`] pins the deterministic policy
//! of Statuses — Sanki §Which Request rules — the earliest conforming Request
//! by canonical timing, smallest event id as tiebreaker; "not yet adjudicated"
//! stays the caller's ledger (once the canonical Adjudication exists, the
//! session is terminated and every later Request is moot).

use crate::event::{AdjudicationRequest, Attestation, Ply};
use crate::implicit::draw_acceptance;
use crate::natural_state::{natural_state, Conclusion, NaturalState};
use crate::race_resolution::canonical_timing;
use crate::session::SessionParams;
use sashite_sanki_engine::clock::tick;
use sashite_sanki_engine::domain::outcome::Verdict;
use sashite_sanki_engine::domain::side::Side;
use sashite_sanki_engine::domain::status::{Outcome3, Status};
use sashite_sanki_engine::domain::time::Duration;

/// The arbiter's binding verdict: a termination status and a result
/// distribution.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Adjudication {
    status: Status,
    result: Outcome3,
}

impl Adjudication {
    /// The termination cause (the Adjudication's `content`).
    #[inline]
    #[must_use]
    pub const fn status(&self) -> Status {
        self.status
    }

    /// The result distribution.
    #[inline]
    #[must_use]
    pub const fn result(&self) -> Outcome3 {
        self.result
    }

    /// The score (`0`, `50`, or `100`) assigned to `side`.
    #[inline]
    #[must_use]
    pub const fn score(&self, side: Side) -> u8 {
        match (self.result, side) {
            (Outcome3::Draw, _) => 50,
            (Outcome3::FirstWins, Side::First) | (Outcome3::SecondWins, Side::Second) => 100,
            _ => 0,
        }
    }

    /// Builds an adjudication from a terminal verdict, or `None` if the verdict
    /// is `Ongoing` (unreachable from [`adjudicate`], kept as a defensive seam).
    #[inline]
    fn from_verdict(verdict: Verdict) -> Option<Self> {
        match verdict {
            Verdict::Terminated { status, result } => Some(Self { status, result }),
            Verdict::Ongoing => None,
        }
    }
}

/// Rules on a session from its public events, cut off at the triggering
/// Request's canonical timing.
///
/// Returns `None` when no ruling is possible: the Request is non-conforming
/// (another session or arbiter, or a non-player signer — kind `3424` §Semantic
/// constraints, items 2–4), or it has no canonical timing yet.
#[must_use]
pub fn adjudicate(
    params: &SessionParams,
    plies: &[Ply],
    attestations: &[Attestation],
    request: &AdjudicationRequest,
) -> Option<Adjudication> {
    // A Request for another session or another arbiter is non-conforming
    // (kind 3424 §Semantic constraints, items 2 and 4): no ruling — a
    // cross-session invocation must never resolve as a resignation here.
    if request.session != params.session() || request.arbiter != params.arbiter() {
        return None;
    }

    // A Request from a non-player is non-conforming (kind 3424 §Semantic
    // constraints, item 3): no ruling.
    let invoker = params.side_of(request.signer)?;

    // The natural state is also the gate: no canonical Request attestation, no
    // cutoff, no ruling. The replay has already selected and applied the chain.
    let natural = natural_state(params, plies, attestations, request)?;

    let verdict = resolve_play(params, &natural, request, invoker);
    Adjudication::from_verdict(verdict)
}

/// Selects **which Request rules** (Statuses — Sanki §Which Request rules):
/// among the conforming Requests — this session, this arbiter, a session-player
/// signer — that have canonical timing, the earliest by canonical timing,
/// smallest Request event id as tiebreaker. Returns `None` when no conforming
/// Request is canonically timed yet.
///
/// "Not yet adjudicated" is the caller's ledger: once the canonical
/// Adjudication exists the session is terminated and every later Request is
/// moot, so the caller simply stops selecting.
#[must_use]
pub fn select_request<'a>(
    params: &SessionParams,
    requests: &'a [AdjudicationRequest],
    attestations: &[Attestation],
) -> Option<&'a AdjudicationRequest> {
    requests
        .iter()
        .filter(|request| {
            request.session == params.session()
                && request.arbiter == params.arbiter()
                && params.side_of(request.signer).is_some()
        })
        .filter_map(|request| {
            canonical_timing(
                attestations,
                request.id,
                request.created_at,
                params.timestamper(),
            )
            .map(|at| (at, request))
        })
        .min_by(|(at_a, req_a), (at_b, req_b)| at_a.cmp(at_b).then_with(|| req_a.id.cmp(&req_b.id)))
        .map(|(_, request)| request)
}

/// The verdict the play produces: the natural state's terminal verdict if the
/// replay reached one, otherwise the invocation resolved at the cutoff on the
/// ongoing end position — in order: draw acceptance, abandonment timeout,
/// residual resignation.
fn resolve_play(
    params: &SessionParams,
    natural: &NaturalState<'_>,
    request: &AdjudicationRequest,
    invoker: Side,
) -> Verdict {
    let state = match &natural.conclusion {
        // The replay terminated (a rule-system ending or a played-Ply timeout):
        // that is the verdict.
        Conclusion::Terminal(verdict, _at) => return *verdict,
        // Still ongoing: resolve the invocation at the cutoff.
        Conclusion::Ongoing(state) => state,
    };

    // 2a. Draw acceptance: a standing offer accepted by the offeree.
    if let Some(verdict) = draw_acceptance(params, natural, request) {
        return verdict;
    }

    // 2b. Abandonment timeout: the player on move let their clock run out
    // before the cutoff (whether or not they are the invoker). The clock ticked
    // is the one the replay produced, from the chain's last attestation — t₀ for
    // an empty chain — to the cutoff.
    let on_move = state.position().active_side();
    let anchor = state.last_attestation();
    // `duration_since` answers `None` in two cases that must NOT be conflated —
    // the same distinction `kernel::step` draws for a played Ply. A cutoff
    // BEFORE the anchor is an inverted span, clamped to zero (`elapsed =
    // max(0, cutoff − T)`, time-accounting §Elapsed time); a forward span too
    // wide for the representation is an abandonment so long it overflows, and
    // charging zero for it would pardon the abandoning player outright. It
    // saturates instead, flagging under any finite time control.
    let elapsed = match natural.cutoff.duration_since(anchor) {
        Some(elapsed) => elapsed,
        None if natural.cutoff < anchor => Duration::ZERO,
        None => Duration::from_secs(u64::MAX),
    };
    if tick(params.time_control(), state.clocks().get(on_move), elapsed).is_flagged() {
        return Verdict::decisive(Status::Timeout, on_move);
    }

    // 2c. Residual resignation: the invocation matches no other cause, so the
    // invoker abandons — whatever the turn.
    Verdict::decisive(Status::Resignation, invoker)
}

#[cfg(test)]
mod tests {
    #![allow(
        clippy::unwrap_used,
        clippy::expect_used,
        clippy::panic,
        clippy::indexing_slicing
    )]

    use super::adjudicate;
    use crate::event::{AdjudicationRequest, Attestation, EventId, Ply, PublicKey};
    use crate::session::SessionParams;
    use sashite_sanki_engine::domain::status::{Outcome3, Status};
    use sashite_sanki_engine::domain::time::{Duration, Timestamp};
    use sashite_sanki_engine::domain::time_control::{Period, TimeControl};
    use sashite_sanki_engine::position::Position;

    const FIRST: u8 = 10;
    const SECOND: u8 = 20;
    const TIMESTAMPER: u8 = 99;
    const SESSION: u8 = 50;
    const REQUEST: u8 = 170;

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
        Ply::new(
            eid(id),
            pk(signer),
            eid(SESSION),
            step,
            false,
            content.to_owned(),
            ts(0),
        )
    }

    fn ply_draw(id: u8, signer: u8, step: u32, content: &str) -> Ply {
        Ply::new(
            eid(id),
            pk(signer),
            eid(SESSION),
            step,
            true,
            content.to_owned(),
            ts(0),
        )
    }

    fn att(id: u8, attests: u8, at: i64) -> Attestation {
        Attestation::new(eid(id), pk(TIMESTAMPER), eid(attests), ts(at))
    }

    fn request(signer: u8) -> AdjudicationRequest {
        AdjudicationRequest::new(eid(REQUEST), pk(signer), eid(SESSION), pk(2), ts(0))
    }

    fn params(feen: &str, tc_secs: u64, anchor: i64) -> SessionParams {
        let period = Period::new(Duration::from_secs(tc_secs), None, None).expect("period");
        SessionParams::new(
            eid(SESSION),
            pk(2),
            Some(pk(TIMESTAMPER)),
            pk(FIRST),
            pk(SECOND),
            TimeControl::new(period, Vec::new()),
            Position::parse(feen).expect("valid FEEN"),
            ts(anchor),
        )
    }

    #[test]
    fn mate_by_chain_replay() {
        // Ra1-a8 mates the walled-in black King: checkmate, first player wins.
        let plies = [ply(1, FIRST, 1, "[\"a1\",\"a8\",null]")];
        let atts = [att(101, 1, 100), att(171, REQUEST, 1000)];
        let p = params("7k^/6pp/8/8/8/8/8/R3K^3 / W/w", 600, 0);
        let adj = adjudicate(&p, &plies, &atts, &request(SECOND)).expect("verdict");
        assert_eq!(adj.status(), Status::Checkmate);
        assert_eq!(adj.result(), Outcome3::FirstWins);
    }

    #[test]
    fn illegal_move_in_the_chain_is_skipped_not_a_loss() {
        // No piece on a1: the first player's only Ply (a1-a4 @100) is illegal. Under
        // the two-window forgiving rule it is skipped (no `illegalmove`), leaving the
        // chain empty and the first player still on move. The second player invokes
        // within time (cutoff 400 ≤ 600): a residual resignation against the invoker —
        // the illegal move is NOT a loss for the first player.
        let plies = [ply(1, FIRST, 1, "[\"a1\",\"a4\",null]")];
        let atts = [att(101, 1, 100), att(171, REQUEST, 400)];
        let p = params("4k^3/8/8/8/8/8/8/4K^3 / W/w", 600, 0);
        let adj = adjudicate(&p, &plies, &atts, &request(SECOND)).expect("verdict");
        assert_eq!(adj.status(), Status::Resignation);
        assert_eq!(adj.result(), Outcome3::FirstWins);
    }

    #[test]
    fn differing_contents_are_not_an_equivocation_loss() {
        // The first player publishes two differing step-1 contents (a4 @100, a5
        // @200). Under the forgiving rule there is no equivocation sanction: the
        // earliest qualifying candidate (a4) simply fills the slot and the later
        // divergent a5 is ignored. The second player's invocation at 400 (within
        // time) is then a residual resignation — not a loss for the first player.
        let plies = [
            ply(1, FIRST, 1, "[\"a1\",\"a4\",null]"),
            ply(2, FIRST, 1, "[\"a1\",\"a5\",null]"),
        ];
        let atts = [att(101, 1, 100), att(102, 2, 200), att(171, REQUEST, 400)];
        let p = params("4k^3/8/8/8/8/8/8/R3K^3 / W/w", 600, 0);
        let adj = adjudicate(&p, &plies, &atts, &request(SECOND)).expect("verdict");
        assert_eq!(adj.status(), Status::Resignation);
        assert_eq!(adj.result(), Outcome3::FirstWins);
    }

    #[test]
    fn own_turn_invocation_without_cause_is_resignation() {
        // The second player invokes on their own turn without playing, well
        // within their time (elapsed 300 ≤ 600): residual resignation.
        let plies = [ply(1, FIRST, 1, "[\"a1\",\"a4\",null]")];
        let atts = [att(101, 1, 100), att(171, REQUEST, 400)];
        let p = params("4k^3/8/8/8/8/8/8/R3K^3 / W/w", 600, 0);
        let adj = adjudicate(&p, &plies, &atts, &request(SECOND)).expect("verdict");
        assert_eq!(adj.status(), Status::Resignation);
        assert_eq!(adj.result(), Outcome3::FirstWins);
    }

    #[test]
    fn off_turn_invocation_without_cause_is_resignation() {
        // The first player invokes while the second is on move and within time
        // (elapsed 300 ≤ 600): residual resignation against the invoker —
        // invocation is turn-independent.
        let plies = [ply(1, FIRST, 1, "[\"a1\",\"a4\",null]")];
        let atts = [att(101, 1, 100), att(171, REQUEST, 400)];
        let p = params("4k^3/8/8/8/8/8/8/R3K^3 / W/w", 600, 0);
        let adj = adjudicate(&p, &plies, &atts, &request(FIRST)).expect("verdict");
        assert_eq!(adj.status(), Status::Resignation);
        assert_eq!(adj.result(), Outcome3::SecondWins);
    }

    #[test]
    fn draw_by_agreement() {
        // The first player offers the draw (draw flag); the second accepts it by
        // invoking. Checked before the abandonment timeout: even with the
        // second player's clock expired at the cutoff (900 > 600), the
        // acceptance rules.
        let plies = [ply_draw(1, FIRST, 1, "[\"a1\",\"a4\",null]")];
        let atts = [att(101, 1, 100), att(171, REQUEST, 1000)];
        let p = params("4k^3/8/8/8/8/8/8/R3K^3 / W/w", 600, 0);
        let adj = adjudicate(&p, &plies, &atts, &request(SECOND)).expect("verdict");
        assert_eq!(adj.status(), Status::Agreement);
        assert_eq!(adj.result(), Outcome3::Draw);
    }

    #[test]
    fn abandonment_timeout() {
        // The first player moves (elapsed 100 ≤ 600), then the second lets their
        // clock run to the cutoff (elapsed 900 > 600); the first player invokes.
        let plies = [ply(1, FIRST, 1, "[\"a1\",\"a4\",null]")];
        let atts = [att(101, 1, 100), att(171, REQUEST, 1000)];
        let p = params("4k^3/8/8/8/8/8/8/R3K^3 / W/w", 600, 0);
        let adj = adjudicate(&p, &plies, &atts, &request(FIRST)).expect("verdict");
        assert_eq!(adj.status(), Status::Timeout);
        assert_eq!(adj.result(), Outcome3::FirstWins);
    }

    #[test]
    fn own_expired_clock_is_a_timeout_not_a_resignation() {
        // The second player, on move with their clock expired (900 > 600),
        // invokes: the abandonment timeout is tested before the residual
        // resignation — a loss on time, against the invoker.
        let plies = [ply(1, FIRST, 1, "[\"a1\",\"a4\",null]")];
        let atts = [att(101, 1, 100), att(171, REQUEST, 1000)];
        let p = params("4k^3/8/8/8/8/8/8/R3K^3 / W/w", 600, 0);
        let adj = adjudicate(&p, &plies, &atts, &request(SECOND)).expect("verdict");
        assert_eq!(adj.status(), Status::Timeout);
        assert_eq!(adj.result(), Outcome3::FirstWins);
    }

    #[test]
    fn empty_chain_invocation_is_resignation() {
        // No move played, both within time (cutoff 400 ≤ 600): whoever invokes
        // resigns.
        let plies: [Ply; 0] = [];
        let atts = [att(171, REQUEST, 400)];
        let p = params("4k^3/8/8/8/8/8/8/R3K^3 / W/w", 600, 0);
        let adj = adjudicate(&p, &plies, &atts, &request(SECOND)).expect("verdict");
        assert_eq!(adj.status(), Status::Resignation);
        assert_eq!(adj.result(), Outcome3::FirstWins);
    }

    #[test]
    fn unattested_request_no_verdict() {
        let plies = [ply(1, FIRST, 1, "[\"a1\",\"a4\",null]")];
        let atts = [att(101, 1, 100)]; // no attestation for the Request
        let p = params("4k^3/8/8/8/8/8/8/R3K^3 / W/w", 600, 0);
        assert!(adjudicate(&p, &plies, &atts, &request(SECOND)).is_none());
    }

    #[test]
    fn non_player_request_no_verdict() {
        // A Request signed by a non-player is non-conforming: no ruling.
        let plies = [ply(1, FIRST, 1, "[\"a1\",\"a4\",null]")];
        let atts = [att(101, 1, 100), att(171, REQUEST, 1000)];
        let p = params("4k^3/8/8/8/8/8/8/R3K^3 / W/w", 600, 0);
        assert!(adjudicate(&p, &plies, &atts, &request(77)).is_none());
    }

    #[test]
    fn cross_session_request_no_verdict() {
        // A Request referencing another session is non-conforming (kind 3424
        // §Semantic constraints, item 2): no ruling — never a resignation in
        // the wrong session.
        let plies = [ply(1, FIRST, 1, "[\"a1\",\"a4\",null]")];
        let atts = [att(101, 1, 100), att(171, REQUEST, 400)];
        let p = params("4k^3/8/8/8/8/8/8/R3K^3 / W/w", 600, 0);
        let foreign = AdjudicationRequest::new(eid(REQUEST), pk(SECOND), eid(51), pk(2), ts(0));
        assert!(adjudicate(&p, &plies, &atts, &foreign).is_none());
    }

    #[test]
    fn wrong_arbiter_request_no_verdict() {
        // A Request naming another arbiter is non-conforming (item 4).
        let plies = [ply(1, FIRST, 1, "[\"a1\",\"a4\",null]")];
        let atts = [att(101, 1, 100), att(171, REQUEST, 400)];
        let p = params("4k^3/8/8/8/8/8/8/R3K^3 / W/w", 600, 0);
        let foreign =
            AdjudicationRequest::new(eid(REQUEST), pk(SECOND), eid(SESSION), pk(7), ts(0));
        assert!(adjudicate(&p, &plies, &atts, &foreign).is_none());
    }

    #[test]
    fn select_request_earliest_conforming_timed() {
        use super::select_request;

        let p = params("4k^3/8/8/8/8/8/8/R3K^3 / W/w", 600, 0);
        let requests = [
            // Conforming, attested @300.
            AdjudicationRequest::new(eid(170), pk(FIRST), eid(SESSION), pk(2), ts(0)),
            // Conforming, attested @200 — the earliest: rules.
            AdjudicationRequest::new(eid(172), pk(SECOND), eid(SESSION), pk(2), ts(0)),
            // Non-conforming (foreign session), attested @100: skipped.
            AdjudicationRequest::new(eid(174), pk(FIRST), eid(51), pk(2), ts(0)),
            // Conforming but unattested: pending, skipped.
            AdjudicationRequest::new(eid(176), pk(FIRST), eid(SESSION), pk(2), ts(0)),
        ];
        let atts = [att(201, 170, 300), att(202, 172, 200), att(203, 174, 100)];
        let selected = select_request(&p, &requests, &atts).expect("a request rules");
        assert_eq!(*selected.id.as_bytes(), [172; 32]);

        // Tie on timing: the smallest Request event id rules.
        let tied = [
            AdjudicationRequest::new(eid(180), pk(FIRST), eid(SESSION), pk(2), ts(0)),
            AdjudicationRequest::new(eid(178), pk(SECOND), eid(SESSION), pk(2), ts(0)),
        ];
        let tied_atts = [att(211, 180, 500), att(212, 178, 500)];
        let selected = select_request(&p, &tied, &tied_atts).expect("a request rules");
        assert_eq!(*selected.id.as_bytes(), [178; 32]);

        // No conforming timed Request at all: None.
        assert!(select_request(&p, &requests[3..], &atts).is_none());
    }

    #[test]
    fn select_request_never_picks_a_non_conforming_or_untimed_one() {
        // Every rejected Request below is canonically timed EARLIER than the one
        // that rules, so a filter that leaked would change the answer: an
        // earlier-timed impostor would fix a different cutoff, hence a different
        // verdict. The order of the slices must not matter either.
        use super::select_request;

        let p = params("4k^3/8/8/8/8/8/8/R3K^3 / W/w", 600, 0);
        let requests = [
            // Conforming, attested @200 — the earliest conforming: rules.
            AdjudicationRequest::new(eid(172), pk(SECOND), eid(SESSION), pk(2), ts(0)),
            // Another session (item 2), attested @100.
            AdjudicationRequest::new(eid(174), pk(FIRST), eid(51), pk(2), ts(0)),
            // Another arbiter (item 4), attested @90.
            AdjudicationRequest::new(eid(175), pk(FIRST), eid(SESSION), pk(7), ts(0)),
            // A non-player signer (item 3), attested @50.
            AdjudicationRequest::new(eid(177), pk(77), eid(SESSION), pk(2), ts(0)),
            // Conforming but untimed (no attestation from the timestamper).
            AdjudicationRequest::new(eid(178), pk(FIRST), eid(SESSION), pk(2), ts(0)),
            // Conforming, attested @300 — later, so it does not rule.
            AdjudicationRequest::new(eid(170), pk(FIRST), eid(SESSION), pk(2), ts(0)),
        ];
        let atts = [
            att(202, 172, 200),
            att(203, 174, 100),
            att(204, 175, 90),
            att(205, 177, 50),
            att(201, 170, 300),
            // An attestation of the untimed Request, but signed by a player
            // rather than by the designated timestamper: it confers nothing.
            Attestation::new(eid(206), pk(FIRST), eid(178), ts(10)),
        ];

        let expected = *select_request(&p, &requests, &atts)
            .expect("a request rules")
            .id
            .as_bytes();
        assert_eq!(expected, [172; 32]);

        // Rotating both slices leaves the selection where it is.
        for rotation in 0..requests.len() {
            let mut rotated = requests;
            rotated.rotate_left(rotation);
            let mut rotated_atts = atts;
            rotated_atts.rotate_left(rotation % atts.len());
            let selected = select_request(&p, &rotated, &rotated_atts).expect("a request rules");
            assert_eq!(*selected.id.as_bytes(), expected, "rotation {rotation}");
        }

        // With every conforming timed Request removed, the impostors alone yield
        // nothing — "not yet adjudicated" stays the caller's ledger.
        let impostors = [requests[1], requests[2], requests[3], requests[4]];
        assert!(select_request(&p, &impostors, &atts).is_none());
        let empty: [AdjudicationRequest; 0] = [];
        assert!(select_request(&p, &empty, &atts).is_none());
    }

    #[test]
    fn score_matches_the_point_split_for_every_outcome() {
        // `score` must agree with the normative split of the `result` tags
        // (`Outcome3::points()`, statuses-sanki) on every (result, side) pair,
        // each reached through a real ruling rather than a hand-built value.
        use sashite_sanki_engine::domain::side::Side;

        let mate = params("7k^/6pp/8/8/8/8/8/R3K^3 / W/w", 600, 0);
        let mating = [ply(1, FIRST, 1, "[\"a1\",\"a8\",null]")];
        let mate_atts = [att(101, 1, 100), att(171, REQUEST, 300)];

        let p = params("4k^3/8/8/8/8/8/8/R3K^3 / W/w", 600, 0);
        let none: [Ply; 0] = [];
        let empty_atts = [att(171, REQUEST, 300)];
        let offer = [ply_draw(1, FIRST, 1, "[\"a1\",\"a4\",null]")];
        let offer_atts = [att(101, 1, 100), att(171, REQUEST, 300)];

        let cases = [
            // A back-rank mate: the first player wins.
            (
                adjudicate(&mate, &mating, &mate_atts, &request(SECOND)),
                Outcome3::FirstWins,
            ),
            // The first player invokes with nothing else to rule on: they resign.
            (
                adjudicate(&p, &none, &empty_atts, &request(FIRST)),
                Outcome3::SecondWins,
            ),
            // The second player accepts the first player's offer.
            (
                adjudicate(&p, &offer, &offer_atts, &request(SECOND)),
                Outcome3::Draw,
            ),
        ];
        for (adjudication, expected) in cases {
            let adjudication = adjudication.expect("a ruling");
            assert_eq!(adjudication.result(), expected);
            let (first, second) = expected.points();
            assert_eq!(adjudication.score(Side::First), first, "{expected:?} first");
            assert_eq!(
                adjudication.score(Side::Second),
                second,
                "{expected:?} second"
            );
            assert_eq!(
                u16::from(adjudication.score(Side::First))
                    + u16::from(adjudication.score(Side::Second)),
                100,
                "{expected:?}: the split must total 100"
            );
        }
    }

    #[test]
    fn abandonment_span_too_wide_saturates_instead_of_pardoning() {
        // The elapsed span is `cutoff − last attestation`, and `duration_since`
        // answers `None` for TWO distinct reasons that must not be conflated
        // (the distinction `kernel::step` draws for a played Ply): a cutoff
        // BEFORE the anchor (inverted time — clamped to zero), and a forward
        // span too wide for the representation. With t₀ = 0 the whole i64 range
        // fits and the abandonment flags; one second earlier (t₀ = −1) the same
        // span overflows, and swallowing it as zero would PARDON an
        // astronomically long abandonment — the verdict flipping from `timeout`
        // to a residual `resignation` on a one-second change of t₀.
        let plies: [Ply; 0] = [];
        let atts = [att(171, REQUEST, i64::MAX)];

        let fits = params("4k^3/8/8/8/8/8/8/R3K^3 / W/w", 600, 0);
        let adj = adjudicate(&fits, &plies, &atts, &request(SECOND)).expect("verdict");
        assert_eq!(adj.status(), Status::Timeout);
        assert_eq!(adj.result(), Outcome3::SecondWins);

        let overflows = params("4k^3/8/8/8/8/8/8/R3K^3 / W/w", 600, -1);
        assert_eq!(
            ts(i64::MAX).duration_since(ts(-1)),
            None,
            "the span must be the one that overflows"
        );
        let adj = adjudicate(&overflows, &plies, &atts, &request(SECOND)).expect("verdict");
        assert_eq!(adj.status(), Status::Timeout);
        assert_eq!(adj.result(), Outcome3::SecondWins);
    }

    #[test]
    fn cutoff_before_t0_charges_nothing() {
        // The other `None` branch: a Request canonically timed BEFORE t₀. The
        // span is negative, so the clock is charged nothing (`elapsed =
        // max(0, cutoff − T)`, time-accounting §Elapsed time) — no `timeout`,
        // and the invocation falls through to the residual resignation. No Ply
        // qualifies either (a candidate needs `t₀ ≤ at ≤ cutoff`), so the chain
        // is empty.
        let plies = [ply(1, FIRST, 1, "[\"a1\",\"a4\",null]")];
        let atts = [att(101, 1, 1100), att(171, REQUEST, 500)];
        let p = params("4k^3/8/8/8/8/8/8/R3K^3 / W/w", 600, 1000);
        let adj = adjudicate(&p, &plies, &atts, &request(SECOND)).expect("verdict");
        assert_eq!(adj.status(), Status::Resignation);
        assert_eq!(adj.result(), Outcome3::FirstWins);
    }

    #[test]
    fn empty_chain_abandonment_charges_the_player_who_never_moved() {
        // Nothing played at all: the on-move player's clock is ticked from t₀
        // (the empty chain's anchor) to the cutoff. At 600 the budget is exactly
        // spent (`elapsed > remaining` is the flag rule) — a resignation; at 601
        // the first player, on move since t₀, has abandoned. The verdict does
        // not depend on who invokes.
        let plies: [Ply; 0] = [];
        let p = params("4k^3/8/8/8/8/8/8/R3K^3 / W/w", 600, 0);

        let atts = [att(171, REQUEST, 600)];
        let adj = adjudicate(&p, &plies, &atts, &request(SECOND)).expect("verdict");
        assert_eq!(adj.status(), Status::Resignation);

        let atts = [att(171, REQUEST, 601)];
        for invoker in [FIRST, SECOND] {
            let adj = adjudicate(&p, &plies, &atts, &request(invoker)).expect("verdict");
            assert_eq!(adj.status(), Status::Timeout);
            assert_eq!(adj.result(), Outcome3::SecondWins);
        }
    }

    #[test]
    fn abandonment_spends_the_overtime_periods() {
        // A `[600]` main bank followed by a `[0, +30, /1]` overtime: the bank
        // exhausting rolls the overspend into the next period (kind `3420`
        // §time_control), so the flag falls only past 600 + 30. The gate must
        // account the whole control, not the first period alone.
        let main = Period::new(Duration::from_secs(600), None, None).expect("period");
        let overtime = Period::new(
            Duration::from_secs(0),
            Some(Duration::from_secs(30)),
            Some(1),
        )
        .expect("period");
        let plies: [Ply; 0] = [];
        for (cutoff, expected) in [(630_i64, Status::Resignation), (631, Status::Timeout)] {
            let p = SessionParams::new(
                eid(SESSION),
                pk(2),
                Some(pk(TIMESTAMPER)),
                pk(FIRST),
                pk(SECOND),
                TimeControl::new(main, vec![overtime]),
                Position::parse("4k^3/8/8/8/8/8/8/R3K^3 / W/w").expect("valid FEEN"),
                ts(0),
            );
            let atts = [att(171, REQUEST, cutoff)];
            let adj = adjudicate(&p, &plies, &atts, &request(SECOND)).expect("verdict");
            assert_eq!(adj.status(), expected, "cutoff {cutoff}");
        }
    }

    #[test]
    fn abandonment_reads_the_replayed_clock_not_the_initial_one() {
        // Fischer `[60, +30]`. The first player moves at 10 and 30, the second at
        // 20, so the replay leaves the second player on move with 60 − 10 + 30 =
        // 80 s banked and the anchor at 30. The increment is a POST-ply bonus,
        // not an allowance during the ply, so the flag falls at 30 + 80 + 1: the
        // gate must use the clock the replay produced (80 s), not the period's
        // initial 60 s.
        let period = Period::new(Duration::from_secs(60), Some(Duration::from_secs(30)), None)
            .expect("period");
        let plies = [
            ply(1, FIRST, 1, "[\"a1\",\"a4\",null]"),
            ply(2, SECOND, 1, "[\"e8\",\"e7\",null]"),
            ply(3, FIRST, 2, "[\"a4\",\"a5\",null]"),
        ];
        for (cutoff, expected) in [(110_i64, Status::Resignation), (111, Status::Timeout)] {
            let p = SessionParams::new(
                eid(SESSION),
                pk(2),
                Some(pk(TIMESTAMPER)),
                pk(FIRST),
                pk(SECOND),
                TimeControl::new(period, Vec::new()),
                Position::parse("4k^3/8/8/8/8/8/8/R3K^3 / W/w").expect("valid FEEN"),
                ts(0),
            );
            let atts = [
                att(101, 1, 10),
                att(102, 2, 20),
                att(103, 3, 30),
                att(171, REQUEST, cutoff),
            ];
            let adj = adjudicate(&p, &plies, &atts, &request(FIRST)).expect("verdict");
            assert_eq!(adj.status(), expected, "cutoff {cutoff}");
            if expected == Status::Timeout {
                // Charged to the second player, who is on move — not to the invoker.
                assert_eq!(adj.result(), Outcome3::FirstWins);
            }
        }
    }

    #[test]
    fn the_engine_turn_tracks_the_arbiter_play_order() {
        // The abandonment gate charges `state.position().active_side()`, while the
        // replay fills the slot of `side_at(next_half_move)`. The two views must
        // never diverge, or the clock ticked would not be the clock of the player
        // the arbiter is waiting for. Pinned over every prefix of a five-half-move
        // chain (strict alternation from a `first`-to-move founding position).
        use crate::natural_state::{natural_state, Conclusion};

        let moves: [(u8, u8, u32, &str); 5] = [
            (1, FIRST, 1, "[\"a1\",\"a4\",null]"),
            (2, SECOND, 1, "[\"e8\",\"e7\",null]"),
            (3, FIRST, 2, "[\"a4\",\"a5\",null]"),
            (4, SECOND, 2, "[\"e7\",\"e6\",null]"),
            (5, FIRST, 3, "[\"a5\",\"a6\",null]"),
        ];
        let p = params("4k^3/8/8/8/8/8/8/R3K^3 / W/w", 100_000, 0);
        for prefix in 0..=moves.len() {
            let plies: Vec<Ply> = moves
                .iter()
                .take(prefix)
                .map(|&(id, signer, step, content)| ply(id, signer, step, content))
                .collect();
            let mut atts: Vec<Attestation> = moves
                .iter()
                .take(prefix)
                .enumerate()
                .map(|(i, &(id, _, _, _))| att(100_u8.wrapping_add(id), id, 100 * (i as i64 + 1)))
                .collect();
            atts.push(att(171, REQUEST, 10_000));
            let ns = natural_state(&p, &plies, &atts, &request(FIRST)).expect("attested request");
            assert_eq!(ns.chain.len(), prefix);
            match &ns.conclusion {
                Conclusion::Ongoing(state) => assert_eq!(
                    state.position().active_side(),
                    p.side_at(ns.next_half_move()),
                    "turn/play-order divergence after {prefix} half-moves"
                ),
                Conclusion::Terminal(..) => panic!("the chain must stay ongoing"),
            }
        }
    }

    #[test]
    fn terminal_outranks_a_standing_draw_offer() {
        // The mating Ply itself carries the `draw` flag: the replay's terminal
        // verdict is the verdict, and the post-chain resolution (which would have
        // read the flag as a standing offer) never runs.
        let plies = [ply_draw(1, FIRST, 1, "[\"a1\",\"a8\",null]")];
        let atts = [att(101, 1, 100), att(171, REQUEST, 300)];
        let p = params("7k^/6pp/8/8/8/8/8/R3K^3 / W/w", 600, 0);
        let adj = adjudicate(&p, &plies, &atts, &request(SECOND)).expect("verdict");
        assert_eq!(adj.status(), Status::Checkmate);
        assert_eq!(adj.result(), Outcome3::FirstWins);
    }

    #[test]
    fn self_timed_session_rules_on_the_events_own_created_at() {
        // No designated timestamper: each event's relay-enforced `created_at` IS
        // its canonical timing, and any attestation present is inert. The Request
        // therefore always has a cutoff — `adjudicate` never withholds a ruling
        // for want of one.
        let period = Period::new(Duration::from_secs(600), None, None).expect("period");
        let p = SessionParams::new(
            eid(SESSION),
            pk(2),
            None, // self-timed
            pk(FIRST),
            pk(SECOND),
            TimeControl::new(period, Vec::new()),
            Position::parse("4k^3/8/8/8/8/8/8/R3K^3 / W/w").expect("valid FEEN"),
            ts(0),
        );
        let plies = [Ply::new(
            eid(1),
            pk(FIRST),
            eid(SESSION),
            1,
            false,
            "[\"a1\",\"a4\",null]".to_owned(),
            ts(100),
        )];
        let no_atts: [Attestation; 0] = [];

        // Within time: the residual resignation, against the invoker.
        let req = AdjudicationRequest::new(eid(REQUEST), pk(SECOND), eid(SESSION), pk(2), ts(400));
        let adj = adjudicate(&p, &plies, &no_atts, &req).expect("verdict");
        assert_eq!(adj.status(), Status::Resignation);
        assert_eq!(adj.result(), Outcome3::FirstWins);

        // Past the second player's budget (701 − 100 > 600): the abandonment.
        let req = AdjudicationRequest::new(eid(REQUEST), pk(SECOND), eid(SESSION), pk(2), ts(701));
        let adj = adjudicate(&p, &plies, &no_atts, &req).expect("verdict");
        assert_eq!(adj.status(), Status::Timeout);
        assert_eq!(adj.result(), Outcome3::FirstWins);

        // A stray attestation cannot move a self-timed cutoff.
        let stray = [att(171, REQUEST, 100_000)];
        let req = AdjudicationRequest::new(eid(REQUEST), pk(SECOND), eid(SESSION), pk(2), ts(400));
        let adj = adjudicate(&p, &plies, &stray, &req).expect("verdict");
        assert_eq!(adj.status(), Status::Resignation);
    }

    #[test]
    fn non_conforming_requests_never_resolve_as_a_resignation() {
        // The two `None` gates stand ahead of the residual resignation, on events
        // that WOULD otherwise produce one. Every rejection is checked against the
        // conforming control below, so the test cannot pass by accident.
        let plies = [ply(1, FIRST, 1, "[\"a1\",\"a4\",null]")];
        let atts = [att(101, 1, 100), att(171, REQUEST, 400)];
        let p = params("4k^3/8/8/8/8/8/8/R3K^3 / W/w", 600, 0);

        let control = adjudicate(&p, &plies, &atts, &request(SECOND)).expect("verdict");
        assert_eq!(control.status(), Status::Resignation);

        // Item 3 — a signer who is not a session player: the arbiter, the
        // timestamper, a stranger, and a key one byte away from a player's.
        let mut near_player = [FIRST; 32];
        near_player[31] = FIRST.wrapping_add(1);
        for signer in [
            pk(2),
            pk(TIMESTAMPER),
            pk(77),
            PublicKey::from_bytes(near_player),
        ] {
            let foreign =
                AdjudicationRequest::new(eid(REQUEST), signer, eid(SESSION), pk(2), ts(0));
            assert!(
                adjudicate(&p, &plies, &atts, &foreign).is_none(),
                "signer {signer} must not obtain a ruling"
            );
        }

        // Item 2 — another session, down to a single differing byte.
        let mut near_session = [SESSION; 32];
        near_session[31] = SESSION.wrapping_add(1);
        for session in [eid(51), EventId::from_bytes(near_session)] {
            let foreign = AdjudicationRequest::new(eid(REQUEST), pk(SECOND), session, pk(2), ts(0));
            assert!(adjudicate(&p, &plies, &atts, &foreign).is_none());
        }

        // Item 4 — another arbiter, down to a single differing byte.
        let mut near_arbiter = [2_u8; 32];
        near_arbiter[0] = 3;
        for arbiter in [pk(7), PublicKey::from_bytes(near_arbiter)] {
            let foreign =
                AdjudicationRequest::new(eid(REQUEST), pk(SECOND), eid(SESSION), arbiter, ts(0));
            assert!(adjudicate(&p, &plies, &atts, &foreign).is_none());
        }
    }

    #[test]
    fn the_verdict_does_not_depend_on_the_input_order() {
        // The verdict is a pure function of the event SET: shuffling the slices
        // (duplicate contents, a premove, an illegal candidate, a draw offer, and
        // their attestations) must not move it.
        let p = params("4k^3/8/8/8/8/8/8/R3K^3 / W/w", 600, 0);
        let plies = [
            ply(1, FIRST, 1, "[\"a1\",\"a4\",null]"),
            ply(2, FIRST, 1, "[\"a1\",\"a4\",null]"), // identical-content retry
            ply(3, FIRST, 1, "[\"a1\",\"a5\",null]"), // a divergent alternative
            ply_draw(4, SECOND, 1, "[\"e8\",\"e7\",null]"), // premoved, with an offer
            ply(5, SECOND, 1, "[\"e8\",\"e6\",null]"), // illegal
            ply(6, FIRST, 2, "[\"a4\",\"a5\",null]"),
        ];
        let atts = [
            att(101, 1, 100),
            att(102, 2, 110),
            att(103, 3, 120),
            att(104, 4, 90),
            att(105, 5, 95),
            att(106, 6, 300),
            att(171, REQUEST, 400),
        ];
        let reference = adjudicate(&p, &plies, &atts, &request(SECOND)).expect("verdict");

        // A deterministic LCG shuffle — no dev-dependency, no wall-clock seed.
        let mut seed = 0x2545_F491_4F6C_DD1D_u64;
        let mut next = move || {
            seed = seed
                .wrapping_mul(6_364_136_223_846_793_005)
                .wrapping_add(1_442_695_040_888_963_407);
            (seed >> 33) as usize
        };
        for round in 0..64 {
            let mut shuffled_plies = plies.clone();
            let mut shuffled_atts = atts;
            for i in (1..shuffled_plies.len()).rev() {
                shuffled_plies.swap(i, next() % (i + 1));
            }
            for i in (1..shuffled_atts.len()).rev() {
                shuffled_atts.swap(i, next() % (i + 1));
            }
            assert_eq!(
                adjudicate(&p, &shuffled_plies, &shuffled_atts, &request(SECOND)),
                Some(reference),
                "round {round}: the verdict moved with the input order"
            );
        }
    }

    #[test]
    fn score_per_side() {
        let plies = [ply(1, FIRST, 1, "[\"a1\",\"a8\",null]")];
        let atts = [att(101, 1, 100), att(171, REQUEST, 1000)];
        let p = params("7k^/6pp/8/8/8/8/8/R3K^3 / W/w", 600, 0);
        let adj = adjudicate(&p, &plies, &atts, &request(SECOND)).expect("verdict");
        assert_eq!(
            adj.score(sashite_sanki_engine::domain::side::Side::First),
            100
        );
        assert_eq!(
            adj.score(sashite_sanki_engine::domain::side::Side::Second),
            0
        );
    }
}
