//! Typed Nostr event models the session kernel reasons about.
//!
//! The kernel consumes events that the application has already received,
//! signature-verified (NIP-01), and parsed from their raw tag form. This module
//! gives those events a typed shape reduced to what the kernel needs:
//!
//! - [`Ply`] (kind `3423`) — a played half-move: its `step`, signer, optional
//!   `draw` flag, and opaque `content` (decoded later by the kernel);
//! - [`Attestation`] (kind `3410`) — the designated timestamper's receipt
//!   witness, carrying the **canonical timing** of the attested event;
//! - [`Conclusion`] (kind `3425`) — a player's verdict on the session, binding
//!   by correctness: its canonical timing is the **cutoff** at which the
//!   session is evaluated, its signer the **invoker**, and the verdict it
//!   *claims* is checked against the one the rule system yields
//!   ([`crate::verdict::check`]).
//!
//! Timing depends on the session's mode. A suite event's own `created_at` is the
//! signer's self-claim. When the session designates a timestamper (attested
//! mode), that self-claim is superseded by the [`Attestation`]'s `created_at`
//! and never drives timing (kind `3423` §Time accounting; kind `3425`
//! §Attestation by the designated timestamper). When the session is self-timed
//! — no timestamper, timing relays instead — there is no attestation, and the
//! `created_at` **as accepted by a designated timing relay** IS the canonical
//! timing (Canonical Timing §Timing modes and mode selection). [`Ply`] and
//! [`Conclusion`] therefore carry `created_at`; it is consulted only in the
//! self-timed branch of [`crate::timing::canonical_timing`].
//!
//! **Precondition (self-timed mode).** This crate cannot observe a relay's
//! acceptance. The caller MUST offer it only Plies and Conclusions whose
//! acceptance by one of the session's designated timing relays is established
//! (fetched from such a relay, or seen accepted by one); an event without that
//! is *pending* (Canonical Timing §The pending state) and is not a candidate
//! for anything. Passing it would time it by a clock no party agreed to.
//!
//! **Precondition (identity).** Event ids are unique: a Nostr id is the hash
//! of the event, so two distinct events never share one. The kernel relies on
//! it — a slot's candidates are told apart by id, and the id is the race
//! tiebreak — and does not re-verify it; a caller that has not checked the
//! ids it parsed (NIP-01) must not expect a defined answer for two events
//! offered under one id.
//!
//! Identity is carried by [`EventId`] and [`PublicKey`], 32-byte newtypes over
//! the canonical Nostr encoding. [`EventId`] is ordered: the byte order is the
//! "smallest event ID" tiebreak of selection and meta-resolution.

use crate::verdict::Verdict;
use sashite_sanki_engine::domain::time::Timestamp;

/// A 32-byte Nostr event identifier.
///
/// Ordered by raw bytes, which is the tiebreak of selection and meta-resolution
/// ("smallest event ID").
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct EventId([u8; 32]);

/// A 32-byte Nostr public key (x-only), the signer identity.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct PublicKey([u8; 32]);

impl EventId {
    /// Wraps the 32 raw bytes of an event id.
    #[inline]
    #[must_use]
    pub const fn from_bytes(bytes: [u8; 32]) -> Self {
        Self(bytes)
    }

    /// The raw bytes.
    #[inline]
    #[must_use]
    pub const fn as_bytes(&self) -> &[u8; 32] {
        &self.0
    }

    /// Parses a 64-character lowercase/uppercase hex string, or `None` if it is
    /// not exactly 64 hex digits.
    #[inline]
    #[must_use]
    pub fn parse(hex: &str) -> Option<Self> {
        parse_hex32(hex).map(Self)
    }
}

impl PublicKey {
    /// Wraps the 32 raw bytes of a public key.
    #[inline]
    #[must_use]
    pub const fn from_bytes(bytes: [u8; 32]) -> Self {
        Self(bytes)
    }

    /// The raw bytes.
    #[inline]
    #[must_use]
    pub const fn as_bytes(&self) -> &[u8; 32] {
        &self.0
    }

    /// Parses a 64-character hex string, or `None` if it is not exactly 64 hex
    /// digits.
    #[inline]
    #[must_use]
    pub fn parse(hex: &str) -> Option<Self> {
        parse_hex32(hex).map(Self)
    }
}

impl core::fmt::Display for EventId {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        write_hex(f, &self.0)
    }
}

impl core::fmt::Display for PublicKey {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        write_hex(f, &self.0)
    }
}

/// Decodes exactly 32 bytes from a 64-digit hex string.
fn parse_hex32(hex: &str) -> Option<[u8; 32]> {
    if hex.len() != 64 {
        return None;
    }
    let mut bytes = [0_u8; 32];
    let mut digits = hex.chars();
    for byte in bytes.iter_mut() {
        let high = digits.next()?.to_digit(16)?;
        let low = digits.next()?.to_digit(16)?;
        *byte = u8::try_from(high.checked_mul(16)?.checked_add(low)?).ok()?;
    }
    Some(bytes)
}

/// Writes 32 bytes as lowercase hex.
fn write_hex(f: &mut core::fmt::Formatter<'_>, bytes: &[u8; 32]) -> core::fmt::Result {
    for byte in bytes {
        write!(f, "{byte:02x}")?;
    }
    Ok(())
}

/// A played half-move (kind `3423`).
///
/// `content` is the opaque move encoding; its syntax and legality are the
/// kernel's concern, not this model's. `created_at` is the event's own
/// timestamp — the canonical timing in self-timed mode (on the module's
/// acceptance precondition), ignored in attested mode.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Ply {
    /// The Ply event's id (race-resolution tiebreak).
    pub id: EventId,
    /// The moving player's pubkey.
    pub signer: PublicKey,
    /// The referenced Game Session (kind `3422`).
    pub session: EventId,
    /// The signer's own move ordinal (`>= 1`), per kind `3423` §Step semantics
    /// and play order: each player numbers their own moves independently, and
    /// the slot of a Ply is `(session, signer, step)`.
    pub step: u32,
    /// Whether the optional `draw` flag is present.
    pub draw: bool,
    /// The played half-move, in the rule system's encoding.
    pub content: String,
    /// The event's own `created_at`. The canonical timing when the session is
    /// self-timed — on the module's precondition that its acceptance by a
    /// designated timing relay is established; ignored when a timestamper
    /// attests it.
    pub created_at: Timestamp,
}

impl Ply {
    /// Assembles a typed Ply from its kernel-relevant fields.
    #[inline]
    #[must_use]
    pub const fn new(
        id: EventId,
        signer: PublicKey,
        session: EventId,
        step: u32,
        draw: bool,
        content: String,
        created_at: Timestamp,
    ) -> Self {
        Self {
            id,
            signer,
            session,
            step,
            draw,
            content,
            created_at,
        }
    }
}

/// An Event Timestamp Attestation (kind `3410`).
///
/// Authoritative for timing only when `signer` is the session's designated
/// timestamper; the kernel applies that restriction. `created_at` is the
/// canonical timing the attestation confers on the attested event.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Attestation {
    /// The attestation event's id (meta-resolution tiebreak).
    pub id: EventId,
    /// The attesting signer (authoritative iff the designated timestamper).
    pub signer: PublicKey,
    /// The attested event (a Ply, a Conclusion, …).
    pub attests: EventId,
    /// The canonical timing conferred on the attested event.
    pub created_at: Timestamp,
}

impl Attestation {
    /// Assembles a typed attestation.
    #[inline]
    #[must_use]
    pub const fn new(
        id: EventId,
        signer: PublicKey,
        attests: EventId,
        created_at: Timestamp,
    ) -> Self {
        Self {
            id,
            signer,
            attests,
            created_at,
        }
    }
}

/// A Conclusion (kind `3425`): a player's verdict on the session.
///
/// The event fixes two things the kernel reads — **who** concludes (its signer,
/// the invoker of the post-chain conventions: draw acceptance, residual
/// resignation) and **when** (its canonical timing, the cutoff at which the
/// natural state is evaluated) — and **claims** one thing the kernel checks: a
/// [`Verdict`], the termination status (the event's `content`) and the result
/// distribution (its `result` tags). A Conclusion is conforming iff its claim
/// equals the verdict the rule system yields at its cutoff (kind `3425`
/// §Semantic constraints, item 8 — *binding by correctness*); see
/// [`crate::verdict::check`].
///
/// The application maps the `result` tags to an outcome
/// ([`crate::session::SessionParams::outcome_from_scores`]) and the `content`
/// to a status, then builds the claim with [`Verdict::new`]; a Conclusion whose
/// tags admit no such mapping (an unknown status token, an uncommon split, a
/// status and an outcome of different kinds) cannot be conforming under the
/// `sanki` rule system and needs no kernel to be refused.
///
/// The claim is inert for the *computation*: [`crate::verdict::expected_verdict`]
/// reads only the signer and the cutoff, and [`crate::verdict::check`] compares
/// the claim afterwards. A caller wanting to know what verdict it would carry
/// by concluding *now* — a client before it signs, a bot deciding whether to
/// claim a win on time — calls [`crate::verdict::verdict_at`] with its side and
/// the present instant; no synthetic event is needed.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Conclusion {
    /// The Conclusion event's id (meta-resolution tiebreak).
    pub id: EventId,
    /// The concluding player's pubkey — the invoker.
    pub signer: PublicKey,
    /// The referenced Game Session (kind `3422`).
    pub session: EventId,
    /// The claimed verdict: the event's `content` and its `result` tags.
    pub claim: Verdict,
    /// The event's own `created_at` — the cutoff when the session is self-timed
    /// (on the module's acceptance precondition); ignored when attested.
    pub created_at: Timestamp,
}

impl Conclusion {
    /// Assembles a typed Conclusion.
    #[inline]
    #[must_use]
    pub const fn new(
        id: EventId,
        signer: PublicKey,
        session: EventId,
        claim: Verdict,
        created_at: Timestamp,
    ) -> Self {
        Self {
            id,
            signer,
            session,
            claim,
            created_at,
        }
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

    use super::{Attestation, Conclusion, EventId, Ply, PublicKey};
    use crate::verdict::Verdict;
    use sashite_sanki_engine::domain::status::{Outcome3, Status};
    use sashite_sanki_engine::domain::time::Timestamp;

    #[test]
    fn event_id_hex_round_trip() {
        let hex = "deadbeef".repeat(8); // 64 characters
        let id = EventId::parse(&hex).expect("valid hex");
        assert_eq!(id.to_string(), hex);
    }

    #[test]
    fn event_id_ordered_by_bytes() {
        // The smallest identifier (race-resolution tiebreak).
        let small = EventId::parse(&"0".repeat(64)).expect("valid hex");
        let large = EventId::parse(&format!("{}1", "0".repeat(63))).expect("valid hex");
        assert!(small < large);
    }

    #[test]
    fn parse_hex_rejects_invalid_inputs() {
        assert!(EventId::parse("too short").is_none());
        assert!(EventId::parse(&"z".repeat(64)).is_none()); // not hexadecimal
        assert!(PublicKey::parse(&"0".repeat(63)).is_none()); // 63 != 64
    }

    #[test]
    fn parse_accepts_uppercase_and_normalizes_to_lowercase() {
        // Nostr ids are canonically lowercase hex, but the decoding is
        // case-insensitive; `Display` re-encodes in the canonical form, so a
        // round trip normalizes rather than preserving the input's case.
        let upper = "AABBCCDD".repeat(8);
        let lower = "aabbccdd".repeat(8);
        let id = EventId::parse(&upper).expect("valid hex");
        assert_eq!(id.to_string(), lower);
        assert_eq!(id, EventId::parse(&lower).expect("valid hex"));

        let mixed = "AbCdEf0123456789".repeat(4);
        assert_eq!(
            PublicKey::parse(&mixed),
            PublicKey::parse(&mixed.to_lowercase()),
        );
        assert_eq!(
            PublicKey::parse(&mixed).expect("valid hex").to_string(),
            mixed.to_lowercase(),
        );
    }

    #[test]
    fn parse_measures_bytes_and_decodes_ascii_only() {
        // The length gate counts BYTES while the decoding walks CHARS: a string
        // of exactly 64 bytes but fewer characters passes the gate and must then
        // be rejected by the decoding, not read past its end.
        let multibyte = format!("{}{}", "é".repeat(2), "0".repeat(60)); // 64 bytes, 62 chars
        assert_eq!(multibyte.len(), 64);
        assert_eq!(multibyte.chars().count(), 62);
        assert!(EventId::parse(&multibyte).is_none());
        assert!(PublicKey::parse(&multibyte).is_none());

        // A non-ASCII decimal digit is not a hex digit either.
        let arabic_indic = format!("{}{}", '\u{0663}', "0".repeat(62)); // 64 bytes
        assert_eq!(arabic_indic.len(), 64);
        assert!(EventId::parse(&arabic_indic).is_none());

        // Neighbouring lengths and non-hex ASCII, none of which may panic.
        for rejected in [
            String::new(),
            "0".repeat(63),
            "0".repeat(65),
            format!(" {}", "0".repeat(63)),
            format!("{}+=", "0".repeat(62)),
            format!("{}gg", "0".repeat(62)),
        ] {
            assert!(EventId::parse(&rejected).is_none(), "{rejected:?}");
        }
    }

    #[test]
    fn event_id_order_is_the_raw_byte_order() {
        // The ordering is the race tiebreak ("smallest event ID") and is shared
        // with the TypeScript client, which compares the canonical lowercase hex
        // STRINGS: the two orders must coincide, or the two implementations would
        // select different events on a tie.
        let hexes = [
            "00".to_owned() + &"0".repeat(62),
            "0f".to_owned() + &"0".repeat(62),
            "10".to_owned() + &"0".repeat(62),
            "7f".to_owned() + &"0".repeat(62),
            "80".to_owned() + &"0".repeat(62),
            "ff".to_owned() + &"0".repeat(62),
            "ff".to_owned() + &"f".repeat(62),
        ];
        let mut by_bytes: Vec<EventId> = hexes
            .iter()
            .map(|hex| EventId::parse(hex).expect("valid hex"))
            .collect();
        by_bytes.sort();
        let mut by_string = hexes.clone();
        by_string.sort();
        let encoded: Vec<String> = by_bytes.iter().map(ToString::to_string).collect();
        assert_eq!(encoded, by_string.to_vec());

        // The comparison is big-endian: the first differing byte decides, and the
        // last byte still separates two otherwise identical ids.
        let mut high = [0_u8; 32];
        high[0] = 1;
        let mut low = [255_u8; 32];
        low[0] = 0;
        assert!(EventId::from_bytes(high) > EventId::from_bytes(low));
        let mut a = [7_u8; 32];
        let mut b = [7_u8; 32];
        a[31] = 8;
        b[31] = 9;
        assert!(EventId::from_bytes(a) < EventId::from_bytes(b));
    }

    #[test]
    fn public_key_equality() {
        let a = PublicKey::from_bytes([7; 32]);
        let b = PublicKey::from_bytes([7; 32]);
        let c = PublicKey::from_bytes([9; 32]);
        assert_eq!(a, b);
        assert_ne!(a, c);
    }

    #[test]
    fn ply_exposes_its_fields() {
        let ply = Ply::new(
            EventId::from_bytes([1; 32]),
            PublicKey::from_bytes([2; 32]),
            EventId::from_bytes([3; 32]),
            7,
            true,
            "[\"e2\",\"e4\",null]".to_owned(),
            Timestamp::from_unix(1000),
        );
        assert_eq!(ply.step, 7);
        assert!(ply.draw);
        assert_eq!(ply.signer, PublicKey::from_bytes([2; 32]));
        assert_eq!(ply.created_at, Timestamp::from_unix(1000));
    }

    #[test]
    fn attestation_carries_the_canonical_timing() {
        let attestation = Attestation::new(
            EventId::from_bytes([1; 32]),
            PublicKey::from_bytes([2; 32]),
            EventId::from_bytes([3; 32]),
            Timestamp::from_unix(1_700_000_000),
        );
        assert_eq!(attestation.created_at, Timestamp::from_unix(1_700_000_000));
        assert_eq!(attestation.attests, EventId::from_bytes([3; 32]));
    }

    #[test]
    fn conclusion_links_session_and_carries_its_claim() {
        let claim = Verdict::new(Status::Checkmate, Outcome3::FirstWins).expect("coherent");
        let conclusion = Conclusion::new(
            EventId::from_bytes([1; 32]),
            PublicKey::from_bytes([2; 32]),
            EventId::from_bytes([4; 32]),
            claim,
            Timestamp::from_unix(2000),
        );
        assert_eq!(conclusion.session, EventId::from_bytes([4; 32]));
        assert_eq!(conclusion.signer, PublicKey::from_bytes([2; 32]));
        assert_eq!(conclusion.created_at, Timestamp::from_unix(2000));
        assert_eq!(conclusion.claim, claim);
        assert_eq!(conclusion.claim.status(), Status::Checkmate);
        assert_eq!(conclusion.claim.outcome(), Outcome3::FirstWins);
    }
}
