//! The kernel result and the top-level [`kernel_result`] orchestration; the
//! conformance of a Conclusion ([`conforms`]) and which Conclusion rules
//! ([`select_conclusion`]).
//!
//! [`kernel_result`] turns a session's public events into the verdict the rule
//! system yields at a Conclusion's cutoff (kind `3425` §Natural state of events
//! at the cutoff; Statuses — Sanki §Verdict resolution): a termination
//! [`Status`] (the event's `content`) and a result distribution ([`Outcome3`],
//! the `result` tags). It composes every layer below it. A Conclusion is
//! **binding by correctness** (kind `3425` §Semantic constraints, item 8): it
//! terminates the session iff the verdict it claims *is* this result —
//! [`conforms`] is that check.
//!
//! # Verdict resolution
//!
//! Under the forgiving-premove model the verdict is **entirely play-derived** —
//! there is no separate equivocation sanction. The natural-state replay
//! ([`crate::natural_state`]) selects and applies the canonical Ply of each slot
//! and yields one of two chain ends:
//!
//! - a **terminal** verdict reached during replay — a rule-system ending
//!   (checkmate, …) or a played-Ply timeout — which is the verdict directly; or
//! - a still-**ongoing** end position, on which the invocation is resolved at the
//!   cutoff, in order: draw acceptance (`agreement`, [`crate::implicit`]);
//!   abandonment timeout (`timeout`: the on-move player's clock, ticked from the
//!   chain's last attestation — or t₀ for an empty chain — to the cutoff, has
//!   expired); otherwise **residual resignation** (`resignation`, decisive
//!   against the concluding player, whatever the turn).
//!
//! An illegal candidate — premove or live — is never a cause: it is skipped during
//! selection (never a loss), so there is no `illegalmove` termination. Because
//! resignation is the residual interpretation, a canonically timed Conclusion
//! from a session player **always has a kernel result** — concluding is at the
//! signer's risk (kind `3425` §Implications). [`kernel_result`] returns `None`
//! only when the Conclusion is structurally out of reach — it does not
//! reference this session, or its signer is not a session player (kind `3425`
//! §Semantic constraints, items 2–3) — or when it has no canonical timing yet
//! (the cutoff is undefined: the Conclusion is *pending*).
//!
//! Several Conclusions may coexist, and their cutoffs differ, hence possibly
//! their verdicts. [`select_conclusion`] pins the deterministic policy of kind
//! `3425` §Idempotence and finality — the earliest **conforming** Conclusion by
//! canonical timing, smallest event id as tiebreaker; a non-conforming one
//! never occupies the slot, whatever its timing.

use crate::event::{Attestation, Conclusion, Ply};
use crate::implicit::draw_acceptance;
use crate::natural_state::{natural_state, ChainEnd, NaturalState};
use crate::race_resolution::canonical_timing;
use crate::session::SessionParams;
use sashite_sanki_engine::clock::tick;
use sashite_sanki_engine::domain::outcome::Verdict;
use sashite_sanki_engine::domain::side::Side;
use sashite_sanki_engine::domain::status::{Outcome3, Status};
use sashite_sanki_engine::domain::time::Duration;

/// The verdict the rule system yields at a cutoff: a termination status and a
/// result distribution. A conforming Conclusion carries exactly this.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct KernelResult {
    status: Status,
    result: Outcome3,
}

impl KernelResult {
    /// The termination cause (the Conclusion's `content`).
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

    /// The score (`0`, `50`, or `100`) assigned to `side` — the value of the
    /// player's `result` tag.
    #[inline]
    #[must_use]
    pub const fn score(&self, side: Side) -> u8 {
        match (self.result, side) {
            (Outcome3::Draw, _) => 50,
            (Outcome3::FirstWins, Side::First) | (Outcome3::SecondWins, Side::Second) => 100,
            _ => 0,
        }
    }

    /// The result as the engine expresses a verdict.
    #[inline]
    #[must_use]
    pub const fn verdict(&self) -> Verdict {
        Verdict::Terminated {
            status: self.status,
            result: self.result,
        }
    }

    /// Builds a result from a terminal verdict, or `None` if the verdict is
    /// `Ongoing` (unreachable from [`kernel_result`], kept as a defensive seam).
    #[inline]
    fn from_verdict(verdict: Verdict) -> Option<Self> {
        match verdict {
            Verdict::Terminated { status, result } => Some(Self { status, result }),
            Verdict::Ongoing => None,
        }
    }
}

/// The verdict the rule system yields for the session's public events at the
/// Conclusion's cutoff, with its signer as the invoker. The Conclusion's own
/// claim is not read: this is what it *should* claim.
///
/// Returns `None` when no result is defined: the Conclusion references another
/// session or is signed by a non-player (kind `3425` §Semantic constraints,
/// items 2–3), or it has no canonical timing yet (pending).
#[must_use]
pub fn kernel_result(
    params: &SessionParams,
    plies: &[Ply],
    attestations: &[Attestation],
    conclusion: &Conclusion,
) -> Option<KernelResult> {
    // A Conclusion for another session is out of reach (kind 3425 §Semantic
    // constraints, item 2): no result — a cross-session Conclusion must never
    // resolve as a resignation here.
    if conclusion.session != params.session() {
        return None;
    }

    // A Conclusion from a non-player is invalid (item 3): no result.
    let invoker = params.side_of(conclusion.signer)?;

    // The natural state is also the gate: no canonical timing, no cutoff, no
    // result. The replay has already selected and applied the chain.
    let natural = natural_state(params, plies, attestations, conclusion)?;

    let verdict = resolve_play(params, &natural, conclusion, invoker);
    KernelResult::from_verdict(verdict)
}

/// Whether `conclusion` is **conforming on the rules axis** — kind `3425`
/// §Semantic constraints, item 8: it has canonical timing, and the verdict it
/// claims equals the one the rule system yields at its cutoff. The structural
/// constraints (items 1–7: tags, players, seats, the `nonce`) are the caller's
/// cross-event validation, performed before the event reaches this crate.
///
/// `false` for a pending Conclusion (no cutoff yet) as much as for a wrong
/// claim: neither terminates the session *now*. A caller that needs to tell the
/// two apart reads [`kernel_result`] directly.
#[inline]
#[must_use]
pub fn conforms(
    params: &SessionParams,
    plies: &[Ply],
    attestations: &[Attestation],
    conclusion: &Conclusion,
) -> bool {
    kernel_result(params, plies, attestations, conclusion)
        .is_some_and(|result| result.verdict() == conclusion.claim())
}

/// Selects the session's **canonical Conclusion** (kind `3425` §Idempotence
/// and finality): among the conforming Conclusions — this session, a
/// session-player signer, canonically timed, a correct claim — the earliest by
/// canonical timing, smallest event id as tiebreaker. Returns `None` when no
/// Conclusion is conforming yet: the session is still open.
///
/// Every candidate is checked by a full replay, so the cost is linear in the
/// number of Conclusions offered; a caller may pre-filter by session and signer
/// to spare the replays of the obviously invalid ones.
#[must_use]
pub fn select_conclusion<'a>(
    params: &SessionParams,
    plies: &[Ply],
    attestations: &[Attestation],
    conclusions: &'a [Conclusion],
) -> Option<&'a Conclusion> {
    conclusions
        .iter()
        .filter(|conclusion| conforms(params, plies, attestations, conclusion))
        .filter_map(|conclusion| {
            canonical_timing(
                attestations,
                conclusion.id,
                conclusion.created_at,
                params.timestamper(),
            )
            .map(|at| (at, conclusion))
        })
        .min_by(|(at_a, a), (at_b, b)| at_a.cmp(at_b).then_with(|| a.id.cmp(&b.id)))
        .map(|(_, conclusion)| conclusion)
}

/// The verdict the play produces: the natural state's terminal verdict if the
/// replay reached one, otherwise the invocation resolved at the cutoff on the
/// ongoing end position — in order: draw acceptance, abandonment timeout,
/// residual resignation.
fn resolve_play(
    params: &SessionParams,
    natural: &NaturalState<'_>,
    conclusion: &Conclusion,
    invoker: Side,
) -> Verdict {
    let state = match &natural.end {
        // The replay terminated (a rule-system ending or a played-Ply timeout):
        // that is the verdict.
        ChainEnd::Terminal(verdict, _at) => return *verdict,
        // Still ongoing: resolve the invocation at the cutoff.
        ChainEnd::Ongoing(state) => state,
    };

    // 2a. Draw acceptance: a standing offer accepted by the offeree.
    if let Some(verdict) = draw_acceptance(params, natural, conclusion) {
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
    // concluding player abandons — whatever the turn.
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

    use super::{conforms, kernel_result, select_conclusion};
    use crate::event::{Attestation, Conclusion, EventId, Ply, PublicKey};
    use crate::session::SessionParams;
    use sashite_sanki_engine::domain::status::{Outcome3, Status};
    use sashite_sanki_engine::domain::time::{Duration, Timestamp};
    use sashite_sanki_engine::domain::time_control::{Period, TimeControl};
    use sashite_sanki_engine::position::Position;

    const FIRST: u8 = 10;
    const SECOND: u8 = 20;
    const TIMESTAMPER: u8 = 99;
    const SESSION: u8 = 50;
    const CONCLUSION: u8 = 170;

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

    /// A Conclusion by `signer`, timed by attestation (its own `created_at` is
    /// inert in attested mode). Its claim is a placeholder: `kernel_result`
    /// does not read it.
    fn conclusion(signer: u8) -> Conclusion {
        conclusion_at(signer, 0)
    }

    fn conclusion_at(signer: u8, created_at: i64) -> Conclusion {
        Conclusion::new(
            eid(CONCLUSION),
            pk(signer),
            eid(SESSION),
            Status::Resignation,
            Outcome3::SecondWins,
            ts(created_at),
        )
    }

    fn params(feen: &str, tc_secs: u64, anchor: i64) -> SessionParams {
        let period = Period::new(Duration::from_secs(tc_secs), None, None).expect("period");
        SessionParams::new(
            eid(SESSION),
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
        let atts = [att(101, 1, 100), att(171, CONCLUSION, 1000)];
        let p = params("7k^/6pp/8/8/8/8/8/R3K^3 / W/w", 600, 0);
        let adj = kernel_result(&p, &plies, &atts, &conclusion(SECOND)).expect("result");
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
        let atts = [att(101, 1, 100), att(171, CONCLUSION, 400)];
        let p = params("4k^3/8/8/8/8/8/8/4K^3 / W/w", 600, 0);
        let adj = kernel_result(&p, &plies, &atts, &conclusion(SECOND)).expect("result");
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
        let atts = [
            att(101, 1, 100),
            att(102, 2, 200),
            att(171, CONCLUSION, 400),
        ];
        let p = params("4k^3/8/8/8/8/8/8/R3K^3 / W/w", 600, 0);
        let adj = kernel_result(&p, &plies, &atts, &conclusion(SECOND)).expect("result");
        assert_eq!(adj.status(), Status::Resignation);
        assert_eq!(adj.result(), Outcome3::FirstWins);
    }

    #[test]
    fn own_turn_conclusion_without_cause_is_resignation() {
        // The second player invokes on their own turn without playing, well
        // within their time (elapsed 300 ≤ 600): residual resignation.
        let plies = [ply(1, FIRST, 1, "[\"a1\",\"a4\",null]")];
        let atts = [att(101, 1, 100), att(171, CONCLUSION, 400)];
        let p = params("4k^3/8/8/8/8/8/8/R3K^3 / W/w", 600, 0);
        let adj = kernel_result(&p, &plies, &atts, &conclusion(SECOND)).expect("result");
        assert_eq!(adj.status(), Status::Resignation);
        assert_eq!(adj.result(), Outcome3::FirstWins);
    }

    #[test]
    fn off_turn_conclusion_without_cause_is_resignation() {
        // The first player invokes while the second is on move and within time
        // (elapsed 300 ≤ 600): residual resignation against the invoker —
        // invocation is turn-independent.
        let plies = [ply(1, FIRST, 1, "[\"a1\",\"a4\",null]")];
        let atts = [att(101, 1, 100), att(171, CONCLUSION, 400)];
        let p = params("4k^3/8/8/8/8/8/8/R3K^3 / W/w", 600, 0);
        let adj = kernel_result(&p, &plies, &atts, &conclusion(FIRST)).expect("result");
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
        let atts = [att(101, 1, 100), att(171, CONCLUSION, 1000)];
        let p = params("4k^3/8/8/8/8/8/8/R3K^3 / W/w", 600, 0);
        let adj = kernel_result(&p, &plies, &atts, &conclusion(SECOND)).expect("result");
        assert_eq!(adj.status(), Status::Agreement);
        assert_eq!(adj.result(), Outcome3::Draw);
    }

    #[test]
    fn abandonment_timeout() {
        // The first player moves (elapsed 100 ≤ 600), then the second lets their
        // clock run to the cutoff (elapsed 900 > 600); the first player invokes.
        let plies = [ply(1, FIRST, 1, "[\"a1\",\"a4\",null]")];
        let atts = [att(101, 1, 100), att(171, CONCLUSION, 1000)];
        let p = params("4k^3/8/8/8/8/8/8/R3K^3 / W/w", 600, 0);
        let adj = kernel_result(&p, &plies, &atts, &conclusion(FIRST)).expect("result");
        assert_eq!(adj.status(), Status::Timeout);
        assert_eq!(adj.result(), Outcome3::FirstWins);
    }

    #[test]
    fn own_expired_clock_is_a_timeout_not_a_resignation() {
        // The second player, on move with their clock expired (900 > 600),
        // invokes: the abandonment timeout is tested before the residual
        // resignation — a loss on time, against the invoker.
        let plies = [ply(1, FIRST, 1, "[\"a1\",\"a4\",null]")];
        let atts = [att(101, 1, 100), att(171, CONCLUSION, 1000)];
        let p = params("4k^3/8/8/8/8/8/8/R3K^3 / W/w", 600, 0);
        let adj = kernel_result(&p, &plies, &atts, &conclusion(SECOND)).expect("result");
        assert_eq!(adj.status(), Status::Timeout);
        assert_eq!(adj.result(), Outcome3::FirstWins);
    }

    #[test]
    fn empty_chain_conclusion_is_resignation() {
        // No move played, both within time (cutoff 400 ≤ 600): whoever invokes
        // resigns.
        let plies: [Ply; 0] = [];
        let atts = [att(171, CONCLUSION, 400)];
        let p = params("4k^3/8/8/8/8/8/8/R3K^3 / W/w", 600, 0);
        let adj = kernel_result(&p, &plies, &atts, &conclusion(SECOND)).expect("result");
        assert_eq!(adj.status(), Status::Resignation);
        assert_eq!(adj.result(), Outcome3::FirstWins);
    }

    #[test]
    fn unattested_conclusion_no_result() {
        let plies = [ply(1, FIRST, 1, "[\"a1\",\"a4\",null]")];
        let atts = [att(101, 1, 100)]; // no attestation for the Conclusion
        let p = params("4k^3/8/8/8/8/8/8/R3K^3 / W/w", 600, 0);
        assert!(kernel_result(&p, &plies, &atts, &conclusion(SECOND)).is_none());
    }

    #[test]
    fn non_player_conclusion_no_result() {
        // A Conclusion signed by a non-player is invalid: no result.
        let plies = [ply(1, FIRST, 1, "[\"a1\",\"a4\",null]")];
        let atts = [att(101, 1, 100), att(171, CONCLUSION, 1000)];
        let p = params("4k^3/8/8/8/8/8/8/R3K^3 / W/w", 600, 0);
        assert!(kernel_result(&p, &plies, &atts, &conclusion(77)).is_none());
    }

    #[test]
    fn cross_session_conclusion_no_result() {
        // A Conclusion referencing another session is out of reach (kind 3425
        // §Semantic constraints, item 2): no result — never a resignation in
        // the wrong session.
        let plies = [ply(1, FIRST, 1, "[\"a1\",\"a4\",null]")];
        let atts = [att(101, 1, 100), att(171, CONCLUSION, 400)];
        let p = params("4k^3/8/8/8/8/8/8/R3K^3 / W/w", 600, 0);
        let foreign = Conclusion::new(
            eid(CONCLUSION),
            pk(SECOND),
            eid(51),
            Status::Resignation,
            Outcome3::FirstWins,
            ts(0),
        );
        assert!(kernel_result(&p, &plies, &atts, &foreign).is_none());
    }

    /// A Conclusion with an explicit id and claim.
    fn claim(id: u8, signer: u8, status: Status, result: Outcome3) -> Conclusion {
        Conclusion::new(eid(id), pk(signer), eid(SESSION), status, result, ts(0))
    }

    #[test]
    fn conforms_iff_the_claim_is_the_kernel_result() {
        // Ra1-a8 mates: the kernel result at any later cutoff is checkmate,
        // first wins. A Conclusion claiming exactly that conforms; one claiming
        // anything else — the right status with the wrong split, the right
        // split with the wrong status — does not; a pending one does not either.
        let plies = [ply(1, FIRST, 1, "[\"a1\",\"a8\",null]")];
        let atts = [att(101, 1, 100), att(171, CONCLUSION, 1000)];
        let p = params("7k^/6pp/8/8/8/8/8/R3K^3 / W/w", 600, 0);

        let right = claim(CONCLUSION, SECOND, Status::Checkmate, Outcome3::FirstWins);
        assert!(conforms(&p, &plies, &atts, &right));
        // Whichever player signs it.
        let by_winner = claim(CONCLUSION, FIRST, Status::Checkmate, Outcome3::FirstWins);
        assert!(conforms(&p, &plies, &atts, &by_winner));

        let wrong_split = claim(CONCLUSION, SECOND, Status::Checkmate, Outcome3::SecondWins);
        assert!(!conforms(&p, &plies, &atts, &wrong_split));
        let wrong_status = claim(CONCLUSION, SECOND, Status::Resignation, Outcome3::FirstWins);
        assert!(!conforms(&p, &plies, &atts, &wrong_status));
        let drawn = claim(CONCLUSION, SECOND, Status::Agreement, Outcome3::Draw);
        assert!(!conforms(&p, &plies, &atts, &drawn));

        // Pending: no attestation of the Conclusion, hence no cutoff.
        let unattested = [att(101, 1, 100)];
        assert!(!conforms(&p, &plies, &unattested, &right));
        assert!(kernel_result(&p, &plies, &unattested, &right).is_none());

        // A non-player's claim never conforms, however right it looks.
        let stranger = claim(CONCLUSION, 77, Status::Checkmate, Outcome3::FirstWins);
        assert!(!conforms(&p, &plies, &atts, &stranger));
    }

    #[test]
    fn a_premature_claim_conforms_only_as_the_claimants_resignation() {
        // Nothing on the board, both clocks within budget at the cutoff: the
        // kernel's residual reading is the concluding player's resignation. A
        // claim of a win on time is non-conforming; the honest claim conforms.
        let plies = [ply(1, FIRST, 1, "[\"a1\",\"a4\",null]")];
        let atts = [att(101, 1, 100), att(171, CONCLUSION, 400)];
        let p = params("4k^3/8/8/8/8/8/8/R3K^3 / W/w", 600, 0);

        let win_on_time = claim(CONCLUSION, FIRST, Status::Timeout, Outcome3::FirstWins);
        assert!(!conforms(&p, &plies, &atts, &win_on_time));
        let resignation = claim(CONCLUSION, FIRST, Status::Resignation, Outcome3::SecondWins);
        assert!(conforms(&p, &plies, &atts, &resignation));

        // At a later cutoff the second player's clock has expired (1000 − 100 >
        // 600): the same win-on-time claim now conforms — the cutoff, not the
        // claimant, decides.
        let later = [att(101, 1, 100), att(171, CONCLUSION, 1000)];
        assert!(conforms(&p, &plies, &later, &win_on_time));
        assert!(!conforms(&p, &plies, &later, &resignation));
    }

    #[test]
    fn select_conclusion_earliest_conforming_timed() {
        // Ra1-a8 mates at 100. Four Conclusions: the earliest CONFORMING one
        // rules — not the earliest, and not the latest conforming.
        let plies = [ply(1, FIRST, 1, "[\"a1\",\"a8\",null]")];
        let p = params("7k^/6pp/8/8/8/8/8/R3K^3 / W/w", 600, 0);
        let conclusions = [
            // Conforming, attested @300.
            claim(170, FIRST, Status::Checkmate, Outcome3::FirstWins),
            // Conforming, attested @200 — the earliest conforming: rules.
            claim(172, SECOND, Status::Checkmate, Outcome3::FirstWins),
            // Non-conforming (a wrong claim), attested @150: skipped.
            claim(174, SECOND, Status::Resignation, Outcome3::FirstWins),
            // Non-conforming (another session), attested @100: skipped.
            Conclusion::new(
                eid(175),
                pk(FIRST),
                eid(51),
                Status::Checkmate,
                Outcome3::FirstWins,
                ts(0),
            ),
            // Conforming but unattested: pending, skipped.
            claim(176, FIRST, Status::Checkmate, Outcome3::FirstWins),
        ];
        let atts = [
            att(101, 1, 100),
            att(201, 170, 300),
            att(202, 172, 200),
            att(203, 174, 150),
            att(204, 175, 100),
        ];
        let selected =
            select_conclusion(&p, &plies, &atts, &conclusions).expect("a conclusion rules");
        assert_eq!(*selected.id.as_bytes(), [172; 32]);

        // Tie on timing: the smallest event id rules.
        let tied = [
            claim(180, FIRST, Status::Checkmate, Outcome3::FirstWins),
            claim(178, SECOND, Status::Checkmate, Outcome3::FirstWins),
        ];
        let tied_atts = [att(101, 1, 100), att(211, 180, 500), att(212, 178, 500)];
        let selected =
            select_conclusion(&p, &plies, &tied_atts, &tied).expect("a conclusion rules");
        assert_eq!(*selected.id.as_bytes(), [178; 32]);

        // No conforming timed Conclusion at all: the session stays open.
        assert!(select_conclusion(&p, &plies, &atts, &conclusions[2..]).is_none());
        let empty: [Conclusion; 0] = [];
        assert!(select_conclusion(&p, &plies, &atts, &empty).is_none());
    }

    #[test]
    fn two_conforming_conclusions_with_different_verdicts_the_earliest_rules() {
        // The first player moves at 100; the second's clock (600) expires at 700.
        // The second player concludes at 400 — premature: a conforming claim
        // there is their own resignation. The first player concludes at 1000
        // with a conforming win on time. Both conform at their own cutoff; the
        // earliest rules, at its signer's risk (kind 3425 §Idempotence and
        // finality).
        let plies = [ply(1, FIRST, 1, "[\"a1\",\"a4\",null]")];
        let p = params("4k^3/8/8/8/8/8/8/R3K^3 / W/w", 600, 0);
        let premature = claim(172, SECOND, Status::Resignation, Outcome3::FirstWins);
        let on_time = claim(170, FIRST, Status::Timeout, Outcome3::FirstWins);
        let atts = [att(101, 1, 100), att(202, 172, 400), att(201, 170, 1000)];
        assert!(conforms(&p, &plies, &atts, &premature));
        assert!(conforms(&p, &plies, &atts, &on_time));
        let offered = [on_time, premature];
        let selected = select_conclusion(&p, &plies, &atts, &offered).expect("a conclusion rules");
        assert_eq!(*selected.id.as_bytes(), [172; 32]);
        assert_eq!(selected.status, Status::Resignation);
    }

    #[test]
    fn score_matches_the_point_split_for_every_outcome() {
        // `score` must agree with the normative split of the `result` tags
        // (`Outcome3::points()`, statuses-sanki) on every (result, side) pair,
        // each reached through a real evaluation rather than a hand-built value.
        use sashite_sanki_engine::domain::side::Side;

        let mate = params("7k^/6pp/8/8/8/8/8/R3K^3 / W/w", 600, 0);
        let mating = [ply(1, FIRST, 1, "[\"a1\",\"a8\",null]")];
        let mate_atts = [att(101, 1, 100), att(171, CONCLUSION, 300)];

        let p = params("4k^3/8/8/8/8/8/8/R3K^3 / W/w", 600, 0);
        let none: [Ply; 0] = [];
        let empty_atts = [att(171, CONCLUSION, 300)];
        let offer = [ply_draw(1, FIRST, 1, "[\"a1\",\"a4\",null]")];
        let offer_atts = [att(101, 1, 100), att(171, CONCLUSION, 300)];

        let cases = [
            // A back-rank mate: the first player wins.
            (
                kernel_result(&mate, &mating, &mate_atts, &conclusion(SECOND)),
                Outcome3::FirstWins,
            ),
            // The first player invokes with nothing else to rule on: they resign.
            (
                kernel_result(&p, &none, &empty_atts, &conclusion(FIRST)),
                Outcome3::SecondWins,
            ),
            // The second player accepts the first player's offer.
            (
                kernel_result(&p, &offer, &offer_atts, &conclusion(SECOND)),
                Outcome3::Draw,
            ),
        ];
        for (result, expected) in cases {
            let result = result.expect("a result");
            assert_eq!(result.result(), expected);
            let (first, second) = expected.points();
            assert_eq!(result.score(Side::First), first, "{expected:?} first");
            assert_eq!(result.score(Side::Second), second, "{expected:?} second");
            assert_eq!(
                u16::from(result.score(Side::First)) + u16::from(result.score(Side::Second)),
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
        let atts = [att(171, CONCLUSION, i64::MAX)];

        let fits = params("4k^3/8/8/8/8/8/8/R3K^3 / W/w", 600, 0);
        let adj = kernel_result(&fits, &plies, &atts, &conclusion(SECOND)).expect("result");
        assert_eq!(adj.status(), Status::Timeout);
        assert_eq!(adj.result(), Outcome3::SecondWins);

        let overflows = params("4k^3/8/8/8/8/8/8/R3K^3 / W/w", 600, -1);
        assert_eq!(
            ts(i64::MAX).duration_since(ts(-1)),
            None,
            "the span must be the one that overflows"
        );
        let adj = kernel_result(&overflows, &plies, &atts, &conclusion(SECOND)).expect("result");
        assert_eq!(adj.status(), Status::Timeout);
        assert_eq!(adj.result(), Outcome3::SecondWins);
    }

    #[test]
    fn cutoff_before_t0_charges_nothing() {
        // The other `None` branch: a Conclusion canonically timed BEFORE t₀. The
        // span is negative, so the clock is charged nothing (`elapsed =
        // max(0, cutoff − T)`, time-accounting §Elapsed time) — no `timeout`,
        // and the invocation falls through to the residual resignation. No Ply
        // qualifies either (a candidate needs `t₀ ≤ at ≤ cutoff`), so the chain
        // is empty.
        let plies = [ply(1, FIRST, 1, "[\"a1\",\"a4\",null]")];
        let atts = [att(101, 1, 1100), att(171, CONCLUSION, 500)];
        let p = params("4k^3/8/8/8/8/8/8/R3K^3 / W/w", 600, 1000);
        let adj = kernel_result(&p, &plies, &atts, &conclusion(SECOND)).expect("result");
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

        let atts = [att(171, CONCLUSION, 600)];
        let adj = kernel_result(&p, &plies, &atts, &conclusion(SECOND)).expect("result");
        assert_eq!(adj.status(), Status::Resignation);

        let atts = [att(171, CONCLUSION, 601)];
        for invoker in [FIRST, SECOND] {
            let adj = kernel_result(&p, &plies, &atts, &conclusion(invoker)).expect("result");
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
                Some(pk(TIMESTAMPER)),
                pk(FIRST),
                pk(SECOND),
                TimeControl::new(main, vec![overtime]),
                Position::parse("4k^3/8/8/8/8/8/8/R3K^3 / W/w").expect("valid FEEN"),
                ts(0),
            );
            let atts = [att(171, CONCLUSION, cutoff)];
            let adj = kernel_result(&p, &plies, &atts, &conclusion(SECOND)).expect("result");
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
                att(171, CONCLUSION, cutoff),
            ];
            let adj = kernel_result(&p, &plies, &atts, &conclusion(FIRST)).expect("result");
            assert_eq!(adj.status(), expected, "cutoff {cutoff}");
            if expected == Status::Timeout {
                // Charged to the second player, who is on move — not to the invoker.
                assert_eq!(adj.result(), Outcome3::FirstWins);
            }
        }
    }

    #[test]
    fn the_engine_turn_tracks_the_kernel_play_order() {
        // The abandonment gate charges `state.position().active_side()`, while the
        // replay fills the slot of `side_at(next_half_move)`. The two views must
        // never diverge, or the clock ticked would not be the clock of the player
        // the kernel is waiting for. Pinned over every prefix of a five-half-move
        // chain (strict alternation from a `first`-to-move founding position).
        use crate::natural_state::{natural_state, ChainEnd};

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
            atts.push(att(171, CONCLUSION, 10_000));
            let ns =
                natural_state(&p, &plies, &atts, &conclusion(FIRST)).expect("attested conclusion");
            assert_eq!(ns.chain.len(), prefix);
            match &ns.end {
                ChainEnd::Ongoing(state) => assert_eq!(
                    state.position().active_side(),
                    p.side_at(ns.next_half_move()),
                    "turn/play-order divergence after {prefix} half-moves"
                ),
                ChainEnd::Terminal(..) => panic!("the chain must stay ongoing"),
            }
        }
    }

    #[test]
    fn terminal_outranks_a_standing_draw_offer() {
        // The mating Ply itself carries the `draw` flag: the replay's terminal
        // verdict is the verdict, and the post-chain resolution (which would have
        // read the flag as a standing offer) never runs.
        let plies = [ply_draw(1, FIRST, 1, "[\"a1\",\"a8\",null]")];
        let atts = [att(101, 1, 100), att(171, CONCLUSION, 300)];
        let p = params("7k^/6pp/8/8/8/8/8/R3K^3 / W/w", 600, 0);
        let adj = kernel_result(&p, &plies, &atts, &conclusion(SECOND)).expect("result");
        assert_eq!(adj.status(), Status::Checkmate);
        assert_eq!(adj.result(), Outcome3::FirstWins);
    }

    #[test]
    fn self_timed_session_rules_on_the_events_own_created_at() {
        // No designated timestamper: each event's relay-enforced `created_at` IS
        // its canonical timing, and any attestation present is inert. The Conclusion
        // therefore always has a cutoff — `kernel_result` never withholds a
        // result for want of one.
        let period = Period::new(Duration::from_secs(600), None, None).expect("period");
        let p = SessionParams::new(
            eid(SESSION),
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
        let adj = kernel_result(&p, &plies, &no_atts, &conclusion_at(SECOND, 400)).expect("result");
        assert_eq!(adj.status(), Status::Resignation);
        assert_eq!(adj.result(), Outcome3::FirstWins);

        // Past the second player's budget (701 − 100 > 600): the abandonment.
        let adj = kernel_result(&p, &plies, &no_atts, &conclusion_at(SECOND, 701)).expect("result");
        assert_eq!(adj.status(), Status::Timeout);
        assert_eq!(adj.result(), Outcome3::FirstWins);

        // A stray attestation cannot move a self-timed cutoff.
        let stray = [att(171, CONCLUSION, 100_000)];
        let adj = kernel_result(&p, &plies, &stray, &conclusion_at(SECOND, 400)).expect("result");
        assert_eq!(adj.status(), Status::Resignation);
    }

    #[test]
    fn out_of_reach_conclusions_never_resolve_as_a_resignation() {
        // The two `None` gates stand ahead of the residual resignation, on events
        // that WOULD otherwise produce one. Every rejection is checked against the
        // conforming control below, so the test cannot pass by accident.
        let plies = [ply(1, FIRST, 1, "[\"a1\",\"a4\",null]")];
        let atts = [att(101, 1, 100), att(171, CONCLUSION, 400)];
        let p = params("4k^3/8/8/8/8/8/8/R3K^3 / W/w", 600, 0);

        let control = kernel_result(&p, &plies, &atts, &conclusion(SECOND)).expect("result");
        assert_eq!(control.status(), Status::Resignation);

        // Item 3 — a signer who is not a session player: a bystander, the
        // timestamper, a stranger, and a key one byte away from a player's.
        let mut near_player = [FIRST; 32];
        near_player[31] = FIRST.wrapping_add(1);
        for signer in [
            pk(2),
            pk(TIMESTAMPER),
            pk(77),
            PublicKey::from_bytes(near_player),
        ] {
            let foreign = Conclusion::new(
                eid(CONCLUSION),
                signer,
                eid(SESSION),
                Status::Resignation,
                Outcome3::FirstWins,
                ts(0),
            );
            assert!(
                kernel_result(&p, &plies, &atts, &foreign).is_none(),
                "signer {signer} must not obtain a result"
            );
            assert!(!conforms(&p, &plies, &atts, &foreign));
        }

        // Item 2 — another session, down to a single differing byte.
        let mut near_session = [SESSION; 32];
        near_session[31] = SESSION.wrapping_add(1);
        for session in [eid(51), EventId::from_bytes(near_session)] {
            let foreign = Conclusion::new(
                eid(CONCLUSION),
                pk(SECOND),
                session,
                Status::Resignation,
                Outcome3::FirstWins,
                ts(0),
            );
            assert!(kernel_result(&p, &plies, &atts, &foreign).is_none());
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
            att(171, CONCLUSION, 400),
        ];
        let reference = kernel_result(&p, &plies, &atts, &conclusion(SECOND)).expect("result");

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
                kernel_result(&p, &shuffled_plies, &shuffled_atts, &conclusion(SECOND)),
                Some(reference),
                "round {round}: the verdict moved with the input order"
            );
        }
    }

    #[test]
    fn score_per_side() {
        let plies = [ply(1, FIRST, 1, "[\"a1\",\"a8\",null]")];
        let atts = [att(101, 1, 100), att(171, CONCLUSION, 1000)];
        let p = params("7k^/6pp/8/8/8/8/8/R3K^3 / W/w", 600, 0);
        let adj = kernel_result(&p, &plies, &atts, &conclusion(SECOND)).expect("result");
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
