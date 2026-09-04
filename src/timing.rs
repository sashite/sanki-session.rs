//! Canonical timing — the instant every rule of the kernel reads an event at.
//!
//! Timing depends on the session's mode (Canonical Timing §Timing modes and
//! mode selection). In **attested** mode the timing is the designated
//! timestamper's Event Timestamp Attestation (kind `3410`) of the event; the
//! event's own `created_at` is a self-claim and is ignored. In **self-timed**
//! mode — no timestamper, designated timing relays instead — there is no
//! attestation, and the event's own `created_at`, as accepted by a designated
//! timing relay, IS the canonical timing. [`canonical_timing`] resolves either
//! mode; [`canonical_attestation`] is the attested-mode primitive
//! (**meta-resolution**: when an event has more than one attestation by the
//! designated timestamper, the canonical one has the smallest `created_at`,
//! ties broken by the smallest attestation event id).
//!
//! **Precondition in self-timed mode.** This crate cannot observe a relay's
//! acceptance; it takes an event's `created_at` as its timing on the caller's
//! word. An event whose acceptance by one of the session's designated timing
//! relays is not established is *pending* (Canonical Timing §The pending
//! state): it cannot win a slot nor conclude, and the caller MUST NOT offer it
//! to the kernel — feeding it a Ply or a Conclusion fetched from an
//! undesignated relay would time it by a clock no party agreed to.
//!
//! Which candidate wins a slot is not decided here: it is the two-window
//! selection rule of [`crate::selection`], applied by
//! [`crate::natural_state`] with the timings this module resolves. The greedy
//! matching of Pairings (kind `3419`) is a founding-time concern, outside
//! per-session evaluation, and is not implemented here.

use crate::event::{Attestation, EventId, PublicKey};
use sashite_sanki_engine::domain::time::Timestamp;

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
/// event's own `created_at`, always `Some` — on the precondition stated in the
/// module documentation that the caller offers only events whose acceptance
/// by a designated timing relay is established.
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

#[cfg(test)]
mod tests {
    #![allow(
        clippy::unwrap_used,
        clippy::expect_used,
        clippy::panic,
        clippy::indexing_slicing
    )]

    use super::{canonical_attestation, canonical_timing};
    use crate::event::{Attestation, EventId, PublicKey};
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

    fn att(id: u8, signer: u8, attests: u8, at: i64) -> Attestation {
        Attestation::new(eid(id), pk(signer), eid(attests), ts(at))
    }

    /// Every permutation of `items`, i.e. every order in which a caller might
    /// supply the same event set. Meta-resolution is a pure function of that
    /// set, so all of them must yield the same canonical attestation.
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
        // DESIGNATED timestamper is authoritative (Canonical Timing §Timing modes).
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
    }

    #[test]
    fn meta_resolution_is_independent_of_the_input_order() {
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
    }

    #[test]
    fn extreme_timings_are_compared_not_saturated() {
        // Nostr `created_at` is an integer Unix second and `Timestamp` wraps a
        // signed `i64`, so pre-epoch and extremal instants are representable.
        // Timing resolution only ever compares them — there is no arithmetic
        // to saturate or wrap — so the extremes rank normally.
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
}
