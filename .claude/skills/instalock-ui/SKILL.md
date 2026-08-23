---
name: instalock-ui
description: "Frontend rules for InstaLock's UI (src/index.html, main.js, style.css, tokens.css, overlay.*). Use for ANY change under src/ — new panels, settings rows, buttons, colours, layout, the titlebar, the overlay, or anything visual. Keeps the Blur-AutoClicker-style design system consistent and prevents hardcoded colours, broken light theme, and missing window permissions."
---

# InstaLock UI

The frontend is **vanilla HTML/CSS/JS with no build step** — `frontendDist`
points at `src/`, so what you write is what ships. No React, no bundler, no
TypeScript, no CSS framework. Keep it that way.

The visual language is modelled on
[Blur-AutoClicker](https://github.com/Blur009/Blur-AutoClicker): frameless
window, custom titlebar with icon tabs, glassmorphism panels, a statusbar, and a
green accent that lights up when the client is connected.

## The one rule

**Colour, type, radius and effect values live in `src/tokens.css` and nowhere
else.** Every other rule consumes `var(--…)`.

Before finishing any CSS change:

```bash
grep -nE '#[0-9a-fA-F]{3,8}\b|rgba?\(' src/style.css src/overlay.css
```

Only three kinds of hit are acceptable:

- `#fff` sitting on an accent fill or over an image (toggle knob, spell timer)
- `rgba(var(--panel-rgb), …)` / `rgb(var(--accent-rgb), …)` compositions
- `rgba(0, 0, 0, …)` in a `box-shadow` or `text-shadow` — shadows are black in
  both themes

Anything else is a bug: it will not follow the theme, and it will not follow the
user's accent.

## Tokens you will actually use

| Token | For |
|---|---|
| `--accent-strong` | Active state, checked toggle, connected dot, focus ring |
| `--accent` / `--accent-soft` / `--accent-glow` | Titlebar rule, tinted chips, glows |
| `--danger` / `--warning` | Errors, close-button hover, disconnect |
| `--bg-base` | The window itself |
| `--bg-panel` | **Every panel surface** — cards, settings rows, titlebar, statusbar |
| `--bg-elevated` | Hover fills |
| `--bg-sunken` | Inputs, the log, segmented-control troughs |
| `--border` / `--border-strong` | Hairlines, hover borders, scrollbar thumbs |
| `--text-primary` / `--text-muted` / `--text-dim` / `--text-faint` | Four-step text ramp |
| `--text-on-accent` | Text sitting on `--accent-strong` (flips per theme) |
| `--r-sm` / `--r-md` / `--r-lg` | 4px controls / 8px panels / 16px window |
| `--fs-small` … `--fs-large`, `--fw-light/medium/heavy` | Type scale |

Three tokens are **live-tuned by the user** from the Aparença tab — never
hardcode over them: `--bg-panel-blur`, `--bg-image-blur`, `--panel-opacity`.

## Panels

A panel surface is just `background: var(--bg-panel)`. The glass blur is **not**
declared per rule — it lives in one block gated on `html.has-bg`, because
without a background image `backdrop-filter` blurs a flat colour: no visual
gain, a compositing layer per panel, and softer edges on every rounded corner.
Add new panel selectors to that block, don't inline the filter.

The background image lives on `.app-root::before` at `z-index: 0`. Anything that
must sit above it needs `position: relative` and `z-index: 1` (panels) or `2`
(titlebar, statusbar).

## The window corner is not yours

`.app-root` has **no** `border-radius`, `border` or `box-shadow`, and the window
is opaque. DWM rounds it and draws the shadow (`apply_native_rounding()` in
`main.rs`). Drawing it in CSS over a transparent window produced a soft, smeared
corner — if you find yourself adding a radius to `.app-root`, that's the bug
coming back.

## Both themes, always

`[data-theme]` sits on `<html>`. A change is not done until it has been checked
in **light** as well as dark — the light overrides flip `--panel-rgb` to white
and invert the whole text ramp, so a hardcoded light-grey border vanishes.

The overlay is a separate document. It imports the same `tokens.css` and gets
the theme pushed to it by the `theme` event emitted from `update_settings` in
`main.rs`. If you add a theme-sensitive style to the overlay, verify it flips.

## Structure

```
.app-root
├── .titlebar       tabs left · title · window controls right   (z-index 2)
├── .panel-area     the visible view; all four are siblings     (z-index 1)
│   ├── #mainView  #settingsView  #appearanceView  #logView
└── .statusbar      dot · launch · queue mode · last log line   (z-index 2)
```

### Adding a tab

1. A `<button class="tb-tab" data-tab="NAME">` in `.tb-tabs` with a 16×16
   stroke SVG (`stroke-width: 1.4`, `fill="none"`, `stroke="currentColor"` — the
   existing icons are hand-drawn, not from a library).
2. A `<div id="NAMEView" class="hidden">` in `.panel-area`.
3. An entry in the `views` map in `main.js`. `showTab()` handles the rest.

**A tab must never be empty.** If a view can have no content, give it an empty
state (`.log-empty` is the pattern: icon, title, one-line hint).

### Adding a settings row

Reuse the existing markup — don't invent a new row type:

```html
<div class="settings-section">
  <div class="settings-section-title">SECCIÓ</div>
  <div class="settings-list">
    <label class="settings-item">           <!-- label = clicking the row toggles -->
      <span class="settings-item-label">Etiqueta</span>
      <input type="checkbox" id="…">
    </label>
    <div class="settings-item">             <!-- div when the row holds a control -->
      <span class="settings-item-label">Etiqueta</span>
      <div class="slider-control">
        <input type="range" class="delay-slider" id="…" min="0" max="10" step="0.5">
        <span class="delay-value" id="…Value">0s</span>
      </div>
    </div>
  </div>
</div>
```

Use `<label class="settings-item">` only for checkboxes. A row containing a
button or text input must be a `<div>`, or clicking the control re-triggers the
label.

Then wire it in **three** places or it silently won't persist:
`applySettings()`, `collectSettings()`, and the `Settings` struct in
`src-tauri/src/settings.rs` (with `#[serde(default …)]`).

## Window controls need permissions

Any `getCurrentWindow()` method called from JS must have its permission in
`src-tauri/capabilities/default.json` (`core:window:allow-minimize`,
`allow-hide`, `allow-set-always-on-top`, …). Missing ones fail at runtime with
nothing in the UI to show for it — check the devtools console.

`data-tauri-drag-region` makes an element drag the window; buttons inside it
work normally without extra markup.

## Champion picker

`setupAutocomplete()` expects this wrapper and finds its parts by class:

```html
<div class="champ-autocomplete">
  <div class="champ-field">
    <span class="champ-avatar"></span>
    <input class="champ-input" …>
    <button class="champ-clear" type="button" tabindex="-1">&times;</button>
  </div>
  <div class="champ-dropdown" id="…"></div>
</div>
```

It sets `.filled` and `.invalid` on the wrapper. Ranking is exact → prefix →
substring; `normalizeWithMap()` is what lets a match found in `kaisa` be
highlighted inside `Kai'Sa`. `normalize()` must stay identical to the Rust
version in `champions.rs`, or the UI will accept names the backend can't resolve.

Icons come from Community Dragon by champion id, which is why the picker uses
`get_champion_options` rather than `get_champions`.

## Verifying

The app is a native window, so browser tools can't drive it directly. Two ways:

1. **Real app** — `npm run tauri dev`. The only way to check the frameless
   window, drag region, tray and always-on-top.
2. **Browser preview** — copy `src/*.{html,css,js}` to a scratch dir, inject a
   stub for `window.__TAURI__` (`invoke` returning canned settings and champion
   options, `listen` a no-op, `getCurrentWindow` returning stub methods), serve
   it, and drive it with the browser tools. Good for layout, both themes, tab
   switching and picker behaviour without a Rust rebuild.

Check `read_console_messages` for errors either way — a typo in a
`getElementById` throws during `init()` and leaves the whole UI dead.
