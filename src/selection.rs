//! Two-window forgiving-premove selection — the per-slot rule that picks a slot's
//! canonical Ply (Move Encoding — Sanki §Slot candidates and selection).
//!
//! A `(session, signer, step)` slot may hold several candidate Plies. Each is
//! classified against the slot's **boundary** — the maximum canonical timing
//! among the preceding half-moves, or t₀ for the first slot — by its own
//! canonical timing:
//!
//! - a candidate timed **strictly before** the boundary is **anterior** (a
//!   premove, committed before the position it faces existed);
//! - one timed **at or after** the boundary is **informed** (a live move,
//!   played in knowledge of the position).
//!
//! The canonical Ply is chosen by trying the two windows in order, anterior first:
//!
//! 1. **Anterior (premoves) — latest legal wins.** Among the `K` most recent
//!    anterior candidates by `(created_at, id)`, scanned newest-first, the first
//!    legal one — the *latest* legal premove — is applied. A re-premove supersedes
//!    an older one; an illegal premove is skipped in favour of the next-newest.
//! 2. **Informed (live moves) — earliest legal wins.** If no anterior candidate is
//!    legal, among the `K` earliest informed candidates by `(created_at, id)`,
//!    scanned oldest-first, the first legal one — the *earliest* legal live move —
//!    is applied. A move played in full knowledge is committed, not overwritten.
//! 3. Otherwise the slot is **unfilled**: the chain stops here.
//!
//! An **illegal candidate is always skipped** — premove or live, never a loss;
//! there is **no `illegalmove` outcome**. Legality is a precondition in both
//! windows; the window governs only which legal candidate binds when several exist.
//!
//! This module is the pure decision primitive — it consumes a **legality
//! probe** the caller supplies (backed by replaying the board,
//! [`crate::natural_state`]) and pins only the *selection*. Legality is probed
//! **lazily, on the capped windows only** — at most `2K` probes per slot, the
//! normative anti-flooding bound of Move Encoding — Sanki §Bounding a slot's
//! candidates: a flood of candidates costs sorting, never unbounded
//! full-rule-system replays. It is driven directly by the shared
//! `selection.json` conformance vectors, so this kernel and the TypeScript client
//! agree bit-for-bit on which Ply is canonical.
//!
//! Identical candidates are **collapsed upstream** for the cap's sake
//! ([`crate::natural_state`] keeps, per window, the one this rule would reach
//! first), so the collapse never changes what this rule selects. The window a
//! candidate belongs to is decided in one place — [`Candidate::is_anterior`] —
//! read by the collapse and by [`select_candidate`] alike.

use core::num::NonZeroUsize;
use sashite_sanki_engine::domain::time::Timestamp;

/// The reference rule-system document's per-window candidate cap `K`
/// (`session.candidate_cap` of the `sanki` manifest): at most the `K`
/// most-recent anterior candidates, or the `K` earliest informed ones, are
/// considered (≤ `2K` legality tests per slot). `K > 1` leaves room for an
/// honest re-premove or retry; a player flooding their own window past `K` only
/// self-harms. A session carries its own document's value
/// ([`crate::session::SessionParams::candidate_cap`]); this constant is the
/// default for the reference document. A cap is at least `1` by type — a cap
/// of `0` would fill no slot ever.
pub const CANDIDATE_CAP: NonZeroUsize = match NonZeroUsize::new(8) {
    Some(cap) => cap,
    // `8 != 0`; the arm is never taken, and the type admits no other value.
    None => NonZeroUsize::MIN,
};

/// A slot candidate reduced to what selection needs: its identity (the race
/// tiebreak) and its canonical timing. Legality is NOT carried — it is probed
/// lazily, through the caller's callback, on the capped windows only.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Candidate<Id> {
    /// The candidate's identity — the event-id race tiebreak.
    pub id: Id,
    /// The candidate's canonical timing.
    pub created_at: Timestamp,
}

impl<Id> Candidate<Id> {
    /// Whether the candidate is **anterior** to `boundary` — timed strictly
    /// before it, a premove committed before the position it faces existed.
    /// Otherwise it is **informed** — timed at or after the boundary, a live
    /// move played in knowledge of the position. The one place the split is
    /// decided (Move Encoding — Sanki §Slot candidates and selection).
    #[inline]
    #[must_use]
    pub fn is_anterior(&self, boundary: Timestamp) -> bool {
        self.created_at < boundary
    }
}

/// The outcome of selecting among a slot's candidates.
#[derive(Debug, PartialEq, Eq)]
pub enum Selection<'a, Id> {
    /// A candidate fills the slot (the chain continues with it).
    Applied(&'a Candidate<Id>),
    /// No candidate qualifies — the slot is unfilled (the chain stops).
    Unfilled,
}

impl<Id> Selection<'_, Id> {
    /// The selected candidate, if any (`None` only for [`Selection::Unfilled`]).
    #[inline]
    #[must_use]
    pub const fn selected(&self) -> Option<&Candidate<Id>> {
        match self {
            Self::Applied(candidate) => Some(candidate),
            Self::Unfilled => None,
        }
    }
}

/// Selects the canonical candidate for a slot with the given `boundary`.
///
/// `candidates` is the slot's unordered candidate set, each anterior or
/// informed per [`Candidate::is_anterior`]. Implements the two-window rule
/// above, with the per-window cap `cap` (`K`); `is_legal` is the caller's
/// legality probe, consulted **only** for candidates inside a capped window —
/// at most `2 × cap` probes per call, however many candidates are flooded.
#[must_use]
pub fn select_candidate<'a, Id: Ord>(
    boundary: Timestamp,
    candidates: &'a [Candidate<Id>],
    cap: NonZeroUsize,
    mut is_legal: impl FnMut(&Id) -> bool,
) -> Selection<'a, Id> {
    // Anterior window (premoves): the K most recent by (created_at, id), newest
    // first — the first legal is the LATEST legal premove (a re-premove
    // supersedes an older one).
    let mut anterior: Vec<&'a Candidate<Id>> = candidates
        .iter()
        .filter(|candidate| candidate.is_anterior(boundary))
        .collect();
    anterior.sort_by(|a, b| {
        b.created_at
            .cmp(&a.created_at)
            .then_with(|| b.id.cmp(&a.id))
    });
    if let Some(chosen) = anterior
        .into_iter()
        .take(cap.get())
        .find(|candidate| is_legal(&candidate.id))
    {
        return Selection::Applied(chosen);
    }

    // Informed window (live moves): the K earliest by (created_at, id), oldest
    // first — the first legal is the EARLIEST legal live move (committed on its
    // first legal instance, not overwritten by a later one).
    let mut informed: Vec<&'a Candidate<Id>> = candidates
        .iter()
        .filter(|candidate| !candidate.is_anterior(boundary))
        .collect();
    informed.sort_by(|a, b| {
        a.created_at
            .cmp(&b.created_at)
            .then_with(|| a.id.cmp(&b.id))
    });
    if let Some(chosen) = informed
        .into_iter()
        .take(cap.get())
        .find(|candidate| is_legal(&candidate.id))
    {
        return Selection::Applied(chosen);
    }

    Selection::Unfilled
}

#[cfg(test)]
mod tests {
    #![allow(
        clippy::unwrap_used,
        clippy::expect_used,
        clippy::panic,
        clippy::indexing_slicing
    )]

    use super::{select_candidate, Candidate, Selection, CANDIDATE_CAP};
    use core::num::NonZeroUsize;
    use sashite_sanki_engine::domain::time::Timestamp;

    fn ts(secs: i64) -> Timestamp {
        Timestamp::from_unix(secs)
    }

    /// A per-window cap `K`.
    fn cap(k: usize) -> NonZeroUsize {
        NonZeroUsize::new(k).expect("a cap is at least 1")
    }

    /// `(id, created_at)` → a candidate with a `&str` id.
    fn cand(id: &'static str, created_at: i64) -> Candidate<&'static str> {
        Candidate {
            id,
            created_at: ts(created_at),
        }
    }

    /// A legality probe: legal iff the id appears in `legal`.
    fn probe(legal: &'static [&'static str]) -> impl FnMut(&&'static str) -> bool {
        move |id| legal.contains(id)
    }

    /// A legality probe that records, in order, every id it is asked about. The
    /// recorded trace is the direct evidence for the normative anti-flooding
    /// bound of Move Encoding — Sanki §Bounding a slot's candidates: legality is
    /// consulted **lazily**, on the capped window ends only, so a candidate the
    /// cap excluded is never probed at all.
    fn recording_probe<'a>(
        legal: &'static [&'static str],
        seen: &'a mut Vec<&'static str>,
    ) -> impl FnMut(&&'static str) -> bool + 'a {
        move |id| {
            seen.push(*id);
            legal.contains(id)
        }
    }

    /// Every permutation of `items`, i.e. every order in which a caller might
    /// supply the same candidate set. Selection is a pure function of that set,
    /// so all of them must yield the same verdict.
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
    fn single_informed_legal_applied() {
        let cs = [cand("a1", 120)];
        assert_eq!(
            select_candidate(ts(100), &cs, CANDIDATE_CAP, probe(&["a1"])),
            Selection::Applied(&cs[0])
        );
    }

    #[test]
    fn single_illegal_unfilled() {
        let cs = [cand("a1", 120)];
        assert_eq!(
            select_candidate(ts(100), &cs, CANDIDATE_CAP, probe(&[])),
            Selection::Unfilled
        );
    }

    #[test]
    fn anterior_latest_legal_wins() {
        // Two legal premoves (both before the boundary): the later — the re-premove — wins.
        let cs = [cand("a1", 20), cand("a2", 60)];
        assert_eq!(
            select_candidate(ts(100), &cs, CANDIDATE_CAP, probe(&["a1", "a2"])),
            Selection::Applied(&cs[1])
        );
    }

    #[test]
    fn anterior_skips_newest_illegal_to_next_legal() {
        // The newest premove is illegal; the next-newest legal premove wins.
        let cs = [cand("a1", 20), cand("a2", 60)];
        assert_eq!(
            select_candidate(ts(100), &cs, CANDIDATE_CAP, probe(&["a1"])),
            Selection::Applied(&cs[0])
        );
    }

    #[test]
    fn informed_earliest_legal_wins() {
        // Two legal live moves (both at/after the boundary): the earliest wins.
        let cs = [cand("a1", 10), cand("a2", 20)];
        assert_eq!(
            select_candidate(ts(0), &cs, CANDIDATE_CAP, probe(&["a1", "a2"])),
            Selection::Applied(&cs[0])
        );
    }

    #[test]
    fn informed_skips_earliest_illegal_to_next_legal() {
        let cs = [cand("a1", 10), cand("a2", 20)];
        assert_eq!(
            select_candidate(ts(0), &cs, CANDIDATE_CAP, probe(&["a2"])),
            Selection::Applied(&cs[1])
        );
    }

    #[test]
    fn legal_anterior_preferred_over_informed() {
        // A legal premove binds even though a legal live move also exists.
        let cs = [cand("p1", 50), cand("L1", 150)];
        assert_eq!(
            select_candidate(ts(100), &cs, CANDIDATE_CAP, probe(&["p1", "L1"])),
            Selection::Applied(&cs[0])
        );
    }

    #[test]
    fn fallthrough_to_informed_when_no_legal_anterior() {
        // The only premove is illegal → fall through to the earliest legal live move.
        let cs = [cand("p1", 50), cand("L1", 150)];
        assert_eq!(
            select_candidate(ts(100), &cs, CANDIDATE_CAP, probe(&["L1"])),
            Selection::Applied(&cs[1])
        );
    }

    #[test]
    fn all_illegal_both_windows_unfilled() {
        let cs = [cand("p1", 50), cand("L1", 150)];
        assert_eq!(
            select_candidate(ts(100), &cs, CANDIDATE_CAP, probe(&[])),
            Selection::Unfilled
        );
    }

    #[test]
    fn anterior_tie_breaks_by_largest_id_first() {
        // Equal timing in the anterior window: the more recent is the larger id.
        let cs = [cand("b1", 60), cand("b2", 60)];
        assert_eq!(
            select_candidate(ts(100), &cs, CANDIDATE_CAP, probe(&["b1", "b2"])),
            Selection::Applied(&cs[1])
        );
    }

    #[test]
    fn informed_tie_breaks_by_smallest_id_first() {
        // Equal timing in the informed window: the earliest is the smaller id.
        let cs = [cand("b1", 20), cand("b2", 20)];
        assert_eq!(
            select_candidate(ts(0), &cs, CANDIDATE_CAP, probe(&["b1", "b2"])),
            Selection::Applied(&cs[0])
        );
    }

    #[test]
    fn cap_anterior_most_recent_buries_older_legal() {
        // cap K=2 considers the 2 MOST RECENT premoves (both illegal); an older legal
        // premove is beyond the cap → unfilled (flooding one's own recent premoves is
        // self-harm).
        let cs = [cand("a1", 10), cand("a2", 800), cand("a3", 900)];
        assert_eq!(
            select_candidate(ts(1000), &cs, cap(2), probe(&["a1"])),
            Selection::Unfilled
        );
        // With K=3 the older legal premove is reached.
        assert_eq!(
            select_candidate(ts(1000), &cs, cap(3), probe(&["a1"])),
            Selection::Applied(&cs[0])
        );
    }

    #[test]
    fn cap_informed_earliest_buries_later_legal() {
        // cap K=2 considers the 2 EARLIEST live moves (both illegal); a later legal
        // live move is beyond the cap → unfilled.
        let cs = [cand("a1", 10), cand("a2", 20), cand("a3", 30)];
        assert_eq!(
            select_candidate(ts(0), &cs, cap(2), probe(&["a3"])),
            Selection::Unfilled
        );
        assert_eq!(
            select_candidate(ts(0), &cs, cap(3), probe(&["a3"])),
            Selection::Applied(&cs[2])
        );
    }

    #[test]
    fn legality_probes_are_bounded_by_the_cap() {
        // The normative anti-flooding bound (Move Encoding — Sanki §Bounding a
        // slot's candidates): 20 candidates flooded on each side of the
        // boundary, cap K=2 — exactly 2K probes, whatever the flood size, and
        // they fall on the *ends of the two capped windows*: the 2 most recent
        // anterior (ids 19, 18, newest first) then the 2 earliest informed (ids
        // 100, 101, oldest first). Asserting the trace — not merely a `<= 2K`
        // ceiling — is what a regression to eager probing must fail.
        let mut cs: Vec<Candidate<usize>> = Vec::new();
        for i in 0..20 {
            cs.push(Candidate {
                id: i,
                created_at: ts(i64::try_from(i).expect("small")),
            }); // anterior (< 100)
            cs.push(Candidate {
                id: 100 + i,
                created_at: ts(200 + i64::try_from(i).expect("small")),
            }); // informed
        }
        let mut probed: Vec<usize> = Vec::new();
        let selection = select_candidate(ts(100), &cs, cap(2), |id| {
            probed.push(*id);
            false // everything illegal: both windows scanned to their cap
        });
        assert_eq!(selection, Selection::Unfilled);
        assert_eq!(probed, [19, 18, 100, 101]);

        // Short-circuiting: the first legal candidate stops the scan, so the same
        // flood costs a single probe when the newest premove is legal.
        let mut probed: Vec<usize> = Vec::new();
        let selection = select_candidate(ts(100), &cs, cap(2), |id| {
            probed.push(*id);
            *id == 19
        });
        assert_eq!(selection.selected().map(|chosen| chosen.id), Some(19));
        assert_eq!(probed, [19]);
    }

    #[test]
    fn first_slot_boundary_t0_is_informed() {
        // boundary = t₀ = 0: a candidate at/after 0 is informed; an illegal one is
        // skipped (no `illegalmove`), leaving the slot unfilled.
        let legal = [cand("a1", 5)];
        assert_eq!(
            select_candidate(ts(0), &legal, CANDIDATE_CAP, probe(&["a1"])),
            Selection::Applied(&legal[0])
        );
        let illegal = [cand("a1", 5)];
        assert_eq!(
            select_candidate(ts(0), &illegal, CANDIDATE_CAP, probe(&[])),
            Selection::Unfilled
        );
        // A candidate timed exactly AT t₀ is informed as well (the `>=` side of
        // the split, pinned discriminatingly by
        // `boundary_is_exclusive_below_and_inclusive_at`), so the first slot can
        // hold no anterior candidate at all: the earliest legal live move binds.
        let at_t0 = [cand("a0", 0), cand("a1", 5)];
        assert_eq!(
            select_candidate(ts(0), &at_t0, CANDIDATE_CAP, probe(&["a0", "a1"]))
                .selected()
                .map(|chosen| chosen.id),
            Some("a0")
        );
    }

    #[test]
    fn boundary_is_exclusive_below_and_inclusive_at() {
        // Move Encoding — Sanki §Slot candidates and selection: strictly before
        // `T` is anterior, **at or after** `T` is informed. A candidate timed
        // exactly at `T` is therefore informed, which these two observations pin
        // together — neither alone would:
        //   (a) it LOSES to a legal candidate one second earlier. Were it
        //       anterior it would be the *more recent* premove and win the
        //       anterior window; it does not, so it is not anterior;
        //   (b) it is nevertheless reached, as a live move, once the anterior
        //       window yields nothing.
        let cs = [cand("before", 99), cand("at_t", 100)];
        assert_eq!(
            select_candidate(ts(100), &cs, CANDIDATE_CAP, probe(&["before", "at_t"]))
                .selected()
                .map(|chosen| chosen.id),
            Some("before")
        );
        assert_eq!(
            select_candidate(ts(100), &cs, CANDIDATE_CAP, probe(&["at_t"]))
                .selected()
                .map(|chosen| chosen.id),
            Some("at_t")
        );
    }

    #[test]
    fn anterior_tie_skips_the_larger_id_when_illegal() {
        // The anterior mirror of the corpus vector
        // `selection.informed-tie-createdat-first-legal-by-id`: at equal canonical
        // timing the anterior window scans by DESCENDING id — the asymmetry with
        // the informed window is deliberate (a same-second re-premove supersedes),
        // so the larger id is probed first and, when illegal, the smaller binds.
        let cs = [cand("b1", 60), cand("b2", 60)];
        let mut probed = Vec::new();
        let chosen = select_candidate(
            ts(100),
            &cs,
            CANDIDATE_CAP,
            recording_probe(&["b1"], &mut probed),
        )
        .selected()
        .map(|chosen| chosen.id);
        assert_eq!(chosen, Some("b1"));
        assert_eq!(probed, ["b2", "b1"]);
    }

    #[test]
    fn anterior_cap_admits_exactly_k_and_no_more() {
        // The anterior window keeps the K MOST RECENT candidates by
        // (created_at, id). With K = 4 and four premoves, the oldest is still
        // inside the window and costs the full four probes.
        let four = [
            cand("a1", 10),
            cand("a2", 20),
            cand("a3", 30),
            cand("a4", 40),
        ];
        let mut probed = Vec::new();
        assert_eq!(
            select_candidate(
                ts(100),
                &four,
                cap(4),
                recording_probe(&["a1"], &mut probed)
            )
            .selected()
            .map(|chosen| chosen.id),
            Some("a1")
        );
        assert_eq!(probed, ["a4", "a3", "a2", "a1"]);

        // A fifth, more recent premove pushes the oldest out of the window. `a1`
        // is still LEGAL and the slot is nonetheless unfilled — and the probe
        // trace proves the cap, not legality, is what excluded it: `a1` is never
        // asked about.
        let five = [
            cand("a1", 10),
            cand("a2", 20),
            cand("a3", 30),
            cand("a4", 40),
            cand("a5", 50),
        ];
        let mut probed = Vec::new();
        assert_eq!(
            select_candidate(
                ts(100),
                &five,
                cap(4),
                recording_probe(&["a1"], &mut probed)
            ),
            Selection::Unfilled
        );
        assert_eq!(probed, ["a5", "a4", "a3", "a2"]);
    }

    #[test]
    fn informed_cap_admits_exactly_k_and_no_more() {
        // The informed window keeps the K EARLIEST candidates — the opposite end.
        // With K = 4 and four live moves, the latest is still inside the window.
        let four = [
            cand("a1", 10),
            cand("a2", 20),
            cand("a3", 30),
            cand("a4", 40),
        ];
        let mut probed = Vec::new();
        assert_eq!(
            select_candidate(ts(0), &four, cap(4), recording_probe(&["a4"], &mut probed))
                .selected()
                .map(|chosen| chosen.id),
            Some("a4")
        );
        assert_eq!(probed, ["a1", "a2", "a3", "a4"]);

        // A fifth, later live move is beyond the cap: legal, unreached, unfilled.
        let five = [
            cand("a1", 10),
            cand("a2", 20),
            cand("a3", 30),
            cand("a4", 40),
            cand("a5", 50),
        ];
        let mut probed = Vec::new();
        assert_eq!(
            select_candidate(ts(0), &five, cap(4), recording_probe(&["a5"], &mut probed)),
            Selection::Unfilled
        );
        assert_eq!(probed, ["a1", "a2", "a3", "a4"]);
    }

    #[test]
    fn empty_input_and_the_smallest_cap_are_handled_without_waste() {
        // No candidate at all: unfilled, and legality is never consulted.
        let none: [Candidate<&'static str>; 0] = [];
        let mut probed = Vec::new();
        assert_eq!(
            select_candidate(
                ts(100),
                &none,
                CANDIDATE_CAP,
                recording_probe(&[], &mut probed)
            ),
            Selection::Unfilled
        );
        assert!(probed.is_empty());

        // K = 1, the smallest cap the type admits (a cap of 0 would fill no
        // slot ever, and is unrepresentable): each window admits exactly its
        // first candidate — the newest premove, the oldest live move — so the
        // ≤ 2K bound holds at its lower extreme, and a legal candidate behind
        // the first one is never reached.
        let cs = [
            cand("p1", 40),
            cand("p2", 50),
            cand("L1", 150),
            cand("L2", 160),
        ];
        let mut probed = Vec::new();
        assert_eq!(
            select_candidate(
                ts(100),
                &cs,
                cap(1),
                recording_probe(&["p1", "L2"], &mut probed)
            ),
            Selection::Unfilled
        );
        assert_eq!(probed, ["p2", "L1"]);
        let mut probed = Vec::new();
        assert_eq!(
            select_candidate(ts(100), &cs, cap(1), recording_probe(&["L1"], &mut probed))
                .selected()
                .map(|chosen| chosen.id),
            Some("L1")
        );
        assert_eq!(probed, ["p2", "L1"]);
    }

    #[test]
    fn the_window_split_has_one_definition() {
        // `Candidate::is_anterior` is the split `select_candidate` filters on
        // and the split the upstream collapse keys on: strictly before the
        // boundary is anterior, at or after it is informed. Pinned at the
        // boundary itself, where the two would diverge if either read `<=`.
        let boundary = ts(100);
        assert!(cand("before", 99).is_anterior(boundary));
        assert!(!cand("at", 100).is_anterior(boundary));
        assert!(!cand("after", 101).is_anterior(boundary));
        // …and the selection agrees with it: the candidate at the boundary is
        // in the informed window, scanned oldest-first behind nothing.
        let cs = [cand("at", 100), cand("after", 101)];
        assert_eq!(
            select_candidate(boundary, &cs, CANDIDATE_CAP, probe(&["at", "after"]))
                .selected()
                .map(|chosen| chosen.id),
            Some("at")
        );
    }

    #[test]
    fn extreme_timestamps_partition_without_saturating() {
        // `Timestamp` is a signed Unix second, so a candidate may legitimately
        // carry a negative or extremal instant. The split is a plain comparison —
        // no arithmetic, hence nothing to saturate or wrap at the extremes.
        // At `i64::MIN` the boundary admits nothing below it: a candidate timed
        // exactly there is informed (`>=`), not anterior.
        let floor = [cand("m", i64::MIN)];
        assert_eq!(
            select_candidate(ts(i64::MIN), &floor, CANDIDATE_CAP, probe(&["m"]))
                .selected()
                .map(|chosen| chosen.id),
            Some("m")
        );
        // At `i64::MAX` everything below is anterior, and the latest legal wins.
        let ceiling = [cand("lo", i64::MIN), cand("hi", i64::MAX - 1)];
        assert_eq!(
            select_candidate(ts(i64::MAX), &ceiling, CANDIDATE_CAP, probe(&["lo", "hi"]))
                .selected()
                .map(|chosen| chosen.id),
            Some("hi")
        );
        // Negative instants order normally: the latest legal premove is the one
        // closest to the boundary.
        let negative = [cand("n1", -100), cand("n2", -50)];
        assert_eq!(
            select_candidate(ts(0), &negative, CANDIDATE_CAP, probe(&["n1", "n2"]))
                .selected()
                .map(|chosen| chosen.id),
            Some("n2")
        );
    }

    #[test]
    fn selection_is_independent_of_the_input_order() {
        // Determinism (README §Design guarantees): the verdict is a pure function
        // of the candidate SET. A partial `sort_by` — one whose comparator ignored
        // the id tiebreak — would surface only here, since the stable sort would
        // then leak the caller's order into the scan. Every one of the 120 input
        // orders of this set must give not only the same selection but the same
        // probe trace: two anterior candidates tied at 40, one at 90, and two
        // informed candidates tied at 100, under a cap of 2.
        let base = [
            cand("p1", 40),
            cand("p2", 40),
            cand("p3", 90),
            cand("L1", 100),
            cand("L2", 100),
        ];
        for order in permutations(&base) {
            let mut probed = Vec::new();
            let chosen = select_candidate(
                ts(100),
                &order,
                cap(2),
                recording_probe(&["p1", "L1"], &mut probed),
            )
            .selected()
            .map(|chosen| chosen.id);
            // The 2 most recent premoves (p3, then p2 — the larger id of the tie)
            // are both illegal, so the earliest legal live move binds.
            assert_eq!(chosen, Some("L1"), "input order {order:?}");
            assert_eq!(probed, ["p3", "p2", "L1"], "input order {order:?}");
        }
        // The same set under the production cap reaches the older legal premove,
        // and that too is order-independent.
        for order in permutations(&base) {
            assert_eq!(
                select_candidate(ts(100), &order, CANDIDATE_CAP, probe(&["p1", "L1"]))
                    .selected()
                    .map(|chosen| chosen.id),
                Some("p1"),
                "input order {order:?}"
            );
        }
    }
}
