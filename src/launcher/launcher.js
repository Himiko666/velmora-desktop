// ============================================================
// Velmora Launcher — client JS
// Communique avec le backend Rust via window.__TAURI__.invoke.
//
// Rôle du launcher (V2.1) : se connecter, lire news/patch/évènements,
// accéder aux ressources externes (forum/discord/wiki), gérer ses
// paramètres, lancer le jeu en SSO. Pas de dashboard de jeu.
// ============================================================

const { invoke } = window.__TAURI__.core;
const opener = window.__TAURI__.opener;
const event = window.__TAURI__.event;

const $  = (sel) => document.querySelector(sel);
const $$ = (sel) => Array.from(document.querySelectorAll(sel));

const els = {
  viewSetup: $("#view-setup"),
  viewLogin: $("#view-login"),
  viewHome: $("#view-home"),
  // setup
  setupBar: $("#setup-bar"),
  setupCurrent: $("#setup-current"),
  setupContinue: $("#setup-continue"),
  setupSteps: $$(".setup-step"),
  ver0: $("#ver0"),
  // login
  loginForm: $("#login-form"),
  loginEmail: $("#login-email"),
  loginPassword: $("#login-password"),
  loginError: $("#login-error"),
  loginSubmit: $("#login-submit"),
  loginStatusDot: $("#login-status-dot"),
  loginStatusText: $("#login-status-text"),
  // topbar
  serverStatusDot: $("#status-dot"),
  serverStatusText: $("#status-text"),
  serverStatusCount: $("#status-count"),
  btnRefresh: $("#btn-refresh"),
  btnSettings: $("#btn-settings"),
  btnLogout: $("#btn-logout"),
  // sidebar profil
  profilePortrait: $("#profile-portrait"),
  profileName: $("#profile-name"),
  profileDyn: $("#profile-dyn"),
  profileLevel: $("#profile-level"),
  profilePrestige: $("#profile-prestige"),
  // tabs + content
  tabs: $$(".tab"),
  panes: $$(".tab-pane"),
  newsList: $("#news-list"),
  patchList: $("#patch-list"),
  eventsList: $("#events-list"),
  // quick links
  quickLinks: $("#quick-links-list"),
  nextMaint: $("#next-maint"),
  // kingdom tab (statut détaillé)
  kingdomStatus: $("#kingdom-status"),
  kingdomOnline: $("#kingdom-online"),
  kingdomOnlineTotal: $("#kingdom-online-total"),
  kingdomNextTick: $("#kingdom-next-tick"),
  kingdomTickInterval: $("#kingdom-tick-interval"),
  kingdomNextMaint: $("#kingdom-next-maint"),
  kingdomLastTick: $("#kingdom-last-tick"),
  kingdomServerTime: $("#kingdom-server-time"),
  // play bar
  btnPlay: $("#btn-play"),
  statusLine: $("#status-line"),
  // update
  updateBanner: $("#update-banner"),
  updateText: $("#update-text"),
  updateBtn: $("#update-btn"),
  // version
  ver1: $("#ver1"),
  ver2: $("#ver2"),
  // settings modal
  settingsModal: $("#settings-modal"),
  setAutostart: $("#set-autostart"),
  setCloseOnPlay: $("#set-close-on-play"),
  setNotifs: $("#set-notifs"),
  setGameWidth: $("#set-game-width"),
  setGameHeight: $("#set-game-height"),
  btnResetSettings: $("#btn-reset-settings"),
};

// ---------------- Boot ----------------

(async function boot() {
  const v = await invoke("app_version");
  els.ver0.textContent = v;
  els.ver1.textContent = v;
  els.ver2.textContent = `Velmora Launcher v${v}`;

  $$("a[data-external]").forEach((a) =>
    a.addEventListener("click", (e) => {
      e.preventDefault();
      opener.openUrl(a.dataset.external);
    })
  );

  els.loginForm.addEventListener("submit", onLogin);
  els.btnLogout.addEventListener("click", onLogout);
  els.btnRefresh.addEventListener("click", () => refreshAll(true));
  els.btnPlay.addEventListener("click", onPlay);
  els.btnSettings.addEventListener("click", openSettings);
  els.updateBtn.addEventListener("click", onInstallUpdate);

  // Onglets
  els.tabs.forEach((tab) =>
    tab.addEventListener("click", () => switchTab(tab.dataset.tab))
  );

  // Modal close (overlay + bouton ✕)
  $$("[data-close-modal]").forEach((b) =>
    b.addEventListener("click", () => els.settingsModal.classList.add("hidden"))
  );
  els.btnResetSettings.addEventListener("click", resetSettings);

  // Vérif statut serveur (avant login aussi)
  refreshServerStatus();
  setInterval(refreshServerStatus, 60_000);

  // Check updater (silencieux si rien)
  checkUpdates();

  // Premier lancement → écran de pré-install (téléchargement assets)
  const firstRun = await invoke("is_first_run").catch(() => false);
  if (firstRun) {
    await runFirstSetup();
    return; // runFirstSetup() bascule sur 'login' à la fin
  }

  if (await invoke("is_logged_in")) {
    try {
      await refreshAll(true);
      showView("home");
    } catch (e) {
      console.warn("auto-login échoué", e);
      showView("login");
    }
  } else {
    showView("login");
  }
})();

// ---------------- Premier lancement (setup) ----------------

async function runFirstSetup() {
  showView("setup");

  const stepEls = new Map(els.setupSteps.map((el) => [el.dataset.step, el]));

  const unlisten = await event.listen("setup://progress", (e) => {
    const { step, label, current, total } = e.payload || {};
    if (!step) return;

    // Marque l'étape courante "active" et les précédentes "done"
    const order = ["server", "manifest", "assets", "config"];
    const idx = order.indexOf(step);
    order.forEach((name, i) => {
      const el = stepEls.get(name);
      if (!el) return;
      el.classList.remove("active", "done");
      const check = el.querySelector(".setup-check");
      if (i < idx) {
        el.classList.add("done");
        if (check) check.textContent = "✓";
      } else if (i === idx) {
        el.classList.add("active");
        if (check) check.textContent = "◐";
      } else {
        if (check) check.textContent = "○";
      }
    });

    if (step === "done") {
      stepEls.forEach((el) => {
        el.classList.remove("active");
        el.classList.add("done");
        const check = el.querySelector(".setup-check");
        if (check) check.textContent = "✓";
      });
    }

    els.setupCurrent.textContent = label || "";
    const pct = total > 0 ? Math.round((current / total) * 100) : 0;
    els.setupBar.style.width = `${pct}%`;
  });

  try {
    await invoke("prepare_first_run");
    els.setupCurrent.textContent = "Le royaume est prêt à t'accueillir.";
    els.setupBar.style.width = "100%";
    els.setupContinue.classList.remove("hidden");
    els.setupContinue.onclick = () => {
      showView("login");
      unlisten();
    };
  } catch (e) {
    console.error("setup échoué", e);
    els.setupCurrent.textContent = "Une étape a échoué — tu peux continuer quand même.";
    els.setupContinue.textContent = "Continuer";
    els.setupContinue.classList.remove("hidden");
    els.setupContinue.onclick = () => {
      showView("login");
      unlisten();
    };
  }
}

// ---------------- Helpers ----------------

function showView(name) {
  els.viewSetup.classList.toggle("hidden", name !== "setup");
  els.viewLogin.classList.toggle("hidden", name !== "login");
  els.viewHome.classList.toggle("hidden", name !== "home");
}
function setStatus(msg) { els.statusLine.textContent = msg; }
function showLoginError(msg) {
  els.loginError.textContent = msg;
  els.loginError.classList.remove("hidden");
}
function hideLoginError() { els.loginError.classList.add("hidden"); }

function fmtDate(s) {
  if (!s) return "";
  try {
    return new Date(s).toLocaleDateString("fr-FR", {
      day: "2-digit", month: "short", year: "numeric",
    });
  } catch { return s; }
}
function fmtDateTime(s) {
  if (!s) return "";
  try {
    return new Date(s).toLocaleString("fr-FR", {
      day: "2-digit", month: "short", hour: "2-digit", minute: "2-digit",
    });
  } catch { return s; }
}
// Format relatif type "dans 3 min", "il y a 12 min", "à l'instant".
function fmtRelative(s) {
  if (!s) return "";
  const target = new Date(s).getTime();
  if (Number.isNaN(target)) return "";
  const diffSec = Math.round((target - Date.now()) / 1000);
  const abs = Math.abs(diffSec);
  if (abs < 30) return "à l'instant";
  if (abs < 90) return diffSec > 0 ? "dans 1 min" : "il y a 1 min";
  if (abs < 3600) {
    const m = Math.round(abs / 60);
    return diffSec > 0 ? `dans ${m} min` : `il y a ${m} min`;
  }
  if (abs < 86400) {
    const h = Math.round(abs / 3600);
    return diffSec > 0 ? `dans ${h} h` : `il y a ${h} h`;
  }
  const d = Math.round(abs / 86400);
  return diffSec > 0 ? `dans ${d} j` : `il y a ${d} j`;
}
function escapeHtml(s) {
  return String(s ?? "").replace(/[&<>"']/g, (c) => ({
    "&": "&amp;", "<": "&lt;", ">": "&gt;", '"': "&quot;", "'": "&#39;",
  })[c]);
}

// ---------------- Statut serveur ----------------

async function refreshServerStatus() {
  try {
    const s = await invoke("server_status");
    updateStatusBadge(s);
  } catch (e) {
    updateStatusBadge({ status: "offline" });
  }
}

function updateStatusBadge(s) {
  const status = s?.status || "offline";
  const label = status === "online" ? "En jeu" : status === "maintenance" ? "Maintenance" : "Hors-ligne";

  // Topbar (vue home)
  if (els.serverStatusDot) {
    els.serverStatusDot.className = `status-dot ${status}`;
    els.serverStatusText.textContent = label;
    const count = Number(s?.online_count || 0);
    els.serverStatusCount.textContent = count > 0 ? `· ${count} en ligne` : "";
  }

  // Vue login
  if (els.loginStatusDot) {
    els.loginStatusDot.className = `status-dot ${status}`;
    els.loginStatusText.textContent = label + (s?.online_count ? ` — ${s.online_count} seigneurs en ligne` : "");
  }

  // Prochaine maintenance
  if (els.nextMaint) {
    const nm = s?.next_maintenance_at;
    els.nextMaint.textContent = nm ? `Prochaine maintenance : ${fmtDateTime(nm)}` : "";
  }

  // Onglet Royaume (statut détaillé)
  if (els.kingdomStatus) {
    els.kingdomStatus.textContent = label;
    els.kingdomStatus.dataset.status = status;
  }
  if (els.kingdomOnline) {
    els.kingdomOnline.textContent = Number(s?.online_count || 0).toString();
  }
  if (els.kingdomOnlineTotal) {
    const total = Number(s?.total_lords || 0);
    els.kingdomOnlineTotal.textContent = total > 0 ? `sur ${total} seigneurs forgés` : "";
  }
  if (els.kingdomNextTick) {
    els.kingdomNextTick.textContent = s?.next_tick_at ? fmtRelative(s.next_tick_at) : "—";
  }
  if (els.kingdomTickInterval) {
    const tm = Number(s?.tick_minutes || 0);
    els.kingdomTickInterval.textContent = tm > 0 ? `intervalle ${tm} min` : "";
  }
  if (els.kingdomNextMaint) {
    els.kingdomNextMaint.textContent = s?.next_maintenance_at ? fmtDateTime(s.next_maintenance_at) : "—";
  }
  if (els.kingdomLastTick) {
    els.kingdomLastTick.textContent = s?.last_tick_at ? fmtRelative(s.last_tick_at) : "—";
  }
  if (els.kingdomServerTime) {
    els.kingdomServerTime.textContent = s?.server_time ? `Heure serveur : ${fmtDateTime(s.server_time)}` : "";
  }
}

// ---------------- Login / Logout ----------------

async function onLogin(e) {
  e.preventDefault();
  hideLoginError();
  els.loginSubmit.disabled = true;
  els.loginSubmit.textContent = "Chevauchée…";

  try {
    await invoke("login", {
      email: els.loginEmail.value.trim(),
      password: els.loginPassword.value,
    });
    els.loginPassword.value = "";
    await refreshAll(true);
    showView("home");
  } catch (err) {
    const msg = typeof err === "string" ? err : err?.message || "Connexion impossible.";
    if (msg.includes("invalid") || msg.includes("Identifiants")) {
      showLoginError("Identifiants invalides.");
    } else if (msg.includes("réseau") || msg.includes("network")) {
      showLoginError("Pas de connexion au royaume. Vérifie ton réseau.");
    } else {
      showLoginError(msg);
    }
  } finally {
    els.loginSubmit.disabled = false;
    els.loginSubmit.textContent = "Entrer dans le royaume";
  }
}

async function onLogout() {
  setStatus("Déconnexion…");
  try { await invoke("logout"); } catch (e) { console.warn("logout", e); }
  showView("login");
  setStatus("Prêt");
}

// ---------------- Identité + feed ----------------

async function refreshAll(silent = false) {
  if (!silent) setStatus("Rafraîchissement…");
  try {
    const [meResp, feedResp] = await Promise.all([
      invoke("me"),
      invoke("launcher_feed"),
    ]);
    renderProfile(meResp?.user || {});
    renderFeed(feedResp || {});
    setStatus("À jour");
    // Vérif silencieuse du manifest d'assets : si la version a changé côté
    // serveur (nouvelle forteresse, icône remplacée…), le backend Rust
    // re-télécharge en parallèle dans `app_cache_dir/assets/` sans bloquer
    // l'UI. No-op rapide quand rien n'a bougé.
    invoke("recheck_manifest").catch((e) => console.debug("recheck_manifest", e));
  } catch (err) {
    const msg = typeof err === "string" ? err : err?.message || "Erreur inconnue.";
    if (msg.includes("authentifié") || msg.includes("Unauthenticated")) {
      showView("login");
      return;
    }
    setStatus("Hors-ligne");
    throw err;
  }
}

function renderProfile(user) {
  const lord = user?.lord || {};
  els.profileName.textContent = lord?.name || user?.name || "Seigneur inconnu";
  els.profileDyn.textContent = lord?.dynasty ? `Ordre ${lord.dynasty}` : "Sans ordre";
  els.profileLevel.textContent = `Lv ${lord?.level ?? "?"}`;
  els.profilePrestige.textContent = `⚜ ${lord?.prestige ?? 0}`;
  if (lord?.portrait_url) {
    els.profilePortrait.src = lord.portrait_url;
    els.profilePortrait.style.visibility = "visible";
  } else {
    els.profilePortrait.style.visibility = "hidden";
  }
}

function renderFeed(feed) {
  renderItems(els.newsList, feed?.news || [], "news");
  renderItems(els.patchList, feed?.patch_notes || [], "patch");
  renderItems(els.eventsList, feed?.events || [], "event");
  renderQuickLinks(feed?.quick_links || []);
}

function renderItems(container, items, kind) {
  if (!items?.length) {
    container.innerHTML = `<div class="muted">Rien à signaler.</div>`;
    return;
  }
  // Pour les patch notes : on rend le body_html riche (admin-curé, source de
  // confiance via API authentifiée). Pour news/events on garde l'excerpt
  // condensé — listes longues, lisibilité prioritaire.
  const useRichBody = kind === "patch";
  container.innerHTML = items
    .map((n) => {
      const title = n?.title || "—";
      const date = fmtDate(n?.published_at || n?.date || n?.created_at);
      const tag = n?.tag ? `<span class="badge gold" style="margin-right:0.4rem">${escapeHtml(n.tag)}</span>` : "";
      let bodyHtml = "";
      if (useRichBody && typeof n?.body_html === "string" && n.body_html.trim() !== "") {
        bodyHtml = `<div class="news-body">${sanitizeAdminHtml(n.body_html)}</div>`;
      } else {
        const txt = n?.excerpt || n?.body || n?.summary || "";
        if (txt) bodyHtml = `<div class="news-excerpt">${escapeHtml(txt)}</div>`;
      }
      return `<article class="news-item${useRichBody ? " news-item--rich" : ""}">
        <header class="news-head">
          <h4 class="news-title">${tag}${escapeHtml(title)}</h4>
          <time class="news-date">${escapeHtml(date)}</time>
        </header>
        ${bodyHtml}
      </article>`;
    })
    .join("");
}

/**
 * Sanitiseur minimal : on autorise un vocabulaire HTML restreint pour les
 * patch notes (h2/h3/h4, p, ul/ol/li, strong/em, br, hr, svg + path/circle
 * /rect/line/polyline/polygon/g + attributs viewBox/d/cx/cy/r/x/y/width
 * /height/fill/stroke/stroke-width/opacity/transform). On vire le reste —
 * notamment script, iframe, on* handlers et style externe.
 */
function sanitizeAdminHtml(raw) {
  const template = document.createElement("template");
  template.innerHTML = String(raw);
  const ALLOWED = new Set([
    "H2","H3","H4","P","UL","OL","LI","STRONG","EM","BR","HR","SPAN","DIV",
    "FIGURE","FIGCAPTION","IMG","SMALL","MARK","Q",
    "SVG","PATH","CIRCLE","RECT","LINE","POLYLINE","POLYGON","G","DEFS",
    "LINEARGRADIENT","RADIALGRADIENT","STOP","TITLE","TSPAN","TEXT",
  ]);
  const ALLOWED_ATTRS = new Set([
    "viewbox","d","cx","cy","r","x","y","x1","x2","y1","y2","width","height",
    "fill","stroke","stroke-width","stroke-linejoin","stroke-linecap",
    "opacity","transform","points","class","aria-hidden","preserveaspectratio",
    "offset","stop-color","stop-opacity","gradientunits","gradienttransform",
    "id","href","xlink:href","xmlns",
    "src","alt","loading","decoding","srcset","sizes",
  ]);
  const walk = (node) => {
    [...node.childNodes].forEach((child) => {
      if (child.nodeType === Node.ELEMENT_NODE) {
        if (!ALLOWED.has(child.tagName.toUpperCase())) {
          child.replaceWith(...child.childNodes);
          return;
        }
        [...child.attributes].forEach((attr) => {
          const name = attr.name.toLowerCase();
          if (!ALLOWED_ATTRS.has(name)) {
            child.removeAttribute(attr.name);
            return;
          }
          // src / href : seulement https:// ou chemin relatif velmora.cc.
          if (name === "src" || name === "href" || name === "xlink:href") {
            const v = String(attr.value || "").trim();
            const safe = /^https:\/\/velmora\.cc\//i.test(v) || /^https:\/\/[a-z0-9.-]+\.velmora\.cc\//i.test(v);
            if (!safe) { child.removeAttribute(attr.name); }
          }
        });
        walk(child);
      } else if (child.nodeType === Node.COMMENT_NODE) {
        child.remove();
      }
    });
  };
  walk(template.content);
  return template.innerHTML;
}

function renderQuickLinks(links) {
  if (!links?.length) {
    els.quickLinks.innerHTML = `<div class="muted" style="padding:0.4rem 0.6rem;font-size:0.78rem">Aucun lien.</div>`;
    return;
  }
  els.quickLinks.innerHTML = links
    .map((l) => `<button class="quick-link" data-url="${escapeHtml(l.url)}">
      <span class="ql-icon">${escapeHtml(l.icon || "·")}</span>
      <span>${escapeHtml(l.label)}</span>
    </button>`)
    .join("");
  els.quickLinks.querySelectorAll(".quick-link").forEach((b) =>
    b.addEventListener("click", () => opener.openUrl(b.dataset.url))
  );
}

// ---------------- Onglets ----------------

function switchTab(name) {
  els.tabs.forEach((t) => t.classList.toggle("active", t.dataset.tab === name));
  els.panes.forEach((p) => p.classList.toggle("active", p.dataset.pane === name));
}

// ---------------- JOUER (SSO) ----------------

async function onPlay() {
  els.btnPlay.disabled = true;
  setStatus("Ouverture de la session…");
  try {
    await invoke("launch_game");
    setStatus("Session ouverte");
    const close = await invoke("get_setting", { key: "close_on_play" });
    if (close === true) {
      // Le launcher se ferme : le user voulait juste le jeu.
      setTimeout(() => window.close(), 800);
    }
  } catch (e) {
    console.error("launch_game", e);
    const msg = typeof e === "string" ? e : e?.message || "Échec";
    setStatus(`Échec : ${msg.slice(0, 60)}`);
  } finally {
    setTimeout(() => {
      els.btnPlay.disabled = false;
      setStatus("Prêt");
    }, 2000);
  }
}

// ---------------- Settings ----------------

async function openSettings() {
  // Lecture des valeurs courantes
  try {
    const [autostart, closeOnPlay, notifs, gw, gh] = await Promise.all([
      invoke("get_autostart").catch(() => false),
      invoke("get_setting", { key: "close_on_play" }),
      invoke("get_setting", { key: "notifications_enabled" }),
      invoke("get_setting", { key: "game_width" }),
      invoke("get_setting", { key: "game_height" }),
    ]);
    els.setAutostart.checked = !!autostart;
    els.setCloseOnPlay.checked = closeOnPlay === true;
    els.setNotifs.checked = notifs !== false;
    els.setGameWidth.value = Number(gw) || 1280;
    els.setGameHeight.value = Number(gh) || 800;
  } catch (e) {
    console.warn("settings load", e);
  }
  els.settingsModal.classList.remove("hidden");

  // Câblage onChange : applique direct
  els.setAutostart.onchange = () => invoke("set_autostart", { enabled: els.setAutostart.checked }).catch(console.warn);
  els.setCloseOnPlay.onchange = () => invoke("set_setting", { key: "close_on_play", value: els.setCloseOnPlay.checked });
  els.setNotifs.onchange = () => invoke("set_setting", { key: "notifications_enabled", value: els.setNotifs.checked });
  els.setGameWidth.onchange = () => invoke("set_setting", { key: "game_width", value: Number(els.setGameWidth.value) });
  els.setGameHeight.onchange = () => invoke("set_setting", { key: "game_height", value: Number(els.setGameHeight.value) });
}

async function resetSettings() {
  if (!confirm("Réinitialiser tous les paramètres ?")) return;
  await Promise.all([
    invoke("set_autostart", { enabled: false }).catch(() => {}),
    invoke("set_setting", { key: "close_on_play", value: false }),
    invoke("set_setting", { key: "notifications_enabled", value: true }),
    invoke("set_setting", { key: "game_width", value: 1280 }),
    invoke("set_setting", { key: "game_height", value: 800 }),
  ]);
  openSettings();
}

// ---------------- Updater ----------------

async function checkUpdates() {
  try {
    const info = await invoke("check_for_updates");
    if (info?.available) {
      els.updateText.textContent = `Une nouvelle version du launcher est disponible (v${info.version}).`;
      els.updateBanner.classList.remove("hidden");
    }
  } catch (e) {
    console.debug("update check skipped:", e);
  }
}

async function onInstallUpdate() {
  els.updateBtn.disabled = true;
  els.updateBtn.textContent = "Installation…";
  try {
    await event.listen("updater://progress", (e) => {
      els.updateBtn.textContent = `Installation ${e.payload}%…`;
    });
    await invoke("install_update");
  } catch (e) {
    console.error(e);
    els.updateBtn.disabled = false;
    els.updateBtn.textContent = "Réessayer";
  }
}
