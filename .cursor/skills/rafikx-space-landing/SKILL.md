---
name: rafikx-space-landing
description: Builds and updates the RafikX space-themed landing page and operational flow diagrams. Use when editing RAFIKX_WORKFLOW.html, landing HTML, 우주 스타일, 랜딩페이지, or workflow diagrams.
---

# RafikX space landing

## Visual system

- Background: near-black space (`#03060e`), not flat gray.
- Accents: starlight gold `#e8d5a3`, nebula violet `#6b5cff`, ion cyan `#5ee7ff`.
- Type: display serif for titles, clean sans for Korean body.
- Motion: slow starfield, rare shooting stars, path dash along the **ops** route. No rainbow gradients, no clip-art planets.

## Page job

This is a **landing + mission-control** page, not a documentation dump.

1. Hero: what RafikX is, one command (`rafikx`).
2. Operational journey: the real daily path a user takes.
3. Interactive diagram: one signal moving through live stations.
4. Safety and accounts as short cards, not extra mermaid clones.

## Operational flow (must stay true)

Install → first-run short wizard (provider → key or login → default model) → connect account(s) → `rafikx` TUI / `ask`/`agent`/`chat`/`telegram` → classify (simple/medium/advanced/dev) → bind profile → auto/manual model from **registered only** → pick account (ready first, 429 switches) → pack context → model call → tools + jail + approval → verify if coder → save run → usage footer. Inspector never auto-edits. Telegram non-allowlist is silent. Remote `--yes` forbidden.

## File

Edit `RAFIKX_WORKFLOW.html` as a single self-contained file (CDN fonts OK). Keep Korean user-facing copy.
