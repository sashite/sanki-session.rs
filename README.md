# Sashité Sanki Session

[![Crates.io](https://img.shields.io/crates/v/sashite-sanki-session.svg)](https://crates.io/crates/sashite-sanki-session)
[![Docs.rs](https://docs.rs/sashite-sanki-session/badge.svg)](https://docs.rs/sashite-sanki-session)
[![License](https://img.shields.io/badge/license-Apache--2.0-blue.svg)](https://github.com/sashite/sanki-session.rs/blob/main/LICENSE)

The **session kernel** of the **Sanki** game suite, built for
[Sashité](https://sashite.com/). The L2 layer over
[`sashite-sanki-engine`](https://github.com/sashite/sanki-engine.rs): given a
session's public events, it computes the verdict the rule system yields at a
given instant — and therefore whether a player's **Conclusion** (kind `3425`),
which is *binding by correctness*, actually terminates the session. Published
under the Apache-2.0 license.

Until 0.12 this crate was `sashite-sanki-arbiter`: the same replay, run by a
designated arbiter that signed the verdict. Since the suite's 2026-09-04
revision (ADR-0033) there is no arbiter — either player concludes, and every
consumer recomputes. The crate is the reference implementation of the session
part of the `sashite.sanki.kernel/1` rule system.

The event model is **abstract** and carries **no Nostr dependency**: the same
kernel can be driven from any context able to supply the events — a rating
authority, a client, a bot, an indexer. Identity, transport, signature
verification and cross-event structural validation are the caller's
responsibility.

## Event model

The kernel reasons over plain values the caller has already received,
signature-verified, and parsed — there is no cryptography and no I/O here:

- `Ply` — a move played at a step (the kind-3423 content);
- `Attestation` — a timestamper's attestation of an event (kind 3410);
- `Conclusion` — a player's verdict on the session (kind 3425): who concludes,
  when (its canonical timing is the **cutoff**), and what it *claims*;
- `EventId` / `PublicKey` — opaque 32-byte identities.

Timing depends on the session's **mode**: in *attested* mode (a designated
timestamper) each event's canonical timing is its attestation's `created_at`,
the event's own being an informational self-claim; in *self-timed* mode (no
timestamper — the Sashité default for `sanki`) the event's own relay-enforced
`created_at` is the canonical timing. `Ply` and `Conclusion` carry their
`created_at` for the self-timed case.

## Evaluation

`kernel_result(params, plies, attestations, conclusion) -> Option<KernelResult>`
computes the verdict the rule system yields at the Conclusion's cutoff, with its
signer as the invoker:

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
  candidate — premove or live — is always **skipped**, never a loss (there is
  no `illegalmove`);
- the verdict is entirely **play-derived** (there is no equivocation sanction):
  a termination reached during replay — a rule-system ending or a played-Ply
  timeout — otherwise, on a still-ongoing position, the invocation resolved in
  order: draw acceptance, abandonment timeout, **residual resignation** (decisive
  against the concluding player, whatever the turn). Concluding is at the
  signer's risk: they choose *when*, never *what*.

`kernel_result` returns `None` only when the Conclusion is out of reach — it
does not reference this session, or its signer is not a session player (kind
3425 §Semantic constraints, items 2–3) — or when it has no canonical timing yet
(it is *pending*).

Two functions turn the result into the suite's protocol rules:

- `conforms(params, plies, attestations, conclusion) -> bool` — item 8 of the
  Conclusion's semantic constraints: the claim equals the kernel result. A
  non-conforming Conclusion does not terminate the session, whatever its
  timing;
- `select_conclusion(params, plies, attestations, conclusions) -> Option<&Conclusion>`
  — the session's **canonical Conclusion** (kind 3425 §Idempotence and
  finality): the earliest *conforming* one by canonical timing, smallest event
  id as tiebreaker. Two conforming Conclusions with different cutoffs may carry
  different verdicts (a premature claim resolving to its signer's resignation,
  a later win on time); the earliest rules.

A synthetic Conclusion timed "now" is the natural **probe**: a client about to
sign one, or a bot deciding whether to claim a win on time, reads
`kernel_result` for it and publishes exactly that.

## Design guarantees

- **Panic-free by construction.** Crate lints forbid `unsafe`, and deny
  `unwrap`/`expect`/`panic`, slice indexing, and overflowing arithmetic.
- **Deterministic.** The result is a pure function of the events; identity,
  transport, and signature verification are the caller's responsibility.
- **In lockstep with the client.** The vendored conformance corpus
  (`tests/conformance/`) is shared with Sashité's TypeScript client, which
  replays the same vectors: the two implementations cannot drift on which Ply
  is canonical nor on how a chain ends.

## Usage

```toml
[dependencies]
sashite-sanki-session = "0.13"
```

```rust
use sashite_sanki_session::event::{Attestation, Conclusion, EventId, Ply, PublicKey};
use sashite_sanki_session::session::SessionParams;
use sashite_sanki_session::verdict::{conforms, kernel_result};
use sashite_sanki_engine::domain::side::Side;
use sashite_sanki_engine::domain::status::{Outcome3, Status};
use sashite_sanki_engine::domain::time::{Duration, Timestamp};
use sashite_sanki_engine::domain::time_control::{Period, TimeControl};
use sashite_sanki_engine::position::Position;

// Identities (opaque 32-byte values; the caller maps them from its own source).
let session = EventId::from_bytes([50; 32]);
let timestamper = PublicKey::from_bytes([99; 32]);
let first = PublicKey::from_bytes([10; 32]);
let second = PublicKey::from_bytes([20; 32]);

// The session's invariant parameters, including the initial position (FEEN).
// A designated timestamper puts the session in attested mode; `None` would
// make it self-timed (each event's own `created_at` authoritative).
let period = Period::new(Duration::from_secs(600), None, None).expect("valid period");
let params = SessionParams::new(
    session,
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

// The second player concludes, claiming checkmate for the first player; the
// timestamper has attested both the ply (t=100) and the Conclusion (t=1000,
// the cutoff).
let conclusion = Conclusion::new(
    EventId::from_bytes([170; 32]),
    second,
    session,
    Status::Checkmate,
    Outcome3::FirstWins,
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

// What the rule system yields at the cutoff…
let result = kernel_result(&params, &plies, &attestations, &conclusion).expect("a result");
assert_eq!(result.status(), Status::Checkmate);
assert_eq!(result.result(), Outcome3::FirstWins);
assert_eq!(result.score(Side::First), 100);

// …is what the Conclusion claims: it conforms and terminates the session.
assert!(conforms(&params, &plies, &attestations, &conclusion));
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

Licensed under the [Apache License, Version 2.0](https://github.com/sashite/sanki-session.rs/blob/main/LICENSE). See [NOTICE](https://github.com/sashite/sanki-session.rs/blob/main/NOTICE).
