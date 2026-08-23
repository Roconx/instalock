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
const braveryEnabled = document.getElementById("braveryEnabled");
const pickChampion = document.getElementById("pickChampion");
const banChampion = document.getElementById("banChampion");
const pickDropdown = document.getElementById("pickDropdown");
const banDropdown = document.getElementById("banDropdown");
const logEl = document.getElementById("log");
const logEmpty = document.getElementById("logEmpty");
const logCount = document.getElementById("logCount");
const logClear = document.getElementById("logClear");
const sbLastLog = document.getElementById("sbLastLog");
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
  settings: document.getElementById("settingsView"),
  appearance: document.getElementById("appearanceView"),
  log: document.getElementById("logView"),
};

// State
let champOptions = [];
let saveTimeout = null;

// Normalize: same logic as Rust backend
function normalize(name) {
  return name.toLowerCase().replace(/['\s.]/g, "").replace(/&/g, "and");
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

function championIconUrl(id) {
  return `${CHAMP_ICON_BASE}/${id}.png`;
}

// Rank matches: exact name, then prefix, then substring; ties alphabetically.
// The old plain `includes` filter put "Lee Sin" above "Sion" for "si".
function rankChampions(query) {
  if (!query.trim()) return champOptions.slice();
  const q = normalize(query);
  if (!q) return champOptions.slice();

  const scored = [];
  for (const champ of champOptions) {
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
  return champOptions.find((c) => c.norm === q) || null;
}

// Format delay value
function formatDelay(val) {
  const n = parseFloat(val);
  return n % 1 === 0 ? n + "s" : n.toFixed(1) + "s";
}

// Autocomplete setup
function setupAutocomplete(input, dropdown) {
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
    const bad = strict ? !champ : rankChampions(value).length === 0;
    wrapper.classList.toggle("invalid", value.length > 0 && known && bad);

    avatar.style.backgroundImage = champ ? `url("${championIconUrl(champ.id)}")` : "";
  }

  function optionEl(champ) {
    const row = document.createElement("div");
    row.className = "champ-option";

    const icon = document.createElement("span");
    icon.className = "champ-option-icon";
    icon.style.backgroundImage = `url("${championIconUrl(champ.id)}")`;

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
    show(rankChampions(input.value));
  });

  // Focusing opens the list instead of nothing. When the field already holds a
  // valid champion there is nothing left to narrow, so show the whole roster
  // and preselect the text so typing replaces it.
  input.addEventListener("focus", () => {
    const exact = findChampion(input.value);
    if (exact) input.select();
    show(rankChampions(exact ? "" : input.value));
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
      show(rankChampions(input.value));
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

// Toggle card-body disabled state + bravery exclusivity
function updateCardBodyStates() {
  document.querySelectorAll(".card-body[data-toggle]").forEach((body) => {
    const toggleId = body.dataset.toggle;
    const checkbox = document.getElementById(toggleId);
    if (checkbox) {
      body.classList.toggle("disabled", !checkbox.checked);
    }
  });
  // Bravery disabled when auto pick is OFF. It only applies to Arena, so the
  // champion input stays visible: it is what gets picked in every other mode.
  braveryEnabled.disabled = !autoPick.checked;
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

  // Setup autocompletes
  setupAutocomplete(pickChampion, pickDropdown);
  setupAutocomplete(banChampion, banDropdown);

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
  braveryEnabled.addEventListener("change", () => {
    updateCardBodyStates();
    saveSettingsDebounced();
  });

  // Save champion inputs on blur
  pickChampion.addEventListener("blur", () => saveSettingsDebounced());
  banChampion.addEventListener("blur", () => saveSettingsDebounced());

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
  braveryEnabled.checked = s.braveryEnabled;
  pickChampion.value = s.pickChampion || "";
  banChampion.value = s.banChampion || "";

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

  // Overlay settings
  const overlayEnabled = document.getElementById("overlayEnabled");
  if (overlayEnabled) overlayEnabled.checked = s.overlayEnabled !== false;
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

function collectSettings() {
  const restoreFocusEl = document.getElementById("restoreFocus");
  return {
    autoAccept: autoAccept.checked,
    autoPick: autoPick.checked,
    autoBan: autoBan.checked,
    braveryEnabled: braveryEnabled.checked,
    pickChampion: pickChampion.value.trim(),
    banChampion: banChampion.value.trim(),
    restoreFocusAfterAction: restoreFocusEl ? restoreFocusEl.checked : true,
    hoverPick: document.getElementById("hoverPick")?.checked || false,
    acceptDelaySecs: parseFloat(acceptDelay.value) || 0,
    pickDelaySecs: parseFloat(pickDelay.value) || 0,
    banDelaySecs: parseFloat(banDelay.value) || 0,
    actionMarginSecs: parseFloat(actionMargin.value) || 1.5,
    overlayEnabled: document.getElementById("overlayEnabled")?.checked ?? true,
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
    return;
  }
  document.body.dataset.queueMode = payload.gameMode;
  modeName.textContent = payload.displayName;
  modeIndicator.hidden = false;
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
  sbLastLog.textContent = "";
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

  // Mirror the latest line into the statusbar; the full history stays in the tab
  sbLastLog.textContent = text;
  refreshLogChrome();
}

init();
