const { invoke } = window.__TAURI__.core;
const { listen } = window.__TAURI__.event;
const { getCurrentWindow } = window.__TAURI__.window;

const overlayEl = document.getElementById("overlay");
const enemiesEl = document.getElementById("enemies");
const dragHandle = document.getElementById("dragHandle");

// Timer state: [enemy_idx][spell_idx] = { endTime, intervalId }
let timers = {};

// Community Dragon CDN base for icons
const CD_BASE = "https://raw.communitydragon.org/latest/plugins/rcp-be-lol-game-data/global/default/v1";

function champIconUrl(championId, championName) {
  if (championId && championId > 0) {
    return `${CD_BASE}/champion-icons/${championId}.png`;
  }
  // Fallback: use ddragon by champion name
  return `https://ddragon.leagueoflegends.com/cdn/img/champion/tiles/${championName}_0.jpg`;
}

// Spell icon URLs from Community Dragon (paths match summoner-spells.json iconPath)
const CD_SPELL = "https://raw.communitydragon.org/latest/plugins/rcp-be-lol-game-data/global/default/data/spells/icons2d";
const SPELL_ICONS = {
  1: `${CD_SPELL}/summoner_boost.png`,        // Cleanse
  3: `${CD_SPELL}/summoner_exhaust.png`,      // Exhaust
  4: `${CD_SPELL}/summoner_flash.png`,        // Flash
  6: `${CD_SPELL}/summoner_haste.png`,        // Ghost
  7: `${CD_SPELL}/summoner_heal.png`,         // Heal
  11: `${CD_SPELL}/summoner_smite.png`,       // Smite
  12: `${CD_SPELL}/summoner_teleport.png`,     // Teleport
  14: `${CD_SPELL}/summoner_ignite.png`,      // Ignite
  21: `${CD_SPELL}/summoner_barrier.png`,     // Barrier
  32: `${CD_SPELL}/summoner_mark.png`,        // Mark (ARAM)
};

function formatTime(secs) {
  if (secs <= 0) return "";
  const m = Math.floor(secs / 60);
  const s = secs % 60;
  return m > 0 ? `${m}:${String(s).padStart(2, "0")}` : `${s}`;
}

function timerKey(enemyIdx, spellIdx) {
  return `${enemyIdx}-${spellIdx}`;
}

function startTimer(enemyIdx, spellIdx, cooldownSecs, startedAt) {
  const key = timerKey(enemyIdx, spellIdx);

  // Cancel existing timer if any
  if (timers[key]) {
    clearInterval(timers[key].intervalId);
  }

  const now = Math.floor(Date.now() / 1000);
  const elapsed = startedAt ? now - startedAt : 0;
  const remaining = Math.max(0, cooldownSecs - elapsed);

  if (remaining <= 0) return;

  const endTime = Date.now() + remaining * 1000;

  const spellEl = document.querySelector(`[data-enemy="${enemyIdx}"][data-spell="${spellIdx}"]`);
  if (!spellEl) return;

  spellEl.classList.add("on-cooldown");
  const timerEl = spellEl.querySelector(".spell-timer");

  function update() {
    const left = Math.max(0, Math.ceil((endTime - Date.now()) / 1000));
    timerEl.textContent = formatTime(left);
    if (left <= 0) {
      clearInterval(timers[key].intervalId);
      delete timers[key];
      spellEl.classList.remove("on-cooldown");
      spellEl.classList.add("flash-ready");
      setTimeout(() => spellEl.classList.remove("flash-ready"), 600);
      timerEl.textContent = "";
    }
  }

  const intervalId = setInterval(update, 1000);
  timers[key] = { endTime, intervalId };
  update();
}

function cancelTimer(enemyIdx, spellIdx) {
  const key = timerKey(enemyIdx, spellIdx);
  if (timers[key]) {
    clearInterval(timers[key].intervalId);
    delete timers[key];
  }
  const spellEl = document.querySelector(`[data-enemy="${enemyIdx}"][data-spell="${spellIdx}"]`);
  if (spellEl) {
    spellEl.classList.remove("on-cooldown");
    spellEl.querySelector(".spell-timer").textContent = "";
  }
}

function renderEnemies(enemies) {
  enemiesEl.innerHTML = "";

  if (!enemies || enemies.length === 0) {
    enemiesEl.innerHTML = '<div class="loading">Esperant dades...</div>';
    return;
  }

  enemies.forEach((enemy, idx) => {
    const row = document.createElement("div");
    row.className = "enemy-row";

    // Champion icon
    const champDiv = document.createElement("div");
    champDiv.className = "champ-icon";
    const champImg = document.createElement("img");
    champImg.src = champIconUrl(enemy.championId, enemy.championName);
    champImg.alt = enemy.championName;
    champImg.onerror = () => { champImg.style.display = "none"; };
    champDiv.appendChild(champImg);

    // Champion name
    const nameDiv = document.createElement("div");
    nameDiv.className = "champ-name";
    nameDiv.textContent = enemy.championName;
    nameDiv.title = enemy.championName;

    // Rune badges
    const badges = [];
    if (enemy.hasCosmicInsight) badges.push("CI");
    if (enemy.hasUnsealedSpellbook) badges.push("SB");

    row.appendChild(champDiv);
    row.appendChild(nameDiv);

    if (badges.length > 0) {
      const badgeEl = document.createElement("span");
      badgeEl.className = "rune-badge";
      badgeEl.textContent = badges.join(" ");
      badgeEl.title = [
        enemy.hasCosmicInsight ? "Cosmic Insight (-18s)" : "",
        enemy.hasUnsealedSpellbook ? "Unsealed Spellbook" : "",
      ].filter(Boolean).join(", ");
      row.appendChild(badgeEl);
    }

    // Spells
    const spellsDiv = document.createElement("div");
    spellsDiv.className = "spells";

    [
      { id: enemy.spell1Id, name: enemy.spell1Name, cd: enemy.spell1Cooldown, idx: 0 },
      { id: enemy.spell2Id, name: enemy.spell2Name, cd: enemy.spell2Cooldown, idx: 1 },
    ].forEach((spell) => {
      const spellDiv = document.createElement("div");
      spellDiv.className = "spell";
      spellDiv.dataset.enemy = idx;
      spellDiv.dataset.spell = spell.idx;
      const spellKnown = spell.id > 0 && SPELL_ICONS[spell.id];
      spellDiv.title = spellKnown ? `${spell.name} (${spell.cd}s)` : "?";

      const img = document.createElement("img");
      if (spellKnown) {
        img.src = SPELL_ICONS[spell.id];
        img.alt = spell.name;
        img.onerror = () => { img.style.display = "none"; };
      } else {
        img.style.display = "none";
      }
      spellDiv.appendChild(img);

      const timerEl = document.createElement("div");
      timerEl.className = "spell-timer";
      spellDiv.appendChild(timerEl);

      // Click to start/cancel timer
      spellDiv.addEventListener("click", async () => {
        const key = timerKey(idx, spell.idx);
        if (timers[key]) {
          // Cancel
          cancelTimer(idx, spell.idx);
          await invoke("cancel_timer_event", { enemyIdx: idx, spellIdx: spell.idx });
        } else {
          // Start
          const now = Math.floor(Date.now() / 1000);
          startTimer(idx, spell.idx, spell.cd, now);
          await invoke("send_timer_event", {
            enemyIdx: idx,
            spellIdx: spell.idx,
            cooldownSecs: spell.cd,
          });
        }
      });

      // Right-click to toggle Cosmic Insight manually
      spellDiv.addEventListener("contextmenu", (e) => {
        e.preventDefault();
        // Toggle cosmic insight for this enemy (local only)
        const enemy = currentEnemies[idx];
        if (!enemy) return;
        enemy.hasCosmicInsight = !enemy.hasCosmicInsight;
        const newCd = enemy.hasCosmicInsight
          ? Math.max(0, spell.cd - 18)
          : spell.cd + 18;
        if (spell.idx === 0) enemy.spell1Cooldown = newCd;
        else enemy.spell2Cooldown = newCd;
        renderEnemies(currentEnemies);
      });

      spellsDiv.appendChild(spellDiv);
    });

    row.appendChild(spellsDiv);
    enemiesEl.appendChild(row);
  });
}

let currentEnemies = [];

// Drag handle
dragHandle.addEventListener("mousedown", () => {
  getCurrentWindow().startDragging();
});

// Save position on window move
let moveTimeout = null;
async function savePosition() {
  const pos = await getCurrentWindow().outerPosition();
  await invoke("save_overlay_position", {
    x: pos.x,
    y: pos.y,
  });
}

// Listen for events from backend
async function init() {
  // Load settings for opacity
  const settings = await invoke("get_settings");
  overlayEl.style.setProperty("--opacity", settings.overlayOpacity || 0.8);

  // Listen for opacity changes
  await listen("overlay-opacity", (event) => {
    overlayEl.style.setProperty("--opacity", event.payload);
  });

  // Listen for enemy data
  await listen("overlay-data", (event) => {
    currentEnemies = event.payload;
    renderEnemies(currentEnemies);
  });

  // Listen for sync messages from teammates
  await listen("sync-message", (event) => {
    const msg = event.payload;
    if (msg.type === "timer_start") {
      startTimer(msg.enemy_idx, msg.spell_idx, msg.cooldown_secs, msg.started_at);
    } else if (msg.type === "timer_cancel") {
      cancelTimer(msg.enemy_idx, msg.spell_idx);
    } else if (msg.type === "room_state") {
      // Apply existing timers from room state (reconnection)
      for (const t of msg.timers) {
        startTimer(t.enemy_idx, t.spell_idx, t.cooldown_secs, t.started_at);
      }
    }
  });

  // Listen for interactive mode toggle
  await listen("overlay-interactive", (event) => {
    const interactive = event.payload;
    overlayEl.classList.toggle("interactive", interactive);
    if (!interactive) {
      // Save position when going back to click-through
      savePosition();
    }
  });

  // Try to load initial data with retries
  enemiesEl.innerHTML = '<div class="loading">Esperant dades...</div>';
  for (let i = 0; i < 30; i++) {
    try {
      const enemies = await invoke("get_overlay_data");
      if (enemies && enemies.length > 0) {
        currentEnemies = enemies;
        renderEnemies(currentEnemies);
        break;
      }
    } catch { /* ignore */ }
    await new Promise(r => setTimeout(r, 2000));
  }
}

init();
