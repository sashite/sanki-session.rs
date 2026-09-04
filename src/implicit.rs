//! Implicit draw by agreement (Statuses — Sanki §Implicit draw by agreement).
//!
//! A player offers a draw by attaching the `draw` flag to their Ply; the
//! opponent accepts by concluding the session while that offer is the last
//! half-move of the chain. Playing the next half-move instead implicitly
//! declines (the chain extends past the offer, and the condition below fails).
//!
//! - **Implicit draw by agreement** — the last Ply in the consecutive chain
//!   carries the `draw` flag (an offer by its signer), and the Conclusion is
//!   signed by that signer's **opponent**: the invocation accepts the offer.
//!   Result: `50/50`.
//!
//! The offer is carried by the chain's **last half-move at the cutoff** — the
//! Ply the replay actually selected and applied for the last filled slot. Three
//! consequences worth naming, all pinned by the tests below: a candidate that
//! carried the flag but lost its slot never offered anything; a reply that the
//! two-window rule **skipped** (illegal) or that the cutoff excluded did not
//! extend the chain, so the earlier offer is still the tail; and, under the
//! forgiving-premove rule, the tail may be an **anterior** Ply (a premove
//! carrying an offer, applied after the half-move it was published before).
//!
//! The other implicit convention — **residual resignation** — needs no
//! detection of its own: per Statuses — Sanki §Verdict resolution, the
//! post-chain resolution is ordered `agreement` → abandonment `timeout` →
//! `resignation`, and resignation is simply the fall-through, decisive against
//! the invoker, whatever the turn. That ordering lives in [`crate::verdict`];
//! this module only detects the acceptance.

use crate::event::Conclusion;
use crate::natural_state::NaturalState;
use crate::session::SessionParams;
use sashite_sanki_engine::domain::outcome::Verdict;
use sashite_sanki_engine::domain::status::Status;

/// The `agreement` verdict, if the invocation accepts a standing draw offer.
///
/// Returns `Some(agreement)` when the last chain Ply offers a draw and the
/// Conclusion is signed by its signer's opponent; `None` otherwise (no offer, an
/// offer extended past by play, an offerer invoking on their own offer, or a
/// non-player invoker).
///
/// It reads the chain's tail and nothing else: whether the replay had already
/// terminated (a mate outranks any standing offer) and whether a clock has run
/// out are the caller's ordering concern — see [`crate::verdict`].
#[must_use]
pub fn draw_acceptance(
    params: &SessionParams,
    natural: &NaturalState<'_>,
    conclusion: &Conclusion,
) -> Option<Verdict> {
    let invoker = params.side_of(conclusion.signer)?;
    let last = natural.chain.last()?;
    if !last.ply.draw {
        return None;
    }
    let offerer = params.side_of(last.ply.signer)?;
    (invoker == offerer.flip()).then(|| Verdict::drawn(Status::Agreement))
}

#[cfg(test)]
mod tests {
    #![allow(
        clippy::unwrap_used,
        clippy::expect_used,
        clippy::panic,
        clippy::indexing_slicing
    )]

    use super::draw_acceptance;
    use crate::event::{Conclusion, EventId, Ply, PublicKey};
    use crate::natural_state::{ChainEnd, NaturalState};
    use crate::race_resolution::CanonicalPly;
    use crate::session::SessionParams;
    use sashite_sanki_engine::domain::outcome::Verdict;
    use sashite_sanki_engine::domain::status::{Outcome3, Status};
    use sashite_sanki_engine::domain::time::{Duration, Timestamp};
    use sashite_sanki_engine::domain::time_control::{Period, TimeControl};
    use sashite_sanki_engine::position::Position;

    const FIRST: u8 = 10;
    const SECOND: u8 = 20;
    const SESSION: u8 = 50;

    fn pk(byte: u8) -> PublicKey {
        PublicKey::from_bytes([byte; 32])
    }

    fn eid(byte: u8) -> EventId {
        EventId::from_bytes([byte; 32])
    }

    fn ts(secs: i64) -> Timestamp {
        Timestamp::from_unix(secs)
    }

    fn ply(id: u8, signer: u8, step: u32, draw: bool) -> Ply {
        Ply::new(
            eid(id),
            pk(signer),
            eid(SESSION),
            step,
            draw,
            String::new(),
            ts(0),
        )
    }

    fn params() -> SessionParams {
        let period = Period::new(Duration::from_secs(600), None, None).expect("valid period");
        SessionParams::new(
            eid(SESSION),
            Some(pk(99)),
            pk(FIRST),
            pk(SECOND),
            TimeControl::new(period, Vec::new()),
            Position::parse("4k^3/8/8/8/8/8/8/4K^3 / W/w").expect("valid FEEN"),
            ts(0),
        )
    }

    fn conclusion(signer: u8) -> Conclusion {
        Conclusion::new(
            eid(170),
            pk(signer),
            eid(SESSION),
            Status::Agreement,
            Outcome3::Draw,
            ts(0),
        )
    }

    #[test]
    fn agreement_when_opponent_accepts_the_draw() {
        // Last half-move (first, step 1) marked `draw`; `second` (the opponent)
        // invokes: acceptance.
        let p1 = ply(1, FIRST, 1, true);
        let natural = NaturalState {
            chain: vec![CanonicalPly {
                ply: &p1,
                at: ts(100),
            }],
            cutoff: ts(1000),
            end: ChainEnd::Ongoing(Box::new(params().initial_state())),
        };
        let verdict = draw_acceptance(&params(), &natural, &conclusion(SECOND));
        assert_eq!(verdict, Some(Verdict::drawn(Status::Agreement)));
    }

    #[test]
    fn no_acceptance_without_a_draw_flag() {
        let p1 = ply(1, FIRST, 1, false);
        let natural = NaturalState {
            chain: vec![CanonicalPly {
                ply: &p1,
                at: ts(100),
            }],
            cutoff: ts(1000),
            end: ChainEnd::Ongoing(Box::new(params().initial_state())),
        };
        assert!(draw_acceptance(&params(), &natural, &conclusion(SECOND)).is_none());
    }

    #[test]
    fn offer_extended_past_by_play_is_declined() {
        // `first` offers at (first, 1); `second` answers at (second, 1) instead of
        // invoking: the offer is no longer the last half-move.
        let p1 = ply(1, FIRST, 1, true);
        let p2 = ply(2, SECOND, 1, false);
        let natural = NaturalState {
            chain: vec![
                CanonicalPly {
                    ply: &p1,
                    at: ts(100),
                },
                CanonicalPly {
                    ply: &p2,
                    at: ts(200),
                },
            ],
            cutoff: ts(1000),
            end: ChainEnd::Ongoing(Box::new(params().initial_state())),
        };
        assert!(draw_acceptance(&params(), &natural, &conclusion(FIRST)).is_none());
    }

    #[test]
    fn offerer_cannot_accept_their_own_offer() {
        // `first` offers the draw then invokes: not an acceptance (the residual
        // resolution in `verdict` decides what the invocation means).
        let p1 = ply(1, FIRST, 1, true);
        let natural = NaturalState {
            chain: vec![CanonicalPly {
                ply: &p1,
                at: ts(100),
            }],
            cutoff: ts(1000),
            end: ChainEnd::Ongoing(Box::new(params().initial_state())),
        };
        assert!(draw_acceptance(&params(), &natural, &conclusion(FIRST)).is_none());
    }

    #[test]
    fn empty_chain_has_no_offer() {
        let natural = NaturalState {
            chain: Vec::new(),
            cutoff: ts(1000),
            end: ChainEnd::Ongoing(Box::new(params().initial_state())),
        };
        assert!(draw_acceptance(&params(), &natural, &conclusion(SECOND)).is_none());
    }

    #[test]
    fn non_player_invoker_does_not_accept() {
        let p1 = ply(1, FIRST, 1, true);
        let natural = NaturalState {
            chain: vec![CanonicalPly {
                ply: &p1,
                at: ts(100),
            }],
            cutoff: ts(1000),
            end: ChainEnd::Ongoing(Box::new(params().initial_state())),
        };
        assert!(draw_acceptance(&params(), &natural, &conclusion(77)).is_none());
    }

    // --- Which Ply carries the standing offer, over a REAL replay ----------
    //
    // The tests above hand-build the chain; those below drive the natural-state
    // replay (`crate::natural_state`) so that the chain — hence the tail the
    // offer hangs on — is the one the selection rule actually produces.

    const TIMESTAMPER: u8 = 99;
    const CONCLUSION: u8 = 170;
    // A rook-and-king endgame: a stock of legal moves for the replay.
    const ROOK_KING: &str = "4k^3/8/8/8/8/8/8/R3K^3 / W/w";
    const RA1A4: &str = "[\"a1\",\"a4\",null]"; // first, step 1
    const RA1A5: &str = "[\"a1\",\"a5\",null]"; // first, step 1 (a divergent alternative)
    const KE8E7: &str = "[\"e8\",\"e7\",null]"; // second, step 1
    const KE8E6: &str = "[\"e8\",\"e6\",null]"; // second, step 1 — illegal (two squares)
    const RA4A5: &str = "[\"a4\",\"a5\",null]"; // first, step 2

    fn played(id: u8, signer: u8, step: u32, draw: bool, content: &str) -> Ply {
        Ply::new(
            eid(id),
            pk(signer),
            eid(SESSION),
            step,
            draw,
            content.to_owned(),
            ts(0),
        )
    }

    fn att(id: u8, attests: u8, at: i64) -> crate::event::Attestation {
        crate::event::Attestation::new(eid(id), pk(TIMESTAMPER), eid(attests), ts(at))
    }

    /// The session the replayed tests rule on: attested mode, a budget large
    /// enough that no clock interferes with the chain.
    fn rook_params() -> SessionParams {
        let period = Period::new(Duration::from_secs(100_000), None, None).expect("valid period");
        SessionParams::new(
            eid(SESSION),
            Some(pk(TIMESTAMPER)),
            pk(FIRST),
            pk(SECOND),
            TimeControl::new(period, Vec::new()),
            Position::parse(ROOK_KING).expect("valid FEEN"),
            ts(0),
        )
    }

    /// `(accepted by first, accepted by second)` for a replayed chain.
    fn acceptances(
        params: &SessionParams,
        plies: &[Ply],
        attestations: &[crate::event::Attestation],
    ) -> (bool, bool) {
        let natural =
            crate::natural_state::natural_state(params, plies, attestations, &conclusion(FIRST))
                .expect("attested conclusion");
        let agreement = Some(Verdict::drawn(Status::Agreement));
        (
            draw_acceptance(params, &natural, &conclusion(FIRST)) == agreement,
            draw_acceptance(params, &natural, &conclusion(SECOND)) == agreement,
        )
    }

    #[test]
    fn an_anterior_premove_may_carry_the_standing_offer() {
        // `second` premoves their step 1 at 50 WITH an offer, before `first`'s
        // step 1 lands at 100. The forgiving rule applies the premove as the
        // second half-move, so the chain's tail — the standing offer — is
        // `second`'s, anterior though its timing is. `first` accepts.
        let p = rook_params();
        let plies = [
            played(1, FIRST, 1, false, RA1A4),
            played(2, SECOND, 1, true, KE8E7),
        ];
        let atts = [
            att(101, 1, 100),
            att(102, 2, 50),
            att(171, CONCLUSION, 1000),
        ];
        assert_eq!(acceptances(&p, &plies, &atts), (true, false));
    }

    #[test]
    fn only_the_tail_offer_stands_when_both_half_moves_offer() {
        // Both players attach the flag. Only the tail's signer is the offerer, so
        // the acceptance belongs to `first` alone — `second` would be accepting
        // their own offer.
        let p = rook_params();
        let plies = [
            played(1, FIRST, 1, true, RA1A4),
            played(2, SECOND, 1, true, KE8E7),
        ];
        let atts = [
            att(101, 1, 100),
            att(102, 2, 200),
            att(171, CONCLUSION, 1000),
        ];
        assert_eq!(acceptances(&p, &plies, &atts), (true, false));
    }

    #[test]
    fn an_offer_two_half_moves_back_is_no_longer_standing() {
        // `first` offers at their step 1; `second` replies; `first` plays on. The
        // offer is buried in the chain and stands for nobody.
        let p = rook_params();
        let plies = [
            played(1, FIRST, 1, true, RA1A4),
            played(2, SECOND, 1, false, KE8E7),
            played(3, FIRST, 2, false, RA4A5),
        ];
        let atts = [
            att(101, 1, 100),
            att(102, 2, 200),
            att(103, 3, 300),
            att(171, CONCLUSION, 1000),
        ];
        assert_eq!(acceptances(&p, &plies, &atts), (false, false));
    }

    #[test]
    fn an_offer_on_a_ply_that_lost_its_slot_never_stood() {
        // `first` publishes two divergent step-1 contents: Ra1-a4 at 100 (no
        // offer) and Ra1-a5 at 200 WITH one. The informed window binds the
        // earliest legal candidate, so Ra1-a4 fills the slot and the flag on the
        // losing alternative is not an offer at all.
        let p = rook_params();
        let plies = [
            played(1, FIRST, 1, false, RA1A4),
            played(2, FIRST, 1, true, RA1A5),
        ];
        let atts = [
            att(101, 1, 100),
            att(102, 2, 200),
            att(171, CONCLUSION, 1000),
        ];
        assert_eq!(acceptances(&p, &plies, &atts), (false, false));
    }

    #[test]
    fn a_reply_the_selection_skipped_does_not_decline_the_offer() {
        // `first` offers at their step 1; `second` answers with an ILLEGAL move,
        // which the two-window rule skips (never a loss, and never a played
        // half-move). The chain therefore still ends on the offer, and `second`
        // may still accept it.
        let p = rook_params();
        let plies = [
            played(1, FIRST, 1, true, RA1A4),
            played(2, SECOND, 1, false, KE8E6),
        ];
        let atts = [
            att(101, 1, 100),
            att(102, 2, 200),
            att(171, CONCLUSION, 1000),
        ];
        assert_eq!(acceptances(&p, &plies, &atts), (false, true));
    }

    #[test]
    fn a_reply_after_the_cutoff_does_not_decline_the_offer() {
        // `second`'s reply is canonically timed after the cutoff, so it is not
        // part of the natural state the kernel evaluates: at the cutoff the offer
        // was still the last half-move, and the invocation accepts it.
        let p = rook_params();
        let plies = [
            played(1, FIRST, 1, true, RA1A4),
            played(2, SECOND, 1, false, KE8E7),
        ];
        let atts = [
            att(101, 1, 100),
            att(102, 2, 900),
            att(171, CONCLUSION, 500),
        ];
        assert_eq!(acceptances(&p, &plies, &atts), (false, true));
    }
}
