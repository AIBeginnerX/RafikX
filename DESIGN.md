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
- A completed turn clears transient work rows and renders one high-contrast comprehensive result.

## Approval

- Tool approval is a modal rendered after every other layer.
- Preview content may scroll or clip, but `[Yes]`, `[No]`, and `[Always]` remain visible at the modal bottom.
- Buttons support direct mouse selection and Y/N/A keyboard shortcuts.
- Mouse capture is enabled only while approval is active.

## Responsive behavior

- Narrow terminals use the full terminal for modal surfaces.
- Tables, code, and transcript lines wrap to terminal display width.
- Todo rows remain above the input and yield space to the transcript only when terminal height requires it.

## Accessibility and accepted constraints

- Color never carries approval meaning alone; every action has a text label.
- Keyboard operation remains complete without mouse input.
- Raster image rendering is intentionally excluded because terminal image protocols are not portable.
