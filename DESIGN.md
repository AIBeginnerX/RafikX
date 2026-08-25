# RafikX TUI Design Contract

## Surface

RafikX uses a dense terminal workspace with three persistent zones:

1. Header: product, version, harness mode, engine, workspace.
2. Transcript: user, agent, tool, warning, and final-result content.
3. Status/footer: selected model, execution state, mode, context, cache, and Todo progress.

## Language

- Header and footer operational labels are English.
- Conversation content may follow the user's language.
- The selected model must be visible from the first rendered frame.

## Transcript

- Role labels use a seven-column prefix without a vertical pipe.
- Model work is muted and italic only while work is in progress.
- A completed turn clears transient work rows but never discards the model's final answer.
- The final surface renders two distinct layers in this order:
  1. `Answer`: the complete assistant Markdown, including tables, code, charts, links, and direct answers.
  2. `Run summary`: compact execution metadata, verification, changed files, model, context, cache, memory, and next actions.
- Paragraph spacing is preserved as a single blank row. Repeated blank rows collapse to one.
- Thinking and transient work use muted italic text; final answer text uses the highest body contrast.

## OMO Native adaptation

- Preserve the finalized assistant message as the primary artifact; do not regenerate or replace it with telemetry.
- Use semantic terminal roles rather than one accent for everything: accent, text, thinking, success, warning, error, border, panel.
- Working state uses one accent spinner plus muted status text. Large decorative progress bars are excluded.
- Footer priority follows OMO's width ladder: model and context are anchors; cache, memory, Todo, and secondary state may elide on narrow terminals.
- Context display includes the automatic-compaction state. A completed turn reports whether compaction ran and how many external memory sources were injected.
- Terminal fonts remain host-controlled. Hierarchy is expressed with bold final headings, normal body text, italic thinking text, and tabular/monospace terminal figures.

## Final answer anatomy

1. Full assistant answer rendered with the existing Markdown/table/code pipeline.
2. One muted divider row.
3. A compact `Run summary` block containing:
   - provider/model;
   - elapsed time and model iterations;
   - Todo completion and changed files;
   - context usage, cache hit rate, automatic compaction, and external memory sources;
   - tool errors and verified next actions.
4. Optional numbered actions after the summary.

The answer remains useful if the summary is removed. The summary must never be the only user-visible result.

## Approval

- Tool approval is a modal rendered after every other layer.
- Preview content may scroll or clip, but `[Yes]`, `[No]`, and `[Always]` remain visible at the modal bottom.
- Buttons support direct mouse selection and Y/N/A keyboard shortcuts.
- Mouse capture is enabled only while approval is active.

## Responsive behavior

- Narrow terminals use the full terminal for modal surfaces.
- Tables, code, and transcript lines wrap to terminal display width.
- Final answers own transcript scroll; Header, input, status, and footer remain fixed.
- On narrow widths, execution metadata elides before model and context anchors.
- Todo rows remain above the input and yield space to the transcript only when terminal height requires it.

## Accessibility and accepted constraints

- Color never carries approval meaning alone; every action has a text label.
- Keyboard operation remains complete without mouse input.
- Raster image rendering is intentionally excluded because terminal image protocols are not portable.
