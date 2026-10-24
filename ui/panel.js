/**
 * Search panel — zero-framework on purpose: it has to appear within ~100 ms of
 * the hotkey, so the page is plain DOM and one `innerHTML` render.
 *
 * Runs inside Tauri (`window.__TAURI_INTERNALS__` present) or in a plain browser
 * via `pnpm dev`, where every command resolves against the mock backend below.
 */

import { setupPanelI18n } from "./panel-i18n.js";
import { translateDom } from "./shared/i18n-core.js";

const HOSTED = Boolean(window.__TAURI_INTERNALS__);

/** Set once `start()` has awaited the word list; `t` is safe to call meanwhile. */
let i18n = null;
const t = (key, options) => (i18n ? i18n.t(key, options) : key);

async function invoke(cmd, args = {}) {
  if (HOSTED) return window.__TAURI_INTERNALS__.invoke(cmd, args);
  return MOCK[cmd] ? MOCK[cmd](args) : null;
}

// Browser-preview sample data. Real tool names/descriptions come from tool.json
// (and, once tool manifests carry `locales`, in the selected language), so these
// stay Chinese on purpose: they are mock content, not UI copy.
const MOCK_TOOLS = [
  {
    id: "sprite-align",
    name: "序列帧对齐",
    description: "切分 sprite sheet、锚点对齐、导出 ZIP",
    source: "official",
    accent: "#4ea1ff",
    iconDataUrl: null,
    error: "",
    actions: [
      { id: "open", title: "打开序列帧对齐", combo: "Ctrl+Alt+S" },
      { id: "realign", title: "重新对齐", combo: "Ctrl+Alt+Z" },
      { id: "export-zip", title: "导出为 ZIP", combo: "Ctrl+Alt+E" },
    ],
  },
  {
    id: "rename-batch",
    name: "批量重命名",
    description: "正则批量改名，支持预览与撤销",
    source: "installed",
    accent: "#3ecf8e",
    iconDataUrl: null,
    error: "",
    actions: [{ id: "open", title: "打开批量重命名", combo: "Ctrl+Alt+R" }],
  },
  {
    id: "color-picker",
    name: "取色器",
    description: "屏幕取色、历史记录、格式转换",
    source: "installed",
    accent: "#e8b23a",
    iconDataUrl: null,
    error: "",
    actions: [
      { id: "open", title: "打开取色器", combo: "Ctrl+Alt+C" },
      { id: "pick", title: "立即取色", combo: "Ctrl+Alt+Shift+C" },
    ],
  },
];

const MOCK = {
  get_state: () => ({
    version: "0.2.0",
    portable: true,
    tools: MOCK_TOOLS,
    config: { panelHotkey: "Ctrl+Shift+T" },
  }),
  open_tool: ({ id }) => {
    console.log("open_tool", id);
    return null;
  },
  open_main: () => {
    console.log("open_main");
    return null;
  },
  close_window: () => null,
};

/* ------------------------------------------------------------------- state */

const state = { tools: [], version: "", query: "", rows: [], sel: 0 };

const $search = document.getElementById("search");
const $results = document.getElementById("results");
const $version = document.getElementById("version");

/* ----------------------------------------------------------------- helpers */

function initial(name) {
  const ch = (name || "?").trim().charAt(0);
  return ch ? ch.toUpperCase() : "?";
}

function rowIcon(tool) {
  const wrap = document.createElement("div");
  wrap.className = "row-icon";
  wrap.style.background = tool.accent || "#2b6cb0";
  if (tool.iconDataUrl) {
    const img = document.createElement("img");
    img.src = tool.iconDataUrl;
    img.alt = "";
    wrap.appendChild(img);
  } else {
    wrap.textContent = initial(tool.name);
  }
  return wrap;
}

/* ------------------------------------------------------------------- build */

function buildRows() {
  const q = state.query.trim().toLowerCase();
  const rows = [];
  // A tool that is switched off in the main window must not be launchable from
  // the panel either; broken manifests are hidden the same way.
  const enabled = state.tools.filter((tool) => !tool.error && tool.enabled !== false);

  for (const tool of enabled) {
    const toolHit =
      !q ||
      tool.name.toLowerCase().includes(q) ||
      tool.id.includes(q) ||
      (tool.description || "").toLowerCase().includes(q);
    // `searchText` (added with Bridge v2) carries every available language's
    // name/description/action titles, so a query in either language matches.
    const searchText = (tool.searchText || "").toLowerCase();
    const textHit = Boolean(q) && searchText.includes(q);
    const matchedActions = (tool.actions || []).filter(
      (a) => !q || a.title.toLowerCase().includes(q) || tool.name.toLowerCase().includes(q)
    );

    if ((toolHit || textHit) && !q) {
      rows.push({ kind: "tool", tool, action: null, title: tool.name, sub: tool.description });
    } else if (q) {
      if (toolHit || textHit) {
        rows.push({ kind: "tool", tool, action: null, title: tool.name, sub: tool.description });
      }
      for (const a of matchedActions) {
        if (toolHit && a.id === "open") continue; // avoid duplicate of the tool row
        rows.push({ kind: "action", tool, action: a, title: a.title, sub: tool.name });
      }
    }
  }
  state.rows = rows;
  if (state.sel >= rows.length) state.sel = Math.max(0, rows.length - 1);
  render();
}

function render() {
  $results.innerHTML = "";
  if (!state.rows.length) {
    const d = document.createElement("div");
    d.className = "empty";
    d.textContent = state.query ? t("panel:empty.noMatch") : t("panel:empty.noTools");
    $results.appendChild(d);
    return;
  }

  const label = document.createElement("div");
  label.className = "group-label";
  label.textContent = state.query
    ? t("panel:group.results", { count: state.rows.length })
    : t("panel:group.all");
  $results.appendChild(label);

  state.rows.forEach((r, i) => {
    const row = document.createElement("div");
    row.className = "row" + (i === state.sel ? " sel" : "");
    row.dataset.index = String(i);

    row.appendChild(rowIcon(r.tool));

    const main = document.createElement("div");
    main.className = "row-main";
    const title = document.createElement("div");
    title.className = "row-title";
    title.textContent = r.title;
    const sub = document.createElement("div");
    sub.className = "row-sub";
    sub.textContent =
      r.kind === "action" ? t("panel:row.action", { tool: r.tool.name }) : r.sub || "";
    main.append(title, sub);
    row.appendChild(main);

    if (r.kind === "action" && r.action.combo) {
      const kbd = document.createElement("kbd");
      kbd.textContent = r.action.combo;
      row.appendChild(kbd);
    } else if (r.tool.source === "installed") {
      const badge = document.createElement("span");
      badge.className = "row-badge";
      badge.textContent = t("panel:badge.installed");
      row.appendChild(badge);
    }

    row.addEventListener("mouseenter", () => {
      state.sel = i;
      paintSelection();
    });
    row.addEventListener("click", () => activate(i));
    $results.appendChild(row);
  });
  scrollSelIntoView();
}

function paintSelection() {
  const rows = $results.querySelectorAll(".row");
  rows.forEach((el) => el.classList.toggle("sel", Number(el.dataset.index) === state.sel));
  scrollSelIntoView();
}

function scrollSelIntoView() {
  const el = $results.querySelector(`.row[data-index="${state.sel}"]`);
  if (el) el.scrollIntoView({ block: "nearest" });
}

async function activate(i) {
  const r = state.rows[i];
  if (!r) return;
  await invoke("open_tool", { id: r.tool.id });
  hidePanel();
}

function hidePanel() {
  invoke("close_window", { label: "panel" });
}

async function load() {
  try {
    const s = await invoke("get_state");
    state.tools = s.tools || [];
    state.version = s.version || "";
    $version.textContent = "v" + state.version;
    // The frosted surface needs a real backdrop: only the host can add one
    // (CSS `backdrop-filter` has nothing to blur in a transparent window), so
    // the translucent panel colours are opt-in behind this attribute.
    applyGlass(s.env && s.env.glass);
    buildRows();
  } catch (e) {
    const d = document.createElement("div");
    d.className = "empty";
    d.textContent = t("panel:error.load", { error: String(e) });
    $results.replaceChildren(d);
  }
}

/**
 * `on === true` → use the translucent surface over the acrylic backdrop.
 * Anything else (no env, old host, acrylic unavailable) keeps the opaque panel.
 */
function applyGlass(on) {
  document.documentElement.setAttribute("data-glass", on ? "on" : "off");
}

/* -------------------------------------------------------------------- theme */

/** Re-point the token block; the value comes from the host `theme` event. */
function applyTheme(theme) {
  if (theme !== "light" && theme !== "dark") return;
  const root = document.documentElement;
  root.setAttribute("data-theme", theme);
  root.style.colorScheme = theme;
}

if (window.__hostOn) window.__hostOn("theme", applyTheme);

/* --------------------------------------------------------------------- drag */

// decorations(false) means there is no native title bar to grab, and
// `data-tauri-drag-region` is inert here: withGlobalTauri is false, so
// bundle.global.js (the script that installs that listener) is never injected.
// Call the window plugin command directly — the tauri 2 JS API is just
// `invoke("plugin:window|start_dragging", { label })`.
const NON_DRAG_TAGS = new Set(["INPUT", "BUTTON", "A", "SELECT", "TEXTAREA", "LABEL", "SUMMARY"]);

function isDragSurface(target) {
  for (let el = target; el && el !== document.body; el = el.parentElement) {
    if (!(el instanceof HTMLElement)) continue;
    if (NON_DRAG_TAGS.has(el.tagName) || el.isContentEditable) return false;
    if (el.classList.contains("row") || el.classList.contains("fbtn")) return false;
    if (el.id === "panel") return true;
  }
  return false;
}

document.addEventListener("mousedown", (e) => {
  if (e.button !== 0 || !HOSTED || !isDragSurface(e.target)) return;
  // Keep the search box focused: the default mousedown action would move the
  // focus to the document body.
  e.preventDefault();
  window.__TAURI_INTERNALS__.invoke("plugin:window|start_dragging", { label: "panel" }).catch((err) =>
    console.warn("[panel] start_dragging failed:", err)
  );
});

/* ------------------------------------------------------------------- events */

$search.addEventListener("input", () => {
  state.query = $search.value;
  state.sel = 0;
  buildRows();
});

document.addEventListener("keydown", (e) => {
  if (e.key === "Escape") {
    if ($search.value) {
      $search.value = "";
      state.query = "";
      state.sel = 0;
      buildRows();
    } else {
      hidePanel();
    }
    e.preventDefault();
    return;
  }
  if (e.key === "ArrowDown") {
    state.sel = Math.min(state.sel + 1, state.rows.length - 1);
    paintSelection();
    e.preventDefault();
  } else if (e.key === "ArrowUp") {
    state.sel = Math.max(state.sel - 1, 0);
    paintSelection();
    e.preventDefault();
  } else if (e.key === "Enter") {
    activate(state.sel);
    e.preventDefault();
  }
});

document.querySelectorAll(".fbtn").forEach((btn) => {
  btn.addEventListener("click", async () => {
    const act = btn.dataset.act;
    if (act === "main") {
      await invoke("open_main");
      hidePanel();
    } else if (act === "refresh") {
      await load();
    }
  });
});

// focus search as soon as we appear
window.addEventListener("focus", () => $search.focus());
document.addEventListener("visibilitychange", () => {
  if (!document.hidden) {
    $search.focus();
    $search.select();
  }
});

/**
 * Boot: load the word list before the first render so the panel never flashes
 * untranslated text. The resources are bundled (no fetch), so this adds no
 * network round trip to the hotkey path.
 */
async function start() {
  i18n = await setupPanelI18n();
  translateDom();
  document.title = t("panel:title");
  i18n.on("languageChanged", () => {
    translateDom();
    document.title = t("panel:title");
    render();
  });
  $search.focus();
  await load();
}

start();
