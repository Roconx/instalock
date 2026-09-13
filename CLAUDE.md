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
| `lcu.rs` | Polls the League Client API; emits `LcuEvent` (Connected, ReadyCheck, ChampSelect, Lobby, Search, PickableChampions…) |
| `lol_state.rs` | **Typed snapshot of the client** — lobby, champ select, availability, grid. Everything downstream reads this, not raw JSON |
| `selection.rs` | **Pure**: which champion to pick or ban, given a list and a snapshot. Where the tests are |
| `actions.rs` | The actual LCU calls: accept, hover, pick, ban, bravery |
| `champions.rs` | Champion table from Community Dragon; name↔id resolution |
| `queue.rs` | **Pure**: whether to start matchmaking, and the Catalan reason when not. Tested |
| `queues.rs` | The client's own queue table: queue id → mode, map and display name |
| `summoners.rs` | puuid → Riot ID, cached. The lobby stopped carrying names |
| `settings.rs` | `Settings` struct, per-mode champion lists, JSON persistence in `%APPDATA%/instalock/` |
| `http.rs` | The two HTTP clients. Certificate verification is off for Riot's local APIs and **only** for those |
| `overlay.rs` | Enemy data and rune polling for the overlay window |
| `sync.rs` | WebSocket client for the timer relay, with reconnection |
| `focus.rs` | Restores window focus after an action steals it |
| `hotkey.rs` | Shift-key polling that toggles overlay click-through |
| `launcher.rs` | Finds and starts the League client |

### Pick and ban selection

Availability is decided **before** the LCU call, never by trying and handling
the failure. `selection::choose` walks the list and takes the first champion
that passes `pickable-champion-ids` / `bannable-champion-ids` plus the live grid
(`isBanned`, `pickedByOtherOrBanned`, `pickIntented`). It is recomputed on every
session event, so a champion banned mid-countdown is dropped and the next one
takes its place. No source documents what the client returns for an unavailable
champion, so nothing matches on the error body.

Champion lists are `ModeLists` — a map of LCU `gameMode` → `RoleLists`, plus the
literal key `*` for entries that apply everywhere. **There is no UI for the mode
keys**: the app edits whichever mode you are in, so an Arena pick never turns up
in SoloQ. Two rules the frontend must mirror exactly, or the list on screen is
not the list that runs:

- `effective_list` = the `*` bucket, then the current mode's bucket.
- `RoleLists::for_role` = the role's own entries, then `default` **appended**
  beneath them (not replaced by them).

`src/main.js` reimplements both in `displayedEntries` / `entriesForRole`. The
Rust side is covered by `settings::list_rules`.

### Auto queue

`queue::evaluate(snapshot, settings) -> Gate` is the single gate; the three
triggers the UI offers only change one check inside it. It is re-run on every
Lobby, Search and GameflowPhase event and after `update_settings`, and it logs a
Catalan line **only when the verdict changes** — the client sends two Update
events per lobby change. The verdict itself rides along in `client-state`.

Order matters: specific reasons (not leader, dodge penalty, queue restriction,
waiting for members) are checked before `canStartActivity`, which is the
client's own catch-all and would otherwise swallow every explanation.

A pending timer is retired by bumping `auto_queue_generation`; the timer
re-checks the gate against fresh state after it sleeps, so a lobby that changed
during the delay does not get queued on a stale verdict.

⚠️ "Tothom ready" is only a real per-person ready in **Arena**. Elsewhere the
client sets `ready` for everyone who is merely queueable and clears it when the
search starts. The trigger hint in Ajustos says so; don't quietly drop that.

### Info panel (`client-state`)

`emit_state` in `main.rs` builds the whole Partida tab in one payload: phase,
queue gate, lobby members, both champ-select teams, bans, and `last_decision` —
which champion the lists chose and, named one by one, what they stepped over and
why. That last part is the only thing that makes pick/ban debuggable from
outside.

Throttled to `STATE_EMIT_INTERVAL` (400 ms) because champ select re-emits on
every hover and every timer tick; `force` skips the throttle for rare events.

⚠️ `summonerId` routinely exceeds 2^53 — it is stringified before it crosses
into JavaScript. And the lobby payload carries `multiUserChatPassword`,
`mucJwtDto` and `bustedLeaverAccessToken`: the typed structs in `lol_state.rs`
deliberately do not declare them, which is what keeps them out of logs. Never
log the raw lobby payload.


### Frontend (`src/`)

Two independent documents, not a SPA:

- `index.html` + `main.js` + `style.css` — the main window
- `overlay.html` + `overlay.js` + `overlay.css` — the transparent in-game HUD
- `tokens.css` — **the single source of colour, type and radius**, imported by both

The main window has five tabs: Principal, Partida, Ajustos, Aparença, Registre.
Partida is read-only except for its two queue buttons, and is fed entirely by
the `client-state` event.

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
  `dragDropEnabled` is `false` in `tauri.conf.json`: Tauri's own file-drop
  handler swallows HTML5 drag events, which is what the champion lists use to
  reorder. The app has no file-drop feature, so nothing is lost.
- **overlay** — built in `main.rs`, always-on-top, click-through unless Shift is
  held, `skip_taskbar`, `WS_EX_TOOLWINDOW`. Created on **InProgress** — not on
  champ select, whatever the old note here said — and destroyed on
  EndOfGame/Lobby. **Off by default** on a fresh install: nothing should put a
  window over the game unasked.

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
`%APPDATA%/instalock/settings.json` keeps loading. The one migration that does
exist is `migrate()` in `settings.rs`, which seeds the lists from the old
`pickChampion` / `banChampion` scalars; the frontend no longer sends those, so
the next save clears them and it stops firing.

The backend also writes settings on its own — spending one-shot list entries
when a game starts, and recording a recently used champion. Both emit
`settings-changed`, which the frontend applies; without that the UI drifts from
disk until a restart.

Writes are debounced 300 ms in the frontend. Anything that closes or hides the
window must call `flushSettings()` first: `hide()` never fires `beforeunload`.

## Build and run

```bash
npm run tauri dev
```

Tests: `cargo test --workspace`. The valuable ones are `selection.rs` (which
champion gets chosen), `settings::list_rules` (how the lists stack) and
`lol_state.rs` (parsing LCU payloads that change shape between patches).

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
- `body[data-queue-mode]` drives mode-specific UI: `ARAM` hides both the pick
  and ban cards, and `#modeNote` says why - a main tab with one lonely toggle
  reads as broken. **Arena is not in that list: Arena does have bans.**
- **Bravery is a list entry, not a switch** (`settings::BRAVERY_ENTRY`,
  `BRAVERY_ID = -3`). It is offered by the pick picker only in Arena, resolves
  straight to the LCU sentinel without touching the champion table, and is
  committed by `pick_bravery` rather than hover+lock - hovering -3 is rejected.
  Being in the list is what makes it orderable and one-shottable for free. The
  old `braveryEnabled` flag is migrated into an Arena pick-list entry.
- **Auto queue is configured entirely on Principal**, in `#queueCard`, which is
  shown **only to the lobby leader** - nobody else can start a search. Nothing
  about it is in Ajustos. The controls collapse while the switch is off, the
  trigger explanations live behind the `?`, and the two numbers share one row -
  a slider each cost three rows of chrome for two single-digit values.
- The "N membres" choice is capped by the lobby's own `maxSize`: SoloQ tops out
  at 2, so offering 3-5 there offers a condition that can never be met. Arena is
  the exception in the other direction (it reports 16-18 for a solo queue), so
  the ceiling stays at 5. The delay control is pinned right (`.queue-field.end`)
  so it does not jump when "Mínim" appears.
- The current mode is resolved in **one** place, `refresh_queue_mode` in
  `main.rs`, and every transition is logged with its queue id and map. Order of
  authority: the lobby, then the champ-select `queueId` through `queues.rs`,
  then the last known value. That middle step is not optional - **the client
  deletes the lobby the moment champ select starts**, and a hand-written
  `queueId` match knew only 450/1700/1710, so Arena 3x6 (1750) and ARAM: Mayhem
  (2400) fell through to Summoner's Rift and the app read the wrong per-mode
  list. With no mode known at all the card says "Per defecte" rather than
  naming one.
- Per-mode lists are keyed on `gameMode`, so SoloQ, Flex and Normal Draft share
  the `CLASSIC` bucket. The card label shows the **queue** name from the client
  ("Ranked Flex"), which is not the same thing - the tooltip says so.
- Log entries are classified by matching the **Catalan** message text in
  `logSeverity()`. Renaming a log string can change its colour.
- A remote build script has previously copied files one level too deep, creating
  `src/src/` and `src-tauri/src-tauri/`. They're gitignored; if they come back,
  the copy step is at fault.

## Bugs that are easy to reintroduce

Each of these was real, shipped, and found by audit rather than by use.

- **A partial lobby push must not replace the snapshot.** Every field of `Lobby`
  is `#[serde(default)]`, so a payload with no `gameConfig` parses *successfully*
  into an empty lobby — no mode, no leader, capacity 0 — and overwrites a good
  one. `lcu.rs` only forwards a lobby that carries `gameConfig`.
- **The lobby is deleted the moment champ select starts.** Anything that reads
  the mode from the lobby alone goes blind exactly when the pick and ban lists
  are about to be used. `resolve_queue_mode` falls through to the session's
  `queueId`, and holds the last known mode while the phase is ChampSelect or
  InProgress.
- **Never hand-write a `queueId → mode` table.** It knew 450/1700/1710, so Arena
  3x6 (1750) and ARAM: Mayhem (2400) resolved to nothing and the SoloQ list got
  read in an Arena draft. `queues.rs` asks the client.
- **Auto queue must not re-queue after a cancel** - and the cancel usually
  happens in the League client, not in this app, where it produces no signal of
  its own. `note_search_state` watches the transition instead of the actor: in
  the queue, then not, then back in the lobby without a champ select means the
  search was cancelled whoever did it. `queue_cancelled_for` holds a fingerprint
  of the lobby (party, queue, members) until one of them changes; the switch and
  the manual Buscar partida button both lift it. Without this the DELETE's own
  Search push re-evaluated the gate and re-queued about two seconds later, so
  the queue could not be left at all.
- **`Trigger::Full` cannot compare members to `maxLobbySize`.** Arena reports a
  capacity of 16–18 while you queue solo or as a duo. Use the client's own
  `isLobbyFull`.
- **`normalize()` in `main.js` must use `[' .]`, not `['\s.]`.** `\s` also
  strips U+00A0, so a pasted name validated in the UI and resolved to nothing
  in Rust.
- **Dedup guards key on action ids, which restart every draft.** `last_action`
  *and* `last_intent` are both reset when the phase leaves champ select.
- **`isCurrentlyInQueue` does not mean "still searching".** The client leaves it
  `true` through champ select and the whole game — it means "this lobby has a
  match in flight". `Snapshot::is_searching()` folds in the gameflow phase;
  reading the field alone put "Buscant partida" on screen during a draft.
- **Every explanation in this window lives behind a `?`** (`setupHelpToggle`),
  never parked on screen. The window is a fixed 580px: two lines of prose cost
  more than they earn once they have been read. The card legend is a two-column
  `dl` of term and meaning, not sentences - as prose every line wrapped.
- **Recents only list champions that are not already on screen** - `visibleNames`,
  not `namesOnScreen`. The two answer different questions: `namesOnScreen`
  excludes inherited rows because adding one to the current role is a real
  action, and reusing it here left a recent chip for a champion two rows above.
- **A help target must not live inside a block that collapses.** The queue `?`
  toggled a hint inside `#queueConfig`, which is hidden whenever the switch is
  off, so the button did nothing at all in the state you most want help in.
- **The statusbar does not echo the log.** A past event parked next to the live
  connection state reads as live state, and a stale line sat there for a whole
  champ select. The history is one tab away.
- **The lobby payload no longer carries names.** On a current client every
  member arrives with `summonerName: ""` and only a puuid — Riot IDs live on
  `/lol-summoner/v2/summoners/puuid/{puuid}`. `summoners.rs` resolves and caches
  them; `resolve_lobby_names` is spawned, not awaited, so the lobby paints at
  once and fills in. The tag goes in the tooltip, not the row.
- **`bannable-champion-ids` has been wrong.** In an Arena draft it did not list
  K'Sante, every entry was skipped as unavailable and the ban was never cast.
  `selection::ignoring_availability` is the narrow escape hatch: when the *only*
  reason every candidate failed is that table, the ban is attempted anyway — a
  rejected PATCH costs nothing, not banning costs the action. Any other skip
  reason (banned, taken, ally hovering) is evidence and is never second-guessed.
  The champ-select log line now carries `pickable=` and `bannable=` sizes so the
  next disagreement is visible.
