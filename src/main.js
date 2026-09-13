const { invoke } = window.__TAURI__.core;
const { listen } = window.__TAURI__.event;
const { getCurrentWindow } = window.__TAURI__.window;

const appWindow = getCurrentWindow();

// DOM elements
const statusDot = document.getElementById("statusDot");
const statusText = document.getElementById("statusText");
const modeIndicator = document.getElementById("modeIndicator");
const modeName = document.getElementById("modeName");
const autoAccept = document.getElementById("autoAccept");
const autoPick = document.getElementById("autoPick");
const autoBan = document.getElementById("autoBan");
const pickChampion = document.getElementById("pickChampion");
const banChampion = document.getElementById("banChampion");
const pickDropdown = document.getElementById("pickDropdown");
const banDropdown = document.getElementById("banDropdown");
const logEl = document.getElementById("log");
const logEmpty = document.getElementById("logEmpty");
const logCount = document.getElementById("logCount");
const logClear = document.getElementById("logClear");
const launchLol = document.getElementById("launchLol");

// Delay elements
const acceptDelay = document.getElementById("acceptDelay");
const pickDelay = document.getElementById("pickDelay");
const banDelay = document.getElementById("banDelay");
const acceptDelayValue = document.getElementById("acceptDelayValue");
const pickDelayValue = document.getElementById("pickDelayValue");
const banDelayValue = document.getElementById("banDelayValue");
const actionMargin = document.getElementById("actionMargin");
const actionMarginValue = document.getElementById("actionMarginValue");

// Titlebar
const titlebar = document.getElementById("titlebar");
const tbPin = document.getElementById("tbPin");
const tbMinimize = document.getElementById("tbMinimize");
const tbClose = document.getElementById("tbClose");

// Appearance
const themeSegmented = document.getElementById("themeSegmented");
const panelOpacity = document.getElementById("panelOpacity");
const panelOpacityValue = document.getElementById("panelOpacityValue");
const alwaysOnTop = document.getElementById("alwaysOnTop");
const minimizeToTray = document.getElementById("minimizeToTray");
const appearanceReset = document.getElementById("appearanceReset");

// Tabs
const views = {
  main: document.getElementById("mainView"),
  game: document.getElementById("gameView"),
  settings: document.getElementById("settingsView"),
  appearance: document.getElementById("appearanceView"),
  log: document.getElementById("logView"),
};

// State
let champOptions = [];
let saveTimeout = null;

// Ordered champion preferences per assigned role. Role names match
// `assignedPosition` in the champ select session exactly (lowercase), and
// `default` covers ARAM, blind pick and any role left empty — the backend's
// RoleLists::for_role falls back to it.
const ROLES = ["default", "top", "jungle", "middle", "bottom", "utility"];

function emptyLists() {
  return Object.fromEntries(ROLES.map((r) => [r, []]));
}

const activeRole = { pick: "default", ban: "default" };

// Normalize: must behave identically to `champions::normalize` in Rust, or the
// UI accepts names the backend cannot resolve and the pick silently no-ops.
//
// The character class is a literal apostrophe, space and dot — NOT `\s`. `\s`
// also matches a tab, a newline and U+00A0, so a name pasted with a
// non-breaking space ("Lee Sin") validated here, showed an avatar, and
// then resolved to nothing on the Rust side.
function normalize(name) {
  return name.toLowerCase().replace(/[' .]/g, "").replace(/&/g, "and");
}

// Same normalization, but keeping an index back into the original string so a
// match found in "kaisa" can be highlighted in the displayed "Kai'Sa".
function normalizeWithMap(name) {
  let norm = "";
  const map = [];
  for (let i = 0; i < name.length; i++) {
    const ch = name[i].toLowerCase();
    if (ch === "'" || ch === " " || ch === ".") continue;
    if (ch === "&") {
      norm += "and";
      map.push(i, i, i);
      continue;
    }
    norm += ch;
    map.push(i);
  }
  return { norm, map };
}

const CHAMP_ICON_BASE =
  "https://raw.communitydragon.org/latest/plugins/rcp-be-lol-game-data/global/default/v1/champion-icons";

// The client's own Bravery art. There is no champion icon for the -3 sentinel -
// the champion table only defines -1 - so it comes from the champ-select
// plugin, which is where the client draws it too.
const BRAVERY_ICON =
  "https://raw.communitydragon.org/latest/plugins/rcp-fe-lol-champ-select/global/default/images/champion-grid/bravery-champion-circle.png";

function championIconUrl(id) {
  return `${CHAMP_ICON_BASE}/${id}.png`;
}

// Paint a champion icon, or the Bravery mark for the one entry that has no
// portrait to paint — its id is the LCU sentinel, not a champion.
function paintChampIcon(el, champ) {
  el.classList.toggle("is-bravery", !!champ?.bravery);

  if (!champ) {
    el.style.backgroundImage = "";
    return;
  }
  const src = champ.bravery ? BRAVERY_ICON : championIconUrl(champ.id);
  el.style.backgroundImage = `url("${src}")`;
}

// Bravery is an Arena-only list entry, not a champion: it has no id and never
// resolves through the champion table. It is offered by the picker only in
// Arena, because anywhere else it would sit in the list doing nothing.
// Mirrors `settings::BRAVERY_ENTRY` and `is_bravery` on the Rust side.
const BRAVERY_ENTRY = "Bravery";

function isBravery(name) {
  return normalize(name) === normalize(BRAVERY_ENTRY);
}

function braveryOption() {
  return { id: -3, name: BRAVERY_ENTRY, bravery: true, ...normalizeWithMap(BRAVERY_ENTRY) };
}

// Everything the picker may offer right now.
function pickableOptions(kind) {
  // Never a ban: there is no such thing as banning Bravery.
  if (kind === "pick" && currentGameMode === "CHERRY") {
    return [braveryOption(), ...champOptions];
  }
  return champOptions;
}

// Rank matches: exact name, then prefix, then substring; ties alphabetically.
// The old plain `includes` filter put "Lee Sin" above "Sion" for "si".
function rankChampions(query, kind) {
  const options = pickableOptions(kind);
  if (!query.trim()) return options.slice();
  const q = normalize(query);
  if (!q) return options.slice();

  const scored = [];
  for (const champ of options) {
    const at = champ.norm.indexOf(q);
    if (at < 0) continue;
    scored.push({ champ, at, score: champ.norm === q ? 0 : at === 0 ? 1 : 2 });
  }
  scored.sort(
    (a, b) =>
      a.score - b.score || a.at - b.at || a.champ.name.localeCompare(b.champ.name)
  );
  return scored.map((s) => ({ ...s.champ, matchAt: s.at, matchLen: q.length }));
}

function findChampion(value) {
  const q = normalize(value);
  if (!q) return null;
  if (isBravery(value)) return braveryOption();
  return champOptions.find((c) => c.norm === q) || null;
}

// Format delay value
function formatDelay(val) {
  const n = parseFloat(val);
  return n % 1 === 0 ? n + "s" : n.toFixed(1) + "s";
}

// Autocomplete setup.
//
// `onPick` turns the field from "this is the value" into "add this to a list":
// the champion is handed over, the input clears, and the dropdown reopens on
// the full roster so several can be added in a row.
function setupAutocomplete(input, dropdown, kind, onPick) {
  const wrapper = input.closest(".champ-autocomplete");
  const avatar = wrapper.querySelector(".champ-avatar");
  const clearBtn = wrapper.querySelector(".champ-clear");
  let activeIdx = -1;

  // Reflect the current value: avatar, clear button, and whether the typed name
  // actually resolves to a champion (a typo would otherwise fail silently at
  // pick time, with nothing in the UI to explain it).
  //
  // `strict` is for when the user is done with the field. Mid-typing, "Kar" is
  // not yet a champion but is on its way to one, so only a query that matches
  // nothing at all is worth flagging.
  function refresh({ strict = false } = {}) {
    const value = input.value.trim();
    const champ = findChampion(value);
    wrapper.classList.toggle("filled", value.length > 0);

    // Don't flag anything while the champion list is still loading.
    const known = champOptions.length > 0;
    const bad = strict ? !champ : rankChampions(value, kind).length === 0;
    wrapper.classList.toggle("invalid", value.length > 0 && known && bad);

    paintChampIcon(avatar, champ);
  }

  function optionEl(champ) {
    const row = document.createElement("div");
    row.className = "champ-option";

    const icon = document.createElement("span");
    icon.className = "champ-option-icon";
    paintChampIcon(icon, champ);

    const label = document.createElement("span");
    label.className = "champ-option-name";
    if (champ.matchLen) {
      // Map the match back onto the display name through the index table
      const from = champ.map[champ.matchAt];
      const to = champ.map[champ.matchAt + champ.matchLen - 1] + 1;
      label.append(champ.name.slice(0, from));
      const hit = document.createElement("mark");
      hit.textContent = champ.name.slice(from, to);
      label.append(hit, champ.name.slice(to));
    } else {
      label.textContent = champ.name;
    }

    row.append(icon, label);
    row.addEventListener("mousedown", (e) => {
      e.preventDefault();
      select(champ.name);
    });
    return row;
  }

  function select(name) {
    if (onPick) {
      onPick(name);
      input.value = "";
      refresh();
      // Stay open on the full roster: building a list of five is the normal
      // case, and reopening it by hand each time would be tedious.
      show(rankChampions("", kind));
      return;
    }
    input.value = name;
    dropdown.classList.remove("open");
    refresh();
    saveSettingsDebounced();
  }

  function show(matches) {
    dropdown.innerHTML = "";
    activeIdx = -1;

    if (!champOptions.length) {
      dropdown.innerHTML = '<div class="champ-empty">Carregant champions…</div>';
      dropdown.classList.add("open");
      return;
    }
    if (!matches.length) {
      dropdown.innerHTML = '<div class="champ-empty">Cap champion trobat</div>';
      dropdown.classList.add("open");
      return;
    }

    const frag = document.createDocumentFragment();
    matches.forEach((champ) => frag.appendChild(optionEl(champ)));
    dropdown.appendChild(frag);
    dropdown.classList.add("open");
    dropdown.scrollTop = 0;
  }

  input.addEventListener("input", () => {
    refresh();
    show(rankChampions(input.value, kind));
  });

  // Focusing opens the list instead of nothing. When the field already holds a
  // valid champion there is nothing left to narrow, so show the whole roster
  // and preselect the text so typing replaces it.
  input.addEventListener("focus", () => {
    const exact = findChampion(input.value);
    if (exact) input.select();
    show(rankChampions(exact ? "" : input.value, kind));
  });

  input.addEventListener("blur", () => {
    setTimeout(() => dropdown.classList.remove("open"), 150);
    refresh({ strict: true });
  });

  clearBtn.addEventListener("click", () => {
    input.value = "";
    refresh();
    saveSettingsDebounced();
    input.focus();
  });

  input.addEventListener("keydown", (e) => {
    const options = dropdown.querySelectorAll(".champ-option");

    if (e.key === "Escape") {
      dropdown.classList.remove("open");
      return;
    }
    if (e.key === "ArrowDown" && !dropdown.classList.contains("open")) {
      e.preventDefault();
      show(rankChampions(input.value, kind));
      return;
    }
    if (!options.length) return;

    if (e.key === "ArrowDown") {
      e.preventDefault();
      activeIdx = (activeIdx + 1) % options.length;
    } else if (e.key === "ArrowUp") {
      e.preventDefault();
      activeIdx = (activeIdx - 1 + options.length) % options.length;
    } else if (e.key === "Enter") {
      e.preventDefault();
      const pick =
        activeIdx >= 0 ? options[activeIdx] : options.length === 1 ? options[0] : null;
      if (pick) select(pick.querySelector(".champ-option-name").textContent);
      return;
    } else if (e.key === "Tab") {
      // Tabbing away with one obvious candidate completes it
      if (options.length === 1) {
        select(options[0].querySelector(".champ-option-name").textContent);
      }
      return;
    } else {
      return;
    }

    options.forEach((o, i) => o.classList.toggle("active", i === activeIdx));
    options[activeIdx].scrollIntoView({ block: "nearest" });
  });

  // Re-run when the champion list finally arrives. Strict here: the value came
  // from settings.json, so a name that no longer resolves should show as broken
  // rather than sit there looking fine and silently never picking.
  input.addEventListener("champions-ready", () => refresh({ strict: true }));
  refresh();
}

// ── Champion priority lists ──
//
// Stored per game mode, keyed by the LCU's own `gameMode`. There is no UI for
// the keys: the app edits whichever mode you are currently in, so a Zac added
// in Arena never turns up in SoloQ and you never have to think about it. The
// `*` bucket is the escape hatch — entries there apply in every mode, which is
// how "always ban K'Sante" is expressed.
const GLOBAL_MODE = "*";
const DEFAULT_MODE = "CLASSIC";

// { pick: { "*": RoleLists, CLASSIC: RoleLists, ... }, ban: {...} }
const modeLists = { pick: {}, ban: {} };
const recents = { pick: [], ban: [] };
// What the next champion added becomes. Answered inline, in the field.
const addOnce = { pick: false, ban: false };
let perModeLists = true;

// Catalan names for the modes we can actually label. Anything else shows its
// raw gameMode rather than vanishing — a rotating mode still gets its own list.
const MODE_NAMES = {
  CLASSIC: "Summoner's Rift",
  ARAM: "ARAM",
  CHERRY: "Arena",
  URF: "URF",
  ONEFORALL: "One for All",
  NEXUSBLITZ: "Nexus Blitz",
  STRAWBERRY: "Swarm",
  TUTORIAL: "Tutorial",
};

// Which bucket is being read and written right now.
function modeKey() {
  if (!perModeLists) return GLOBAL_MODE;
  return currentGameMode || DEFAULT_MODE;
}

function modeLabel(key) {
  if (key === GLOBAL_MODE) return "Tots els modes";
  return MODE_NAMES[key] || key;
}

function bucket(kind, key) {
  if (!modeLists[kind][key]) modeLists[kind][key] = emptyLists();
  return modeLists[kind][key];
}

function entriesIn(kind, key, role) {
  return bucket(kind, key)[role] || [];
}

function isBucketEmpty(kind, key) {
  return ROLES.every((role) => entriesIn(kind, key, role).length === 0);
}

// Mirror of RoleLists::for_role in settings.rs: a role's own entries first,
// then the Defecte list beneath them as the safety net. The two have to agree,
// or the list on screen is not the list that gets used.
function entriesForRole(kind, key, role) {
  if (role === "default") {
    return entriesIn(kind, key, "default").map((entry, index) => ({ entry, index, inherited: false }));
  }
  const rows = entriesIn(kind, key, role).map((entry, index) => ({
    entry,
    index,
    inherited: false,
  }));
  const taken = rows.map((r) => normalize(r.entry.name));
  entriesIn(kind, key, "default").forEach((entry, index) => {
    if (!taken.includes(normalize(entry.name))) {
      rows.push({ entry, index, inherited: true });
    }
  });
  return rows;
}

// What the user sees is exactly the effective list, in the order the backend
// will try it: global entries first, then the ones belonging to the mode being
// played. Rows inherited from the Defecte list are flagged so they can be shown
// as coming from somewhere else rather than looking editable in place.
function displayedEntries(kind) {
  const role = activeRole[kind];
  const key = modeKey();
  const rows = [];

  const push = (bucketKey) => {
    for (const { entry, index, inherited } of entriesForRole(kind, bucketKey, role)) {
      rows.push({
        entry,
        index,
        key: bucketKey,
        inherited,
        sourceRole: inherited ? "default" : role,
      });
    }
  };

  push(GLOBAL_MODE);
  if (key !== GLOBAL_MODE) push(key);
  return rows;
}

function renderList(kind) {
  const container = document.getElementById(kind === "pick" ? "pickList" : "banList");
  const rows = displayedEntries(kind);
  container.innerHTML = "";

  // An empty list gets no placeholder text: the field directly above already
  // says "Afegeix un champion...", and the legend behind the "?" explains the
  // rest. A line saying "empty" over an empty box earned nothing.
  if (rows.length) {
    const frag = document.createDocumentFragment();
    rows.forEach((row, position) => frag.appendChild(listItemEl(kind, row, position)));
    container.appendChild(frag);

    // Rows on loan from the Defecte list used to carry a two-line explanation
    // under the list. It cost more room than it earned: the italics, the
    // disabled rank button and the row tooltip already say it.
  }

  // With per-mode memory off, anything filed under a specific mode is still on
  // disk but no longer applies. Saying so beats the user watching their list
  // shrink and assuming it was lost.
  if (!perModeLists) {
    const parked = Object.keys(modeLists[kind]).filter(
      (key) => key !== GLOBAL_MODE && !isBucketEmpty(kind, key)
    );
    if (parked.length) {
      const note = document.createElement("div");
      note.className = "champ-list-note";
      note.textContent = `Tens llistes guardades per a  que ara no s'apliquen.`;
      note.title =
        "Torna a activar «Recordar les llistes per mode de joc» a Ajustos per fer-les servir.";
      container.appendChild(note);
    }
  }

  renderRecents(kind);
  refreshModeLabel(kind);
}

function listItemEl(kind, row, position) {
  const { entry, index, key, inherited } = row;
  const champ = findChampion(entry.name);

  const el = document.createElement("div");
  el.className = "champ-item";
  // An inherited row lives in the Defecte list, so reordering it here would
  // silently reorder that one instead. Its controls still work, because they
  // act on its real home and that is what the note explains.
  el.draggable = !inherited;
  el.dataset.position = position;
  if (inherited) el.classList.add("inherited");
  if (entry.disabled) el.classList.add("off");
  // A name that no longer resolves would silently never be picked; flag it
  // rather than let it sit there looking fine. Not while the roster is still
  // loading, though - everything would look broken for the first few seconds.
  if (champOptions.length && !champ) el.classList.add("invalid");

  const rank = document.createElement("button");
  rank.className = "champ-rank";
  rank.type = "button";
  rank.textContent = position + 1;
  // Drag is the natural gesture, but a click-to-promote is a guaranteed way to
  // reorder that does not depend on the webview's drag support.
  rank.title = inherited
    ? "Ve de la llista Defecte"
    : index === 0
      ? "Primera opció"
      : "Puja a la primera posició";
  rank.disabled = inherited;
  rank.addEventListener("click", () => moveEntry(kind, key, row.sourceRole, index, 0));

  const icon = document.createElement("span");
  icon.className = "champ-item-icon";
  paintChampIcon(icon, champ);

  const label = document.createElement("span");
  label.className = "champ-item-name";
  label.textContent = entry.name;
  const origin = inherited ? " · ve de la llista Defecte" : "";
  label.title = champ
    ? entry.name + origin
    : `${entry.name} — no s'ha trobat cap champion amb aquest nom`;

  el.append(rank, icon, label);
  el.append(onceBadge(kind, row), globalBadge(kind, row), removeButton(kind, row));
  attachDragHandlers(kind, el, row, position);
  return el;
}

// How long the entry lasts. A spent one-shot shows as off and re-arms with one
// click, which is why the game start disables it instead of deleting it.
function onceBadge(kind, row) {
  const { entry, index } = row;
  const badge = document.createElement("button");
  badge.className = "entry-badge";
  badge.type = "button";
  badge.textContent = entry.once ? "1×" : "∞";
  badge.classList.toggle("on", entry.once && !entry.disabled);

  if (entry.disabled) {
    badge.title = "Gastat en l'última partida. Clica per tornar-lo a armar.";
  } else if (entry.once) {
    badge.title = "Només per a la propera partida. Clica per fer-lo permanent.";
  } else {
    badge.title = "Permanent. Clica per fer-lo servir només una partida.";
  }

  badge.addEventListener("click", () => {
    const target = sourceEntries(kind, row)[index];
    if (!target) return;
    if (target.disabled) {
      // Re-arm a spent one-shot rather than toggling it to permanent: the user
      // clicked the thing that says "gastat", so they want it back.
      target.disabled = false;
    } else {
      target.once = !target.once;
    }
    renderList(kind);
    saveSettingsDebounced();
  });
  return badge;
}

// How widely the entry applies. This is the manual override on top of per-mode
// memory: "always ban K'Sante, whatever we're playing".
function globalBadge(kind, { entry, key, sourceRole }) {
  const isGlobal = key === GLOBAL_MODE;
  const badge = document.createElement("button");
  badge.className = "entry-badge";
  badge.type = "button";
  // Short label, not a sentence: the pill is 34px wide and the tooltip below
  // carries the meaning.
  badge.textContent = "Tot";
  badge.classList.toggle("on", isGlobal);
  badge.title = isGlobal
    ? "S'aplica a tots els modes. Clica per limitar-lo a " + modeLabel(modeKey()) + "."
    : "Només a " + modeLabel(modeKey()) + ". Clica perquè s'apliqui a tots els modes.";

  // With per-mode memory off everything is global already, so the badge would
  // be a lie and a no-op.
  if (!perModeLists) {
    badge.disabled = true;
    badge.title = "Les llistes per mode estan desactivades: tot s'aplica a tot.";
  }

  badge.addEventListener("click", () => {
    if (!perModeLists) return;
    moveBetweenBuckets(kind, entry, key, sourceRole, isGlobal ? modeKey() : GLOBAL_MODE);
  });
  return badge;
}

function removeButton(kind, row) {
  const { index } = row;
  const button = document.createElement("button");
  button.className = "champ-item-remove";
  button.type = "button";
  button.title = "Treure";
  button.textContent = "×";
  button.addEventListener("click", () => {
    sourceEntries(kind, row).splice(index, 1);
    renderList(kind);
    refreshRoleTabs(kind);
    saveSettingsDebounced();
  });
  return button;
}

// The array an on-screen row actually lives in. For an inherited row that is
// the Defecte list, not the role being viewed - acting on the visible role
// would edit a list the user is not looking at.
function sourceEntries(kind, row) {
  return entriesIn(kind, row.key, row.sourceRole);
}

function moveBetweenBuckets(kind, entry, fromKey, fromRole, toKey) {
  const from = entriesIn(kind, fromKey, fromRole);
  const at = from.indexOf(entry);
  if (at >= 0) from.splice(at, 1);
  bucket(kind, toKey)[fromRole].push(entry);
  renderList(kind);
  refreshRoleTabs(kind);
  saveSettingsDebounced();
}

function moveEntry(kind, key, role, from, to) {
  const entries = entriesIn(kind, key, role);
  if (from === to || from < 0 || from >= entries.length) return;
  const [moved] = entries.splice(from, 1);
  entries.splice(Math.max(0, Math.min(to, entries.length)), 0, moved);
  renderList(kind);
  saveSettingsDebounced();
}

// The row being dragged. One at a time, so a module-level value is enough and
// avoids threading it through every handler.
let dragFrom = null;

// Entries only reorder within the exact array they live in. The mode bucket is
// not enough: an inherited row is shown here but lives in the Defecte list, and
// dropping a Mid entry onto one used to splice an unrelated champion out of
// Defecte and leave the dragged row where it was.
function sameBucket(kind, row) {
  return (
    !!dragFrom &&
    dragFrom.kind === kind &&
    dragFrom.key === row.key &&
    dragFrom.sourceRole === row.sourceRole
  );
}

function attachDragHandlers(kind, el, row, position) {
  el.addEventListener("dragstart", (e) => {
    dragFrom = { kind, key: row.key, index: row.index, sourceRole: row.sourceRole, position };
    el.classList.add("dragging");
    e.dataTransfer.effectAllowed = "move";
    // WebView2 refuses to start a drag with no payload attached.
    e.dataTransfer.setData("text/plain", String(position));
  });

  el.addEventListener("dragend", () => {
    dragFrom = null;
    el.classList.remove("dragging");
    clearDropMarkers(kind);
  });

  el.addEventListener("dragover", (e) => {
    if (!sameBucket(kind, row)) return;
    e.preventDefault();
    e.dataTransfer.dropEffect = "move";
    const box = el.getBoundingClientRect();
    const after = e.clientY > box.top + box.height / 2;
    clearDropMarkers(kind);
    el.classList.add(after ? "drop-after" : "drop-before");
  });

  el.addEventListener("drop", (e) => {
    if (!sameBucket(kind, row)) return;
    e.preventDefault();
    const box = el.getBoundingClientRect();
    const after = e.clientY > box.top + box.height / 2;
    let to = after ? row.index + 1 : row.index;
    // Removing the source first shifts everything after it down by one.
    if (dragFrom.index < to) to -= 1;
    moveEntry(kind, row.key, row.sourceRole, dragFrom.index, to);
  });
}

function clearDropMarkers(kind) {
  const container = document.getElementById(kind === "pick" ? "pickList" : "banList");
  container.querySelectorAll(".champ-item").forEach((el) => {
    el.classList.remove("drop-before", "drop-after");
  });
}

// Every name currently in the list on screen, for duplicate checks.
// Names already in the list being edited. Inherited rows are excluded: adding
// one here is a real action - it gives this role a list of its own, which is
// exactly what the note under the list says.
// Names that adding to the current role would duplicate. Inherited rows are
// excluded on purpose: they live in the Defecte list, so adding one here is a
// real action - it promotes it to this role.
function namesOnScreen(kind) {
  return displayedEntries(kind)
    .filter((row) => !row.inherited)
    .map((row) => normalize(row.entry.name));
}

// Every name the user can actually see right now, inherited rows included.
// A different question from the one above, and answering it with that one put a
// recent chip for a champion sitting two rows above it.
function visibleNames(kind) {
  return displayedEntries(kind).map((row) => normalize(row.entry.name));
}

function addToList(kind, name) {
  // Duplicates in a fallback list are dead weight: the second one can never be
  // reached, because whatever ruled the first one out rules it out too.
  if (namesOnScreen(kind).includes(normalize(name))) return;

  bucket(kind, modeKey())[activeRole[kind]].push({
    name,
    once: addOnce[kind],
    disabled: false,
  });
  renderList(kind);
  refreshRoleTabs(kind);
  saveSettingsDebounced();
}

// ── Recently used ──

function renderRecents(kind) {
  const container = document.getElementById(kind === "pick" ? "pickRecents" : "banRecents");
  container.innerHTML = "";

  // Only champions that are not already on screen. A recent chip for something
  // sitting in the list two rows above it is a row of height spent telling you
  // what you can already see — and it was rendered disabled, so it could not
  // even be clicked.
  const present = visibleNames(kind);
  const items = (recents[kind] || []).filter((name) => !present.includes(normalize(name)));

  container.hidden = items.length === 0;
  if (!items.length) return;

  for (const name of items) {
    const chip = document.createElement("button");
    chip.className = "recent-chip";
    chip.type = "button";
    chip.title = `Afegir ${name}`;

    const icon = document.createElement("span");
    icon.className = "recent-chip-icon";
    paintChampIcon(icon, findChampion(name));

    chip.append(icon, document.createTextNode(name));
    chip.addEventListener("click", () => addToList(kind, name));
    container.appendChild(chip);
  }
}

// ── Role tabs and the mode label ──

// What the current queue actually looks like. The backend derives it from the
// lobby's own showPositionSelector rather than from a hardcoded list of queue
// ids, so a new rotating mode doesn't need a code change here.
let modeContext = { hasPositions: true, assignedPosition: "", phase: "" };
let currentGameMode = null;
// What the client itself calls the current queue ("Ranked Flex", "Arena 3x6",
// "ARAM: Mayhem"). The bucket is keyed on the game mode, which is coarser -
// SoloQ, Flex and Normal Draft all share CLASSIC - so the chip shows the queue
// and the tooltip explains the sharing.
let currentQueueName = null;

// Mark which role tabs actually hold a list, so it's visible without clicking
// through all six, and mark the one the client has actually assigned us.
function refreshRoleTabs(kind) {
  const tabs = document.getElementById(kind === "pick" ? "pickRoles" : "banRoles");
  const key = modeKey();
  tabs.querySelectorAll("button").forEach((btn) => {
    const role = btn.dataset.role;
    const filled =
      entriesIn(kind, GLOBAL_MODE, role).length > 0 ||
      (key !== GLOBAL_MODE && entriesIn(kind, key, role).length > 0);
    btn.classList.toggle("active", role === activeRole[kind]);
    btn.classList.toggle("filled", filled);

    const assigned = role !== "default" && role === modeContext.assignedPosition;
    btn.classList.toggle("assigned", assigned);
    btn.title = assigned
      ? "El rol que t'ha tocat aquesta partida"
      : role === "default"
        ? "ARAM, blind, i qualsevol rol sense llista"
        : "";
  });
}

// Per-mode lists are meant to be invisible, but silently swapping the list out
// from under the user would be baffling. This is the minimum that makes it
// legible: which mode's list is on screen.
function refreshModeLabel(kind) {
  const label = document.getElementById(kind === "pick" ? "pickModeLabel" : "banModeLabel");
  if (!label) return;
  if (!perModeLists) {
    label.hidden = true;
    return;
  }
  label.hidden = false;

  // With no lobby open there is no mode to report. Naming one anyway said
  // "Summoner's Rift" while the user was sitting in an Arena lobby, which is
  // worse than saying nothing.
  const bucket = modeLabel(modeKey());
  label.textContent = currentGameMode ? currentQueueName || bucket : "Per defecte";
  label.title = currentGameMode
    ? `Llista de ${bucket}, compartida per totes les cues d'aquest mode. Les entrades marcades Tot s'apliquen sempre.`
    : `Cap sala oberta: s'edita la llista de ${bucket}, que és la que s'usa quan no se sap el mode.`;
}

function setupRoleTabs(kind) {
  const tabs = document.getElementById(kind === "pick" ? "pickRoles" : "banRoles");
  tabs.querySelectorAll("button").forEach((btn) => {
    btn.addEventListener("click", () => {
      activeRole[kind] = btn.dataset.role;
      refreshRoleTabs(kind);
      renderList(kind);
    });
  });
}

// The "add as" pill lives inside the champion field: it is the once-or-always
// question asked where the answer is used, rather than spending a whole row.
function setupOncePill(kind) {
  const pill = document.getElementById(kind === "pick" ? "pickOncePill" : "banOncePill");
  if (!pill) return;
  const paint = () => {
    pill.textContent = addOnce[kind] ? "1 partida" : "Sempre";
    pill.classList.toggle("on", addOnce[kind]);
    pill.title = addOnce[kind]
      ? "Els champions que afegeixis valdran només per a la propera partida"
      : "Els champions que afegeixis es quedaran fins que els treguis";
  };
  pill.addEventListener("click", () => {
    addOnce[kind] = !addOnce[kind];
    paint();
  });
  paint();
}

// ARAM, Arena, URF and blind pick assign no lane, so the six role tabs are
// noise there: hide them and edit the single default list instead.
function applyModeContext(ctx) {
  modeContext = { ...modeContext, ...(ctx || {}) };
  const hasPositions = modeContext.hasPositions !== false;

  document.body.dataset.hasPositions = hasPositions ? "true" : "false";
  // ARAM and its variants have no pick or ban phase at all. The backend decides
  // this from the client map id, not from the mode codename, because the
  // codenames rotate every patch (ARAM, KIWI, KIWI_JADE...).
  document.body.dataset.hasChampSelect =
    modeContext.hasChampSelect === false ? "false" : "true";
  updateModeNote();

  for (const kind of ["pick", "ban"]) {
    if (!hasPositions && activeRole[kind] !== "default") {
      // Editing a lane list that can never be used would be misleading.
      activeRole[kind] = "default";
    } else if (hasPositions && activeRole[kind] === "default") {
      // Jump to the list that is about to be used, so what you see is what
      // will happen. Champ select is authoritative once it has assigned a
      // lane; before that, the position you asked for in the lobby is the
      // best guess available and is usually what you get.
      const role = modeContext.assignedPosition || modeContext.lobbyPosition;
      if (role && ROLES.includes(role)) activeRole[kind] = role;
    }
    refreshRoleTabs(kind);
    renderList(kind);
  }
}

// Grey out a card body whose switch is off, and keep the mode note honest.
function updateCardBodyStates() {
  document.querySelectorAll(".card-body[data-toggle]").forEach((body) => {
    const toggleId = body.dataset.toggle;
    const checkbox = document.getElementById(toggleId);
    if (checkbox) {
      body.classList.toggle("disabled", !checkbox.checked);
    }
  });
  updateModeNote();
}


// Some modes have no pick or ban phase at all, so their cards are hidden. Say
// why, or the main tab looks like it failed to load.
function updateModeNote() {
  const note = document.getElementById("modeNote");
  if (!note) return;

  note.textContent =
    modeContext.hasChampSelect === false
      ? "Aquest mode no té ni pick ni ban: InstaLock només accepta la partida."
      : "";
}

// ── Tabs ──

function showTab(name) {
  for (const [key, el] of Object.entries(views)) {
    el.classList.toggle("hidden", key !== name);
  }
  document.querySelectorAll(".tb-tab").forEach((btn) => {
    btn.classList.toggle("active", btn.dataset.tab === name);
  });
  // The appearance tab drops the accent rule so it doesn't fight the previews.
  titlebar.classList.toggle("flat", name === "appearance");
}

// ── Appearance ──

function applyTheme(theme) {
  document.documentElement.dataset.theme = theme;
  themeSegmented.querySelectorAll("button").forEach((b) => {
    b.classList.toggle("active", b.dataset.themeValue === theme);
  });
}

// Appearance defaults — must match Settings::default() in settings.rs
const APPEARANCE_DEFAULTS = {
  theme: "dark",
  panelOpacity: 0.55,
  alwaysOnTop: false,
};

async function resetAppearance() {
  applyTheme(APPEARANCE_DEFAULTS.theme);
  applyPanelOpacity(APPEARANCE_DEFAULTS.panelOpacity);
  applyAlwaysOnTop(APPEARANCE_DEFAULTS.alwaysOnTop);
  await flushSettings();
  addLog("Aparença restablerta");
}

function applyPanelOpacity(op) {
  panelOpacity.value = op;
  panelOpacityValue.textContent = parseFloat(op).toFixed(2);
  document.documentElement.style.setProperty("--panel-opacity", op);
}

// ── Init ──

async function init() {
  // Register event listeners FIRST
  await listen("lcu-status", (event) => setConnected(event.payload));
  await listen("log", (event) => addLog(event.payload));
  await listen("champions-loaded", () => loadChampions());
  await listen("queue-mode", (event) => applyQueueMode(event.payload));
  await listen("mode-context", (event) => applyModeContext(event.payload));
  // The whole info panel, throttled in the backend: champ select re-emits on
  // every hover and every timer tick.
  await listen("client-state", (event) => renderGame(event.payload));
  // The backend edits settings by itself in two cases — spending one-shot
  // entries when a game starts, and recording a recently used champion — so it
  // pushes the result back rather than letting the UI drift until a restart.
  await listen("settings-changed", (event) => {
    if (!event.payload) return;
    // A pending debounced save would overwrite what just arrived.
    if (saveTimeout) {
      clearTimeout(saveTimeout);
      saveTimeout = null;
    }
    applySettings(event.payload);
  });

  // Load initial data
  const settings = await invoke("get_settings");
  applySettings(settings);

  const connected = await invoke("is_lcu_connected");
  setConnected(connected);

  // Hide the button entirely on a machine with no Riot Client installed.
  canLaunchLeague = await invoke("can_launch_league");
  updateLaunchButton();

  launchLol.addEventListener("click", async () => {
    if (launchLol.disabled) return;
    launchLol.disabled = true;
    launchLol.textContent = "Obrint…";

    try {
      await invoke("launch_league");
    } catch (e) {
      // The backend already emits the failure to the log; don't double-report.
      console.error("launch_league failed", e);
    }

    // The client takes a good while to come up and the button disappears on
    // its own once the LCU connects. Re-arm in case it never does.
    setTimeout(() => {
      launchLol.disabled = false;
      launchLol.textContent = "Obrir LoL";
    }, 10000);
  });

  // Load autostart state
  const autoStartEl = document.getElementById("autoStart");
  autoStartEl.checked = await invoke("get_autostart");
  autoStartEl.addEventListener("change", () => {
    invoke("set_autostart", { enabled: autoStartEl.checked });
  });

  // Focus restore toggle
  const restoreFocusEl = document.getElementById("restoreFocus");
  restoreFocusEl.addEventListener("change", () => saveSettingsDebounced());

  // Hover pick toggle
  const hoverPickEl = document.getElementById("hoverPick");
  hoverPickEl.addEventListener("change", () => saveSettingsDebounced());

  // Setup autocompletes. In list mode the field adds to the list for the role
  // currently selected above it rather than holding a value of its own.
  setupRoleTabs("pick");
  setupRoleTabs("ban");
  setupOncePill("pick");
  setupOncePill("ban");
  setupAutocomplete(pickChampion, pickDropdown, "pick", (name) => addToList("pick", name));
  setupAutocomplete(banChampion, banDropdown, "ban", (name) => addToList("ban", name));

  const perModeEl = document.getElementById("perModeLists");
  if (perModeEl) {
    perModeEl.addEventListener("change", () => {
      perModeLists = perModeEl.checked;
      // Which bucket is on screen changes, so everything below re-renders.
      for (const kind of ["pick", "ban"]) {
        refreshRoleTabs(kind);
        renderList(kind);
      }
      saveSettingsDebounced();
    });
  }

  // Load champions with retry
  await loadChampionsWithRetry();

  // Toggle listeners
  autoAccept.addEventListener("change", () => {
    updateCardBodyStates();
    saveSettingsDebounced();
  });
  autoPick.addEventListener("change", () => {
    updateCardBodyStates();
    saveSettingsDebounced();
  });
  autoBan.addEventListener("change", () => {
    updateCardBodyStates();
    saveSettingsDebounced();
  });

  const avoidAllyHoverEl = document.getElementById("avoidAllyHover");
  if (avoidAllyHoverEl)
    avoidAllyHoverEl.addEventListener("change", () => saveSettingsDebounced());

  // Delay slider listeners
  [
    [acceptDelay, acceptDelayValue],
    [pickDelay, pickDelayValue],
    [banDelay, banDelayValue],
    [actionMargin, actionMarginValue],
  ].forEach(([slider, label]) => {
    slider.addEventListener("input", () => {
      label.textContent = formatDelay(slider.value);
      saveSettingsDebounced();
    });
  });

  // Overlay settings listeners
  const overlayEnabled = document.getElementById("overlayEnabled");
  if (overlayEnabled) overlayEnabled.addEventListener("change", () => {
    // Save immediately for overlay toggle (not debounced) to avoid losing state
    invoke("update_settings", { settings: collectSettings() });
  });
  const overlayOpacity = document.getElementById("overlayOpacity");
  const overlayOpacityValue = document.getElementById("overlayOpacityValue");
  if (overlayOpacity) {
    overlayOpacity.addEventListener("input", () => {
      overlayOpacityValue.textContent = parseFloat(overlayOpacity.value).toFixed(2);
      saveSettingsDebounced();
    });
  }

  // Sync settings listeners
  const syncEnabled = document.getElementById("syncEnabled");
  if (syncEnabled) syncEnabled.addEventListener("change", () => saveSettingsDebounced());
  const syncServerUrl = document.getElementById("syncServerUrl");
  if (syncServerUrl) syncServerUrl.addEventListener("blur", () => saveSettingsDebounced());

  minimizeToTray.addEventListener("change", () => saveSettingsDebounced());

  // Tabs
  document.querySelectorAll(".tb-tab").forEach((btn) => {
    btn.addEventListener("click", () => showTab(btn.dataset.tab));
  });

  setupAutoQueue();
  setupHelpToggle("pickHelpBtn", "pickHelp");
  setupHelpToggle("banHelpBtn", "banHelp");
  setupGamePanel();
  setupWindowControls();
  setupAppearanceControls();

  logClear.addEventListener("click", clearLog);
  refreshLogChrome();

  // Initial card body states
  updateCardBodyStates();
}

function setupWindowControls() {
  tbMinimize.addEventListener("click", () => appWindow.minimize());

  tbClose.addEventListener("click", async () => {
    // hide() never fires beforeunload, so flush pending settings by hand.
    await flushSettings();
    if (minimizeToTray.checked) {
      await appWindow.hide();
    } else {
      await appWindow.close();
    }
  });

  tbPin.addEventListener("click", () => {
    alwaysOnTop.checked = !alwaysOnTop.checked;
    applyAlwaysOnTop(alwaysOnTop.checked);
    saveSettingsDebounced();
  });
}

function applyAlwaysOnTop(on) {
  appWindow.setAlwaysOnTop(on);
  tbPin.classList.toggle("pinned", on);
  alwaysOnTop.checked = on;
}

function setupAppearanceControls() {
  themeSegmented.querySelectorAll("button").forEach((btn) => {
    btn.addEventListener("click", () => {
      applyTheme(btn.dataset.themeValue);
      saveSettingsDebounced();
    });
  });

  panelOpacity.addEventListener("input", () => {
    applyPanelOpacity(panelOpacity.value);
    saveSettingsDebounced();
  });

  alwaysOnTop.addEventListener("change", () => {
    applyAlwaysOnTop(alwaysOnTop.checked);
    saveSettingsDebounced();
  });

  appearanceReset.addEventListener("click", resetAppearance);
}

function applySettings(s) {
  autoAccept.checked = s.autoAccept;
  autoPick.checked = s.autoPick;
  autoBan.checked = s.autoBan;

  // Read before the lists: which bucket is on screen depends on it.
  const perModeEl = document.getElementById("perModeLists");
  perModeLists = s.perModeLists !== false;
  if (perModeEl) perModeEl.checked = perModeLists;

  recents.pick = Array.isArray(s.recentPicks) ? s.recentPicks.slice() : [];
  recents.ban = Array.isArray(s.recentBans) ? s.recentBans.slice() : [];

  // The backend has already migrated a pre-list settings.json into pickLists /
  // banLists, so there is nothing to read from the old scalar fields here.
  applyLists("pick", s.pickLists);
  applyLists("ban", s.banLists);

  const avoidAllyHoverEl = document.getElementById("avoidAllyHover");
  if (avoidAllyHoverEl) avoidAllyHoverEl.checked = s.avoidAllyHover !== false;

  const restoreFocusEl = document.getElementById("restoreFocus");
  if (restoreFocusEl) {
    restoreFocusEl.checked = s.restoreFocusAfterAction !== false;
  }
  const hoverPickEl = document.getElementById("hoverPick");
  if (hoverPickEl) {
    hoverPickEl.checked = s.hoverPick || false;
  }

  // Delays
  acceptDelay.value = s.acceptDelaySecs || 0;
  acceptDelayValue.textContent = formatDelay(acceptDelay.value);
  pickDelay.value = s.pickDelaySecs || 0;
  pickDelayValue.textContent = formatDelay(pickDelay.value);
  banDelay.value = s.banDelaySecs || 0;
  banDelayValue.textContent = formatDelay(banDelay.value);
  actionMargin.value = s.actionMarginSecs ?? 1.5;
  actionMarginValue.textContent = formatDelay(actionMargin.value);

  // Auto queue
  autoQueueTrigger = ["full", "members", "ready"].includes(s.autoQueueTrigger)
    ? s.autoQueueTrigger
    : "full";
  autoQueueMinMembers = Math.min(5, Math.max(1, parseInt(s.autoQueueMinMembers, 10) || 2));
  autoQueueDelaySecs = Math.min(10, Math.max(0, Number(s.autoQueueDelaySecs ?? 2)));
  setAutoQueue(s.autoQueue === true);

  // Overlay settings
  const overlayEnabled = document.getElementById("overlayEnabled");
  if (overlayEnabled) overlayEnabled.checked = s.overlayEnabled === true;
  const overlayOpacity = document.getElementById("overlayOpacity");
  const overlayOpacityValue = document.getElementById("overlayOpacityValue");
  if (overlayOpacity) {
    overlayOpacity.value = s.overlayOpacity ?? 0.8;
    overlayOpacityValue.textContent = parseFloat(overlayOpacity.value).toFixed(2);
  }

  // Sync settings
  const syncEnabled = document.getElementById("syncEnabled");
  if (syncEnabled) syncEnabled.checked = s.syncEnabled || false;
  const syncServerUrl = document.getElementById("syncServerUrl");
  if (syncServerUrl) syncServerUrl.value = s.syncServerUrl || "";

  // Appearance
  applyTheme(s.theme === "light" ? "light" : "dark");
  applyPanelOpacity(s.panelOpacity ?? 0.55);
  applyAlwaysOnTop(s.alwaysOnTop || false);
  minimizeToTray.checked = s.minimizeToTray !== false;

  updateCardBodyStates();
}

// Load the per-mode buckets, defensively: settings.json is a file the user can
// edit by hand, and a malformed entry must not take the whole UI down with it.
function applyLists(kind, incoming) {
  for (const key of Object.keys(modeLists[kind])) delete modeLists[kind][key];

  for (const [key, roleLists] of Object.entries(incoming || {})) {
    if (!roleLists || typeof roleLists !== "object") continue;
    const target = bucket(kind, key);
    for (const role of ROLES) {
      const entries = roleLists[role];
      if (!Array.isArray(entries)) continue;
      target[role] = entries
        // A bare string is what a hand-written settings.json is likeliest to
        // contain; accept it rather than dropping the entry.
        .map((e) => (typeof e === "string" ? { name: e } : e))
        .filter((e) => e && typeof e.name === "string" && e.name.trim())
        .map((e) => ({ name: e.name, once: !!e.once, disabled: !!e.disabled }));
    }
  }
  refreshRoleTabs(kind);
  renderList(kind);
}

function collectSettings() {
  const restoreFocusEl = document.getElementById("restoreFocus");
  return {
    autoAccept: autoAccept.checked,
    autoPick: autoPick.checked,
    autoBan: autoBan.checked,
    // pickChampion / banChampion are deliberately not sent. They are the
    // pre-list fields, the backend seeded the lists from them on load, and
    // omitting them here is what finally clears them from settings.json.
    pickLists: modeLists.pick,
    banLists: modeLists.ban,
    perModeLists,
    recentPicks: recents.pick,
    recentBans: recents.ban,
    avoidAllyHover: document.getElementById("avoidAllyHover")?.checked !== false,
    restoreFocusAfterAction: restoreFocusEl ? restoreFocusEl.checked : true,
    hoverPick: document.getElementById("hoverPick")?.checked || false,
    acceptDelaySecs: parseFloat(acceptDelay.value) || 0,
    pickDelaySecs: parseFloat(pickDelay.value) || 0,
    banDelaySecs: parseFloat(banDelay.value) || 0,
    actionMarginSecs: parseFloat(actionMargin.value) || 1.5,
    autoQueue: document.getElementById("autoQueue")?.checked === true,
    autoQueueTrigger,
    autoQueueMinMembers,
    autoQueueDelaySecs,
    overlayEnabled: document.getElementById("overlayEnabled")?.checked ?? false,
    overlayOpacity: parseFloat(document.getElementById("overlayOpacity")?.value) || 0.8,
    syncEnabled: document.getElementById("syncEnabled")?.checked || false,
    syncServerUrl: document.getElementById("syncServerUrl")?.value?.trim() || "ws://localhost:9876",
    theme: document.documentElement.dataset.theme === "light" ? "light" : "dark",
    panelOpacity: parseFloat(panelOpacity.value) || 0.55,
    alwaysOnTop: alwaysOnTop.checked,
    minimizeToTray: minimizeToTray.checked,
  };
}

function saveSettingsDebounced() {
  if (saveTimeout) clearTimeout(saveTimeout);
  saveTimeout = setTimeout(async () => {
    saveTimeout = null;
    await invoke("update_settings", { settings: collectSettings() });
  }, 300);
}

// Force a pending debounced save through immediately.
async function flushSettings() {
  if (saveTimeout) clearTimeout(saveTimeout);
  saveTimeout = null;
  await invoke("update_settings", { settings: collectSettings() });
}

// Save immediately on close so debounced changes aren't lost
window.addEventListener("beforeunload", () => {
  if (saveTimeout) {
    clearTimeout(saveTimeout);
    invoke("update_settings", { settings: collectSettings() });
  }
});

async function loadChampions() {
  const entries = await invoke("get_champion_options");
  if (entries.length === 0) return false;
  // Precompute the normalized form and its index map once, not per keystroke
  champOptions = entries.map((c) => ({ ...c, ...normalizeWithMap(c.name) }));
  // Let the pickers repaint their avatar / validity now that names are known
  [pickChampion, banChampion].forEach((el) =>
    el.dispatchEvent(new Event("champions-ready"))
  );
  // Same for the lists: their icons and their "this name resolves to nothing"
  // flag both depend on the roster.
  renderList("pick");
  renderList("ban");
  return true;
}

async function loadChampionsWithRetry() {
  for (let i = 0; i < 10; i++) {
    if (await loadChampions()) return;
    await new Promise((r) => setTimeout(r, 2000));
  }
}

// The launch button depends on two things that resolve at different times:
// whether the LCU is up, and whether there is a Riot Client to drive at all.
let lcuConnected = false;
let canLaunchLeague = false;

function updateLaunchButton() {
  launchLol.hidden = lcuConnected || !canLaunchLeague;
}

function setConnected(connected) {
  lcuConnected = connected;
  updateLaunchButton();

  if (connected) {
    statusDot.classList.add("connected");
    statusText.textContent = "Connectat";
  } else {
    statusDot.classList.remove("connected");
    statusText.textContent = "Desconnectat";
  }
  // Titlebar rule mirrors the connection, like the reference's running state
  titlebar.classList.toggle("running", connected);
  titlebar.classList.toggle("off", !connected);
}

function applyQueueMode(payload) {
  if (!payload) {
    delete document.body.dataset.queueMode;
    modeIndicator.hidden = true;
    currentGameMode = null;
    currentQueueName = null;
  } else {
    document.body.dataset.queueMode = payload.gameMode;
    modeName.textContent = payload.displayName;
    modeIndicator.hidden = false;
    currentGameMode = payload.gameMode;
    currentQueueName = payload.displayName || null;
  }

  // The mode decides which bucket is on screen, so the lists follow it.
  for (const kind of ["pick", "ban"]) {
    refreshRoleTabs(kind);
    renderList(kind);
  }
  updateCardBodyStates();
}

function logSeverity(text) {
  if (text.includes("Error")) return "error";
  if (text.includes("connectat") && !text.includes("des")) return "connect";
  if (text.includes("desconnectat")) return "disconnect";
  return "action";
}

function countLogEntries() {
  return logEl.querySelectorAll(".log-entry").length;
}

function refreshLogChrome() {
  const n = countLogEntries();
  logEmpty.hidden = n > 0;
  logCount.textContent =
    n === 0 ? "Cap entrada" : n === 1 ? "1 entrada" : `${n} entrades`;
  logClear.disabled = n === 0;
}

function clearLog() {
  logEl.querySelectorAll(".log-entry").forEach((e) => e.remove());
  refreshLogChrome();
}

function addLog(text) {
  const entry = document.createElement("div");
  entry.className = `log-entry ${logSeverity(text)}`;

  const time = document.createElement("span");
  time.className = "log-time";
  time.textContent = new Date().toLocaleTimeString("ca", {
    hour: "2-digit",
    minute: "2-digit",
    second: "2-digit",
  });

  const msg = document.createElement("span");
  msg.className = "log-msg";
  msg.textContent = text;

  entry.append(time, msg);
  logEl.appendChild(entry);
  logEl.scrollTop = logEl.scrollHeight;

  // Cap the history, but only ever drop entries - the empty-state node lives
  // in the same container and must survive.
  let entries = logEl.querySelectorAll(".log-entry");
  while (entries.length > 50) {
    entries[0].remove();
    entries = logEl.querySelectorAll(".log-entry");
  }

  // Deliberately not mirrored into the statusbar. A past event parked next to
  // the live connection state reads as live state - "Buscant partida" sat there
  // through a whole champ select. The history is one tab away.
  refreshLogChrome();
}


// ── Auto queue ──
//
// The gate itself lives in the backend (queue.rs); this is only the settings UI
// and the reason line it reports back.

let autoQueueTrigger = "full";
let autoQueueMinMembers = 2;
let autoQueueDelaySecs = 2;

// The phases in which starting a search is a thing that can happen. Mirrors
// `Snapshot::is_searching` on the Rust side.
const QUEUEABLE_PHASES = ["", "None", "Lobby", "Matchmaking", "ReadyCheck"];

// The current lobby capacity, so the "N membres" choice cannot offer a number
// this queue will never reach.
let lobbyMaxSize = 0;

// Every auto-queue control lives on Principal, in the card that only appears
// when you are the lobby leader — the one moment any of it matters, and the one
// place you are already looking. Nothing about it is in Ajustos.
function setAutoQueue(on) {
  const sw = document.getElementById("autoQueue");
  if (sw) sw.checked = on;
  refreshAutoQueueUI();
}

// The Principal card, driven by the same `client-state` payload as the Partida
// tab. Hidden outright unless we are the leader of an open lobby: nobody else
// can start a search, so for them it would be a control that cannot act.
function renderQueueCard(state) {
  const card = document.getElementById("queueCard");
  if (!card) return;

  const lobby = state?.lobby;
  const leader = !!lobby?.iAmLeader;

  const capacity = lobby?.maxSize || 0;
  if (capacity !== lobbyMaxSize) {
    lobbyMaxSize = capacity;
    refreshAutoQueueUI();
  }
  const searching = state?.search?.searching === true;

  // The lobby survives into champ select in most queues, so being the leader is
  // not enough: outside the phases where a search can actually be started this
  // is a control that cannot act, and it was reporting the phase back at you in
  // the middle of a draft.
  const queueable = QUEUEABLE_PHASES.includes(state?.phase || "None");
  card.hidden = !leader || !queueable;
  if (card.hidden) return;

  // Arena reports maxLobbySize 18 while you queue solo or as a duo, so the
  // denominator is only shown when it is a number that can actually be reached.
  const count = document.getElementById("queueCardMode");
  if (count) {
    const max = lobby.maxSize;
    count.textContent =
      max > 0 && max <= 5 ? `${lobby.members.length}/${max}` : `${lobby.members.length}`;
    count.hidden = false;
  }

  const gate = document.getElementById("queueGateMain");
  if (!gate) return;

  if (searching) {
    gate.textContent = "Buscant partida.";
    gate.className = "info-gate good";
  } else if (state.queue?.autoQueue) {
    gate.textContent = state.queue.gate || "";
    gate.className = state.queue.ready ? "info-gate good" : "info-gate";
  } else {
    gate.textContent = "Activa-ho i InstaLock encuarà sol quan toqui.";
    gate.className = "info-gate muted";
  }
}

function refreshAutoQueueUI() {
  const on = document.getElementById("autoQueue")?.checked;

  // Collapsed while off: four controls under a switch that is not on read as
  // settings that are doing something.
  const config = document.getElementById("queueConfig");
  if (config) config.hidden = !on;

  document
    .getElementById("autoQueueTrigger")
    ?.querySelectorAll("button")
    .forEach((btn) => {
      btn.classList.toggle("active", btn.dataset.trigger === autoQueueTrigger);
    });

  // Only "N membres" has a number to set; offering it for the other two would
  // invite setting something that is never read.
  const minRow = document.getElementById("autoQueueMinRow");
  if (minRow) minRow.hidden = autoQueueTrigger !== "members";

  renderMinMembers();
  renderQueueDelay();
}

// A small choice is small buttons, not a slider: the slider needed a label, a
// track and a readout - three times the height for one digit.
//
// The range is the lobby's own capacity, not a fixed 1-5. SoloQ caps at 2, so
// offering 3, 4 and 5 there is offering a condition that can never be met.
// Arena is the exception in the other direction: it reports 16-18 while you
// queue solo or as a duo, so the ceiling stays at 5.
const MAX_MIN_MEMBERS = 5;

function minMembersCap() {
  return Math.max(1, Math.min(MAX_MIN_MEMBERS, lobbyMaxSize > 0 ? lobbyMaxSize : MAX_MIN_MEMBERS));
}

function renderMinMembers() {
  const host = document.getElementById("autoQueueMin");
  if (!host) return;
  host.replaceChildren();

  const cap = minMembersCap();
  // A stored value the current lobby cannot reach would sit there looking
  // selected while the gate never fired.
  if (autoQueueMinMembers > cap) {
    autoQueueMinMembers = cap;
    saveSettingsDebounced();
  }

  for (let n = 1; n <= cap; n++) {
    const btn = document.createElement("button");
    btn.type = "button";
    btn.textContent = n;
    btn.classList.toggle("active", n === autoQueueMinMembers);
    btn.title = n === 1 ? "Encua tot sol" : `Encua amb ${n} o més`;
    btn.addEventListener("click", () => {
      autoQueueMinMembers = n;
      refreshAutoQueueUI();
      flushSettings();
    });
    host.append(btn);
  }
}

function renderQueueDelay() {
  const out = document.getElementById("autoQueueDelayValue");
  if (out) out.textContent = formatDelay(autoQueueDelaySecs);
}

// A "?" in a card header that shows and hides an explanation inside the card.
// Everything explanatory in this window works this way: written once, read
// once, and then out of the way rather than parked on screen forever.
function setupHelpToggle(buttonId, targetId) {
  const button = document.getElementById(buttonId);
  const target = document.getElementById(targetId);
  if (!button || !target) return;

  button.addEventListener("click", () => {
    target.hidden = !target.hidden;
    button.classList.toggle("on", !target.hidden);
    button.setAttribute("aria-expanded", String(!target.hidden));
  });
}

function setupAutoQueue() {
  document.getElementById("autoQueue")?.addEventListener("change", (e) => {
    setAutoQueue(e.target.checked);
    // Not debounced: the backend re-evaluates the gate on the settings write,
    // and a 300 ms wait to start queueing is a strange thing to sit through.
    flushSettings();
  });

  document.getElementById("autoQueueTrigger")?.querySelectorAll("button").forEach((btn) => {
    btn.addEventListener("click", () => {
      autoQueueTrigger = btn.dataset.trigger;
      refreshAutoQueueUI();
      flushSettings();
    });
  });

  // The trigger explanations are read once and then only take up room, so they
  // sit behind the "?" rather than under the control forever.
  setupHelpToggle("queueHelp", "queueHelpBox");

  document.querySelectorAll(".stepper button[data-step]").forEach((btn) => {
    btn.addEventListener("click", () => {
      const next = autoQueueDelaySecs + parseFloat(btn.dataset.step);
      autoQueueDelaySecs = Math.min(10, Math.max(0, Math.round(next * 2) / 2));
      renderQueueDelay();
      saveSettingsDebounced();
    });
  });
}

// ── Partida panel ──
//
// Read-only mirror of what the client is doing, fed by the `client-state`
// event. The point of it is that pick and ban stop being a black box: the last
// decision section says which champion was taken and what it stepped over.

const PROFILE_ICON_BASE =
  "https://raw.communitydragon.org/latest/plugins/rcp-be-lol-game-data/global/default/v1/profile-icons";

const ROLE_LABELS = {
  top: "Top",
  jungle: "Jungla",
  middle: "Mid",
  bottom: "Bot",
  utility: "Support",
};

function roleLabel(position) {
  if (!position) return "";
  return ROLE_LABELS[position.toLowerCase()] ?? position;
}

// "1:05". Queue times are read at a glance, so seconds alone stop being useful
// past a minute.
function clock(seconds) {
  const total = Math.max(0, Math.round(seconds || 0));
  const m = Math.floor(total / 60);
  const s = total % 60;
  return m > 0 ? `${m}:${String(s).padStart(2, "0")}` : `${s}s`;
}

function mkEl(tag, className, text) {
  const node = document.createElement(tag);
  if (className) node.className = className;
  if (text !== undefined) node.textContent = text;
  return node;
}

const PHASE_LABELS = {
  None: "Al client",
  Lobby: "A la sala",
  Matchmaking: "Buscant partida",
  ReadyCheck: "Acceptant",
  ChampSelect: "Champion select",
  InProgress: "En partida",
  WaitingForStats: "Acabant",
  PreEndOfGame: "Acabant",
  EndOfGame: "Final de partida",
  Reconnect: "Reconnectant",
};

function renderGame(state) {
  const body = document.getElementById("gameBody");
  const empty = document.getElementById("gameEmpty");
  if (!body || !empty) return;

  // Nothing to show is a real state, not a bug — say so rather than render four
  // empty cards.
  const idle = !state || !state.connected;
  body.classList.toggle("hidden", idle);
  empty.classList.toggle("hidden", !idle);
  if (idle) {
    renderQueueCard(null);
    return;
  }

  // The mode arrives with the state as well as on its own event: the event
  // fires on change, so a window that opened between two of them would never
  // learn the mode. Only applied on a real change - it re-renders both lists.
  const mode = state.queueMode;
  if ((mode?.gameMode || null) !== currentGameMode) applyQueueMode(mode);

  renderQueueCard(state);
  renderQueueSection(state);
  renderLobbySection(state.lobby);
  renderDraftSection(state.champSelect);
  renderDecisionSection(state.decision);
}

function renderQueueSection(state) {
  const phase = state.phase || "None";
  document.getElementById("gamePhase").textContent = PHASE_LABELS[phase] || phase;

  const gate = document.getElementById("queueGate");
  const searching = state.search?.searching === true;

  // The auto-queue reason only means something where queueing is possible. In
  // champ select or in game it would read as a complaint about nothing.
  const queueable = QUEUEABLE_PHASES.includes(phase);
  gate.hidden = !queueable;
  document.querySelector("#gameBody .info-actions").hidden = !queueable;

  if (searching) {
    gate.textContent = "A la cua.";
    gate.className = "info-gate";
  } else if (state.queue?.autoQueue) {
    gate.textContent = state.queue.gate || "";
    // "Ready" is the gate saying it would queue; anything else is a reason.
    gate.className = state.queue.ready ? "info-gate good" : "info-gate";
  } else {
    gate.textContent = "La cua automàtica està desactivada.";
    gate.className = "info-gate muted";
  }

  const searchRow = document.getElementById("searchRow");
  searchRow.hidden = !searching;
  if (searching) {
    const waited = clock(state.search.timeInQueue);
    const estimate =
      state.search.estimated > 0 ? ` · estimat ${clock(state.search.estimated)}` : "";
    document.getElementById("searchTime").textContent = waited + estimate;
  }

  // Only offer the buttons where they can do something: outside a lobby the
  // client has nothing to search for.
  const inLobby = !!state.lobby && phase === "Lobby";
  document.getElementById("queueStart").classList.toggle("hidden", searching || !inLobby);
  document.getElementById("queueCancel").classList.toggle("hidden", !searching);
}

function renderLobbySection(lobby) {
  const card = document.getElementById("lobbyCard");
  card.classList.toggle("hidden", !lobby);
  if (!lobby) return;

  const max = lobby.maxSize > 0 ? `/${lobby.maxSize}` : "";
  document.getElementById("lobbyCount").textContent = `${lobby.members.length}${max}`;

  const list = document.getElementById("lobbyMembers");
  list.replaceChildren();

  for (const member of lobby.members) {
    const row = mkEl("div", "member");

    const icon = mkEl("span", "member-icon");
    if (member.iconId > 0) {
      icon.style.backgroundImage = `url("${PROFILE_ICON_BASE}/${member.iconId}.jpg")`;
    }

    const text = mkEl("span", "member-text");
    const nameRow = mkEl("span", "member-name-row");
    const name = mkEl("span", "member-name", member.name || "Sense nom");
    // The tag disambiguates two people with the same game name, but it is
    // noise in a five-row list, so it lives in the tooltip.
    if (member.fullName) name.title = member.fullName;
    nameRow.append(name);
    if (member.isLeader) {
      const lead = mkEl("span", "member-tag lead", "Líder");
      lead.title = "És qui pot buscar partida";
      nameRow.append(lead);
    }
    if (member.isSpectator) nameRow.append(mkEl("span", "member-tag", "Espectador"));
    text.append(nameRow);

    const detail = [];
    if (member.level > 0) detail.push(`Nivell ${member.level}`);
    const positions = (member.positions || "")
      .split(" / ")
      .map(roleLabel)
      .filter(Boolean)
      .join(" / ");
    if (positions) detail.push(positions);
    if (detail.length) text.append(mkEl("span", "member-detail", detail.join(" · ")));

    row.append(icon, text);

    // Readiness only means something where there is a ready-up step; elsewhere
    // the client sets it for anyone who can queue, so it is shown as a quiet
    // dot rather than as a claim.
    const dot = mkEl("span", "member-ready");
    dot.classList.toggle("on", member.ready === true);
    dot.title = member.ready ? "Ready" : "No ready";
    row.append(dot);

    list.append(row);
  }
}

function renderDraftSection(draft) {
  const card = document.getElementById("draftCard");
  card.classList.toggle("hidden", !draft);
  if (!draft) return;

  const role = roleLabel(draft.assignedPosition);
  const left = Number.isFinite(draft.timeLeft) ? clock(draft.timeLeft) : "";
  document.getElementById("draftTimer").textContent =
    [role, left].filter(Boolean).join(" · ") || "—";

  renderTeam("myTeam", draft.myTeam, true);
  renderTeam("theirTeam", draft.theirTeam, false);
  renderBans("myBans", draft.myBans);
  renderBans("theirBans", draft.theirBans);
}

function renderTeam(id, players, mine) {
  const list = document.getElementById(id);
  list.replaceChildren();

  for (const player of players || []) {
    const row = mkEl("div", "cell");
    if (player.isMe) row.classList.add("me");

    // A locked champion is the truth; a hover is an intent and is shown as one.
    const locked = player.championId > 0;
    const name = player.championName || player.intentName;

    const icon = mkEl("span", "cell-icon");
    const iconId = locked ? player.championId : player.intentId;
    if (iconId > 0) icon.style.backgroundImage = `url("${championIconUrl(iconId)}")`;
    if (!locked) icon.classList.add("hovering");

    const label = mkEl("span", "cell-name", name || (mine ? "Sense triar" : "—"));
    if (!name) label.classList.add("pending");

    row.append(icon, label);

    if (player.position) row.append(mkEl("span", "cell-role", roleLabel(player.position)));
    if (player.isAutofilled) {
      const tag = mkEl("span", "cell-tag", "auto");
      tag.title = "Autofill";
      row.append(tag);
    }
    if (!locked && (player.intentId || 0) > 0) {
      const tag = mkEl("span", "cell-tag", "hover");
      tag.title = "Encara no l'ha lockejat";
      row.append(tag);
    }

    list.append(row);
  }
}

function renderBans(id, bans) {
  const target = document.getElementById(id);
  target.replaceChildren();
  if (!bans || !bans.length) return;

  target.append(mkEl("span", "bans-label", "Bans"));
  for (const ban of bans) {
    const icon = mkEl("span", "ban-icon");
    icon.style.backgroundImage = `url("${championIconUrl(ban.championId)}")`;
    icon.title = ban.championName || "";
    target.append(icon);
  }
}

function renderDecisionSection(decision) {
  const card = document.getElementById("decisionCard");
  card.classList.toggle("hidden", !decision);
  if (!decision) return;

  document.getElementById("decisionWhat").textContent =
    decision.what === "ban" ? "Ban" : "Pick";

  const body = document.getElementById("decisionBody");
  body.replaceChildren();

  if (decision.chosen) {
    const line = mkEl("div", "decision-chosen");
    const icon = mkEl("span", "cell-icon");
    // A Bravery decision carries the LCU sentinel, not a champion id.
    paintChampIcon(icon, findChampion(decision.chosen) || { id: decision.championId });
    line.append(icon, mkEl("span", "cell-name", decision.chosen));
    body.append(line);
  } else {
    body.append(mkEl("p", "decision-none", "Cap candidat de la llista era utilitzable."));
  }

  // The whole reason this section exists: naming what was skipped and why.
  for (const skipped of decision.passedOver || []) {
    const line = mkEl("div", "decision-skip");
    line.append(mkEl("span", "decision-skip-name", skipped.name));
    line.append(mkEl("span", "decision-skip-why", skipped.reason));
    body.append(line);
  }
}

function setupGamePanel() {
  document.getElementById("queueStart")?.addEventListener("click", async () => {
    try {
      await invoke("start_queue");
    } catch (e) {
      addLog(`Error encuant: ${e}`);
    }
  });

  document.getElementById("queueCancel")?.addEventListener("click", async () => {
    try {
      await invoke("cancel_queue");
    } catch (e) {
      addLog(`Error cancel·lant: ${e}`);
    }
  });

  // Event-fed, but the window can open between two events. Ask once so a live
  // lobby is on screen from the first paint.
  invoke("get_client_state")
    .then(renderGame)
    .catch(() => renderGame(null));
}

init();
