const { invoke } = window.__TAURI__.core;
const { listen } = window.__TAURI__.event;

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

// State
let championNames = [];
let saveTimeout = null;

// Normalize: same logic as Rust backend
function normalize(name) {
  return name.toLowerCase().replace(/['\s.]/g, "").replace(/&/g, "and");
}

function fuzzyMatch(champions, query) {
  if (!query) return [];
  const q = normalize(query);
  return champions.filter((name) => normalize(name).includes(q)).slice(0, 8);
}

// Autocomplete setup
function setupAutocomplete(input, dropdown) {
  let activeIdx = -1;

  function show(matches) {
    dropdown.innerHTML = "";
    if (matches.length === 0) {
      dropdown.classList.remove("open");
      return;
    }
    activeIdx = -1;
    for (const name of matches) {
      const div = document.createElement("div");
      div.className = "champ-option";
      div.textContent = name;
      div.addEventListener("mousedown", (e) => {
        e.preventDefault();
        input.value = name;
        dropdown.classList.remove("open");
        saveSettingsDebounced();
      });
      dropdown.appendChild(div);
    }
    dropdown.classList.add("open");
  }

  input.addEventListener("input", () => {
    show(fuzzyMatch(championNames, input.value));
  });

  input.addEventListener("focus", () => {
    if (input.value) {
      show(fuzzyMatch(championNames, input.value));
    }
  });

  input.addEventListener("blur", () => {
    // Small delay so mousedown on option fires first
    setTimeout(() => dropdown.classList.remove("open"), 150);
  });

  input.addEventListener("keydown", (e) => {
    const options = dropdown.querySelectorAll(".champ-option");
    if (!options.length) return;

    if (e.key === "ArrowDown") {
      e.preventDefault();
      activeIdx = Math.min(activeIdx + 1, options.length - 1);
    } else if (e.key === "ArrowUp") {
      e.preventDefault();
      activeIdx = Math.max(activeIdx - 1, 0);
    } else if (e.key === "Enter") {
      e.preventDefault();
      // If one is highlighted use that, otherwise auto-select if only one match
      const pick = activeIdx >= 0 ? options[activeIdx] : (options.length === 1 ? options[0] : null);
      if (pick) {
        input.value = pick.textContent;
        dropdown.classList.remove("open");
        saveSettingsDebounced();
      }
      return;
    } else if (e.key === "Escape") {
      dropdown.classList.remove("open");
      return;
    } else {
      return;
    }

    options.forEach((o, i) => o.classList.toggle("active", i === activeIdx));
    if (activeIdx >= 0) options[activeIdx].scrollIntoView({ block: "nearest" });
  });
}

// Initialize
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

  // Load autostart state
  const autoStartEl = document.getElementById("autoStart");
  autoStartEl.checked = await invoke("get_autostart");
  autoStartEl.addEventListener("change", () => {
    invoke("set_autostart", { enabled: autoStartEl.checked });
  });

  // Focus restore toggle
  const restoreFocusEl = document.getElementById("restoreFocus");
  restoreFocusEl.addEventListener("change", () => saveSettingsDebounced());

  // Setup autocompletes
  setupAutocomplete(pickChampion, pickDropdown);
  setupAutocomplete(banChampion, banDropdown);

  // Load champions with retry
  await loadChampionsWithRetry();

  // Toggle listeners
  autoAccept.addEventListener("change", () => saveSettingsDebounced());
  autoPick.addEventListener("change", () => saveSettingsDebounced());
  autoBan.addEventListener("change", () => saveSettingsDebounced());
  braveryEnabled.addEventListener("change", () => saveSettingsDebounced());

  // Save champion inputs on blur/enter (covers manual typing without dropdown)
  pickChampion.addEventListener("blur", () => saveSettingsDebounced());
  banChampion.addEventListener("blur", () => saveSettingsDebounced());
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
  };
}

function saveSettingsDebounced() {
  if (saveTimeout) clearTimeout(saveTimeout);
  saveTimeout = setTimeout(async () => {
    await invoke("update_settings", { settings: collectSettings() });
  }, 300);
}

async function loadChampions() {
  const names = await invoke("get_champions");
  if (names.length === 0) return false;
  championNames = names;
  return true;
}

async function loadChampionsWithRetry() {
  for (let i = 0; i < 10; i++) {
    if (await loadChampions()) return;
    await new Promise((r) => setTimeout(r, 2000));
  }
}

function setConnected(connected) {
  if (connected) {
    statusDot.classList.add("connected");
    statusText.textContent = "Connectat";
  } else {
    statusDot.classList.remove("connected");
    statusText.textContent = "Desconnectat";
  }
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

function addLog(text) {
  const entry = document.createElement("div");
  entry.className = "log-entry";

  if (text.includes("Error")) {
    entry.classList.add("error");
  } else if (text.includes("connectat") && !text.includes("des")) {
    entry.classList.add("connect");
  } else if (text.includes("desconnectat")) {
    entry.classList.add("disconnect");
  } else {
    entry.classList.add("action");
  }

  const time = new Date().toLocaleTimeString("ca", {
    hour: "2-digit",
    minute: "2-digit",
    second: "2-digit",
  });
  entry.textContent = `${time}  ${text}`;
  logEl.appendChild(entry);
  logEl.scrollTop = logEl.scrollHeight;

  while (logEl.children.length > 50) {
    logEl.removeChild(logEl.firstChild);
  }
}

init();
