# RafikX design contract

## 0. Research log

- Existing-surface audit: the TUI already had strong information density, but its empty state was one sentence and exposed no execution model. The desktop shell used a familiar three-column dashboard, several raw purple/cyan gradients, and no explicit run state.
- Embedded shortlist: `warp` for warm terminal-native material, `voltagent` for agent topology, and `raycast` for keyboard-first command ergonomics. Selected `warp` because it treats the terminal as a calm working environment rather than a neon AI demo.
- Layer A: `redesign-skill` preserves the current Rust/ratatui and vanilla Tauri stack while replacing weak hierarchy and missing states.
- Interaction sources: beui.dev `loader` supplies the ordered signal/glyph mechanism and reduced-motion opacity fallback. `action-swap` supplies the send-to-cancel label swap through opacity, filter, and transform.
- Layout source: the app-shell contract names one scroll owner per region and requires `min-block-size: 0` on the transcript body.
- Direction: **Warm signal chamber**. RafikX opens as a quiet, warm near-black command surface. A single amber packet travels through `Context → Plan → Execute → Verify`; that path is the memorable object and the truthful explanation of the harness.
- Anti-references: generic purple/blue AI gradients, floating glass-card mosaics, decorative idle loops, fake terminal transcripts, and raster screenshots.

## 1. Product atmosphere

RafikX should feel like a terminal rebuilt around an observable agent runtime: calm enough for long sessions, exact enough for code work, and visibly alive only when state changes.

- Material: charred warm canvas, parchment text, earth-toned inset surfaces, thin mist borders.
- Color story: near-monochrome warm neutrals with one signal-amber ramp. Semantic green, ochre, and red appear only for outcomes.
- Signature moment: the run signal path boots once, then advances only from real lifecycle events.
- Density: cinematic empty state, compact working state, dense transcript only after the user begins.
- Copy: direct Korean or user language; operational labels stay short and English where already established.

## 2. Semantic tokens

All desktop colors must be CSS custom properties. TUI colors map to the same semantic roles in `palette.rs`.

### Dark palette

| Token | Value | Role |
|---|---:|---|
| `canvas` | `#11100e` | warm page/terminal background |
| `canvas-deep` | `#0b0b09` | input and recessed code surface |
| `surface` | `#181713` | fixed rails and composer |
| `surface-raised` | `#211f1a` | modal and selected surface |
| `text-primary` | `#faf9f6` | parchment foreground |
| `text-secondary` | `#c6c1b7` | readable supporting copy |
| `text-muted` | `#8d897f` | labels and metadata |
| `line` | `rgba(250, 249, 246, 0.14)` | standard containment |
| `line-strong` | `rgba(250, 249, 246, 0.28)` | focus and active containment |
| `signal-strong` | `#e2bd73` | active node and primary action |
| `signal` | `#b99455` | active metadata and code accent |
| `signal-dim` | `#655338` | visited or inactive signal |
| `success` | `#8fbd91` | succeeded state |
| `warning` | `#d3a15d` | waiting/limited state |
| `danger` | `#d77f73` | failure/cancellation state |

### Light palette

| Token | Value | Role |
|---|---:|---|
| `canvas` | `#f1eee6` | warm light canvas |
| `canvas-deep` | `#e7e1d6` | recessed light surface |
| `surface` | `#faf7ef` | fixed rails and composer |
| `surface-raised` | `#ffffff` | modal and selected surface |
| `text-primary` | `#24211b` | primary ink |
| `text-secondary` | `#4e493f` | supporting ink |
| `text-muted` | `#736d61` | labels and metadata |
| `line` | `rgba(36, 33, 27, 0.16)` | standard containment |
| `line-strong` | `rgba(36, 33, 27, 0.34)` | focus and active containment |
| `signal-strong` | `#79591f` | active node and primary action |
| `signal` | `#947033` | active metadata and code accent |
| `signal-dim` | `#cfc0a2` | visited or inactive signal |
| `success` | `#347044` | succeeded state |
| `warning` | `#8a5d18` | waiting/limited state |
| `danger` | `#9b4038` | failure/cancellation state |

The signal path alone may use `filter: drop-shadow(...)` to read as emitted light. General cards remain border-first and shadow-free.

## 3. Typography and spacing

### Typography

- Desktop display/body: system sans stack with Korean coverage: `ui-sans-serif`, `-apple-system`, `BlinkMacSystemFont`, `Apple SD Gothic Neo`, `Pretendard`, `Noto Sans KR`, `sans-serif`.
- Desktop code/data: `ui-monospace`, `SFMono-Regular`, `Menlo`, `Consolas`, `monospace`.
- TUI: host terminal font only.
- Display: regular 400, `clamp(2rem, 5vw, 4.75rem)`, line-height `0.96`, negative tracking.
- Section/title: medium 500, `1.25rem` to `1.5rem`.
- Body: regular 400, `0.875rem` to `1rem`, line-height `1.55`.
- Label: mono 500, `0.6875rem` to `0.75rem`, uppercase, `0.14em` tracking.
- Numeric metadata uses tabular figures.

### Spacing

- Base unit: `4px`.
- Named scale: `1=4`, `2=8`, `3=12`, `4=16`, `5=20`, `6=24`, `8=32`, `10=40`, `12=48`, `16=64`.
- Outer desktop gutters: `16px` narrow, `24px` tablet, `32px` wide.
- Radius: `4px` signal nodes, `8px` controls, `12px` panels, `16px` modals. Pills are reserved for compact status/action controls.
- Touch targets: minimum `44px` on pointer surfaces.

## 4. Layout and responsive rules

### Desktop shell

- Root is a `100dvb` grid with fixed navigation, `minmax(0, 1fr)` center, and optional inspector.
- The transcript is the only vertical scroll owner in the center region and must have `min-block-size: 0`.
- Navigation and inspector own independent scroll only when their content exceeds the viewport.
- At `≤1080px`, the inspector becomes an on-demand rail and the center keeps priority.
- At `≤760px`, navigation collapses into a top utility band; start content is one column.
- At `375px`, there is no horizontal page scroll; long paths use middle ellipsis or `overflow-wrap:anywhere`.

### TUI shell

- Header, composer, approval choices, and footer remain fixed.
- Transcript owns scroll. The start screen uses that same region and never creates a second scrollbar.
- At 96 columns and above, the workspace uses only the header width left after brand, harness, and lifecycle signals; Unicode display-width compaction preserves a visible `…/<last-directory>` tail instead of relying on terminal clipping.
- Below 72 columns, the signal path stacks as two rows and optional explanation copy elides before model/workspace.
- Below 18 rows, the start screen keeps brand, signal path, selected model, workspace, and input; supporting copy disappears.

## 5. Primitives and state requirements

### `AppShell`

States: wide, medium, narrow, modal-open. Fixed regions must remain fixed; transcript remains the only center scroll owner.

### `StartStage`

Content jobs: hook with the new runtime idea, explain the four-stage harness, prove configuration with real model/workspace, and convert through the existing composer.

States: booting, ready-unconfigured, ready-configured, and hidden-after-first-message. It must never show a fake transcript or fake run data.

### `RunSignal`

Fixed nodes: `Context`, `Plan`, `Execute`, `Verify`. Each node has inactive, current, visited, waiting, success, limited, failure, and cancelled styling. A moving packet exists only while booting or while a real run is progressing.

Lifecycle mapping:

| Lifecycle state | Visual stage |
|---|---|
| `queued` | Context |
| `planning` | Plan |
| `running`, `waiting_approval`, `delegating`, `cancel_requested` | Execute |
| `answering` | Verify |
| terminal outcome | all visited, final semantic state |

### `ComposerAction`

States: send, cancelling, disabled. Send changes to Cancel while a turn is active; label/icon changes use an opacity/filter swap, preserve button geometry, and update the accessible name.

### `LifecycleStatus`

States: booting, ready-unconfigured, ready-configured, planning, running, waiting-approval, delegating, answering, succeeded, limited, failed, cancelled. Copy must name the state; color is supplementary.

### Existing primitives

- Transcript message: user, assistant, system, tool, warning, final answer, run summary.
- Approval surface: preview, Yes, No, Always, expired/cancelled.
- Session row, harness row, model chip, graph node, settings navigation, text field, select, modal.
- Every interactive primitive requires default, hover, focus-visible, active, disabled, and busy states where applicable.

## 6. Motion and interaction

Named tokens:

| Token | Value | Use |
|---|---:|---|
| `motion-instant` | `90ms` | press feedback |
| `motion-fast` | `160ms` | opacity/color feedback |
| `motion-swap` | `220ms` | send/cancel blur swap |
| `motion-stage` | `360ms` | packet movement between nodes |
| `motion-boot` | `1200ms` | one-shot first-frame traversal |
| `ease-standard` | `cubic-bezier(.2,.8,.2,1)` | opacity/filter transitions |
| `ease-signal` | `cubic-bezier(.34,1.15,.64,1)` | interruptible spatial packet |

Rules:

- Animate only `transform`, `opacity`, and `filter`.
- Boot animation runs once and settles. Ready idle is static.
- Real lifecycle updates retarget the packet immediately; transitions never queue or block input.
- Waiting approval uses a slow opacity breath and a textual `approval` state, not spatial motion.
- Send-to-cancel follows the beui.dev action-swap mechanism: overlapping labels, blur/opacity crossfade, stable geometry.
- `prefers-reduced-motion: reduce` removes all transforms, makes state changes instantaneous, and permits one calm opacity change only.
- TUI reduced motion comes from `[ui].reduced_motion=true` or `RAFIKX_REDUCE_MOTION=1`; glyphs update only when the semantic state changes.

## 7. Accessibility and inclusive personas

### Personas

1. **Min, keyboard-first maintainer**: uses a 90-column terminal and never reaches for a pointer. Must start, cancel, approve, inspect state, and resume without focus loss.
2. **Jiyun, low-vision Korean developer**: uses 200% zoom/high text size and reads mixed CJK/code. Must distinguish every lifecycle state without relying on hue and encounter no clipped Korean labels.
3. **Alex, vestibular-sensitive incident responder**: enables reduced motion during a high-pressure failure. Must receive the same state information with no travelling packet, looping spatial effect, or delayed control.

Constraints:

- Semantic landmarks, labels, and live regions describe state changes.
- Focus-visible treatment uses `line-strong` plus a 2px outline/offset; focus is never removed.
- Color is never the sole carrier. Nodes include a glyph and label; outcomes include text.
- Contrast targets WCAG AA for body text and controls.
- The default TUI progress/thinking text is `#817e77` on `#11100e` (about `4.69:1`), above the AA body-text threshold while remaining subordinate to muted metadata and body copy.
- Keyboard order follows visual order. Escape cancels the current run only when no modal owns Escape.
- Dynamic lifecycle announcements are polite except approval and failure, which are assertive.
- CJK content wraps by display width; code/path tokens use `overflow-wrap:anywhere` on the desktop.
- Reduced-motion and light/dark preferences are honored from the first rendered frame.

## 8. Accepted constraints and debt policy

- Terminal raster imagery remains excluded because terminal image protocols are not portable.
- Desktop uses local/system fonts so startup and offline use do not depend on a font CDN.
- No animation library is added: CSS transitions and deterministic ratatui glyph frames cover the required mechanisms with zero bundle cost.
- Critical or major accessibility issues cannot be accepted as debt. Minor debt must name affected users, location, fix, owner, and user acknowledgement before release.
- Packaged application icons are outside this release.
