# AGENTS.md

- MUST read `product.md` before any behavior change.
- MUST keep implementation consistent with `product.md`.
- MUST update `product.md` in the same change when an explicitly requested behavior conflicts with current documentation.
- MUST bump the `product.md` version when documented behavior changes.
- MUST NOT use `product.md` as a changelog, decision log, roadmap, ADR, or implementation history. It documents only current product behavior, features, rules, and journeys.
- MUST write self-explanatory code.
- MUST NOT add code comments.
- MUST preserve established architecture and conventions unless explicitly instructed otherwise.
- MUST use Conventional Commits for every commit.
- MUST keep each commit logically coherent.
- MUST run relevant tests before considering a change complete.
- MUST treat the current explicit user request as authoritative when it conflicts with `product.md`, then update `product.md` accordingly.