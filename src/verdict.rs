//! The verdict — what the rule system yields at a cutoff, and what a Conclusion
//! claims — and the three questions consumers ask of it: what a Conclusion
//! *should* claim ([`expected_verdict`]), whether a Conclusion conforms
//! ([`check`], [`conforms`]), and which Conclusion rules the session
//! ([`select_conclusion`]).
//!
//! The primitive is [`verdict_at`]: given the session, its public events, an
//! **invoker** (the side ending the session) and a **cutoff** (the instant the
//! natural state is evaluated at), it yields the verdict — Kernel — Sanki
//! §II.1's invocation. A Conclusion (kind `3425`) is one invocation with a
//! claim attached: its signer is the invoker, its canonical timing the cutoff
//! ([`cutoff_of`]), and it is **binding by correctness** (kind `3425` §Semantic
//! constraints, item 8): it terminates the session iff the verdict it claims
//! *is* the verdict the rule system yields there. A client about to conclude,
//! or a bot deciding whether to claim a win on time, calls [`verdict_at`] with
//! its own side and the present instant and publishes exactly that.
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
//!   chain's anchor — t₀ for an empty chain — to the cutoff, has expired);
//!   otherwise **residual resignation** (`resignation`, decisive against the
//!   invoker, whatever the turn).
//!
//! An illegal candidate — premove or live — is never a cause: it is skipped during
//! selection (never a loss), so there is no `illegalmove` termination. Because
//! resignation is the residual interpretation, every invocation has a verdict:
//! concluding is at the signer's risk (kind `3425` §Implications). The only
//! way [`verdict_at`] answers no verdict, from t₀ on, is a replay that hit a
//! broken internal invariant ([`crate::natural_state::ChainEnd::Inconsistent`])
//! — reported as [`NoVerdict::Inconsistent`], never resolved into a wrong
//! resignation.
//!
//! Several Conclusions may coexist, and their cutoffs differ, hence possibly
//! their verdicts. [`select_conclusion`] pins the deterministic policy of kind
//! `3425` §Idempotence and finality — the earliest **conforming** Conclusion by
//! canonical timing, smallest event id as tiebreaker; a non-conforming one
//! never occupies the slot, whatever its timing.

use crate::event::{Attestation, Conclusion, Ply, PublicKey};
use crate::implicit::accepts_standing_offer;
use crate::natural_state::{natural_state, ChainEnd, NaturalState};
use crate::session::SessionParams;
use crate::timing::canonical_timing;
use sashite_sanki_engine::clock::tick;
use sashite_sanki_engine::domain::outcome::Verdict as EngineVerdict;
use sashite_sanki_engine::domain::side::Side;
use sashite_sanki_engine::domain::status::{Outcome3, ResultKind, Status};
use sashite_sanki_engine::domain::time::{Duration, Timestamp};

/// A verdict: a termination status and a result distribution — what the rule
/// system yields at a cutoff ([`verdict_at`]), and what a Conclusion claims
/// ([`Conclusion::claim`]). The two are compared as values: a conforming
/// Conclusion carries exactly the verdict the rule system yields at its cutoff.
///
/// The pair is **coherent by construction**: a decisive status (`checkmate`,
/// `timeout`, `resignation`) carries a decisive outcome, a draw status a draw —
/// the `Status`/result-kind mapping of Statuses — Sanki. The wire admits more
/// (kind `3425` constrains its `content` and its `result` tags separately); a
/// pair no verdict of this kernel can yield is refused by [`Verdict::new`], so a
/// Conclusion carrying one cannot be built and needs no replay to be refused.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct Verdict {
    status: Status,
    outcome: Outcome3,
}

impl Verdict {
    /// A verdict from a status and an outcome — a Conclusion's `content` and
    /// the outcome its `result` tags map to
    /// ([`SessionParams::outcome_from_scores`]). `None` when the outcome's
    /// kind is not the status's: a draw status with a decisive outcome, or a
    /// decisive status with a draw.
    #[inline]
    #[must_use]
    pub const fn new(status: Status, outcome: Outcome3) -> Option<Self> {
        let coherent = match (status.result_kind(), outcome) {
            (ResultKind::Draw, Outcome3::Draw) => true,
            (ResultKind::Decisive, Outcome3::FirstWins | Outcome3::SecondWins) => true,
            (ResultKind::Draw, Outcome3::FirstWins | Outcome3::SecondWins)
            | (ResultKind::Decisive, Outcome3::Draw) => false,
        };
        if coherent {
            Some(Self { status, outcome })
        } else {
            None
        }
    }

    /// The `agreement` draw — the acceptance of a standing offer.
    const AGREEMENT: Self = Self {
        status: Status::Agreement,
        outcome: Outcome3::Draw,
    };

    /// The abandonment `timeout`, decisive against the player whose clock ran out.
    #[inline]
    const fn timeout_against(loser: Side) -> Self {
        Self {
            status: Status::Timeout,
            outcome: Outcome3::loss_for(loser),
        }
    }

    /// The residual `resignation`, decisive against the invoker.
    #[inline]
    const fn resignation_by(invoker: Side) -> Self {
        Self {
            status: Status::Resignation,
            outcome: Outcome3::loss_for(invoker),
        }
    }

    /// The verdict the engine reached, or `None` if its verdict is `Ongoing`
    /// (or, defensively, an incoherent pair the engine never produces).
    #[inline]
    #[must_use]
    pub(crate) const fn from_engine(verdict: EngineVerdict) -> Option<Self> {
        match verdict {
            EngineVerdict::Terminated { status, result } => Self::new(status, result),
            EngineVerdict::Ongoing => None,
        }
    }

    /// The termination cause (the Conclusion's `content`).
    #[inline]
    #[must_use]
    pub const fn status(&self) -> Status {
        self.status
    }

    /// The result distribution.
    #[inline]
    #[must_use]
    pub const fn outcome(&self) -> Outcome3 {
        self.outcome
    }

    /// The score (`0`, `50`, or `100`) assigned to `side` — the value of the
    /// player's `result` tag ([`Outcome3::points`], on the seat axis).
    #[inline]
    #[must_use]
    pub const fn score(&self, side: Side) -> u8 {
        let (first, second) = self.outcome.points();
        match side {
            Side::First => first,
            Side::Second => second,
        }
    }

    /// The two `result` tags of a Conclusion carrying this verdict — one
    /// `(player, score)` per player, `first` then `second` — for a publisher;
    /// the inverse of [`SessionParams::outcome_from_scores`].
    #[inline]
    #[must_use]
    pub fn scores(&self, params: &SessionParams) -> [(PublicKey, u8); 2] {
        [
            (params.player(Side::First), self.score(Side::First)),
            (params.player(Side::Second), self.score(Side::Second)),
        ]
    }
}

/// Why no verdict is defined — for an invocation ([`verdict_at`]) or for a
/// Conclusion ([`expected_verdict`], [`check`]). Consumers must tell the
/// transient reason from the final ones: only a pending Conclusion may still
/// become effective.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum NoVerdict {
    /// The Conclusion references another Game Session (kind `3425` §Semantic
    /// constraints, item 2): invalid here, whatever it is elsewhere.
    OtherSession,
    /// The Conclusion's signer is not one of the session's two players (item
    /// 3): invalid, and to be ignored for good.
    NotAPlayer,
    /// The Conclusion has no canonical timing yet — in attested mode, no
    /// attestation by the designated timestamper — so its cutoff is undefined
    /// and its verdict cannot be checked: **pending**, to be re-examined once
    /// timed (kind `3425` §Until the Conclusion has canonical timing).
    Pending,
    /// The cutoff precedes t₀ (kind `3425` §Signing party and §Semantic
    /// constraints, item 9; kind `3422` §Canonical session start): a session
    /// cannot be concluded before it is playable, and the kernel is not
    /// invoked before it. Final for a Conclusion: a canonical timing can only
    /// move *earlier* (meta-resolution keeps the smallest attestation), never
    /// later, so a Conclusion timed before t₀ stays there.
    BeforeStart,
    /// The replay hit a broken internal invariant
    /// ([`crate::natural_state::ChainEnd::Inconsistent`]): no verdict is
    /// defined, and the session must be treated as unresolved rather than
    /// concluded.
    Inconsistent,
}

impl NoVerdict {
    /// Whether the Conclusion may still become effective: only a pending one.
    #[inline]
    #[must_use]
    pub const fn is_transient(self) -> bool {
        matches!(self, Self::Pending)
    }
}

impl core::fmt::Display for NoVerdict {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self {
            Self::OtherSession => f.write_str("the Conclusion references another session"),
            Self::NotAPlayer => f.write_str("the Conclusion is not signed by a session player"),
            Self::Pending => f.write_str("the Conclusion has no canonical timing yet"),
            Self::BeforeStart => f.write_str("the cutoff precedes the session start"),
            Self::Inconsistent => {
                f.write_str("the replay hit a broken internal invariant; no verdict is defined")
            }
        }
    }
}

impl core::error::Error for NoVerdict {}

/// The verdict the rule system yields for the session's public events at
/// `cutoff`, with `invoker` as the concluding side (Kernel — Sanki §II.1's
/// invocation): the natural state's terminal verdict if the replay reached
/// one, otherwise the invocation resolved on the ongoing end position — draw
/// acceptance, abandonment timeout, residual resignation, in that order.
///
/// Defined for every cutoff at or after t₀ (a session is not playable before
/// it, and a Conclusion timed before it concludes nothing), but for a replay
/// that hit a broken invariant.
///
/// # Errors
///
/// [`NoVerdict::BeforeStart`] when `cutoff` precedes [`SessionParams::start`];
/// [`NoVerdict::Inconsistent`] when the replay cannot define a verdict.
pub fn verdict_at(
    params: &SessionParams,
    plies: &[Ply],
    attestations: &[Attestation],
    invoker: Side,
    cutoff: Timestamp,
) -> Result<Verdict, NoVerdict> {
    if cutoff < params.start() {
        return Err(NoVerdict::BeforeStart);
    }
    let natural = natural_state(params, plies, attestations, cutoff);
    resolve_play(params, &natural, invoker)
}

/// The invocation a Conclusion fixes — its signer's side and its canonical
/// timing — once the Conclusion is in reach: this session, a player signer,
/// timed, and timed at or after t₀.
///
/// # Errors
///
/// [`NoVerdict::OtherSession`], [`NoVerdict::NotAPlayer`],
/// [`NoVerdict::Pending`] (transient) or [`NoVerdict::BeforeStart`], checked
/// in that order — so the first reason reported is the one that holds
/// whatever later happens to the Conclusion's timing.
pub fn cutoff_of(
    params: &SessionParams,
    attestations: &[Attestation],
    conclusion: &Conclusion,
) -> Result<(Side, Timestamp), NoVerdict> {
    // A Conclusion for another session is out of reach (kind 3425 §Semantic
    // constraints, item 2): a cross-session Conclusion must never resolve as a
    // resignation here.
    if conclusion.session != params.session() {
        return Err(NoVerdict::OtherSession);
    }
    // A Conclusion from a non-player is invalid (item 3).
    let invoker = params
        .side_of(conclusion.signer)
        .ok_or(NoVerdict::NotAPlayer)?;
    // No canonical timing, no cutoff: pending.
    let cutoff = canonical_timing(
        attestations,
        conclusion.id,
        conclusion.created_at,
        params.timestamper(),
    )
    .ok_or(NoVerdict::Pending)?;
    // A Conclusion timed before t₀ concludes nothing (item 9): the session
    // was not playable yet — no Ply is valid before t₀ either.
    if cutoff < params.start() {
        return Err(NoVerdict::BeforeStart);
    }
    Ok((invoker, cutoff))
}

/// The verdict the rule system yields at a Conclusion's cutoff, with its
/// signer as the invoker — what the Conclusion is *expected* to claim, and
/// what a conforming Conclusion there carries. Its own claim is not read.
///
/// # Errors
///
/// [`NoVerdict`] when no verdict is defined: another session, a non-player
/// signer, a pending Conclusion, a Conclusion timed before t₀, or an
/// inconsistent replay.
pub fn expected_verdict(
    params: &SessionParams,
    plies: &[Ply],
    attestations: &[Attestation],
    conclusion: &Conclusion,
) -> Result<Verdict, NoVerdict> {
    let (invoker, cutoff) = cutoff_of(params, attestations, conclusion)?;
    verdict_at(params, plies, attestations, invoker, cutoff)
}

/// The outcome of checking a Conclusion on the rules axis — kind `3425`
/// §Semantic constraints, item 8. The constraints the cutoff depends on are
/// re-checked here (items 2, 3 and 9 — this session, a player signer, timed
/// at or after t₀ — the [`NoVerdict`] reasons); the other structural
/// constraints (the tags, the `p` and `seat` mirrors, the `nonce`) are the
/// caller's cross-event validation, performed before the event reaches this
/// crate.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Check {
    /// The claim equals the verdict the rule system yields at the cutoff: the
    /// Conclusion is conforming and, if canonical ([`select_conclusion`]),
    /// terminates the session.
    Conforming(Verdict),
    /// The claim differs from the verdict the rule system yields:
    /// non-conforming, of no effect, whatever its timing. `expected` is what a
    /// conforming Conclusion at this cutoff would carry.
    Wrong {
        /// The verdict the Conclusion claims.
        claimed: Verdict,
        /// The verdict the rule system yields at its cutoff.
        expected: Verdict,
    },
    /// No verdict is defined for the Conclusion — see [`NoVerdict`]; only
    /// [`NoVerdict::Pending`] may change.
    NoVerdict(NoVerdict),
}

impl Check {
    /// Whether the Conclusion is conforming.
    #[inline]
    #[must_use]
    pub const fn is_conforming(&self) -> bool {
        matches!(self, Self::Conforming(_))
    }
}

/// Checks a Conclusion on the rules axis: [`Check::Conforming`] iff it has
/// canonical timing and the verdict it claims equals the one the rule system
/// yields at its cutoff (kind `3425` §Semantic constraints, item 8).
#[must_use]
pub fn check(
    params: &SessionParams,
    plies: &[Ply],
    attestations: &[Attestation],
    conclusion: &Conclusion,
) -> Check {
    match expected_verdict(params, plies, attestations, conclusion) {
        Ok(expected) if expected == conclusion.claim => Check::Conforming(expected),
        Ok(expected) => Check::Wrong {
            claimed: conclusion.claim,
            expected,
        },
        Err(reason) => Check::NoVerdict(reason),
    }
}

/// Whether a Conclusion is conforming on the rules axis — [`check`] reduced to
/// a boolean. `false` for a pending Conclusion as much as for a wrong claim:
/// neither terminates the session *now*; a caller that must tell the two apart
/// reads [`check`].
#[inline]
#[must_use]
pub fn conforms(
    params: &SessionParams,
    plies: &[Ply],
    attestations: &[Attestation],
    conclusion: &Conclusion,
) -> bool {
    check(params, plies, attestations, conclusion).is_conforming()
}

/// The session's canonical Conclusion, as [`select_conclusion`] returns it:
/// the event, its cutoff, and the verdict it carries — so that a consumer
/// (a rating authority, a client) needs no second replay to act on it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CanonicalConclusion<'a> {
    /// The canonical Conclusion.
    pub conclusion: &'a Conclusion,
    /// Its cutoff — its canonical timing, the instant the session was
    /// concluded at (a terminal verdict may have been reached earlier).
    pub cutoff: Timestamp,
    /// The verdict it carries — equal to its claim, being conforming.
    pub verdict: Verdict,
}

/// Selects the session's **canonical Conclusion** (kind `3425` §Idempotence
/// and finality): among the conforming Conclusions — this session, a
/// session-player signer, canonically timed at or after t₀, a correct claim —
/// the earliest by canonical timing, smallest event id as tiebreaker.
/// `Ok(None)` when no offered Conclusion conforms *with established timing*:
/// the session is open for this consumer — still being played, or awaiting the
/// timing of a pending Conclusion (Canonical Timing §The pending state).
///
/// Every candidate is checked by a full replay, so the cost is linear in the
/// number of Conclusions offered; a caller may pre-filter by session and signer
/// ([`cutoff_of`]) to spare the replays of the obviously invalid ones.
///
/// # Errors
///
/// [`NoVerdict::Inconsistent`] when the replay of any Conclusion in reach hit
/// a broken internal invariant: the session is then unresolved for this
/// consumer, and no other Conclusion is promoted in its place — an
/// inconsistency is reported, never routed around. The out-of-reach reasons
/// (another session, a non-player, pending, before t₀) merely exclude the
/// Conclusion they concern, as a wrong claim does.
pub fn select_conclusion<'a>(
    params: &SessionParams,
    plies: &[Ply],
    attestations: &[Attestation],
    conclusions: &'a [Conclusion],
) -> Result<Option<CanonicalConclusion<'a>>, NoVerdict> {
    let mut canonical: Option<CanonicalConclusion<'a>> = None;
    for conclusion in conclusions {
        let Ok((invoker, cutoff)) = cutoff_of(params, attestations, conclusion) else {
            continue;
        };
        // At a cutoff `cutoff_of` accepted, `verdict_at` answers `Inconsistent`
        // or nothing: an inconsistency is propagated, never routed around.
        let verdict = verdict_at(params, plies, attestations, invoker, cutoff)?;
        if verdict != conclusion.claim {
            continue;
        }
        let candidate = CanonicalConclusion {
            conclusion,
            cutoff,
            verdict,
        };
        let earlier = match canonical {
            None => true,
            Some(held) => {
                (candidate.cutoff, candidate.conclusion.id) < (held.cutoff, held.conclusion.id)
            }
        };
        if earlier {
            canonical = Some(candidate);
        }
    }
    Ok(canonical)
}

/// The verdict the play produces: the natural state's terminal verdict if the
/// replay reached one, otherwise the invocation resolved at the cutoff on the
/// ongoing end position — in order: draw acceptance, abandonment timeout,
/// residual resignation.
fn resolve_play(
    params: &SessionParams,
    natural: &NaturalState<'_>,
    invoker: Side,
) -> Result<Verdict, NoVerdict> {
    let state = match &natural.end {
        // The replay terminated (a rule-system ending or a played-Ply timeout):
        // that is the verdict.
        ChainEnd::Terminal { verdict, .. } => return Ok(*verdict),
        // Still ongoing: resolve the invocation at the cutoff.
        ChainEnd::Ongoing(state) => state,
        // No verdict is defined.
        ChainEnd::Inconsistent => return Err(NoVerdict::Inconsistent),
    };

    // 2a. Draw acceptance: a standing offer accepted by the offeree.
    if accepts_standing_offer(params, natural, invoker) {
        return Ok(Verdict::AGREEMENT);
    }

    // 2b. Abandonment timeout: the player on move let their clock run out
    // before the cutoff (whether or not they are the invoker). The clock ticked
    // is the one the replay produced, from the chain's anchor — t₀ for an
    // empty chain — to the cutoff. The anchor never rewinds (a tail premove's
    // anterior timing does not move it back): time-accounting §Elapsed time,
    // Kernel — Sanki §II.6.
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
        return Ok(Verdict::timeout_against(on_move));
    }

    // 2c. Residual resignation: the invocation matches no other cause, so the
    // concluding player abandons — whatever the turn.
    Ok(Verdict::resignation_by(invoker))
}

#[cfg(test)]
mod tests {
    #![allow(
        clippy::unwrap_used,
        clippy::expect_used,
        clippy::panic,
        clippy::indexing_slicing
    )]

    use super::{
        check, conforms, expected_verdict, select_conclusion, verdict_at, Check, NoVerdict, Verdict,
    };
    use crate::event::{Attestation, Conclusion, EventId, Ply, PublicKey};
    use crate::session::{Seats, SessionParams};
    use sashite_sanki_engine::domain::side::Side;
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
    /// inert in attested mode). Its claim is a placeholder: `expected_verdict`
    /// does not read it.
    fn conclusion(signer: u8) -> Conclusion {
        conclusion_at(signer, 0)
    }

    fn conclusion_at(signer: u8, created_at: i64) -> Conclusion {
        Conclusion::new(
            eid(CONCLUSION),
            pk(signer),
            eid(SESSION),
            verdict(Status::Resignation, Outcome3::SecondWins),
            ts(created_at),
        )
    }

    /// A coherent verdict value.
    fn verdict(status: Status, outcome: Outcome3) -> Verdict {
        Verdict::new(status, outcome).expect("a coherent verdict")
    }

    fn params(feen: &str, tc_secs: u64, anchor: i64) -> SessionParams {
        let period = Period::new(Duration::from_secs(tc_secs), None, None).expect("period");
        SessionParams::new(
            eid(SESSION),
            Some(pk(TIMESTAMPER)),
            Seats::new(pk(FIRST), pk(SECOND)).expect("distinct"),
            TimeControl::new(period, Vec::new()),
            Position::parse(feen).expect("valid FEEN"),
            ts(anchor),
        )
        .expect("first to move")
    }

    #[test]
    fn mate_by_chain_replay() {
        // Ra1-a8 mates the walled-in black King: checkmate, first player wins.
        let plies = [ply(1, FIRST, 1, "[\"a1\",\"a8\",null]")];
        let atts = [att(101, 1, 100), att(171, CONCLUSION, 1000)];
        let p = params("7k^/6pp/8/8/8/8/8/R3K^3 / W/w", 600, 0);
        let adj = expected_verdict(&p, &plies, &atts, &conclusion(SECOND)).expect("result");
        assert_eq!(adj.status(), Status::Checkmate);
        assert_eq!(adj.outcome(), Outcome3::FirstWins);
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
        let adj = expected_verdict(&p, &plies, &atts, &conclusion(SECOND)).expect("result");
        assert_eq!(adj.status(), Status::Resignation);
        assert_eq!(adj.outcome(), Outcome3::FirstWins);
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
        let adj = expected_verdict(&p, &plies, &atts, &conclusion(SECOND)).expect("result");
        assert_eq!(adj.status(), Status::Resignation);
        assert_eq!(adj.outcome(), Outcome3::FirstWins);
    }

    #[test]
    fn own_turn_conclusion_without_cause_is_resignation() {
        // The second player invokes on their own turn without playing, well
        // within their time (elapsed 300 ≤ 600): residual resignation.
        let plies = [ply(1, FIRST, 1, "[\"a1\",\"a4\",null]")];
        let atts = [att(101, 1, 100), att(171, CONCLUSION, 400)];
        let p = params("4k^3/8/8/8/8/8/8/R3K^3 / W/w", 600, 0);
        let adj = expected_verdict(&p, &plies, &atts, &conclusion(SECOND)).expect("result");
        assert_eq!(adj.status(), Status::Resignation);
        assert_eq!(adj.outcome(), Outcome3::FirstWins);
    }

    #[test]
    fn off_turn_conclusion_without_cause_is_resignation() {
        // The first player invokes while the second is on move and within time
        // (elapsed 300 ≤ 600): residual resignation against the invoker —
        // invocation is turn-independent.
        let plies = [ply(1, FIRST, 1, "[\"a1\",\"a4\",null]")];
        let atts = [att(101, 1, 100), att(171, CONCLUSION, 400)];
        let p = params("4k^3/8/8/8/8/8/8/R3K^3 / W/w", 600, 0);
        let adj = expected_verdict(&p, &plies, &atts, &conclusion(FIRST)).expect("result");
        assert_eq!(adj.status(), Status::Resignation);
        assert_eq!(adj.outcome(), Outcome3::SecondWins);
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
        let adj = expected_verdict(&p, &plies, &atts, &conclusion(SECOND)).expect("result");
        assert_eq!(adj.status(), Status::Agreement);
        assert_eq!(adj.outcome(), Outcome3::Draw);
    }

    #[test]
    fn abandonment_timeout() {
        // The first player moves (elapsed 100 ≤ 600), then the second lets their
        // clock run to the cutoff (elapsed 900 > 600); the first player invokes.
        let plies = [ply(1, FIRST, 1, "[\"a1\",\"a4\",null]")];
        let atts = [att(101, 1, 100), att(171, CONCLUSION, 1000)];
        let p = params("4k^3/8/8/8/8/8/8/R3K^3 / W/w", 600, 0);
        let adj = expected_verdict(&p, &plies, &atts, &conclusion(FIRST)).expect("result");
        assert_eq!(adj.status(), Status::Timeout);
        assert_eq!(adj.outcome(), Outcome3::FirstWins);
    }

    #[test]
    fn own_expired_clock_is_a_timeout_not_a_resignation() {
        // The second player, on move with their clock expired (900 > 600),
        // invokes: the abandonment timeout is tested before the residual
        // resignation — a loss on time, against the invoker.
        let plies = [ply(1, FIRST, 1, "[\"a1\",\"a4\",null]")];
        let atts = [att(101, 1, 100), att(171, CONCLUSION, 1000)];
        let p = params("4k^3/8/8/8/8/8/8/R3K^3 / W/w", 600, 0);
        let adj = expected_verdict(&p, &plies, &atts, &conclusion(SECOND)).expect("result");
        assert_eq!(adj.status(), Status::Timeout);
        assert_eq!(adj.outcome(), Outcome3::FirstWins);
    }

    #[test]
    fn empty_chain_conclusion_is_resignation() {
        // No move played, both within time (cutoff 400 ≤ 600): whoever invokes
        // resigns.
        let plies: [Ply; 0] = [];
        let atts = [att(171, CONCLUSION, 400)];
        let p = params("4k^3/8/8/8/8/8/8/R3K^3 / W/w", 600, 0);
        let adj = expected_verdict(&p, &plies, &atts, &conclusion(SECOND)).expect("result");
        assert_eq!(adj.status(), Status::Resignation);
        assert_eq!(adj.outcome(), Outcome3::FirstWins);
    }

    #[test]
    fn unattested_conclusion_no_result() {
        let plies = [ply(1, FIRST, 1, "[\"a1\",\"a4\",null]")];
        let atts = [att(101, 1, 100)]; // no attestation for the Conclusion
        let p = params("4k^3/8/8/8/8/8/8/R3K^3 / W/w", 600, 0);
        assert_eq!(
            expected_verdict(&p, &plies, &atts, &conclusion(SECOND)),
            Err(NoVerdict::Pending)
        );
        assert!(NoVerdict::Pending.is_transient());
    }

    #[test]
    fn non_player_conclusion_no_result() {
        // A Conclusion signed by a non-player is invalid: no result.
        let plies = [ply(1, FIRST, 1, "[\"a1\",\"a4\",null]")];
        let atts = [att(101, 1, 100), att(171, CONCLUSION, 1000)];
        let p = params("4k^3/8/8/8/8/8/8/R3K^3 / W/w", 600, 0);
        assert_eq!(
            expected_verdict(&p, &plies, &atts, &conclusion(77)),
            Err(NoVerdict::NotAPlayer)
        );
        assert!(!NoVerdict::NotAPlayer.is_transient());
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
            verdict(Status::Resignation, Outcome3::FirstWins),
            ts(0),
        );
        assert_eq!(
            expected_verdict(&p, &plies, &atts, &foreign),
            Err(NoVerdict::OtherSession)
        );
    }

    /// A Conclusion with an explicit id and claim.
    fn claim(id: u8, signer: u8, status: Status, result: Outcome3) -> Conclusion {
        Conclusion::new(
            eid(id),
            pk(signer),
            eid(SESSION),
            verdict(status, result),
            ts(0),
        )
    }

    #[test]
    fn conforms_iff_the_claim_is_the_expected_verdict() {
        // Ra1-a8 mates: the expected verdict at any later cutoff is checkmate,
        // first wins. A Conclusion claiming exactly that conforms; one claiming
        // anything else — the right status with the wrong split, the right
        // split with the wrong status — does not; a pending one does not either.
        let plies = [ply(1, FIRST, 1, "[\"a1\",\"a8\",null]")];
        let atts = [att(101, 1, 100), att(171, CONCLUSION, 1000)];
        let p = params("7k^/6pp/8/8/8/8/8/R3K^3 / W/w", 600, 0);

        let right = claim(CONCLUSION, SECOND, Status::Checkmate, Outcome3::FirstWins);
        let right_result =
            |p: &SessionParams| expected_verdict(p, &plies, &atts, &right).expect("in reach");
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
        assert_eq!(
            check(&p, &plies, &unattested, &right),
            Check::NoVerdict(NoVerdict::Pending)
        );
        // `check` tells a wrong claim from an unreachable one, and says what
        // a conforming Conclusion would have carried.
        match check(&p, &plies, &atts, &wrong_split) {
            Check::Wrong { claimed, expected } => {
                assert_eq!(claimed, wrong_split.claim);
                assert_eq!(expected.status(), Status::Checkmate);
                assert_eq!(expected.outcome(), Outcome3::FirstWins);
            }
            other => panic!("expected a wrong claim, got {other:?}"),
        }
        assert_eq!(
            check(&p, &plies, &atts, &right),
            Check::Conforming(right_result(&p))
        );

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
                verdict(Status::Checkmate, Outcome3::FirstWins),
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
        let selected = select_conclusion(&p, &plies, &atts, &conclusions)
            .expect("consistent")
            .expect("a conclusion rules");
        assert_eq!(*selected.conclusion.id.as_bytes(), [172; 32]);
        assert_eq!(selected.cutoff, ts(200));
        assert_eq!(selected.verdict.status(), Status::Checkmate);

        // Tie on timing: the smallest event id rules.
        let tied = [
            claim(180, FIRST, Status::Checkmate, Outcome3::FirstWins),
            claim(178, SECOND, Status::Checkmate, Outcome3::FirstWins),
        ];
        let tied_atts = [att(101, 1, 100), att(211, 180, 500), att(212, 178, 500)];
        let selected = select_conclusion(&p, &plies, &tied_atts, &tied)
            .expect("consistent")
            .expect("a conclusion rules");
        assert_eq!(*selected.conclusion.id.as_bytes(), [178; 32]);

        // No conforming timed Conclusion at all: the session stays open.
        assert_eq!(
            select_conclusion(&p, &plies, &atts, &conclusions[2..]),
            Ok(None)
        );
        let empty: [Conclusion; 0] = [];
        assert_eq!(select_conclusion(&p, &plies, &atts, &empty), Ok(None));
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
        let selected = select_conclusion(&p, &plies, &atts, &offered)
            .expect("consistent")
            .expect("a conclusion rules");
        assert_eq!(*selected.conclusion.id.as_bytes(), [172; 32]);
        assert_eq!(selected.cutoff, ts(400));
        assert_eq!(selected.verdict.status(), Status::Resignation);
        assert_eq!(selected.verdict.outcome(), Outcome3::FirstWins);
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
                expected_verdict(&mate, &mating, &mate_atts, &conclusion(SECOND)),
                Outcome3::FirstWins,
            ),
            // The first player invokes with nothing else to rule on: they resign.
            (
                expected_verdict(&p, &none, &empty_atts, &conclusion(FIRST)),
                Outcome3::SecondWins,
            ),
            // The second player accepts the first player's offer.
            (
                expected_verdict(&p, &offer, &offer_atts, &conclusion(SECOND)),
                Outcome3::Draw,
            ),
        ];
        for (result, expected) in cases {
            let result = result.expect("a result");
            assert_eq!(result.outcome(), expected);
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
        let adj = expected_verdict(&fits, &plies, &atts, &conclusion(SECOND)).expect("result");
        assert_eq!(adj.status(), Status::Timeout);
        assert_eq!(adj.outcome(), Outcome3::SecondWins);

        let overflows = params("4k^3/8/8/8/8/8/8/R3K^3 / W/w", 600, -1);
        assert_eq!(
            ts(i64::MAX).duration_since(ts(-1)),
            None,
            "the span must be the one that overflows"
        );
        let adj = expected_verdict(&overflows, &plies, &atts, &conclusion(SECOND)).expect("result");
        assert_eq!(adj.status(), Status::Timeout);
        assert_eq!(adj.outcome(), Outcome3::SecondWins);
    }

    #[test]
    fn a_conclusion_timed_before_t0_concludes_nothing() {
        // A Conclusion canonically timed BEFORE t₀ (a scheduled start the
        // signer jumped): out of reach, finally — its timing will never move
        // — and never a resignation.
        let plies = [ply(1, FIRST, 1, "[\"a1\",\"a4\",null]")];
        let atts = [att(101, 1, 1100), att(171, CONCLUSION, 500)];
        let p = params("4k^3/8/8/8/8/8/8/R3K^3 / W/w", 600, 1000);
        assert_eq!(
            expected_verdict(&p, &plies, &atts, &conclusion(SECOND)),
            Err(NoVerdict::BeforeStart)
        );
        assert!(!NoVerdict::BeforeStart.is_transient());
        assert!(!conforms(
            &p,
            &plies,
            &atts,
            &claim(CONCLUSION, SECOND, Status::Resignation, Outcome3::FirstWins)
        ));
        // The kernel is not invoked before t₀ either: a client probing "what
        // if I conclude now" ahead of a scheduled start is told so, not told
        // it would resign.
        assert_eq!(
            verdict_at(&p, &plies, &atts, Side::Second, ts(500)),
            Err(NoVerdict::BeforeStart)
        );
        // At t₀ exactly the Conclusion is in reach: an empty chain (the Ply
        // @1100 is past the cutoff), nothing charged, the residual resignation.
        let at_start = [att(101, 1, 1100), att(171, CONCLUSION, 1000)];
        let r = expected_verdict(&p, &plies, &at_start, &conclusion(SECOND)).expect("in reach");
        assert_eq!(r.status(), Status::Resignation);
        assert_eq!(r.outcome(), Outcome3::FirstWins);
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
        let adj = expected_verdict(&p, &plies, &atts, &conclusion(SECOND)).expect("result");
        assert_eq!(adj.status(), Status::Resignation);

        let atts = [att(171, CONCLUSION, 601)];
        for invoker in [FIRST, SECOND] {
            let adj = expected_verdict(&p, &plies, &atts, &conclusion(invoker)).expect("result");
            assert_eq!(adj.status(), Status::Timeout);
            assert_eq!(adj.outcome(), Outcome3::SecondWins);
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
                Seats::new(pk(FIRST), pk(SECOND)).expect("distinct"),
                TimeControl::new(main, vec![overtime]),
                Position::parse("4k^3/8/8/8/8/8/8/R3K^3 / W/w").expect("valid FEEN"),
                ts(0),
            )
            .expect("first to move");
            let atts = [att(171, CONCLUSION, cutoff)];
            let adj = expected_verdict(&p, &plies, &atts, &conclusion(SECOND)).expect("result");
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
                Seats::new(pk(FIRST), pk(SECOND)).expect("distinct"),
                TimeControl::new(period, Vec::new()),
                Position::parse("4k^3/8/8/8/8/8/8/R3K^3 / W/w").expect("valid FEEN"),
                ts(0),
            )
            .expect("first to move");
            let atts = [
                att(101, 1, 10),
                att(102, 2, 20),
                att(103, 3, 30),
                att(171, CONCLUSION, cutoff),
            ];
            let adj = expected_verdict(&p, &plies, &atts, &conclusion(FIRST)).expect("result");
            assert_eq!(adj.status(), expected, "cutoff {cutoff}");
            if expected == Status::Timeout {
                // Charged to the second player, who is on move — not to the invoker.
                assert_eq!(adj.outcome(), Outcome3::FirstWins);
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
            let atts: Vec<Attestation> = moves
                .iter()
                .take(prefix)
                .enumerate()
                .map(|(i, &(id, _, _, _))| att(100_u8.wrapping_add(id), id, 100 * (i as i64 + 1)))
                .collect();
            let ns = natural_state(&p, &plies, &atts, ts(10_000));
            assert_eq!(ns.chain.len(), prefix);
            match &ns.end {
                ChainEnd::Ongoing(state) => assert_eq!(
                    state.position().active_side(),
                    p.side_at(ns.next_half_move()),
                    "turn/play-order divergence after {prefix} half-moves"
                ),
                ChainEnd::Terminal { .. } | ChainEnd::Inconsistent => {
                    panic!("the chain must stay ongoing")
                }
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
        let adj = expected_verdict(&p, &plies, &atts, &conclusion(SECOND)).expect("result");
        assert_eq!(adj.status(), Status::Checkmate);
        assert_eq!(adj.outcome(), Outcome3::FirstWins);
    }

    #[test]
    fn self_timed_session_rules_on_the_events_own_created_at() {
        // No designated timestamper: each event's own `created_at` (as accepted
        // by a designated timing relay — the caller's precondition) IS
        // its canonical timing, and any attestation present is inert. The Conclusion
        // therefore always has a cutoff — `expected_verdict` never withholds a
        // result for want of one.
        let period = Period::new(Duration::from_secs(600), None, None).expect("period");
        let p = SessionParams::new(
            eid(SESSION),
            None, // self-timed
            Seats::new(pk(FIRST), pk(SECOND)).expect("distinct"),
            TimeControl::new(period, Vec::new()),
            Position::parse("4k^3/8/8/8/8/8/8/R3K^3 / W/w").expect("valid FEEN"),
            ts(0),
        )
        .expect("first to move");
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
        let adj =
            expected_verdict(&p, &plies, &no_atts, &conclusion_at(SECOND, 400)).expect("result");
        assert_eq!(adj.status(), Status::Resignation);
        assert_eq!(adj.outcome(), Outcome3::FirstWins);

        // Past the second player's budget (701 − 100 > 600): the abandonment.
        let adj =
            expected_verdict(&p, &plies, &no_atts, &conclusion_at(SECOND, 701)).expect("result");
        assert_eq!(adj.status(), Status::Timeout);
        assert_eq!(adj.outcome(), Outcome3::FirstWins);

        // A stray attestation cannot move a self-timed cutoff.
        let stray = [att(171, CONCLUSION, 100_000)];
        let adj =
            expected_verdict(&p, &plies, &stray, &conclusion_at(SECOND, 400)).expect("result");
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

        let control = expected_verdict(&p, &plies, &atts, &conclusion(SECOND)).expect("result");
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
                verdict(Status::Resignation, Outcome3::FirstWins),
                ts(0),
            );
            assert_eq!(
                expected_verdict(&p, &plies, &atts, &foreign),
                Err(NoVerdict::NotAPlayer),
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
                verdict(Status::Resignation, Outcome3::FirstWins),
                ts(0),
            );
            assert_eq!(
                expected_verdict(&p, &plies, &atts, &foreign),
                Err(NoVerdict::OtherSession)
            );
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
        let reference = expected_verdict(&p, &plies, &atts, &conclusion(SECOND)).expect("result");
        // The reference itself: a4 fills slot 1 (earliest legal live move),
        // second's premoved offer fills slot 2 (the illegal e6 is skipped),
        // a4-a5 fills slot 3 — so the offer is not the tail and the second
        // player's invocation within time is their own resignation.
        assert_eq!(reference.status(), Status::Resignation);
        assert_eq!(reference.outcome(), Outcome3::FirstWins);

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
                expected_verdict(&p, &shuffled_plies, &shuffled_atts, &conclusion(SECOND)),
                Ok(reference),
                "round {round}: the verdict moved with the input order"
            );
        }
    }

    #[test]
    fn verdict_at_is_the_invocation_primitive() {
        // `verdict_at` takes a side and an instant directly — no synthetic
        // Conclusion. A back-rank mate is a first-player win at any later
        // cutoff, whoever "invokes"; before any clock expires, a bare
        // invocation by a side is that side's own resignation.
        let mate = params("7k^/6pp/8/8/8/8/8/R3K^3 / W/w", 600, 0);
        let mating = [ply(1, FIRST, 1, "[\"a1\",\"a8\",null]")];
        let atts = [att(101, 1, 100)];
        for invoker in [Side::First, Side::Second] {
            let r = verdict_at(&mate, &mating, &atts, invoker, ts(1000)).expect("defined");
            assert_eq!(r.status(), Status::Checkmate);
            assert_eq!(r.outcome(), Outcome3::FirstWins);
        }

        let p = params("4k^3/8/8/8/8/8/8/R3K^3 / W/w", 600, 0);
        let none: [Ply; 0] = [];
        let empty: [Attestation; 0] = [];
        // A client probing "what if I conclude now" reads its own resignation.
        let r = verdict_at(&p, &none, &empty, Side::First, ts(400)).expect("defined");
        assert_eq!(r.status(), Status::Resignation);
        assert_eq!(r.outcome(), Outcome3::SecondWins);
        // The score accessor and the seat-axis `scores` agree with the split.
        assert_eq!(r.score(Side::First), 0);
        assert_eq!(r.score(Side::Second), 100);
        let scores = r.scores(&p);
        assert_eq!(scores[0], (p.player(Side::First), 0));
        assert_eq!(scores[1], (p.player(Side::Second), 100));
        // …and round-trips through the seat-axis mapping.
        assert_eq!(p.outcome_from_scores(scores), Some(r.outcome()));
    }

    #[test]
    fn abandonment_is_charged_from_the_chains_last_anchor_not_a_tail_premoves_timing() {
        // first moves @100 (500 s left of 600); second's reply is a PREMOVE
        // timed @50 (anterior to the boundary 100), selected and applied. The
        // chain's last anchor is 100 — the boundary never rewinds to the
        // premove's timing — so first, on move again, flags at a cutoff
        // strictly past 100 + 500 = 600. Charged from the premove's own timing
        // (50) the flag would fall at 551: the two readings differ by exactly
        // the premove's anteriority (Kernel — Sanki §II.6; time-accounting
        // §Elapsed time).
        let plies = [
            ply(1, FIRST, 1, "[\"a1\",\"a4\",null]"),
            ply(2, SECOND, 1, "[\"e8\",\"e7\",null]"),
        ];
        let p = params("4k^3/8/8/8/8/8/8/R3K^3 / W/w", 600, 0);
        for (cutoff, expected) in [
            (551_i64, Status::Resignation),
            (600, Status::Resignation),
            (601, Status::Timeout),
        ] {
            let atts = [
                att(101, 1, 100),
                att(102, 2, 50),
                att(171, CONCLUSION, cutoff),
            ];
            let natural = crate::natural_state::natural_state(&p, &plies, &atts, ts(cutoff));
            assert_eq!(
                natural.chain.len(),
                2,
                "cutoff {cutoff}: the premove is applied"
            );
            let r = expected_verdict(&p, &plies, &atts, &conclusion(SECOND)).expect("in reach");
            assert_eq!(r.status(), expected, "cutoff {cutoff}");
            if expected == Status::Timeout {
                assert_eq!(r.outcome(), Outcome3::SecondWins, "against first, on move");
            }
        }
    }

    #[test]
    fn a_played_ply_timeout_outranks_the_mate_it_delivers() {
        // Ra1-a8 mates — but the mover's 600 s budget was already spent when
        // the Ply was timed (@700): the played-Ply timeout is the termination,
        // against the mover, and the mate on the board changes nothing
        // (time-accounting §The two timeout flavours).
        let plies = [ply(1, FIRST, 1, "[\"a1\",\"a8\",null]")];
        let atts = [att(101, 1, 700), att(171, CONCLUSION, 1000)];
        let p = params("7k^/6pp/8/8/8/8/8/R3K^3 / W/w", 600, 0);
        let r = expected_verdict(&p, &plies, &atts, &conclusion(SECOND)).expect("in reach");
        assert_eq!(r.status(), Status::Timeout);
        assert_eq!(r.outcome(), Outcome3::SecondWins);
        // One second earlier the same Ply is a mate.
        let atts = [att(101, 1, 600), att(171, CONCLUSION, 1000)];
        let r = expected_verdict(&p, &plies, &atts, &conclusion(SECOND)).expect("in reach");
        assert_eq!(r.status(), Status::Checkmate);
        assert_eq!(r.outcome(), Outcome3::FirstWins);
    }

    #[test]
    fn select_conclusion_in_self_timed_mode_reads_the_events_own_timing() {
        // No timestamper: each Conclusion's `created_at` is its cutoff. Two
        // conforming Conclusions — first's honest resignation @400 and
        // second's win on time @1000 — the earliest rules.
        let period = Period::new(Duration::from_secs(600), None, None).expect("period");
        let p = SessionParams::new(
            eid(SESSION),
            None,
            Seats::new(pk(FIRST), pk(SECOND)).expect("distinct"),
            TimeControl::new(period, Vec::new()),
            Position::parse("4k^3/8/8/8/8/8/8/R3K^3 / W/w").expect("valid FEEN"),
            ts(0),
        )
        .expect("first to move");
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
        let resignation = Conclusion::new(
            eid(172),
            pk(FIRST),
            eid(SESSION),
            verdict(Status::Resignation, Outcome3::SecondWins),
            ts(400),
        );
        let on_time = Conclusion::new(
            eid(170),
            pk(FIRST),
            eid(SESSION),
            verdict(Status::Timeout, Outcome3::FirstWins),
            ts(1000),
        );
        // A stray attestation must not move a self-timed cutoff.
        let stray = [att(171, 172, 5000)];
        for atts in [&no_atts[..], &stray[..]] {
            let offered = [on_time, resignation];
            let selected = select_conclusion(&p, &plies, atts, &offered)
                .expect("consistent")
                .expect("rules");
            assert_eq!(selected.conclusion.id, eid(172));
            assert_eq!(selected.cutoff, ts(400));
            assert_eq!(selected.verdict.status(), Status::Resignation);
        }
    }

    #[test]
    fn score_per_side() {
        let plies = [ply(1, FIRST, 1, "[\"a1\",\"a8\",null]")];
        let atts = [att(101, 1, 100), att(171, CONCLUSION, 1000)];
        let p = params("7k^/6pp/8/8/8/8/8/R3K^3 / W/w", 600, 0);
        let adj = expected_verdict(&p, &plies, &atts, &conclusion(SECOND)).expect("result");
        assert_eq!(adj.score(Side::First), 100);
        assert_eq!(adj.score(Side::Second), 0);
    }

    #[test]
    fn a_verdict_is_a_coherent_status_outcome_pair() {
        // `Verdict::new` admits exactly the pairs the rule system can yield:
        // a decisive status with a decisive outcome, a draw status with the
        // draw (Statuses — Sanki, the result-kind column). Every other pair
        // the wire could carry is refused, so no Conclusion claiming one can
        // be built — and `check` never has to call such a claim "wrong".
        use sashite_sanki_engine::domain::status::ResultKind;
        for status in Status::ALL {
            for outcome in [Outcome3::FirstWins, Outcome3::Draw, Outcome3::SecondWins] {
                let coherent = match status.result_kind() {
                    ResultKind::Draw => outcome == Outcome3::Draw,
                    ResultKind::Decisive => outcome != Outcome3::Draw,
                };
                let built = Verdict::new(status, outcome);
                assert_eq!(built.is_some(), coherent, "{status:?} / {outcome:?}");
                if let Some(v) = built {
                    assert_eq!(v.status(), status);
                    assert_eq!(v.outcome(), outcome);
                    let (first, second) = outcome.points();
                    assert_eq!(
                        (v.score(Side::First), v.score(Side::Second)),
                        (first, second)
                    );
                }
            }
        }
        // The three verdicts the post-chain resolution builds are coherent by
        // construction — the same pairs `new` would admit.
        assert_eq!(
            Verdict::AGREEMENT,
            verdict(Status::Agreement, Outcome3::Draw)
        );
        assert_eq!(
            Verdict::timeout_against(Side::First),
            verdict(Status::Timeout, Outcome3::SecondWins)
        );
        assert_eq!(
            Verdict::resignation_by(Side::Second),
            verdict(Status::Resignation, Outcome3::FirstWins)
        );
    }

    #[test]
    fn cutoff_of_reports_the_first_reason_that_holds() {
        // The reasons are checked in a fixed order — other session, non-player,
        // pending, before t₀ — so that the reason reported is the one that
        // stays true whatever later happens to the Conclusion's timing: a
        // stranger's unattested Conclusion is `NotAPlayer` (final), not
        // `Pending` (transient), and a foreign one is `OtherSession` even when
        // it is also unsigned by a player and untimed.
        use super::cutoff_of;
        let p = params("4k^3/8/8/8/8/8/8/R3K^3 / W/w", 600, 1000);
        let at = |id: u8, signer: u8, session: u8| {
            Conclusion::new(
                eid(id),
                pk(signer),
                eid(session),
                verdict(Status::Resignation, Outcome3::FirstWins),
                ts(0),
            )
        };
        let no_atts: [Attestation; 0] = [];
        let early = [att(171, CONCLUSION, 500)];
        let timed = [att(171, CONCLUSION, 1500)];

        // Foreign session, stranger, untimed: the session is reported.
        assert_eq!(
            cutoff_of(&p, &no_atts, &at(CONCLUSION, 77, 51)),
            Err(NoVerdict::OtherSession)
        );
        // This session, stranger, untimed — and timed before t₀: the signer.
        assert_eq!(
            cutoff_of(&p, &no_atts, &at(CONCLUSION, 77, SESSION)),
            Err(NoVerdict::NotAPlayer)
        );
        assert_eq!(
            cutoff_of(&p, &early, &at(CONCLUSION, 77, SESSION)),
            Err(NoVerdict::NotAPlayer)
        );
        // A player, untimed: pending.
        assert_eq!(
            cutoff_of(&p, &no_atts, &at(CONCLUSION, SECOND, SESSION)),
            Err(NoVerdict::Pending)
        );
        // A player, timed before t₀: before start.
        assert_eq!(
            cutoff_of(&p, &early, &at(CONCLUSION, SECOND, SESSION)),
            Err(NoVerdict::BeforeStart)
        );
        // In reach: the signer's side and the canonical timing.
        assert_eq!(
            cutoff_of(&p, &timed, &at(CONCLUSION, SECOND, SESSION)),
            Ok((Side::Second, ts(1500)))
        );
    }
}
