# DeltaDrift — working rules

Source of truth for requirements: `../Project DeltaDrift Master Documentation.docx`.
Knowledge base: `docs/` (start at `docs/README.md`).

## Knowledge base is mandatory, not optional
Every change set updates the knowledge base **in the same change**:
- `docs/ARCHITECTURE.md` — must describe the code as it is *now*. Edit in place.
- `docs/adr/NNNN-title.md` — one per significant/irreversible decision. Never
  rewrite an accepted ADR; supersede it with a new one and mark the old one
  `Superseded by NNNN`.
- `docs/SPRINT_LOG.md` — append: what shipped, measured numbers (with
  hardware + command used), bugs found and root causes, open items.
- `docs/GLOSSARY.md` — add any new domain/technical term.
- Requirement traceability table in `docs/README.md` — keep FR/NFR status current.

## Live-money policy
End goal: live trading as an income stream. LIVE mode stays refused in code until gate G0 in
`docs/LIVE_READINESS.md` passes; every gate needs recorded evidence. Never weaken a risk guard
to make a strategy look better. Keep the live-blocker table current.

## Engineering invariants
- Hot path (feed → strategy → risk → broker) never allocates, locks, or does
  syscalls other than vDSO clock reads. Allocate at construction.
- All inter-stage messages are `Copy`, `repr(C, align(64))`, exactly 64 B
  (compile-time asserted in `dd-core/src/types.rs`).
- `dd-risk` must never depend on `dd-strategy`. Strategies emit views
  (`mu`, `sigma2`); only risk sizes and creates orders.
- `unsafe` lives only in `dd-core` (ring buffer) and `dd-net` (receive buffer,
  io_uring); every block has a `// SAFETY:` comment (enforced by clippy).
  Other crates `#![forbid(unsafe_code)]`.
- Secrets: only via environment / git-ignored `.env`, read with `dd_secrets`,
  held as `Secret`. Never commit, log, or `Debug`-print a secret. Add new names
  to `.env.example` (names only).
- Pinning: never place two busy-spinning stages on SMT siblings; prefer auto core planning.
- Feature estimators are point-in-time: output depends only on strictly prior data.
- Gate before declaring done: `cargo test --release --workspace` and
  `cargo clippy --workspace --all-targets --release` with zero warnings.
