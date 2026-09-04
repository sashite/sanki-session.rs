# Spec -> code traceability

Mapping between this crate's modules and the sections of the Sanki session
kernel specification (Kernel — Sanki, Part II; kind `3425` Conclusion;
Canonical Timing; Statuses — Sanki §Verdict resolution):

| Module | Specification |
|---|---|
| `event` | the event model — Ply (kind `3423`), Attestation (kind `3410`), Conclusion (kind `3425`) |
| `race_resolution` | Canonical Timing §Meta-resolution (per-slot canonical Ply, canonical timing by mode) |
| `selection` | Move Encoding — Sanki §Slot candidates and selection (the two-window forgiving rule, cap `K`) |
| `natural_state` | kind `3425` §Natural state of events at the cutoff (the chain replay, the terminal / ongoing end) |
| `implicit` | Statuses — Sanki §Implicit draw by agreement |
| `verdict` | Statuses — Sanki §Verdict resolution; kind `3425` §Semantic constraints item 8 (`conforms`) and §Idempotence and finality (`select_conclusion`) |
| `session` | kind `3422` — the session-constant parameters (players, seats, time control, initial position, t₀, the optional timestamper) |
