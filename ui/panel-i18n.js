/**
 * The panel's word list: namespaces `common` + `panel` only.
 *
 * The panel is on the hotkey path (must appear in ~100 ms), so its resources are
 * imported statically and bundled instead of fetched — the whole bundle is a few
 * kilobytes and this saves four round trips on first open.
 */
import { initI18n } from './shared/i18n-core.js';

import zhCommon from './locales/zh-CN/common.json';
import zhPanel from './locales/zh-CN/panel.json';
import enCommon from './locales/en-US/common.json';
import enPanel from './locales/en-US/panel.json';

export function setupPanelI18n() {
  return initI18n({
    resources: {
      'zh-CN': { common: zhCommon, panel: zhPanel },
      'en-US': { common: enCommon, panel: enPanel },
    },
    namespaces: ['common', 'panel'],
    defaultNS: 'panel',
  });
}
