# Changelog

All notable changes to this crate are documented in this file. The format is
based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/), and this
crate adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

### Changed

- **Documentation only — the suite's kind numbers moved out of NIP-90's reserved range.** Every reference in the doc comments, the README and the conformance notes now reads `3418`–`3441` instead of `6418`–`6441`.

  This crate holds no kind constant: it reasons about sessions, plies and verdicts
  as data, and names their kinds only to say what it is reasoning about. So
  **nothing here behaves differently**, and a consumer on either numbering links
  against it unchanged — which is also why this is not worth a release of its own.
  It rides along with the next substantive one; until then a reader of the
  published docs sees the old numbers.

  [NIP-90](https://github.com/nostr-protocol/nips/blob/master/90.md) reserves
  `5000-7000` in one block and pairs a job request with its result at a fixed
  offset of a thousand, so a Game Session at `6422` *was* the result of job
  request `5422` to anything that knows NIP-90 (`web-specs.md` README §Kind
  numbers). 128 tests pass unchanged.

## [0.12.0] — 2026-08-01

`sashite-sanki-engine` bumped to 0.9, which carries the whole notation stack
with it. **No adjudication changes**: every verdict, every selection and every
conformance vector is unchanged, and the two corpora (36 scenarios, selection)
pass byte for byte as before.

### Changed

- **`sashite-sanki-engine` 0.8 → 0.9.** The engine moved onto `sashite-feen`
  0.2 and `sashite-qi` 0.2, and hardened `Position::new`, which now rejects any
  board that is not 8×8 with a new `PositionError::NotSankiBoard` variant.
  `sashite-sin`, `sashite-pin` and `sashite-epin` reach 1.1 in the lockfile.

  Nothing in this crate constructs a `Position` from a `Qi` — positions arrive
  through `Position::parse`, which already rejected foreign geometries — so the
  engine's stricter constructor changes no behaviour here. What does reach this
  crate is `Position::to_feen` now resting on a guaranteed invariant rather than
  an unchecked assumption, and FEEN's encoder no longer being able to emit a
  string its own parser would reject.

  This is a minor bump rather than a patch because `Session::initial_position`
  returns an `&sashite_sanki_engine::Position`, so the engine's own breaking
  change is visible in this crate's public API: dependents must move to engine
  0.9 as well.

## [0.11.0] — 2026-07-31

`sashite-sanki-engine` bumped to 0.8, and a reliability review of the whole
crate around it. The bump is not cosmetic: it **changes verdicts**, and the
old ones took the game away from the player who had won it.

### Changed

- **`sashite-sanki-engine` bumped to 0.8** (0.8.2 resolved). Engine 0.8.0 fixed
  a checkmate misreported as `Ongoing` when a cross-variant capture leaves an
  inert, opposite-cased token in the capturer's hand tray: `has_full_legal_move`
  was probing the union of both hands, so the mated side appeared to hold a
  droppable interposition it does not own. `natural_state` applies every Ply
  through `kernel::step`, so the misclassification reached the verdict directly.

  Measured on a real chess-versus-ōgi session, same events, no arbiter code
  change: under engine 0.7 the replay concluded `Ongoing` on a mated board, and
  the invocation fell through to **residual resignation — against the player who
  had just delivered checkmate**. Under 0.8 it concludes `Terminal(checkmate)`
  and rules `FirstWins`. On the post-mate position engine 0.7 answers
  `legal_moves = 0` *and* `status = Ongoing`, which is self-contradictory on its
  face. The whole 36-vector corpus and a nine-pairing verdict matrix were run
  against both engines and diffed: only this class of position changes answer.

  Worth knowing for anyone auditing older rulings: the bug only fires on a
  **distant, blockable** check. A contact-check mate was classified correctly
  even under 0.7, which is why a corpus could have held a cross-variant mate and
  still missed this.

### Added

- **Cross-variant conformance coverage, which did not exist.** Before this
  release every FEEN in the crate — all of `src/`, all of `tests/`, and the
  whole shared corpus — was chess-versus-chess (19) or ōgi-versus-ōgi (1).
  **No cross-variant session, and no xiongqi at all**, in the adjudication
  authority for a game family whose reason to exist is cross-variant play. That
  is precisely why the engine bug above survived a green suite: the corpus could
  not express the pattern that triggers it.
  - `tests/cross_variant.rs` — every pairing adjudicated end-to-end through
    `adjudicate`, plus the inert-tray mate pinned as a regression, castling in
    each variant inside a cross-variant session, a cross-variant capture feeding
    an ōgi hand, cross-variant uchifuzume skipped rather than sanctioned, and
    xiongqi sideways en passant.
  - `tests/conformance/scenarios.json` — **version 7 → 8**, 25 → 36 vectors,
    schema and byte conventions unchanged. All nine style pairs now appear. The
    new vectors bite: run against engine 0.7 the corpus fails with
    `TERMINATION MISMATCH scenario.cross-variant-checkmate-with-an-inert-tray:
    None vs Some("checkmate")`.
  - `is_legal_matches_the_kernel_step_oracle` widened from 10 chess/ōgi cases to
    69 across all nine pairings (18 of them illegal, so it is not vacuous), with
    an `#[ignore]`d exhaustive sweep behind it. This pins the `validate` versus
    `kernel::step` seam: a divergence there would let a candidate be selected and
    then bounce off `StepResult::Illegal` into the defensive seam, silently
    turning a played game into an unfinished one. ~60 M probe pairs over 10 534
    positions found no divergence.

### Fixed

- **The abandonment gate pardoned an unbounded abandonment.**
  `Timestamp::duration_since` answers `None` for two unrelated reasons, and the
  gate conflated them with a single `unwrap_or(Duration::ZERO)`: an *inverted*
  span (cutoff before the anchor), correctly clamped to zero, and a *forward*
  span too wide for `i64` subtraction — which was therefore charged zero seconds
  and flagged nobody. The engine's `kernel::step` draws exactly this distinction
  for a played Ply and saturates the second case, with a comment warning that
  charging zero "would let an astronomically late ply pass free"; the arbiter did
  the thing that comment warns against. Measured: with t₀ = 0 the span fits and
  the verdict is `Timeout`; one second earlier, at t₀ = −1, the identical
  abandonment overflowed and became `Resignation` against the *other* player.
  Within one session the asymmetry was starker still — a player who *plays* at
  the far end of the range was flagged by the kernel, while a player who did
  nothing over the same span was ruled within budget. Now saturates, so the two
  layers agree.

### Notes

Three findings that are **not** code changes, recorded so they are decided
rather than discovered:

- **The identical-content dedup key ignores the `draw` flag**, and that is
  observable and asymmetric. In the *informed* window it is immaterial — the
  earliest legal candidate takes the slot whatever its content. In the
  *anterior* window the latest legal premove wins, so the key decides: two
  premoves differing only in the flag collapse to the earlier, flagless one and
  the offer is destroyed, where the same pair with differing contents keeps both
  and the acceptance rules `agreement`. Whether an offer attached to a
  re-submitted move ought to survive is a normative question about kind 6423
  §Race resolution, and this crate is one of two implementations gated by the
  same corpus — so the behaviour is pinned by
  `the_draw_flag_is_outside_the_identical_content_dedup_key` rather than changed
  here.
- **A self-timed backdated cutoff can resurrect a draw offer declined by play.**
  Self-timed is the Sashité default and the cutoff is then the Request's own
  `created_at`, chosen by its signer. Offer at 100, declined by play at 200,
  play continues at 300: invoking with `created_at = 400` rules `resignation`,
  invoking with `150` rules `agreement` — and `select_request`, preferring the
  earliest, picks the backdated one. Cutoff manipulation is self-harming
  everywhere else (mate, stalemate, timeout-escape and terminal-erasure all
  convert a loss into a loss); draw acceptance is the sole exception. Both halves
  are individually per-spec; the composition needs a decider's call.
- **Nothing validates that a founding position has `first` to move.** With a
  `second`-to-move initial FEEN, slot 1 can never be filled, the chain stays
  empty however much is played, and the abandonment gate charges `second`, who
  is thereby guaranteed to flag. `SessionParams` is documented as assembled
  after cross-event validation, so this is an unenforced precondition rather than
  a defect; enforcing it would need a fallible `SessionParams::new`.

Also closed: two coverage holes found by mutation testing, both of which
survived 100% of the previous suite — the selection boundary's exactness (a
candidate timed *exactly* at `T` is informed; flipping `<` to `<=` passed every
test and every corpus vector) and the informed-window id tiebreak (stable sort
plus ascending-id input order made deleting it a no-op). Both are
cross-implementation risks, not just local ones.

## [0.10.0] — 2026-07-27

No arbiter code change; the rule behaviour changes through the engine.

### Changed

- **Engine bumped to `sashite-sanki-engine` 0.7** — castling extended to ōgi
  and xiongqi (deciders' ruling, 2026-07-27; the chess and ōgi King, the
  xiongqi General `G^`, FIDE mechanics; canonical initial FEENs gain the `-R`
  corner markers). The arbiter's legality, replay, and terminal detection —
  all reached through `kernel::step` — now accept and canonicalize the new
  castlings; see the engine's 0.7.0 changelog for the full rule statement.
- **Conformance: scenarios corpus re-synced to v7.** Two founding positions
  whose active royal stands in check
  (`scenario.insufficiency-closes-the-chain`,
  `scenario.deadposition-chess-kb-closes-the-chain`) now carry the canonical
  `-K^` marker, matching the legality corpus's v2 check-marker
  canonicalization (engine 0.7.0). Chains, expected values, and schema are
  unchanged — the marker is decorative for replay. Byte-identical to the
  editorial source (web-specs.md).

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
