# DeltaDrift Knowledge Base

| Doc | Purpose |
|---|---|
| [ARCHITECTURE.md](ARCHITECTURE.md) | How the system works *today*: stages, crates, data flow, invariants |
| [SPRINT_LOG.md](SPRINT_LOG.md) | Chronological record: what shipped, measurements, bugs + root causes |
| [adr/](adr/) | Architecture Decision Records — why things are the way they are |
| [GLOSSARY.md](GLOSSARY.md) | Terms used across code and docs |
| [../CLAUDE.md](../CLAUDE.md) | Working rules incl. knowledge-base maintenance policy |

## Requirement traceability

Status: ✅ done · 🟡 partial · ⬜ not started

| Req | Summary | Status | Where |
|---|---|---|---|
| FR-1 | Live market WebSockets + FRED | ⬜ | Sprint 2 (`dd-feed`) |
| FR-2 | PIT features, surprise z-scores, exp. decay | 🟡 | `dd-features` (`Ewma`, `DecayedSurprise`); PIT store Sprint 3 |
| FR-3 | Self-improving core (PPO/SAC, GP, bandits, AutoML) | ⬜ | Sprint 5; `Strategy` trait is the plug point |
| FR-4 | Hard risk: stop, daily DD, fractional Kelly | ✅ | `dd-risk` (+ gross cap, rate limit, cooldown) |
| FR-5 | Multi-broker routing, PAPER/LIVE switch | 🟡 | `dd-exec::Broker`, `PaperBroker`; live adapters Sprint 4 |
| FR-6 | Demo mode, $100-normalised reporting | 🟡 | `--demo`, report; privacy/anonymised dashboard Sprint 7 |
| NFR-1 | < 50 ms tick→order (target sub-ms) | ✅ | p50 0.6 µs, p99.9 13 µs (see SPRINT_LOG) |
| NFR-2 | < 8 GB RAM | ✅ | Rings 8 MiB total; no growth after start |
| NFR-3 | AES-256-GCM keys, no hardcoded secrets | ⬜ | Sprint 7 (no secrets exist yet) |
| NFR-4 | 0% look-ahead | 🟡 | Estimator contract + tests; PIT store Sprint 3 |
| NFR-5 | Auto-restart | ⬜ | Sprint 8 (systemd/Docker) |
