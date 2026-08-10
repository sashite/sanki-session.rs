//! Typed Nostr event models the arbiter reasons about.
//!
//! The arbiter consumes events that the application has already received,
//! signature-verified (NIP-01), and parsed from their raw tag form. This module
//! gives those events a typed shape reduced to what arbitration needs:
//!
//! - [`Ply`] (kind `3423`) — a played half-move: its `step`, signer, optional
//!   `draw` flag, and opaque `content` (decoded later by the kernel);
//! - [`Attestation`] (kind `1041`) — the designated timestamper's receipt
//!   witness, carrying the **canonical timing** of the attested event;
//! - [`AdjudicationRequest`] (kind `3424`) — a player's invocation of the
//!   arbiter.
//!
//! Timing depends on the session's mode. A suite event's own `created_at` is the
//! signer's self-claim. When the session designates a timestamper (attested
//! mode), that self-claim is superseded by the [`Attestation`]'s `created_at`
//! and never drives race resolution (kind `3423` §Time accounting; kind `3424`
//! §Invocation timing). When the session is self-timed — no timestamper, the
//! default — there is no attestation, and the relay-enforced `created_at` IS the
//! canonical timing (nostr-integration §Timing). [`Ply`] and
//! [`AdjudicationRequest`] therefore carry `created_at`; it is consulted only in
//! the self-timed branch of [`crate::race_resolution::canonical_timing`].
//!
//! Identity is carried by [`EventId`] and [`PublicKey`], 32-byte newtypes over
//! the canonical Nostr encoding. [`EventId`] is ordered: the byte order is the
//! "smallest event ID" tiebreak of race resolution.

use sashite_sanki_engine::domain::time::Timestamp;

/// A 32-byte Nostr event identifier.
///
/// Ordered by raw bytes, which is the tiebreak used by race resolution
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
/// kernel's concern, not this model's. `created_at` is the event's relay-enforced
/// timestamp — the canonical timing in self-timed mode, ignored in attested mode
/// (see the module documentation).
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
    /// The event's own (relay-enforced) `created_at`. The canonical timing when
    /// the session is self-timed; ignored when a timestamper attests it.
    pub created_at: Timestamp,
}

impl Ply {
    /// Assembles a typed Ply from its arbiter-relevant fields.
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

/// An Event Timestamp Attestation (kind `1041`).
///
/// Authoritative for timing only when `signer` is the session's designated
/// timestamper; the arbiter applies that restriction. `created_at` is the
/// canonical timing the attestation confers on the attested event.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Attestation {
    /// The attestation event's id (meta-resolution tiebreak).
    pub id: EventId,
    /// The attesting signer (authoritative iff the designated timestamper).
    pub signer: PublicKey,
    /// The attested event (a Ply, an Adjudication Request, …).
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

/// An Adjudication Request (kind `3424`): a player's invocation of the arbiter.
///
/// Carries no claims — the arbiter rules on the natural state of events. The
/// request's authoritative timing is its [`Attestation`]'s `created_at` in
/// attested mode, or its own relay-enforced `created_at` when self-timed.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct AdjudicationRequest {
    /// The request event's id.
    pub id: EventId,
    /// The invoking player's pubkey.
    pub signer: PublicKey,
    /// The referenced Game Session (kind `3422`).
    pub session: EventId,
    /// The designated arbiter named by the request's `p` tag.
    pub arbiter: PublicKey,
    /// The event's own (relay-enforced) `created_at` — the canonical cutoff
    /// timing when the session is self-timed; ignored when attested.
    pub created_at: Timestamp,
}

impl AdjudicationRequest {
    /// Assembles a typed adjudication request.
    #[inline]
    #[must_use]
    pub const fn new(
        id: EventId,
        signer: PublicKey,
        session: EventId,
        arbiter: PublicKey,
        created_at: Timestamp,
    ) -> Self {
        Self {
            id,
            signer,
            session,
            arbiter,
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

    use super::{AdjudicationRequest, Attestation, EventId, Ply, PublicKey};
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
    fn adjudication_request_links_session_and_arbiter() {
        let request = AdjudicationRequest::new(
            EventId::from_bytes([1; 32]),
            PublicKey::from_bytes([2; 32]),
            EventId::from_bytes([4; 32]),
            PublicKey::from_bytes([5; 32]),
            Timestamp::from_unix(2000),
        );
        assert_eq!(request.session, EventId::from_bytes([4; 32]));
        assert_eq!(request.arbiter, PublicKey::from_bytes([5; 32]));
        assert_eq!(request.created_at, Timestamp::from_unix(2000));
    }
}
