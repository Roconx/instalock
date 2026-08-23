# InstaLock

Windows desktop app that automates the League of Legends champion select: auto
accept, auto pick, auto ban, plus an in-game overlay of enemy summoner-spell
cooldowns that can be synced with teammates.

UI language is **Catalan**. Code, comments and commit messages are in English.

## Stack

Tauri v2 + Rust backend, **vanilla HTML/CSS/JS frontend with no build step**.
There is no React, no bundler, no TypeScript, no CSS framework — `frontendDist`
points straight at `src/`, so what you write there is what ships. Don't
introduce a build step; if something needs a library, weigh it against that.

The frontend talks to Rust through the global bridge (`withGlobalTauri: true`):

```js
const { invoke } = window.__TAURI__.core;
const { listen } = window.__TAURI__.event;
const { getCurrentWindow } = window.__TAURI__.window;
```

## Layout

Cargo workspace with three members:

| Path | What |
|---|---|
| `src-tauri/` | The app. Rust backend + Tauri config. |
| `instalock-shared/` | `SyncMessage` enum shared by app and relay server. |
| `instalock-server/` | Standalone WebSocket relay for syncing spell timers. |
| `src/` | The whole frontend, shipped as-is. |

### Backend modules (`src-tauri/src/`)

| Module | Responsibility |
|---|---|
| `main.rs` | Tauri commands, builder, tray, window events, the champ-select state machine |
| `lcu.rs` | Polls the League Client API; emits `LcuEvent` (Connected, ReadyCheck, ChampSelect…) |
| `actions.rs` | The actual LCU calls: accept, hover, pick, ban, bravery |
| `champions.rs` | Champion table from Community Dragon; name↔id resolution |
| `settings.rs` | `Settings` struct + JSON persistence in `%APPDATA%/instalock/` |
| `overlay.rs` | Enemy data and rune polling for the overlay window |
| `sync.rs` | WebSocket client for the timer relay |
| `focus.rs` | Restores window focus after an action steals it |
| `hotkey.rs` | Shift-key polling that toggles overlay click-through |
| `launcher.rs` | Finds and starts the League client |

### Frontend (`src/`)

Two independent documents, not a SPA:

- `index.html` + `main.js` + `style.css` — the main window
- `overlay.html` + `overlay.js` + `overlay.css` — the transparent in-game HUD
- `tokens.css` — **the single source of colour, type and radius**, imported by both

## Windows

Both are created without decorations.

- **main** — 440×580, fixed size, opaque, custom titlebar in HTML. Dragging uses
  `data-tauri-drag-region`; minimise/close/pin call `getCurrentWindow()`.
  Closing hides to the tray when `minimizeToTray` is on.

  The rounded corner and drop shadow come from DWM
  (`apply_native_rounding()` in `main.rs` sets `DWMWA_WINDOW_CORNER_PREFERENCE`,
  plus `"shadow": true`). This was previously done in CSS over a transparent
  window and looked wrong: the webview antialiases against an empty surface and
  the outward `box-shadow` had nowhere to go but into the corner it had just cut
  out, leaving a grey smear around a soft curve. **Don't put `border-radius`,
  `border` or `box-shadow` back on `.app-root`.**
- **overlay** — built in `main.rs`, always-on-top, click-through unless Shift is
  held, `skip_taskbar`, `WS_EX_TOOLWINDOW`. Created on ChampSelect/InProgress,
  destroyed on EndOfGame/Lobby.

Any new window API called from JS needs its permission added to
`src-tauri/capabilities/default.json` — it fails silently at runtime otherwise.

## UI conventions

See the **`instalock-ui` skill** before changing anything under `src/`. In short:

- Never write a colour literal outside `tokens.css`; use `var(--…)`.
- Every surface that reads as a panel gets `background: var(--bg-panel)`. Its
  alpha is user-tunable (Aparença → Contrast dels panells), so a panel must
  never hardcode its own background.
- Both themes must work — check `[data-theme="light"]`, not just dark.

## Settings

`Settings` in `settings.rs` is serialised `camelCase` and mirrored by
`collectSettings()` / `applySettings()` in `main.js`. **All three must agree.**

Every new field needs `#[serde(default …)]` so an existing
`%APPDATA%/instalock/settings.json` keeps loading — there is no migration step.

Writes are debounced 300 ms in the frontend. Anything that closes or hides the
window must call `flushSettings()` first: `hide()` never fires `beforeunload`.

## Build and run

```bash
npm run tauri dev
```

Releasing (version bump, build, MSI to Downloads) is the **`release` skill** —
use it rather than driving `tauri build` by hand.

App icons are generated from `app-icon.svg` at the repo root:

```bash
npx tauri icon app-icon.svg
```

That also emits `android/` and `ios/` folders; delete them, this app is
Windows-only.

## Gotchas

- Champion data loads from a CDN with retry. Until it arrives, `resolve_id`
  returns `None` and every pick/ban silently no-ops — that's why the picker
  shows a loading state instead of flagging names as invalid.
- The ready-check event repeats about once a second for the whole window;
  `accept_in_flight` is what stops one accept per event.
- `body[data-queue-mode]` drives mode-specific UI: `ARAM` hides the pick/ban
  cards, `CHERRY` (Arena) reveals the Bravery checkbox.
- Log entries are classified by matching the **Catalan** message text in
  `logSeverity()`. Renaming a log string can change its colour.
- A remote build script has previously copied files one level too deep, creating
  `src/src/` and `src-tauri/src-tauri/`. They're gitignored; if they come back,
  the copy step is at fault.
