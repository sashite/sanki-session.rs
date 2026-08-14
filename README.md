# Sashité Sanki Arbiter

[![Crates.io](https://img.shields.io/crates/v/sashite-sanki-arbiter.svg)](https://crates.io/crates/sashite-sanki-arbiter)
[![Docs.rs](https://docs.rs/sashite-sanki-arbiter/badge.svg)](https://docs.rs/sashite-sanki-arbiter)
[![License](https://img.shields.io/badge/license-Apache--2.0-blue.svg)](https://github.com/sashite/sanki-arbiter.rs/blob/main/LICENSE)

Adjudication logic for the **Sanki** game suite, built for
[Sashité](https://sashite.com/). The L2 layer over
[`sashite-sanki-engine`](https://github.com/sashite/sanki-engine.rs): it rules on
a session from its public events and emits a binding verdict. Published under the
Apache-2.0 license.

The event model is **abstract** and carries **no Nostr dependency**: the same
adjudication logic can be driven from any context able to supply the events — a
Nostr relay or service, a server backend, or even a client. Identity, transport,
and signature verification are the caller's responsibility.

## Event model

The arbiter reasons over plain values the caller has already received,
signature-verified, and parsed — there is no cryptography and no I/O here:

- `Ply` — a move played at a step (the kind-3423 content);
- `Attestation` — a timestamper's attestation of an event (kind 3410);
- `AdjudicationRequest` — a request to rule on a session (kind 3424);
- `EventId` / `PublicKey` — opaque 32-byte identities.

Timing depends on the session's **mode**: in *attested* mode (a designated
timestamper) each event's canonical timing is its attestation's `created_at`,
the event's own being an informational self-claim; in *self-timed* mode (no
timestamper — the Sashité default for `sanki`) the event's own relay-enforced
`created_at` is the canonical timing. `Ply` and `AdjudicationRequest` carry
their `created_at` for the self-timed case.

## Adjudication

`adjudicate(params, plies, attestations, request) -> Option<Adjudication>` rules
on a session, cut off at the triggering Request's canonical attestation:

- **race resolution** gives each Ply a canonical timing (its attestation's
  `created_at` in attested mode, its own relay-enforced `created_at` when
  self-timed; smallest event id as tiebreaker) — `step` being each signer's
  own move ordinal;
- the **natural state** replays the interleaved play order (within each step
  value, side `first` before side `second`), selecting each
  `(session, signer, step)` slot's canonical Ply by the **two-window forgiving**
  rule against the slot's boundary `T` (the predecessor half-move's canonical
  timing, t₀ for the first slot): among *anterior* candidates (timed before `T` —
  premoves) the **latest legal** wins; failing that, among *informed* candidates
  (timed at/after `T` — live moves) the **earliest legal** wins; each within a
  per-window cap `K`, legality probed **lazily on the capped windows only**
  (≤ 2K full-rule probes per slot, the normative anti-flooding bound). A Ply
  timed before t₀ is invalid and never enters a slot; identical-content
  re-submissions are idempotent retries, collapsed to their race-canonical
  representative (smallest timing, then event id) before selection. An illegal
  candidate — premove or live — is always
  **skipped**, never a loss (there is no `illegalmove`);
- the verdict is entirely **play-derived** (there is no equivocation sanction):
  a termination reached during replay — a rule-system ending or a played-Ply
  timeout — otherwise, on a still-ongoing position, the invocation resolved in
  order: draw acceptance, abandonment timeout, **residual resignation** (decisive
  against the invoker, whatever the turn).

`adjudicate` returns `None` only when the Request is non-conforming — it does
not reference this session and this arbiter, or its signer is not a session
player (kind 3424 §Semantic constraints, items 2–4) — or when it has no
canonical timing yet. Several Requests may coexist; `select_request` pins the
deterministic **which Request rules** policy (earliest canonical timing,
smallest event id as tiebreaker, among conforming timed Requests) — "not yet
adjudicated" stays the caller's ledger.

## Design guarantees

- **Panic-free by construction.** Crate lints forbid `unsafe`, and deny
  `unwrap`/`expect`/`panic`, slice indexing, and overflowing arithmetic.
- **Deterministic.** The verdict is a pure function of the events; identity,
  transport, and signature verification are the caller's responsibility.

## Usage

```toml
[dependencies]
sashite-sanki-arbiter = "0.12"
```

```rust
use sashite_sanki_arbiter::event::{AdjudicationRequest, Attestation, EventId, Ply, PublicKey};
use sashite_sanki_arbiter::session::SessionParams;
use sashite_sanki_arbiter::verdict::adjudicate;
use sashite_sanki_engine::domain::side::Side;
use sashite_sanki_engine::domain::status::{Outcome3, Status};
use sashite_sanki_engine::domain::time::{Duration, Timestamp};
use sashite_sanki_engine::domain::time_control::{Period, TimeControl};
use sashite_sanki_engine::position::Position;

// Identities (opaque 32-byte values; the caller maps them from its own source).
let session = EventId::from_bytes([50; 32]);
let arbiter = PublicKey::from_bytes([2; 32]);
let timestamper = PublicKey::from_bytes([99; 32]);
let first = PublicKey::from_bytes([10; 32]);
let second = PublicKey::from_bytes([20; 32]);

// The session's invariant parameters, including the initial position (FEEN).
// A designated timestamper puts the session in attested mode; `None` would
// make it self-timed (each event's own `created_at` authoritative).
let period = Period::new(Duration::from_secs(600), None, None).expect("valid period");
let params = SessionParams::new(
    session,
    arbiter,
    Some(timestamper),
    first,
    second,
    TimeControl::new(period, Vec::new()),
    Position::parse("7k^/6pp/8/8/8/8/8/R3K^3 / W/w").expect("valid FEEN"),
    Timestamp::from_unix(0),
);

// One ply: the first player plays Ra1-a8, a back-rank mate. In attested mode
// the ply's own `created_at` (last argument) is an informational self-claim;
// the timestamper's attestation below is authoritative.
let plies = [Ply::new(
    EventId::from_bytes([1; 32]),
    first,
    session,
    1,
    false,
    r#"["a1","a8",null]"#.to_owned(),
    Timestamp::from_unix(90),
)];

// The second player requests adjudication; the timestamper has attested both the
// ply (t=100) and the request (t=1000, the cutoff).
let request = AdjudicationRequest::new(
    EventId::from_bytes([170; 32]),
    second,
    session,
    arbiter,
    Timestamp::from_unix(900),
);
let attestations = [
    Attestation::new(
        EventId::from_bytes([101; 32]),
        timestamper,
        EventId::from_bytes([1; 32]),
        Timestamp::from_unix(100),
    ),
    Attestation::new(
        EventId::from_bytes([171; 32]),
        timestamper,
        EventId::from_bytes([170; 32]),
        Timestamp::from_unix(1000),
    ),
];

let adjudication = adjudicate(&params, &plies, &attestations, &request).expect("a ruling");
assert_eq!(adjudication.status(), Status::Checkmate);
assert_eq!(adjudication.result(), Outcome3::FirstWins);
assert_eq!(adjudication.score(Side::First), 100);
```

## Built on

[`sashite-sanki-engine`](https://github.com/sashite/sanki-engine.rs) (the rules
engine), which it uses under the full rule system — ōgi's uchifuzume included:
candidate legality is probed via the façade's `validate`, and the selected Ply
is applied via the kernel's per-ply step (whose `StepResult::Illegal`, since
engine 0.5, hands the state back — the defensive seam this crate degrades to an
ongoing end).

## Minimum supported Rust version

Rust 1.81.

## License

Licensed under the [Apache License, Version 2.0](https://github.com/sashite/sanki-arbiter.rs/blob/main/LICENSE). See [NOTICE](https://github.com/sashite/sanki-arbiter.rs/blob/main/NOTICE).
