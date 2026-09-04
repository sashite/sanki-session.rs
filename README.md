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
  when (its canonical timing is the **cutoff**), and what it *claims* — a
  `Verdict`, a termination status and an outcome, coherent by construction
  (`Verdict::new` refuses a decisive status with a draw, or the converse: a
  claim the rule system can never yield is refused before any replay);
- `EventId` / `PublicKey` — opaque 32-byte identities (event ids unique, as
  Nostr guarantees — the kernel tells candidates apart by id).

Timing depends on the session's **mode**: in *attested* mode (a designated
timestamper) each event's canonical timing is its attestation's `created_at`,
the event's own being an informational self-claim; in *self-timed* mode
(designated timing relays, no timestamper — what Sashité deploys for `sanki`)
the event's own `created_at`, as accepted by a designated timing relay, is the
canonical timing. `Ply` and `Conclusion` carry their `created_at` for the
self-timed case — **on the precondition that the caller offers the kernel only
events whose acceptance by a designated timing relay is established**; an
event without that is pending, not a candidate for anything, and this crate
has no way to tell (it sees no relay).

## Evaluation

`verdict_at(params, plies, attestations, invoker, cutoff) -> Result<Verdict, NoVerdict>`
is the primitive: the verdict the rule system yields at `cutoff`, with `invoker`
as the concluding side. `expected_verdict(params, plies, attestations, conclusion)`
wraps it, resolving the invoker and cutoff from a Conclusion (its signer and its
canonical timing, `cutoff_of`) — what that Conclusion is expected to claim:

- **canonical timing** gives each Ply its instant (its attestation's
  `created_at` in attested mode — smallest attestation as tiebreaker — its own
  `created_at` when self-timed) — `step` being each signer's own move ordinal;
- the **natural state** replays the interleaved play order (within each step
  value, side `first` before side `second`), selecting each
  `(session, signer, step)` slot's canonical Ply by the **two-window forgiving**
  rule against the slot's boundary `T` (the maximum canonical timing among the
  preceding half-moves — never rewound by a selected premove — t₀ for the first
  slot): among *anterior* candidates (timed before `T` —
  premoves) the **latest legal** wins; failing that, among *informed* candidates
  (timed at/after `T` — live moves) the **earliest legal** wins; each within a
  per-window cap `K`, legality probed **lazily on the capped windows only**
  (≤ 2K full-rule probes per slot, the normative anti-flooding bound; `K` is
  the session's `candidate_cap`, the rule-system document's, at least 1 by
  type). A Ply timed before t₀ is invalid and never enters a slot (one timed
  at t₀ does); identical candidates (same content, same `draw` flag) count
  once against the cap, represented by the one the window's scan reaches
  first, so the collapse never changes the selection. An illegal candidate —
  premove or live — is always **skipped**, never a loss (there is no
  `illegalmove`);
- the verdict is entirely **play-derived** (there is no equivocation sanction):
  a termination reached during replay — a rule-system ending or a played-Ply
  timeout — otherwise, on a still-ongoing position, the invocation resolved in
  order: draw acceptance, abandonment timeout, **residual resignation** (decisive
  against the concluding player, whatever the turn). Concluding is at the
  signer's risk: they choose *when*, never *what*.

`expected_verdict` returns `Err(NoVerdict)` — a typed reason, not a bare `None`
— when no verdict is defined: `OtherSession` or `NotAPlayer` (kind 3425
§Semantic constraints, items 2–3), `BeforeStart` (a cutoff before t₀ — item 9;
the kernel is not invoked before the session is playable), `Pending` (no
canonical timing yet — the one transient reason, `NoVerdict::is_transient`),
or `Inconsistent` (a broken internal invariant, never a silently wrong
verdict). A consumer must tell the transient case from the final ones: a
pending Conclusion is re-examined once timed, an invalid one is ignored for
good.

Two functions turn the verdict into the suite's protocol rules:

- `check(params, plies, attestations, conclusion) -> Check` — item 8 of the
  Conclusion's semantic constraints: `Conforming(verdict)` when the claim
  equals the verdict the rule system yields, `Wrong { claimed, expected }` when
  it differs (a non-conforming Conclusion does not terminate the session,
  whatever its timing), or `NoVerdict(reason)`. `conforms(…) -> bool` is
  `check` reduced to "is it `Conforming`";
- `select_conclusion(params, plies, attestations, conclusions) -> Result<Option<CanonicalConclusion>, NoVerdict>`
  — the session's **canonical Conclusion** (kind 3425 §Idempotence and
  finality): the earliest *conforming* one by canonical timing, smallest event
  id as tiebreaker, returned with its cutoff and verdict so no second replay is
  needed; `Ok(None)` while none conforms with established timing. Two
  conforming Conclusions with different cutoffs may carry different verdicts
  (a premature claim resolving to its signer's resignation, a later win on
  time); the earliest rules. An inconsistent replay is `Err`: the session is
  unresolved for this consumer, and no other Conclusion is promoted around it.

To decide *whether* to conclude, a client or bot calls `verdict_at` with its own
side and the present instant — no synthetic event — and publishes exactly that
verdict; `Verdict::scores(params)` gives the two `result` tags to carry.

## Design guarantees

- **Panic-free by construction.** Crate lints forbid `unsafe`, and deny
  `unwrap`/`expect`/`panic`, slice indexing, and overflowing arithmetic.
- **Deterministic.** The result is a pure function of the events; identity,
  transport, and signature verification are the caller's responsibility.
- **In lockstep with the client.** The vendored conformance corpus
  (`tests/conformance/`, v9) is shared with Sashité's TypeScript client, which
  replays the same vectors: the two implementations cannot drift on which Ply
  is canonical, on how a chain ends, nor — the vectors carrying an `invoker`
  and an `expectedVerdict` — on the post-chain verdict.
- **Reported, never guessed.** A Conclusion out of reach gets a typed reason;
  a broken internal invariant during replay is `NoVerdict::Inconsistent`, not a
  wrong resignation — and `select_conclusion` propagates it rather than
  promoting the next Conclusion in line.
- **Refused at the boundary, not in the replay.** What the rule system can
  never yield — a self-play `Seats`, a founding position without `first` to
  move, a cap of 0, a status and an outcome of different kinds, a split other
  than 100/0, 50/50, 0/100 — is refused by the constructors (`Option`, or the
  type), so the kernel only ever evaluates sessions and claims it has a
  defined answer for.

## Usage

```toml
[dependencies]
sashite-sanki-session = "0.14"
```

```rust
use sashite_sanki_session::event::{Attestation, Conclusion, EventId, Ply, PublicKey};
use sashite_sanki_session::session::{Seats, SessionParams};
use sashite_sanki_session::verdict::{check, Check, Verdict};
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

// The session's invariant parameters, including the initial position (FEEN,
// `first` to move — as every position a Sanki rule-system document
// prescribes). A designated timestamper puts the session in attested mode;
// `None` would make it self-timed (each event's own `created_at`, as accepted
// by a designated timing relay, authoritative).
let period = Period::new(Duration::from_secs(600), None, None).expect("valid period");
let params = SessionParams::new(
    session,
    Some(timestamper),
    Seats::new(first, second).expect("distinct players"),
    TimeControl::new(period, Vec::new()),
    Position::parse("7k^/6pp/8/8/8/8/8/R3K^3 / W/w").expect("valid FEEN"),
    Timestamp::from_unix(0),
)
.expect("first to move");

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

// The second player concludes, claiming checkmate for the first player (a
// coherent claim: a decisive status with a decisive outcome); the timestamper
// has attested both the ply (t=100) and the Conclusion (t=1000, the cutoff).
let conclusion = Conclusion::new(
    EventId::from_bytes([170; 32]),
    second,
    session,
    Verdict::new(Status::Checkmate, Outcome3::FirstWins).expect("coherent"),
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

// Checking the Conclusion on the rules axis: the claim it carries equals the
// verdict the rule system yields at its cutoff, so it is Conforming and
// terminates the session.
match check(&params, &plies, &attestations, &conclusion) {
    Check::Conforming(verdict) => {
        assert_eq!(verdict, conclusion.claim);
        assert_eq!(verdict.status(), Status::Checkmate);
        assert_eq!(verdict.outcome(), Outcome3::FirstWins);
        assert_eq!(verdict.score(Side::First), 100);
    }
    other => panic!("expected a conforming Conclusion, got {other:?}"),
}
```

## Built on

[`sashite-sanki-engine`](https://github.com/sashite/sanki-engine.rs) (the rules
engine), which it uses under the full rule system — ōgi's uchifuzume included:
candidate legality is probed via the façade's `validate`, and the selected Ply
is applied via the kernel's per-ply step. The two agree by construction
(pinned by an exhaustive oracle test across every variant pairing); should
they ever disagree, the replay reports `ChainEnd::Inconsistent` and no verdict
is derived.

## Minimum supported Rust version

Rust 1.81.

## License

Licensed under the [Apache License, Version 2.0](https://github.com/sashite/sanki-session.rs/blob/main/LICENSE). See [NOTICE](https://github.com/sashite/sanki-session.rs/blob/main/NOTICE).
