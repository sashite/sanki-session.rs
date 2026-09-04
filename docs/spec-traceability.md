# Spec -> code traceability

Mapping between this crate's modules and the sections of the Sanki session
kernel specification (Kernel — Sanki, Part II; kind `3425` Conclusion;
Canonical Timing; Statuses — Sanki §Verdict resolution; Move Encoding — Sanki
§Slot candidates and selection):

| Module | Specification |
|---|---|
| `event` | the event model — Ply (kind `3423`), Attestation (kind `3410`), Conclusion (kind `3425`: signer = invoker, canonical timing = cutoff, `claim: Verdict`); the self-timed acceptance precondition (Canonical Timing §The pending state) and the event-id uniqueness precondition |
| `timing` | Canonical Timing §Timing modes and mode selection (`canonical_timing`: the designated timestamper's attestation, or the event's own `created_at` when self-timed) and §Meta-resolution (`canonical_attestation`: smallest `created_at`, smallest id) |
| `selection` | Move Encoding — Sanki §Slot candidates and selection (the two-window forgiving rule, `Candidate::is_anterior` — strictly before the boundary — and `select_candidate`) and §Bounding a slot's candidates (the cap `K`, `NonZeroUsize`, `CANDIDATE_CAP` = the reference document's 8; ≤ 2K lazy legality probes) |
| `natural_state` | kind `3425` §Natural state of events at the cutoff; Kernel — Sanki §II.3–II.5 (the chain replay: candidates timed in `[t₀, cutoff]`, the boundary `T` = the state's clock anchor, never rewound; the identical-candidate collapse — same content, same `draw` flag, per window; `ChainEnd::Terminal` / `Ongoing` / `Inconsistent`) |
| `implicit` | Statuses — Sanki §Implicit draw by agreement (`accepts_standing_offer`: the chain's tail offers, the invoker is the offeree) |
| `verdict` | Kernel — Sanki §II.1 (`verdict_at`, the invocation: invoker + cutoff ≥ t₀) and §II.6 (abandonment charged from the chain's anchor); Statuses — Sanki §Verdict resolution (terminal → agreement → abandonment timeout → residual resignation; `Verdict`, coherent by the status/result-kind mapping); kind `3425` §Semantic constraints items 2, 3, 8, 9 (`cutoff_of` → `NoVerdict`, `expected_verdict`, `check` / `conforms`), §Until the Conclusion has canonical timing (`NoVerdict::Pending`, the one transient reason) and §Idempotence and finality (`select_conclusion`: the earliest conforming Conclusion, smallest id) |
| `session` | kind `3422` — the session-constant parameters (`Seats`: two distinct players; time control; the initial position, `first` to move; t₀ per §Canonical session start; the optional timestamper — attested vs self-timed mode) and the `rules` term's one session parameter (`candidate_cap`); the play-order model of kind `3423` §Step semantics and play order (`side_at`, `step_at`, `player_at`); the `result`-tag mapping of kind `3425` (`outcome_from_scores`) |
