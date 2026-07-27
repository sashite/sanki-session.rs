# Changelog

All notable changes to this crate are documented in this file. The format is
based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/), and this
crate adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [0.9.3] — 2026-07-27

Test data only, no code change.

### Added

- **Conformance: the chain-level equal-timing races** (scenarios corpus, two
  additive v6 vectors). `scenario.race-equal-timing-informed-smallest-id`:
  same signer, same slot, same canonical timing, two DIFFERENT legal moves
  (the double-publish family) — the informed window breaks the tie to the
  **smallest** event id. `scenario.race-equal-timing-anterior-largest-id`:
  two premoves in the same anterior second — the anterior window breaks to
  the **largest** id (a same-second re-premove still supersedes,
  deterministically). Both pin the id tiebreak the selection rule already
  defines (category B) at the whole-chain level: the arbiter and the client
  must select the same event or they finalise/display different games.
  Byte-identical to the client corpus copy and to the editorial source
  (web-specs.md).

## [0.9.2] — 2026-07-27

Test data only, no code change.

### Changed

- **Conformance corpus re-embedded in its canonical byte form**
  (`tests/conformance/scenarios.json` — formatting only, verified
  vector-by-vector identical in content). The Sanki client's
  cross-implementation gate compares the corpus copies **byte-for-byte**
  against the published crates, so the canonical (prettier-shaped) bytes now
  ship in the crate itself.

## [0.9.1] — 2026-07-26

Follows the engine's 0.6.1 release: the kernel clock anchor never rewinds.

### Fixed

- **The selection boundary never rewinds after an applied premove.** The
  natural-state replay advanced the slot boundary to the selected Ply's
  canonical timing verbatim; a premove's ANTERIOR timing therefore rewound it,
  (a) misclassifying the next slot's blind candidates as informed and (b) — via
  the kernel clock — billing the next mover for time before the position was
  theirs to answer, up to a wrongful `timeout` flagged on that mover's own
  half-move (a 4-second reply read as 13). The boundary now advances
  monotonically (`anchor.max(at)`), mirroring the engine's fixed
  `SessionState::advance`. `sashite-sanki-engine` is required at **0.6.1** —
  the new shared conformance vector `scenario.premove-anchor-never-rewinds`
  fails against 0.6.0.

### Added

- **Conformance: `scenario.premove-anchor-never-rewinds`** (scenarios corpus) —
  an 18-second bank, a step-2 premove attested before the opponent's step-1
  reply, and a 4-real-second answer that must NOT flag. Byte-identical to the
  client corpus copy (sanki.app.sveltekit `conformance/scenarios.json`).

## [0.9.0] — 2026-07-22

Follows the engine's 0.6.0 release, which adds the absolute move cap
(`movecap`) to the shared status vocabulary.

### Changed — breaking

- **`sashite-sanki-engine` bumped to 0.6** (public-API types): the status
  vocabulary gains `Status::MoveCap` — the absolute 300-move (600-half-move)
  cap that draws a game once no decisive progress plausibly remains. It is
  **last** in the terminal priority order, after the resetting 50-move
  `movelimit`: a genuine mate, stalemate, insufficiency, repetition, or move
  limit on the 600th half-move still outranks it. A downstream exhaustive
  `match` on the `Status` this crate re-exposes (e.g. through
  `Adjudication::status`) must now handle the new arm.

### Added

- **The natural-state replay surfaces the `movecap` draw.** A session that
  reaches the 600th half-move with no earlier termination now concludes
  `Terminal(MoveCap)`. No adjudication logic changed — the cap is enforced by
  the engine kernel the replay drives, and the verdict flows through the
  crate's generic `Verdict::Terminated { status, .. }` handling untouched.
- The shared `scenarios.json` gains the
  `scenario.movecap-on-the-six-hundredth-half-move` vector (corpus **v6**): a
  600-half-move, capture-free, mate-free game triggering neither the 50-move
  rule nor a threefold repetition, drawn exactly on the cap. The generic
  conformance harness (`status.to_string()` against the pinned token) exercises
  it with no test-code change.

## [0.8.0] — 2026-07-19

Conformance release following the global audit, on top of the engine's 0.5
correctness release.

### Fixed

- **Identical-content re-submissions are deduplicated.** They are idempotent
  retries, not alternatives (Move Encoding — Sanki §Slot candidates and
  selection): per content, only the race-canonical representative — smallest
  canonical timing, then smallest event id (kind 6423 §Race resolution) —
  enters the two-window selection. Previously each duplicate was a distinct
  alternative, so the anterior window could select the *latest* duplicate,
  shifting the selected event id, the next slot's boundary and the clock
  anchor; duplicates also consumed cap slots.
- **Request conformance is fully enforced.** `adjudicate` now returns `None`
  for a Request that does not reference this session and this arbiter (kind
  6424 §Semantic constraints, items 2 and 4) — a cross-session invocation can
  no longer resolve as a resignation in the wrong session. Item 3 (the signer
  is a session player) was already enforced.
- **The candidate cap now bounds the legality work.** Legality is probed
  lazily through a callback, on the capped windows only — at most 2K
  full-rule-system probes per slot (the normative anti-flooding bound of Move
  Encoding — Sanki §Bounding a slot's candidates); a flood of candidates
  previously cost one full probe each.

### Changed — breaking

- **`sashite-sanki-engine` bumped to 0.5** (public-API types): the nine-status
  vocabulary (`Status::IllegalMove` is gone) and the kernel's new `StepResult`
  enum. The natural-state replay now receives the untouched state back from a
  (defensively unreachable) `StepResult::Illegal` — the 0.7.1 defensive clone
  is gone.
- **`selection::Candidate` no longer carries `legal`**, and `select_candidate`
  takes the legality probe as a callback (the lazy bound above). The shared
  `selection.json` vectors drive the same algorithm through the probe.

### Added

- **`verdict::select_request`** — the deterministic "which Request rules"
  policy of Statuses — Sanki: among conforming, canonically timed Requests,
  the earliest by (canonical timing, event id). "Not yet adjudicated" stays
  the caller's ledger.
- Tests for the audit's coverage gaps: duplicate collapse, pre-t₀ exclusion
  (normative per kind 6423 §Time accounting, deciders' confirmation of
  2026-07-19), the played-Ply timeout surfaced by the replay, the ≤ 2K probe
  bound, and both new Request-conformance rejections.

## [0.7.1] — 2026-07-19

### Changed

- **perf: candidate legality is probed without cloning.** `is_legal` now asks
  `engine::validate` on the replayed position — equivalent to the historical
  kernel-`step` probe since engine 0.4 (uchifuzume included), an agreement
  pinned by a new step-oracle equivalence test — instead of cloning the whole
  `SessionState` (history map included) and running a full step, with its
  terminal classification, per candidate. Applying the selected Ply gains a
  defensive guard: should `step` reject an already-validated candidate (a
  broken internal invariant, unreachable on well-formed input), the chain now
  degrades to an ongoing end — an illegal Ply is never a loss — instead of
  surfacing an `illegalmove` verdict outside the status vocabulary. Rulings
  are unchanged.

## [0.7.0] — 2026-07-19

Tracks the engine's **uchifuzume-exact release** (`sashite-sanki-engine`
0.4.0). The arbiter's own adjudication logic is unchanged: candidate legality
was already judged through the kernel's `step` path, which enforced uchifuzume
before and after this release — the engine change chiefly brings the façade
(`validate` / `legal_moves` / `status`) into line with the legality this crate
always applied. One exactness corner does reach verdicts through the replay:
checkmate/stalemate classification is now uchifuzume-aware
(`has_full_legal_move`), so the vanishingly rare position whose only escape
would be a mating Fu drop now terminates `checkmate` instead of playing on.

### Changed — breaking

- **`sashite-sanki-engine` bumped to 0.4** — a breaking engine release whose
  types appear in this crate's public API: `IllegalReason` gains the
  `Uchifuzume` variant, which kernel outcomes now report for a mating Fu drop
  (previously folded into `IllegalDrop`). No source change was required; the
  `is_legal` doc comment no longer contrasts the kernel path with
  `engine::validate`, the two agreeing on legality since 0.4.

### Fixed

- **README brought back in line with the self-timed API** (0.5's breaking
  changes had not reached it): the usage example now passes the optional
  timestamper (`Option<PublicKey>`) and the events' `created_at`, and the
  timing prose describes both modes. The crate docs now include the README
  (`#![doc = include_str!…]`, the engine's pattern), so the example is a
  doc-test and can no longer rot silently.

## [0.6.0] — 2026-07-14

Tracks the engine's variant-specific **dead-position detection**
(`sashite-sanki-engine` 0.3.0, rules update of 2026-07-13). The arbiter's own
logic is unchanged — the detection lives entirely in the engine's replay — but
verdicts differ where the rules changed: a pure-chess replay now ends in an
immediate `insufficient` draw on K+B vs K, K+N vs K, and same-coloured-Bishops
material, and pure ōgi never draws by dead position.

### Changed — breaking

- **`sashite-sanki-engine` bumped to 0.3** — a breaking engine release whose
  types appear in this crate's public API. No source change was required.

### Added

- Conformance scenario `scenario.deadposition-chess-kb-closes-the-chain`
  (shared corpus v4): the capture that leaves King + Bishop versus King closes
  the chain on that ply and rules `insufficient`; the opponent's legal reply
  is void.

## [0.5.0] — 2026-07-08

Adds **self-timed** adjudication: a session may designate no timestamper (the
default — attestation is a dormant capability), in which case each event's own
relay-enforced `created_at` is its canonical timing (nostr-integration §Timing).

### Changed — breaking

- **`SessionParams` takes `Option<PublicKey>` for the timestamper.**
  `SessionParams::new`'s `timestamper` argument and `SessionParams::timestamper()`
  are now `Option<PublicKey>`; `None` means self-timed. `is_timestamper` is always
  `false` for a self-timed session.
- **`Ply` and `AdjudicationRequest` now carry `created_at`.** Their `::new`
  constructors take a trailing `Timestamp`. It is the canonical timing in
  self-timed mode and ignored (superseded by the attestation) in attested mode.
- **`canonical_ply` takes `Option<PublicKey>`** for the timestamper.

### Added

- **`race_resolution::canonical_timing`** — resolves an event's canonical timing
  in either mode: the timestamper's attestation (attested) or the event's own
  `created_at` (self-timed).

## [0.4.0] — 2026-07-06

Revises the forgiving-premove model to the **two-window** selection: a
slot's premoves and live moves are ranked separately around the predecessor's
timing — the *latest* legal premove binds, else the *earliest* legal live move —
and an illegal candidate (premove or live) is always skipped, so the `illegalmove`
termination is gone.

### Changed — breaking

- **`selection` module — two-window rule.** `select_candidate` now takes the
  slot's `boundary` and a per-window `cap`: `select_candidate(boundary,
  candidates, cap) -> Applied | Unfilled`. A candidate timed **before** the
  boundary is *anterior* (a premove); one **at or after** it is *informed* (a live
  move). Among the `cap` most-recent anterior candidates the **latest legal**
  wins; failing that, among the `cap` earliest informed candidates the **earliest
  legal** wins. The `Selection::IllegalMove` variant is removed (leaving
  `Applied | Unfilled`), and the `ANTERIOR_CAP = 1` constant becomes the
  per-window `CANDIDATE_CAP = 8`.
- **No `illegalmove` termination.** An illegal candidate — premove or live — is
  always skipped, never a loss. `natural_state` no longer produces an
  `illegalmove` verdict; `Conclusion::Terminal` now carries only a rule-system
  ending or a played-Ply timeout, and a slot with no legal candidate in either
  window leaves the chain ongoing. `verdict` drops the informed-illegal cause.

### Removed — breaking

- **`Selection::IllegalMove`** and the **`ANTERIOR_CAP`** constant — superseded by
  the two-window `Selection` (`Applied | Unfilled`) and `CANDIDATE_CAP`.

### Changed

- **Conformance corpus (v3).** The vendored vectors and `tests/conformance.rs`
  track the shared set: `selection.json` gains `boundary` and a per-window `cap`
  (17 vectors), `scenarios.json` uses `timedAt` and adds the re-premove and
  premove-over-live cases (8 vectors) — kept bit-for-bit with the TypeScript
  client.

### Unchanged

- The `adjudicate` entry point and `Adjudication` are source-compatible (same
  signatures and results for legal play).
- Race resolution (`canonical_attestation`, `canonical_ply`) and its tiebreaks.
- The post-chain resolution order (agreement → timeout → resignation) and the
  rule-system / timeout terminations.
- `Status::IllegalMove` remains the engine's internal legality signal (consumed by
  `natural_state::is_legal`); the arbiter simply never emits it as a verdict.

## [0.3.0] — 2026-06-27

Adopts the **forgiving-premove** model: a slot's candidate Plies are
resolved by legality and anteriority — an illegal *blind* premove is forgiven
(skipped), not sanctioned — and the equivocation sanction is removed entirely.

### Added

- **`selection` module** — the pure `select_candidate(anchor, candidates) ->
  Applied | IllegalMove | Unfilled`, generic over the candidate id, implementing
  the selection rule with the normative `K = 1` anterior cap (one premove per
  slot, no re-pre-play). Mirrors the TypeScript client's `selectCandidate`.
- **Selection conformance test** (`tests/conformance.rs`) driving the shared
  `selection.json` vectors (vendored at `tests/conformance/`) through
  `select_candidate`, pinning bit-for-bit parity with the TypeScript client.
  Adds `serde` / `serde_json` as dev-dependencies.

### Changed — breaking

- **Forgiving natural-state replay.** `natural_state` now selects each slot's
  canonical Ply by the forgiving rule and applies it through the engine in a
  single pass (legality is judged on the replayed board). `NaturalState` gains a
  `conclusion: Conclusion` field — `Conclusion::Terminal(verdict, at)` for an
  in-replay ending (informed illegal move, rule-system ending, or played-Ply
  timeout) or `Conclusion::Ongoing(Box<SessionState>)` for the post-chain
  resolution. The chain no longer includes a terminating *informed-illegal* Ply.
- **Play-derived verdict only.** `verdict` drops the equivocation candidate
  family and the separate second replay; the verdict is the natural state's
  terminal conclusion, else the invocation resolved at the cutoff (draw
  acceptance → abandonment timeout → residual resignation).

### Removed — breaking

- **`commitment` module** — the single-content / equivocation / mutual-
  equivocation sanction. Differing contents for a slot are no longer a violation
  but ordinary candidates resolved by `selection`; a misfired blind premove is
  forgiven rather than ruled `illegalmove`.

### Unchanged

- The `adjudicate` entry point and `Adjudication` are source-compatible (same
  signatures and results for legal play).
- Race resolution (`canonical_attestation`, `canonical_ply`) and its tiebreaks.
- The post-chain resolution order (agreement → timeout → resignation) and
  rule-system / timeout terminations.

## [0.2.1] — 2026-06-13

### Changed

- Depend on `sashite-sanki-engine = "0.2"` (was `"0.1"`), tracking the engine's
  rename of `SessionState::step` to `half_move`. No change to this crate's own
  public API or behaviour; only the internal kernel-state accessor call and a
  test assertion are updated.

## [0.2.0] — 2026-06-13

Aligns the crate with the revised Sanki adjudication specifications
(per-player step semantics, residual resignation, equivocation-only
violations, ordered post-chain resolution).

### Changed — breaking

- **Per-player step semantics.** A Ply's `step` is now the signer's own move
  ordinal (kind `6423` §Step semantics and play order); the slot is
  `(session, signer, step)` and the natural-state chain consumes slots in the
  interleaved play order — within each step value, side `first` before side
  `second`. `SessionParams::expected_side` / `expected_signer` are replaced by
  `side_at(half_move)`, `step_at(half_move)`, and `player_at(half_move)`;
  `NaturalState::next_step` is renamed `next_half_move`.
- **Residual, turn-independent resignation.** A conforming, canonically
  attested Request from a session player now always yields a verdict: the
  post-chain resolution is ordered draw acceptance (`agreement`) → abandonment
  timeout (`timeout`, the on-move player's clock) → residual `resignation`
  (decisive against the invoker, whatever the turn). There is no "premature"
  invocation anymore; `adjudicate` returns `None` only for an unattested
  Request or a non-player signer. `implicit::implicit_termination` is replaced
  by `implicit::draw_acceptance`.
- **Equivocation-only violations.** The step-ownership violation is
  structurally inexpressible under per-player steps and is removed.
  `commitment::commitment_violation` / `Violation` / `ViolationKind` become
  `commitment::equivocation` / `Equivocation` (single-content rule only,
  applicable to every slot including pending ones, anchored at the
  second-attested differing Ply).

### Unchanged

- Race resolution (canonical attestation, canonical ply) and its tiebreaks.
- Chain-replay terminations: an illegal or unparseable evaluated Ply rules
  `illegalmove`; rule-system endings and played-Ply timeouts carry their Ply's
  attestation as anchor.
- The candidate ranking by attestation time, an equivocation winning an exact
  tie.

## [0.1.0] — 2026-06-08

Initial release: abstract event model (`Ply`, `Attestation`,
`AdjudicationRequest`), race resolution, natural state, commitment violations,
implicit terminations, and the `adjudicate` orchestration over
`sashite-sanki-engine`.
