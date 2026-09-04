//! `SessionParams` — the session-constant configuration the kernel evaluates.
//!
//! A session's invariant parameters are spread across several founding events;
//! the application assembles them, after cross-event validation, into one
//! aggregate:
//!
//! - from the **Game Session** (kind `3422`): the two players and their seats
//!   ([`Seats`]), the per-player variants (carried by the initial position's
//!   styles), the initial position, and the session's event id;
//! - from the **founding** (kind `3420`, or `3418`/`3419`): the time control
//!   and the OPTIONAL designated timestamper (a timestamper puts the session in
//!   attested mode; `None` means self-timed — the founding designated timing
//!   relays instead, and each event's own `created_at`, once accepted by one of
//!   them, is its canonical timing. Exactly one of the two designations is
//!   present on a conforming founding; a self-timed session's events reach this
//!   crate only once their acceptance is established — see
//!   [`crate::timing`]);
//! - from the **rule-system document** the founding names (its `rules` term):
//!   the slot selection cap `K` (`session.candidate_cap`), the one parameter of
//!   the session kernel the document carries. The position-kernel parameters
//!   (the thresholds, the tables) live in `sashite-sanki-engine`, which
//!   implements the reference document's values; checking that a session's
//!   manifest is one this pair of crates implements is the caller's concern
//!   (`sashite_sanki_engine::rules::verify`);
//! - from t₀, the canonical session start — the Session Start Attestation (kind
//!   `3410`) in attested mode, or the Game Session's own `created_at` when
//!   self-timed (or its `start_at`, when later — kind `3422` §Canonical session
//!   start).
//!
//! This module is a pure aggregate plus the lookups the kernel layers need:
//! mapping a signer to its side, naming the player on a side, recognizing the
//! timestamper, mapping per-player scores to a seat-axis outcome, and mapping a
//! **play-order position** (1-based half-move index) to its slot — the side on
//! move and that side's `step` (the signer's own move ordinal, kind `3423` §Step
//! semantics and play order) — under Sanki's strict alternation. The per-player
//! variants are not duplicated here — they are read from the [`Position`] (its
//! style field), via [`SessionParams::initial_state`] and the kernel.

use crate::event::{EventId, PublicKey};
use crate::selection::CANDIDATE_CAP;
use core::num::NonZeroUsize;
use sashite_sanki_engine::domain::side::Side;
use sashite_sanki_engine::domain::status::Outcome3;
use sashite_sanki_engine::domain::time::Timestamp;
use sashite_sanki_engine::domain::time_control::TimeControl;
use sashite_sanki_engine::kernel::state::SessionState;
use sashite_sanki_engine::position::Position;

/// The two players by seat: who moves `first`, who moves `second` (the Game
/// Session's `seat` tags). The two keys are distinct by construction — a
/// session cannot be played against oneself, and a swap flips every decisive
/// verdict, which is why the constructor names them rather than taking two
/// positional pubkeys.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Seats {
    first: PublicKey,
    second: PublicKey,
}

impl Seats {
    /// The seats, or `None` when the two keys are equal.
    #[inline]
    #[must_use]
    pub fn new(first: PublicKey, second: PublicKey) -> Option<Self> {
        (first != second).then_some(Self { first, second })
    }

    /// The player who moves first.
    #[inline]
    #[must_use]
    pub const fn first(&self) -> PublicKey {
        self.first
    }

    /// The player who moves second.
    #[inline]
    #[must_use]
    pub const fn second(&self) -> PublicKey {
        self.second
    }

    /// The player assigned to `side`.
    #[inline]
    #[must_use]
    pub const fn player(&self, side: Side) -> PublicKey {
        match side {
            Side::First => self.first,
            Side::Second => self.second,
        }
    }

    /// The side a pubkey plays, or `None` if it is not one of the two players.
    #[inline]
    #[must_use]
    pub fn side_of(&self, pubkey: PublicKey) -> Option<Side> {
        if pubkey == self.first {
            Some(Side::First)
        } else if pubkey == self.second {
            Some(Side::Second)
        } else {
            None
        }
    }
}

/// The invariant parameters of a session.
#[derive(Debug, Clone)]
pub struct SessionParams {
    session: EventId,
    /// The designated timestamper (attested mode), or `None` when self-timed.
    timestamper: Option<PublicKey>,
    seats: Seats,
    time_control: TimeControl,
    initial_position: Position,
    start: Timestamp,
    candidate_cap: NonZeroUsize,
}

impl SessionParams {
    /// Assembles the session parameters: the Game Session's id, the optional
    /// timestamper, the seats, the time control, the initial position and
    /// `start` — t₀, the canonical session start (kind `3422` §Canonical
    /// session start). The slot selection cap defaults to the reference
    /// document's `candidate_cap` ([`CANDIDATE_CAP`]); a session founded under
    /// a document carrying another value sets it with
    /// [`SessionParams::with_candidate_cap`].
    ///
    /// `None` iff the initial position does not have `first` to move. The
    /// position a Game Session carries is the one the rule-system document
    /// prescribes (kind `3422` §Content), and every Sanki initial position has
    /// `first` to move; the play-order model ([`SessionParams::side_at`])
    /// rests on it, so a position that breaks it is refused here rather than
    /// evaluated into a chain no Ply can ever fill.
    #[inline]
    #[must_use]
    pub fn new(
        session: EventId,
        timestamper: Option<PublicKey>,
        seats: Seats,
        time_control: TimeControl,
        initial_position: Position,
        start: Timestamp,
    ) -> Option<Self> {
        (initial_position.active_side() == Side::First).then_some(Self {
            session,
            timestamper,
            seats,
            time_control,
            initial_position,
            start,
            candidate_cap: CANDIDATE_CAP,
        })
    }

    /// The same parameters under the slot selection cap `K` the session's
    /// rule-system document carries (`session.candidate_cap`, *Move Encoding —
    /// Sanki* §Bounding a slot's candidates). At least `1` by type: a cap of
    /// `0` would fill no slot ever — a session no Ply could advance, refused
    /// at construction like a position without `first` to move rather than
    /// evaluated into abandonment verdicts.
    #[inline]
    #[must_use]
    pub const fn with_candidate_cap(mut self, candidate_cap: NonZeroUsize) -> Self {
        self.candidate_cap = candidate_cap;
        self
    }

    /// The Game Session event id this session is scoped to.
    #[inline]
    #[must_use]
    pub const fn session(&self) -> EventId {
        self.session
    }

    /// The designated timestamper (whose attestations are authoritative), or
    /// `None` when the session is self-timed (no timestamper was designated).
    #[inline]
    #[must_use]
    pub const fn timestamper(&self) -> Option<PublicKey> {
        self.timestamper
    }

    /// The two players by seat.
    #[inline]
    #[must_use]
    pub const fn seats(&self) -> &Seats {
        &self.seats
    }

    /// The session's time control.
    #[inline]
    #[must_use]
    pub const fn time_control(&self) -> &TimeControl {
        &self.time_control
    }

    /// The initial position.
    #[inline]
    #[must_use]
    pub const fn initial_position(&self) -> &Position {
        &self.initial_position
    }

    /// t₀, the canonical session start.
    #[inline]
    #[must_use]
    pub const fn start(&self) -> Timestamp {
        self.start
    }

    /// The slot selection cap `K` (at least `1`).
    #[inline]
    #[must_use]
    pub const fn candidate_cap(&self) -> NonZeroUsize {
        self.candidate_cap
    }

    /// The player assigned to `side`.
    #[inline]
    #[must_use]
    pub const fn player(&self, side: Side) -> PublicKey {
        self.seats.player(side)
    }

    /// The side a pubkey plays, or `None` if it is not one of the two players.
    #[inline]
    #[must_use]
    pub fn side_of(&self, pubkey: PublicKey) -> Option<Side> {
        self.seats.side_of(pubkey)
    }

    /// Whether `pubkey` is one of the two players.
    #[inline]
    #[must_use]
    pub fn is_player(&self, pubkey: PublicKey) -> bool {
        self.seats.side_of(pubkey).is_some()
    }

    /// Whether `pubkey` is the designated timestamper. Always `false` for a
    /// self-timed session (which designates none), so no event is ever treated
    /// as an authoritative attestation there.
    #[inline]
    #[must_use]
    pub fn is_timestamper(&self, pubkey: PublicKey) -> bool {
        self.timestamper == Some(pubkey)
    }

    /// The seat-axis outcome of a Conclusion's two `result` tags — one
    /// `(player, score)` per player, the scores summing to `100` — or `None`
    /// when the tags do not describe an outcome of the `sanki` rule system: a
    /// pubkey that is not a player, both tags naming the same player, or a split
    /// other than `100/0`, `50/50`, `0/100` (kind `3425` admits other integer
    /// splits on the wire; no verdict of this kernel ever yields one, so a
    /// Conclusion carrying one cannot be conforming and needs no replay to be
    /// refused).
    #[must_use]
    pub fn outcome_from_scores(&self, scores: [(PublicKey, u8); 2]) -> Option<Outcome3> {
        let [(a, score_a), (b, score_b)] = scores;
        let side_a = self.side_of(a)?;
        let side_b = self.side_of(b)?;
        if side_a == side_b {
            return None;
        }
        let (first, second) = match side_a {
            Side::First => (score_a, score_b),
            Side::Second => (score_b, score_a),
        };
        match (first, second) {
            (100, 0) => Some(Outcome3::FirstWins),
            (50, 50) => Some(Outcome3::Draw),
            (0, 100) => Some(Outcome3::SecondWins),
            _ => None,
        }
    }

    /// The side on move at the 1-based position `half_move` of the play order,
    /// under Sanki's strict alternation: within each step value, side `first`
    /// moves before side `second` — so odd positions belong to `first`, even
    /// ones to `second`.
    ///
    /// A pure function of the position — it reads neither the board nor the
    /// session's history — and total over `u32`: the parity is exact at both
    /// ends of the range, so nothing saturates. It presupposes what the Game
    /// Session (kind `3422`) supplies, an **initial position with `first` to
    /// move**; the engine's own turn tracking then stays in lockstep with it
    /// half-move for half-move (pinned by `verdict`'s
    /// `the_engine_turn_tracks_the_kernel_play_order`). `half_move` is 1-based:
    /// `0` denotes no slot and its answer is meaningless.
    #[inline]
    #[must_use]
    pub const fn side_at(&self, half_move: u32) -> Side {
        if half_move & 1 == 1 {
            Side::First
        } else {
            Side::Second
        }
    }

    /// The mover's `step` — their own move ordinal (kind `3423` §Step semantics
    /// and play order) — at the 1-based position `half_move` of the play order:
    /// position 1 → step 1 of `first`, position 2 → step 1 of `second`,
    /// position 3 → step 2 of `first`, …
    #[inline]
    #[must_use]
    pub const fn step_at(&self, half_move: u32) -> u32 {
        half_move.div_ceil(2)
    }

    /// The player on move at the 1-based position `half_move` of the play order.
    #[inline]
    #[must_use]
    pub const fn player_at(&self, half_move: u32) -> PublicKey {
        self.player(self.side_at(half_move))
    }

    /// Builds the initial kernel state: clocks started from the time control, the
    /// FEEN history seeded with the initial position, and t₀ as the timing
    /// anchor.
    #[inline]
    #[must_use]
    pub fn initial_state(&self) -> SessionState {
        SessionState::start(
            self.initial_position.clone(),
            self.time_control.clone(),
            self.start,
        )
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

    use super::{Seats, SessionParams};
    use crate::event::{EventId, PublicKey};
    use sashite_sanki_engine::domain::side::Side;
    use sashite_sanki_engine::domain::status::Outcome3;
    use sashite_sanki_engine::domain::time::{Duration, Timestamp};
    use sashite_sanki_engine::domain::time_control::{Period, TimeControl};
    use sashite_sanki_engine::position::Position;

    fn pk(byte: u8) -> PublicKey {
        PublicKey::from_bytes([byte; 32])
    }

    fn id(byte: u8) -> EventId {
        EventId::from_bytes([byte; 32])
    }

    fn time_control() -> TimeControl {
        let period = Period::new(Duration::from_secs(600), None, None).expect("valid period");
        TimeControl::new(period, Vec::new())
    }

    const START_FEEN: &str = "4k^3/8/8/8/8/8/8/4K^3 / W/w";

    fn params() -> SessionParams {
        SessionParams::new(
            id(1),
            Some(pk(3)),
            Seats::new(pk(10), pk(20)).expect("distinct players"),
            time_control(),
            Position::parse(START_FEEN).expect("valid Sanki FEEN"),
            Timestamp::from_unix(1000),
        )
        .expect("first to move")
    }

    #[test]
    fn maps_pubkey_to_side() {
        let p = params();
        assert_eq!(p.side_of(pk(10)), Some(Side::First));
        assert_eq!(p.side_of(pk(20)), Some(Side::Second));
        assert_eq!(p.side_of(pk(99)), None); // neither one
    }

    #[test]
    fn maps_side_to_player() {
        let p = params();
        assert_eq!(p.player(Side::First), pk(10));
        assert_eq!(p.player(Side::Second), pk(20));
    }

    #[test]
    fn recognizes_player_and_timestamper() {
        let p = params();
        assert!(p.is_player(pk(10)));
        assert!(p.is_player(pk(20)));
        assert!(!p.is_player(pk(3))); // the timestamper is not a player
        assert!(p.is_timestamper(pk(3)));
        assert!(!p.is_timestamper(pk(10)));
    }

    #[test]
    fn self_timed_session_designates_no_timestamper() {
        // A self-timed session carries no timestamper: the accessor is None and no
        // pubkey is ever recognised as the (absent) timestamper.
        let p = SessionParams::new(
            id(1),
            None,
            Seats::new(pk(10), pk(20)).expect("distinct players"),
            time_control(),
            Position::parse(START_FEEN).expect("valid Sanki FEEN"),
            Timestamp::from_unix(1000),
        )
        .expect("first to move");
        assert_eq!(p.timestamper(), None);
        assert!(!p.is_timestamper(pk(3)));
        assert!(!p.is_timestamper(pk(10)));
        // Players and play order still resolve normally.
        assert_eq!(p.player(Side::First), pk(10));
        assert_eq!(p.player_at(2), pk(20));
    }

    #[test]
    fn play_order_positions_map_to_slots() {
        let p = params();
        // Strict alternation: (1,first),(1,second),(2,first),(2,second), …
        assert_eq!(p.side_at(1), Side::First);
        assert_eq!(p.side_at(2), Side::Second);
        assert_eq!(p.side_at(3), Side::First);
        assert_eq!(p.side_at(4), Side::Second);
        assert_eq!(p.step_at(1), 1);
        assert_eq!(p.step_at(2), 1);
        assert_eq!(p.step_at(3), 2);
        assert_eq!(p.step_at(4), 2);
        assert_eq!(p.step_at(5), 3);
        assert_eq!(p.player_at(1), pk(10));
        assert_eq!(p.player_at(2), pk(20));
        assert_eq!(p.player_at(3), pk(10));
    }

    #[test]
    fn play_order_mapping_is_exact_at_the_u32_extremes() {
        // The mapping is a bijection between play-order positions and slots:
        // position `2n − 1` is (first, step n), position `2n` is (second, step n).
        // `arithmetic_side_effects` being denied, `step_at` divides rather than
        // multiplies — the pairing must therefore still hold where a doubling
        // would overflow, at the top of the range.
        let p = params();
        for n in [1_u32, 2, 3, 500, 2_147_483_646, 2_147_483_647] {
            let odd = n
                .checked_mul(2)
                .and_then(|d| d.checked_sub(1))
                .expect("fits");
            let even = n.checked_mul(2).expect("fits");
            assert_eq!(p.side_at(odd), Side::First, "position {odd}");
            assert_eq!(p.step_at(odd), n, "position {odd}");
            assert_eq!(p.side_at(even), Side::Second, "position {even}");
            assert_eq!(p.step_at(even), n, "position {even}");
        }

        // The last two positions a `u32` can name: `u32::MAX` is odd, so it is
        // `first`'s step 2^31 — the doubling that would name it overflows, the
        // halving does not, and nothing is silently saturated.
        assert_eq!(p.side_at(u32::MAX), Side::First);
        assert_eq!(p.step_at(u32::MAX), 2_147_483_648);
        assert_eq!(p.player_at(u32::MAX), pk(10));
        assert_eq!(p.side_at(u32::MAX - 1), Side::Second);
        assert_eq!(p.step_at(u32::MAX - 1), 2_147_483_647);
        assert_eq!(p.player_at(u32::MAX - 1), pk(20));

        // Position 0 is outside the 1-based domain: it is answered without
        // panicking or wrapping, and names a `step` no conforming Ply can carry
        // (kind `3423` §Step semantics: `step >= 1`), so it can match nothing.
        assert_eq!(p.step_at(0), 0);
    }

    #[test]
    fn side_of_maps_only_the_two_players() {
        // The `None` here is the gate that keeps a non-player's Conclusion from
        // resolving as a resignation (kind `3425` §Semantic constraints, item
        // 3), so it must not be approximate: only the two exact 32-byte player
        // keys map.
        let p = params();
        assert_eq!(p.side_of(pk(10)), Some(Side::First));
        assert_eq!(p.side_of(pk(20)), Some(Side::Second));
        for stranger in [
            pk(2), // a bystander
            pk(3), // the timestamper
            pk(0), // the all-zero key
            pk(255),
        ] {
            assert_eq!(p.side_of(stranger), None, "{stranger} is not a player");
            assert!(!p.is_player(stranger));
        }
        // A key one byte away from a player's is a different key.
        for player in [10_u8, 20] {
            let mut near = [player; 32];
            near[31] = player.wrapping_add(1);
            let near = PublicKey::from_bytes(near);
            assert_eq!(p.side_of(near), None);
            let mut near = [player; 32];
            near[0] = player.wrapping_sub(1);
            let near = PublicKey::from_bytes(near);
            assert_eq!(p.side_of(near), None);
        }
    }

    #[test]
    fn initial_kernel_state_starts_in_the_first_period() {
        // The kernel clocks start symmetric on the FIRST period of a multi-period
        // control (kind `3420` §time_control); the later periods are reached by
        // ticking, never at the start.
        let main = Period::new(Duration::from_secs(900), None, None).expect("valid period");
        let overtime = Period::new(
            Duration::from_secs(0),
            Some(Duration::from_secs(30)),
            Some(1),
        )
        .expect("valid period");
        let p = SessionParams::new(
            id(1),
            Some(pk(3)),
            Seats::new(pk(10), pk(20)).expect("distinct players"),
            TimeControl::new(main, vec![overtime]),
            Position::parse(START_FEEN).expect("valid Sanki FEEN"),
            Timestamp::from_unix(1000),
        )
        .expect("first to move");
        assert_eq!(p.time_control().period_count(), 2);
        let state = p.initial_state();
        for side in [Side::First, Side::Second] {
            let clock = state.clocks().get(side);
            assert_eq!(clock.remaining(), Duration::from_secs(900), "{side:?}");
            assert_eq!(clock.period_index(), 0, "{side:?}");
            assert_eq!(clock.plies_in_period(), 0, "{side:?}");
        }
        assert_eq!(state.last_attestation(), Timestamp::from_unix(1000));
        // The founding position is `first` to move — the premise `side_at` rests on.
        assert_eq!(state.position().active_side(), p.side_at(state.half_move()));
    }

    #[test]
    fn seats_reject_a_player_against_themselves() {
        assert!(Seats::new(pk(10), pk(10)).is_none());
        let seats = Seats::new(pk(10), pk(20)).expect("distinct");
        assert_eq!(seats.first(), pk(10));
        assert_eq!(seats.second(), pk(20));
        assert_eq!(seats.player(Side::Second), pk(20));
        assert_eq!(seats.side_of(pk(20)), Some(Side::Second));
        assert_eq!(seats.side_of(pk(30)), None);
    }

    #[test]
    fn scores_map_to_the_seat_axis_outcome_whatever_the_tag_order() {
        let p = params();
        // first = pk(10), second = pk(20).
        assert_eq!(
            p.outcome_from_scores([(pk(10), 100), (pk(20), 0)]),
            Some(Outcome3::FirstWins)
        );
        assert_eq!(
            p.outcome_from_scores([(pk(20), 0), (pk(10), 100)]),
            Some(Outcome3::FirstWins)
        );
        assert_eq!(
            p.outcome_from_scores([(pk(20), 100), (pk(10), 0)]),
            Some(Outcome3::SecondWins)
        );
        assert_eq!(
            p.outcome_from_scores([(pk(10), 50), (pk(20), 50)]),
            Some(Outcome3::Draw)
        );
        // Not an outcome of this kernel: a stranger, a doubled player, an
        // uncommon split, a split not summing to 100.
        assert_eq!(p.outcome_from_scores([(pk(99), 100), (pk(20), 0)]), None);
        assert_eq!(p.outcome_from_scores([(pk(10), 100), (pk(10), 0)]), None);
        assert_eq!(p.outcome_from_scores([(pk(10), 70), (pk(20), 30)]), None);
        assert_eq!(p.outcome_from_scores([(pk(10), 100), (pk(20), 100)]), None);
    }

    #[test]
    fn initial_kernel_state() {
        let p = params();
        let state = p.initial_state();
        assert_eq!(state.half_move(), 1);
        assert_eq!(state.last_attestation(), Timestamp::from_unix(1000));
        assert_eq!(state.position().to_feen(), START_FEEN);
        assert!(!state.move_limit_reached());
    }

    #[test]
    fn accessors() {
        let p = params();
        assert_eq!(p.session(), id(1));
        assert_eq!(p.timestamper(), Some(pk(3)));
        assert_eq!(p.start(), Timestamp::from_unix(1000));
        assert_eq!(p.candidate_cap().get(), 8);
        let three = core::num::NonZeroUsize::new(3).expect("non-zero");
        assert_eq!(p.clone().with_candidate_cap(three).candidate_cap(), three);
        assert_eq!(p.initial_position().to_feen(), START_FEEN);
    }
}
