# Visual QA dependencies

These packages are test-only and never ship with RafikX. Playwright captures the desktop UI in real Chromium, `node-pty` runs the terminal UI through a real pseudo-terminal, and xterm.js renders the resulting ANSI stream exactly as a terminal surface.

Run `npm install` in this directory once, then use `npm run desktop` or `npm run tui` here. The desktop runner starts an isolated local asset server; the TUI runner builds the current all-features binary before opening a real PTY.
