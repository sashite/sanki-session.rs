//! Race resolution: deterministic selection of the canonical event for a slot.
//!
//! Authoritative timing depends on the session's mode (nostr-integration
//! §Timing). In **attested** mode the timing is the designated timestamper's
//! Event Timestamp Attestation (kind `3410`); a suite event's own `created_at`
//! is ignored. In **self-timed** mode — no timestamper, the default — there is
//! no attestation, and the event's own relay-enforced `created_at` IS the
//! canonical timing. [`canonical_timing`] resolves either mode. On top of it:
//!
//! - **Meta-resolution** ([`canonical_attestation`]) — the attested-mode
//!   primitive: when an event has more than one timestamper attestation, the
//!   canonical one has the smallest `created_at`, ties broken by the smallest
//!   attestation event id.
//! - **Slot selection** ([`canonical_ply`]) — among the Plies competing for one
//!   `(session, signer, step)` slot, the canonical one is the Ply whose canonical
//!   timing is smallest, ties broken by the smallest *Ply* event id. A Ply with
//!   no canonical timing (attested mode, no conforming attestation) is excluded
//!   (it is *pending*); in self-timed mode every Ply has timing.
//!
//! The greedy matching of Pairings (kind `3419`) is a founding-time concern,
//! outside per-session adjudication, and is not implemented here.

use crate::event::{Attestation, EventId, Ply, PublicKey};
use sashite_sanki_engine::domain::time::Timestamp;

/// A Ply selected as canonical for its slot, paired with its authoritative
/// timing — its [`canonical_timing`] (the attestation's `created_at` in attested
/// mode, or the Ply's own `created_at` when self-timed).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CanonicalPly<'a> {
    /// The canonical Ply.
    pub ply: &'a Ply,
    /// The authoritative timing of the Ply (its [`canonical_timing`]).
    pub at: Timestamp,
}

/// The canonical attestation of `attested` (meta-resolution).
///
/// Only attestations signed by the designated `timestamper` and referencing
/// `attested` count; among them, the canonical one has the smallest `created_at`,
/// ties broken by the smallest attestation event id. `None` if `attested` has no
/// conforming attestation.
#[must_use]
pub fn canonical_attestation(
    attestations: &[Attestation],
    attested: EventId,
    timestamper: PublicKey,
) -> Option<&Attestation> {
    attestations
        .iter()
        .filter(|attestation| attestation.attests == attested && attestation.signer == timestamper)
        .min_by_key(|attestation| (attestation.created_at, attestation.id))
}

/// The canonical timing of an event, in either timing mode.
///
/// Attested mode (`timestamper` is `Some`): the event's canonical attestation
/// from the designated timestamper (per [`canonical_attestation`]) — `None` when
/// it has none yet (pending). Self-timed mode (`timestamper` is `None`): the
/// event's own relay-enforced `created_at`, always `Some`.
#[must_use]
pub fn canonical_timing(
    attestations: &[Attestation],
    event_id: EventId,
    event_created_at: Timestamp,
    timestamper: Option<PublicKey>,
) -> Option<Timestamp> {
    match timestamper {
        Some(ts) => canonical_attestation(attestations, event_id, ts).map(|a| a.created_at),
        None => Some(event_created_at),
    }
}

/// The canonical Ply among `candidates` (slot selection).
///
/// `candidates` are assumed to share one `(session, signer, step)` slot. Each
/// candidate's authoritative timing is its [`canonical_timing`] for the mode;
/// candidates without one (attested mode, no conforming attestation) are pending
/// and excluded. The canonical Ply has the smallest such timing, ties broken by
/// the smallest Ply event id. `None` if no candidate has canonical timing.
#[must_use]
pub fn canonical_ply<'a>(
    candidates: impl IntoIterator<Item = &'a Ply>,
    attestations: &'a [Attestation],
    timestamper: Option<PublicKey>,
) -> Option<CanonicalPly<'a>> {
    candidates
        .into_iter()
        .filter_map(|ply| {
            canonical_timing(attestations, ply.id, ply.created_at, timestamper)
                .map(|at| CanonicalPly { ply, at })
        })
        .min_by_key(|canonical| (canonical.at, canonical.ply.id))
}

#[cfg(test)]
mod tests {
    #![allow(
        clippy::unwrap_used,
        clippy::expect_used,
        clippy::panic,
        clippy::indexing_slicing
    )]

    use super::{canonical_attestation, canonical_ply, canonical_timing};
    use crate::event::{Attestation, EventId, Ply, PublicKey};
    use sashite_sanki_engine::domain::time::Timestamp;

    const TIMESTAMPER: u8 = 99;

    fn pk(byte: u8) -> PublicKey {
        PublicKey::from_bytes([byte; 32])
    }

    fn eid(byte: u8) -> EventId {
        EventId::from_bytes([byte; 32])
    }

    fn ts(secs: i64) -> Timestamp {
        Timestamp::from_unix(secs)
    }

    fn ply(id: u8, signer: u8, step: u32) -> Ply {
        // created_at is ignored in attested mode (a timestamper is designated); the
        // self-timed tests build their own plies with explicit created_at.
        Ply::new(
            eid(id),
            pk(signer),
            eid(200),
            step,
            false,
            String::new(),
            ts(0),
        )
    }

    fn ply_at(id: u8, signer: u8, step: u32, created_at: i64) -> Ply {
        Ply::new(
            eid(id),
            pk(signer),
            eid(200),
            step,
            false,
            String::new(),
            ts(created_at),
        )
    }

    fn att(id: u8, signer: u8, attests: u8, at: i64) -> Attestation {
        Attestation::new(eid(id), pk(signer), eid(attests), ts(at))
    }

    /// Every permutation of `items`, i.e. every order in which a caller might
    /// supply the same event set. Race resolution is a pure function of that
    /// set, so all of them must yield the same canonical event.
    fn permutations<T: Clone>(items: &[T]) -> Vec<Vec<T>> {
        if items.len() <= 1 {
            return vec![items.to_vec()];
        }
        let mut orders = Vec::new();
        for index in 0..items.len() {
            let mut rest = items.to_vec();
            let head = rest.remove(index);
            for mut order in permutations(&rest) {
                order.insert(0, head.clone());
                orders.push(order);
            }
        }
        orders
    }

    #[test]
    fn meta_resolution_smallest_created_at() {
        let atts = vec![att(1, TIMESTAMPER, 50, 1000), att(2, TIMESTAMPER, 50, 900)];
        let canonical = canonical_attestation(&atts, eid(50), pk(TIMESTAMPER)).expect("attested");
        assert_eq!(canonical.created_at, ts(900));
    }

    #[test]
    fn meta_resolution_tiebreak_by_attestation_id() {
        // equal created_at: the smallest attestation id wins.
        let atts = vec![att(6, TIMESTAMPER, 50, 1000), att(5, TIMESTAMPER, 50, 1000)];
        let canonical = canonical_attestation(&atts, eid(50), pk(TIMESTAMPER)).expect("attested");
        assert_eq!(*canonical.id.as_bytes(), [5; 32]);
    }

    #[test]
    fn meta_resolution_ignores_non_timestamper_signer() {
        let atts = vec![att(1, 7, 50, 100)]; // signer != timestamper
        assert!(canonical_attestation(&atts, eid(50), pk(TIMESTAMPER)).is_none());
    }

    #[test]
    fn meta_resolution_ignores_other_attested_event() {
        let atts = vec![att(1, TIMESTAMPER, 51, 100)]; // attests 51, not 50
        assert!(canonical_attestation(&atts, eid(50), pk(TIMESTAMPER)).is_none());
        // And it cannot win the meta-resolution by being earlier: the `e`-tag
        // scope is a filter, not a ranking input (kind 3410 §Attested event).
        let atts = vec![att(1, TIMESTAMPER, 51, 100), att(2, TIMESTAMPER, 50, 900)];
        assert_eq!(
            canonical_timing(&atts, eid(50), ts(0), Some(pk(TIMESTAMPER))),
            Some(ts(900))
        );
    }

    #[test]
    fn a_strangers_attestation_can_never_move_a_timing() {
        // Anyone may publish a kind-3410 naming any event; only the session's
        // DESIGNATED timestamper is authoritative (nostr-integration §Timing).
        // A forged attestation must therefore be inert whichever way it leans —
        // it must neither pull a timing earlier (which would fake a premove or
        // beat an honest race), nor push it later (which would fake a timeout).
        let earlier = vec![att(1, TIMESTAMPER, 50, 1000), att(2, 7, 50, 100)];
        assert_eq!(
            canonical_timing(&earlier, eid(50), ts(0), Some(pk(TIMESTAMPER))),
            Some(ts(1000))
        );
        let later = vec![att(1, TIMESTAMPER, 50, 1000), att(2, 7, 50, 5000)];
        assert_eq!(
            canonical_timing(&later, eid(50), ts(0), Some(pk(TIMESTAMPER))),
            Some(ts(1000))
        );
        // Nor may a stranger win the id tiebreak at an equal `created_at`: the
        // designated attestation id 9 keeps the slot against the smaller id 1.
        let tied = vec![att(9, TIMESTAMPER, 50, 1000), att(1, 7, 50, 1000)];
        let canonical =
            canonical_attestation(&tied, eid(50), pk(TIMESTAMPER)).expect("the designated one");
        assert_eq!(*canonical.id.as_bytes(), [9; 32]);
        // With no designated attestation at all, a crowd of strangers still
        // leaves the event pending — it never becomes self-timed by default.
        let strangers = vec![att(1, 7, 50, 100), att(2, 8, 50, 200)];
        assert_eq!(
            canonical_timing(&strangers, eid(50), ts(4242), Some(pk(TIMESTAMPER))),
            None
        );
    }

    #[test]
    fn slot_selection_ignores_a_strangers_attestation() {
        // The same forgery at slot level: Ply 20 is genuinely attested @2000 and
        // loses to Ply 10 @1000. A stranger attesting Ply 20 @1 must not flip
        // the slot — nor contribute the winner's reported timing.
        let plies = [ply(10, 1, 1), ply(20, 1, 1)];
        let atts = vec![
            att(100, TIMESTAMPER, 10, 1000),
            att(101, TIMESTAMPER, 20, 2000),
            att(102, 7, 20, 1), // forged
        ];
        let canonical =
            canonical_ply(plies.iter(), &atts, Some(pk(TIMESTAMPER))).expect("a candidate");
        assert_eq!(*canonical.ply.id.as_bytes(), [10; 32]);
        assert_eq!(canonical.at, ts(1000));
    }

    #[test]
    fn attested_timing_ignores_the_events_own_created_at() {
        // In attested mode a suite event's own `created_at` is the signer's
        // self-claim and never drives timing (kind 3423 §Time accounting): the
        // resolved timing is the attestation's, whatever the event claims.
        let atts = vec![att(1, TIMESTAMPER, 50, 900)];
        for claimed in [0, 123_456, -7] {
            assert_eq!(
                canonical_timing(&atts, eid(50), ts(claimed), Some(pk(TIMESTAMPER))),
                Some(ts(900)),
                "self-claim {claimed} leaked into the attested timing"
            );
        }
    }

    #[test]
    fn self_timed_never_consults_attestations() {
        // Self-timed mode designates no timestamper, so attestation is a dormant
        // capability: kind-3410 events addressed to the session are inert, even
        // ones that would change the answer if they were consulted.
        let atts = vec![att(1, TIMESTAMPER, 50, 1), att(2, 7, 50, 2)];
        assert_eq!(
            canonical_timing(&atts, eid(50), ts(1234), None),
            Some(ts(1234))
        );
        // At slot level too: Ply 20's own `created_at` (900) wins over Ply 10's
        // (1000) even though the attestations claim the opposite order.
        let plies = [ply_at(10, 1, 1, 1000), ply_at(20, 1, 1, 900)];
        let atts = vec![
            att(100, TIMESTAMPER, 10, 1),
            att(101, TIMESTAMPER, 20, 9999),
        ];
        let canonical = canonical_ply(plies.iter(), &atts, None).expect("self-timed candidate");
        assert_eq!(*canonical.ply.id.as_bytes(), [20; 32]);
        assert_eq!(canonical.at, ts(900));
    }

    #[test]
    fn race_resolution_is_independent_of_the_input_order() {
        // Determinism (README §Design guarantees): the canonical event is a pure
        // function of the event SET. A `min_by_key` whose key dropped the id
        // tiebreak would return whichever tied element the caller happened to
        // list first, and would surface only here.
        //
        // Attestations 1 and 2 tie at 100; 3 is a stranger's, 4 attests another
        // event, 5 is a later one from the timestamper. The canonical is id 1.
        let atts = [
            att(1, TIMESTAMPER, 50, 100),
            att(2, TIMESTAMPER, 50, 100),
            att(3, 7, 50, 1),
            att(4, TIMESTAMPER, 51, 1),
            att(5, TIMESTAMPER, 50, 200),
        ];
        for order in permutations(&atts) {
            let canonical =
                canonical_attestation(&order, eid(50), pk(TIMESTAMPER)).expect("a conforming one");
            assert_eq!(*canonical.id.as_bytes(), [1; 32]);
            assert_eq!(canonical.created_at, ts(100));
        }

        // Slot selection likewise, over both the Plies' and the attestations'
        // orders: Plies 10 and 20 tie at 7, so the smallest Ply id wins.
        let plies = [ply(10, 1, 1), ply(20, 1, 1), ply(30, 1, 1)];
        let slot_atts = [
            att(100, TIMESTAMPER, 10, 7),
            att(101, TIMESTAMPER, 20, 7),
            att(102, TIMESTAMPER, 30, 9),
        ];
        for ply_order in permutations(&plies) {
            for att_order in permutations(&slot_atts) {
                let canonical = canonical_ply(ply_order.iter(), &att_order, Some(pk(TIMESTAMPER)))
                    .expect("a candidate");
                assert_eq!(*canonical.ply.id.as_bytes(), [10; 32]);
                assert_eq!(canonical.at, ts(7));
            }
        }
    }

    #[test]
    fn extreme_timings_are_compared_not_saturated() {
        // Nostr `created_at` is an integer Unix second and `Timestamp` wraps a
        // signed `i64`, so pre-epoch and extremal instants are representable.
        // Race resolution only ever compares them — there is no arithmetic to
        // saturate or wrap — so the extremes rank normally.
        let floor = vec![
            att(1, TIMESTAMPER, 50, i64::MIN),
            att(2, TIMESTAMPER, 50, 0),
        ];
        assert_eq!(
            canonical_timing(&floor, eid(50), ts(0), Some(pk(TIMESTAMPER))),
            Some(ts(i64::MIN))
        );
        let ceiling = vec![att(1, TIMESTAMPER, 50, i64::MAX)];
        assert_eq!(
            canonical_timing(&ceiling, eid(50), ts(0), Some(pk(TIMESTAMPER))),
            Some(ts(i64::MAX))
        );
        assert_eq!(
            canonical_timing(&[], eid(50), ts(i64::MIN), None),
            Some(ts(i64::MIN))
        );
        let plies = [ply_at(10, 1, 1, i64::MAX), ply_at(20, 1, 1, i64::MIN)];
        let canonical = canonical_ply(plies.iter(), &[], None).expect("self-timed candidate");
        assert_eq!(*canonical.ply.id.as_bytes(), [20; 32]);
        assert_eq!(canonical.at, ts(i64::MIN));
    }

    #[test]
    fn slot_smallest_attestation_created_at() {
        let plies = [ply(10, 1, 1), ply(20, 1, 1)];
        let atts = vec![
            att(100, TIMESTAMPER, 10, 1000),
            att(101, TIMESTAMPER, 20, 900),
        ];
        let canonical =
            canonical_ply(plies.iter(), &atts, Some(pk(TIMESTAMPER))).expect("a candidate");
        assert_eq!(*canonical.ply.id.as_bytes(), [20; 32]);
        assert_eq!(canonical.at, ts(900));
    }

    #[test]
    fn slot_tiebreak_by_ply_id() {
        // Equal canonical timing: the smallest Ply id wins.
        let plies = [ply(10, 1, 1), ply(20, 1, 1)];
        let atts = vec![
            att(100, TIMESTAMPER, 10, 1000),
            att(101, TIMESTAMPER, 20, 1000),
        ];
        let canonical =
            canonical_ply(plies.iter(), &atts, Some(pk(TIMESTAMPER))).expect("a candidate");
        assert_eq!(*canonical.ply.id.as_bytes(), [10; 32]);
    }

    #[test]
    fn slot_excludes_pending_plies() {
        // Ply 5 (smaller id) is not attested: excluded despite its id.
        let plies = [ply(5, 1, 1), ply(10, 1, 1)];
        let atts = vec![att(100, TIMESTAMPER, 10, 1000)];
        let canonical =
            canonical_ply(plies.iter(), &atts, Some(pk(TIMESTAMPER))).expect("a candidate");
        assert_eq!(*canonical.ply.id.as_bytes(), [10; 32]);
    }

    #[test]
    fn slot_no_attested_ply_yields_none() {
        let plies = [ply(5, 1, 1)];
        let atts: Vec<Attestation> = Vec::new();
        assert!(canonical_ply(plies.iter(), &atts, Some(pk(TIMESTAMPER))).is_none());
    }

    #[test]
    fn self_timed_timing_uses_event_created_at() {
        // No timestamper: the canonical timing is the event's own created_at,
        // regardless of any attestations present.
        let atts: Vec<Attestation> = Vec::new();
        assert_eq!(
            canonical_timing(&atts, eid(50), ts(1234), None),
            Some(ts(1234))
        );
    }

    #[test]
    fn self_timed_slot_selects_smallest_created_at() {
        // Two plies, no attestations: self-timed selection uses their own created_at.
        let plies = [ply_at(10, 1, 1, 1000), ply_at(20, 1, 1, 900)];
        let atts: Vec<Attestation> = Vec::new();
        let canonical = canonical_ply(plies.iter(), &atts, None).expect("self-timed candidate");
        assert_eq!(*canonical.ply.id.as_bytes(), [20; 32]);
        assert_eq!(canonical.at, ts(900));
    }

    #[test]
    fn self_timed_slot_tiebreak_by_ply_id() {
        // Equal created_at: the smallest Ply id wins — the same tiebreak as attested.
        let plies = [ply_at(20, 1, 1, 900), ply_at(10, 1, 1, 900)];
        let atts: Vec<Attestation> = Vec::new();
        let canonical = canonical_ply(plies.iter(), &atts, None).expect("candidate");
        assert_eq!(*canonical.ply.id.as_bytes(), [10; 32]);
    }
}
