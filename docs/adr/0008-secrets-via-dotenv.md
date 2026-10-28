# ADR-0008: Secrets from environment / `.env`, held in a redacting zeroizing type

- Status: Accepted · Date: 2026-10-28 · Requested by project owner

## Decision
- Secrets are read from the process environment, populated from a git-ignored `.env`
  (`dd_secrets::load_dotenv`), loaded once in `main` before threads start. Real env vars win.
- `.env.example` (committed) lists variable *names* only. `.gitignore` excludes `.env`, `.env.*`, keys.
- Values live in `dd_secrets::Secret`: `Debug`/`Display` print `<redacted>`, memory zeroed on drop.
  `expose()` is the only way to read; used only to build auth messages.
- Missing required secrets fail at startup, naming the variable.

## Consequences
Phase 2 (Sprint 7, NFR-3): AES-256-GCM encrypted vault + pre-commit secret scanning behind the same API.
