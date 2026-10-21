/**
 * Toolbox host bridge
 * ---------------------------------------------------------------------------
 * Thin, dependency-free wrapper around Tauri 2's internals. The UI can also be
 * opened directly in a normal browser: every command then resolves against the
 * MOCK backend below, so all four tabs stay fully browsable without Tauri.
 */

export const isHosted = Boolean(window.__TAURI_INTERNALS__);

const clone = (value) => JSON.parse(JSON.stringify(value));
const sleep = (ms) => new Promise((resolve) => setTimeout(resolve, ms));

/* ------------------------------------------------------------------------- */
/* Mock backend                                                               */
/* ------------------------------------------------------------------------- */

const MOCK = {
  version: '0.1.0',
  portable: true,
  dataDir: 'D:\\Toolbox\\data',
  officialToolsDir: 'D:\\Toolbox\\tools',
  installedToolsDir: 'D:\\Toolbox\\data\\tools',
  config: {
    locale: 'zh-CN',
    theme: 'dark',
    ballSide: 'right',
    ballY: 0.62,
    panelHotkey: 'Alt+Space',
    autostart: false,
    closeToTray: true
  },
  tools: [
    {
      id: 'sprite-align',
      name: '序列帧对齐',
      description: '导入精灵表，按行列切片并叠加参考线；支持逐帧微调、批量偏移与对齐结果导出。',
      version: '1.2.0',
      author: 'Toolbox 官方',
      source: 'official',
      enabled: true,
      accent: '#4ea1ff',
      iconDataUrl: null,
      dir: 'D:\\Toolbox\\tools\\sprite-align',
      sizeBytes: 2411724,
      actions: [
        { id: 'sprite-align.toggle', title: '显示 / 隐藏面板', combo: 'Ctrl+Alt+1' },
        { id: 'sprite-align.export', title: '导出当前对齐', combo: 'Ctrl+Alt+E' }
      ]
    },
    {
      id: 'batch-rename',
      name: '批量重命名',
      description: '基于规则批量重命名文件，支持序号、日期变量与正则替换，执行前可预览结果。',
      version: '0.9.3',
      author: '社区 · 林深',
      source: 'installed',
      enabled: true,
      accent: '#3ecf8e',
      iconDataUrl: null,
      dir: 'D:\\Toolbox\\data\\tools\\batch-rename',
      sizeBytes: 482304,
      actions: [
        { id: 'batch-rename.run', title: '执行上次重命名规则', combo: 'Ctrl+Alt+R' }
      ]
    },
    {
      id: 'color-picker',
      name: '取色器',
      description: '屏幕放大镜取色，自动复制 HEX / RGB，支持历史色板与多显示器。',
      version: '1.0.0',
      author: '社区 · 阿澈',
      source: 'installed',
      enabled: false,
      accent: '#e8b23a',
      iconDataUrl: null,
      dir: 'D:\\Toolbox\\data\\tools\\color-picker',
      sizeBytes: 318464,
      actions: [
        { id: 'color-picker.pick', title: '开始屏幕取色', combo: '' }
      ]
    },
    {
      id: 'gif-split',
      name: 'GIF 拆帧',
      description: '将 GIF 动图拆分为 PNG 序列，可指定帧区间、输出尺寸与命名规则。',
      version: '1.1.0',
      author: 'Toolbox 官方',
      source: 'official',
      enabled: true,
      accent: '#b084ff',
      iconDataUrl: null,
      dir: 'D:\\Toolbox\\tools\\gif-split',
      sizeBytes: 1269760,
      actions: [
        { id: 'gif-split.open', title: '打开拆帧面板', combo: 'Ctrl+Alt+G' }
      ]
    }
  ],
  actions: [
    { id: 'app.panel', scope: 'app', toolId: null, title: '呼出 / 隐藏面板', combo: 'Alt+Space', defaultCombo: 'Alt+Space', effective: true, reason: null },
    { id: 'app.main', scope: 'app', toolId: null, title: '打开主窗口', combo: 'Ctrl+Alt+T', defaultCombo: 'Ctrl+Alt+T', effective: false, reason: '与 Windows Terminal 的快捷键冲突' },
    { id: 'app.pin', scope: 'app', toolId: null, title: '固定 / 取消固定面板', combo: 'Alt+P', defaultCombo: 'Alt+P', effective: true, reason: null },
    { id: 'app.quit', scope: 'app', toolId: null, title: '退出工具箱', combo: '', defaultCombo: 'Ctrl+Alt+Q', effective: false, reason: '未设置' },
    { id: 'sprite-align.toggle', scope: 'tool', toolId: 'sprite-align', title: '显示 / 隐藏面板', combo: 'Ctrl+Alt+1', defaultCombo: 'Ctrl+Alt+1', effective: true, reason: null },
    { id: 'sprite-align.export', scope: 'tool', toolId: 'sprite-align', title: '导出当前对齐', combo: 'Ctrl+Alt+E', defaultCombo: 'Ctrl+Alt+E', effective: true, reason: null },
    { id: 'batch-rename.run', scope: 'tool', toolId: 'batch-rename', title: '执行上次重命名规则', combo: 'Ctrl+Alt+R', defaultCombo: 'Ctrl+Alt+R', effective: true, reason: null },
    { id: 'color-picker.pick', scope: 'tool', toolId: 'color-picker', title: '开始屏幕取色', combo: '', defaultCombo: 'Ctrl+Shift+C', effective: false, reason: '未设置' },
    { id: 'gif-split.open', scope: 'tool', toolId: 'gif-split', title: '打开拆帧面板', combo: 'Ctrl+Alt+G', defaultCombo: 'Ctrl+Alt+G', effective: false, reason: '被其它程序占用' }
  ],
  hotkeys: {}
};

// The hotkey map mirrors action state in the real host.
MOCK.hotkeys = Object.fromEntries(
  MOCK.actions.map((action) => [
    action.id,
    {
      combo: action.combo,
      defaultCombo: action.defaultCombo,
      effective: action.effective,
      reason: action.reason
    }
  ])
);

let mock = clone(MOCK);
let mockInstallCount = 0;

function findAction(actionId) {
  return mock.actions.find((action) => action.id === actionId) || null;
}

function syncToolAction(action) {
  if (action.scope !== 'tool') return;
  const tool = mock.tools.find((item) => item.id === action.toolId);
  const entry = tool && tool.actions.find((item) => item.id === action.id);
  if (entry) entry.combo = action.combo;
}

function applyHotkeyState(action) {
  mock.hotkeys[action.id] = {
    combo: action.combo,
    defaultCombo: action.defaultCombo,
    effective: action.effective,
    reason: action.reason
  };
  syncToolAction(action);
}

function setMockHotkey(actionId, combo) {
  const action = findAction(actionId);
  if (!action) return { ok: false, reason: '快捷键项不存在' };
  const normalized = String(combo || '').trim();
  if (!normalized) return { ok: false, reason: '组合键不能为空' };
  const conflict = mock.actions.find(
    (item) =>
      item.id !== actionId &&
      item.combo &&
      item.combo.toLowerCase() === normalized.toLowerCase()
  );
  if (conflict) return { ok: false, reason: `与「${conflict.title}」冲突` };
  action.combo = normalized;
  action.effective = true;
  action.reason = null;
  applyHotkeyState(action);
  return { ok: true };
}

function resetMockHotkey(actionId) {
  const action = findAction(actionId);
  if (!action) return { ok: false, reason: '快捷键项不存在' };
  const conflict = mock.actions.find(
    (item) =>
      item.id !== actionId &&
      item.combo &&
      action.defaultCombo &&
      item.combo.toLowerCase() === action.defaultCombo.toLowerCase()
  );
  action.combo = action.defaultCombo;
  action.effective = !conflict && Boolean(action.combo);
  action.reason = conflict
    ? `与「${conflict.title}」冲突`
    : action.combo
      ? null
      : '未设置';
  applyHotkeyState(action);
  return { ok: true };
}

function mockInvoke(cmd, args = {}) {
  switch (cmd) {
    case 'get_state':
      return clone(mock);

    case 'open_tool':
    case 'close_tool':
    case 'open_panel':
    case 'open_main':
    case 'close_window':
    case 'quit_app':
    case 'restart_app':
      return { ok: true };

    case 'set_tool_enabled': {
      const tool = mock.tools.find((item) => item.id === args.id);
      if (!tool) return { ok: false, error: '工具不存在' };
      tool.enabled = Boolean(args.enabled);
      return { ok: true };
    }

    case 'uninstall_tool': {
      const tool = mock.tools.find((item) => item.id === args.id);
      if (!tool) return { ok: false, error: '工具不存在' };
      if (tool.source === 'official') return { ok: false, error: '官方工具不可卸载' };
      mock.tools = mock.tools.filter((item) => item.id !== args.id);
      const removed = new Set(
        mock.actions.filter((action) => action.toolId === args.id).map((action) => action.id)
      );
      mock.actions = mock.actions.filter((action) => action.toolId !== args.id);
      removed.forEach((id) => delete mock.hotkeys[id]);
      return { ok: true };
    }

    case 'install_tool': {
      mockInstallCount += 1;
      const id = `local-tool-${mockInstallCount}`;
      const actionId = `${id}.run`;
      const name = mockInstallCount === 1 ? '我的本地工具' : `我的本地工具 ${mockInstallCount}`;
      mock.tools.push({
        id,
        name,
        description: '从本地 zip 安装的第三方工具示例。安装完成后会出现在「已安装」列表中。',
        version: '0.1.0',
        author: '本地安装',
        source: 'installed',
        enabled: true,
        accent: '#3ecf8e',
        iconDataUrl: null,
        dir: `${mock.installedToolsDir}\\${id}`,
        sizeBytes: 204800,
        actions: [{ id: actionId, title: '运行工具', combo: '' }]
      });
      mock.actions.push({
        id: actionId,
        scope: 'tool',
        toolId: id,
        title: '运行工具',
        combo: '',
        defaultCombo: '',
        effective: false,
        reason: '未设置'
      });
      mock.hotkeys[actionId] = {
        combo: '',
        defaultCombo: '',
        effective: false,
        reason: '未设置'
      };
      return { ok: true, id };
    }

    case 'set_hotkey':
      return setMockHotkey(args.actionId, args.combo);

    case 'reset_hotkey':
      return resetMockHotkey(args.actionId);

    case 'save_config':
      mock.config = { ...mock.config, ...(args.patch || {}) };
      return { ok: true };

    case 'reveal_path': {
      const paths = {
        data: mock.dataDir,
        officialTools: mock.officialToolsDir,
        installedTools: mock.installedToolsDir,
        logs: `${mock.dataDir}\\logs`
      };
      return { ok: true, path: paths[args.kind] || mock.dataDir };
    }

    case 'get_log_tail': {
      const lines = [
        '[10:42:18] INFO  toolbox: host started, version 0.1.0',
        '[10:42:18] INFO  store: config loaded from data/config.json',
        '[10:42:18] INFO  tools: scanning D:\\Toolbox\\tools',
        '[10:42:18] INFO  tools: loaded 2 official tool(s)',
        '[10:42:18] INFO  tools: scanning D:\\Toolbox\\data\\tools',
        '[10:42:18] INFO  tools: loaded 2 installed tool(s)',
        '[10:42:18] INFO  hotkeys: registered 8 global shortcut(s)',
        '[10:42:19] WARN  hotkeys: Ctrl+Alt+T is already taken by another process',
        '[10:42:19] WARN  hotkeys: Ctrl+Alt+G is already taken by another process',
        '[10:42:20] INFO  ball: created at right side, y=42%',
        '[10:42:20] INFO  main: window ready (1000x700)',
        '[10:42:20] INFO  host: ready'
      ];
      const count = Math.max(1, Number(args.lines) || 20);
      return { lines: lines.slice(-count) };
    }

    default:
      return { ok: true };
  }
}

/* ------------------------------------------------------------------------- */
/* Public API                                                                 */
/* ------------------------------------------------------------------------- */

export async function invoke(cmd, args = {}) {
  if (isHosted) {
    return window.__TAURI_INTERNALS__.invoke(cmd, args);
  }
  await sleep(70);
  return mockInvoke(cmd, args);
}

/**
 * Host → UI events.
 *
 * The Rust side emits `tools-changed` / `config-changed` by evaluating
 * `window.__hostEmit(event, payload)` in every host window (see `EmitHosts` in
 * ipc.rs), so this bridge installs that global and fans it out. Registering
 * through Tauri's event plugin as well keeps the page working if the host ever
 * switches to `emit`; duplicate deliveries are harmless because the UI treats
 * these events as "refetch state".
 */
const hostListeners = new Map();

function installEventSink() {
  if (typeof window.__hostEmit === 'function') return;
  window.__hostEmit = (event, payload) => {
    const handlers = hostListeners.get(event);
    if (!handlers) return;
    for (const handler of [...handlers]) {
      try {
        handler(payload);
      } catch (error) {
        console.error('[toolbox] event handler failed', event, error);
      }
    }
  };
}

export function listen(event, cb) {
  installEventSink();
  if (!hostListeners.has(event)) hostListeners.set(event, new Set());
  hostListeners.get(event).add(cb);

  let unlistenPlugin = () => {};
  if (isHosted) {
    const internals = window.__TAURI_INTERNALS__;
    try {
      if (typeof internals.transformCallback === 'function') {
        const handler = internals.transformCallback((payload) => {
          window.__hostEmit(event, payload && typeof payload === 'object' && 'payload' in payload ? payload.payload : payload);
        }, false);
        internals
          .invoke('plugin:event|listen', { event, target: { kind: 'Any' }, handler })
          .then((eventId) => {
            unlistenPlugin = () => {
              internals.invoke('plugin:event|unlisten', { event, eventId }).catch(() => {});
            };
          })
          .catch(() => {});
      }
    } catch (error) {
      console.warn('[toolbox] event plugin unavailable', event, error);
    }
  }

  return () => {
    const handlers = hostListeners.get(event);
    if (handlers) handlers.delete(cb);
    unlistenPlugin();
  };
}

/**
 * Minimize the main window. The IPC contract did not name a command for this,
 * so try the host's own `minimize_window` first, then Tauri 2's built-in window
 * plugin command (needs `core:window:allow-minimize` in the capability).
 */
export async function minimizeWindow() {
  if (!isHosted) return { ok: true, mock: true };
  const attempts = [
    ['minimize_window', { label: 'main' }],
    ['plugin:window|minimize', { label: 'main' }]
  ];
  let lastError = null;
  for (const [cmd, args] of attempts) {
    try {
      await window.__TAURI_INTERNALS__.invoke(cmd, args);
      return { ok: true };
    } catch (error) {
      lastError = error;
    }
  }
  return { ok: false, error: lastError && lastError.message ? lastError.message : String(lastError) };
}
