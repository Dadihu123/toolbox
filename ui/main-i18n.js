/**
 * The main window's word list: namespaces `common` + `main` only.
 *
 * `main` is the default namespace, so page copy is addressed as `main:…` and
 * shared strings (Close, Refresh, Not set, …) as `common:…` — never with a dot.
 * Both pages import their resources statically, like the panel: the word lists
 * are a few kilobytes and this keeps the first paint free of a round trip.
 */
import { initI18n } from './shared/i18n-core.js';

import zhCommon from './locales/zh-CN/common.json';
import zhMain from './locales/zh-CN/main.json';
import enCommon from './locales/en-US/common.json';
import enMain from './locales/en-US/main.json';

export function setupMainI18n() {
  return initI18n({
    resources: {
      'zh-CN': { common: zhCommon, main: zhMain },
      'en-US': { common: enCommon, main: enMain },
    },
    namespaces: ['common', 'main'],
    defaultNS: 'main',
  });
}
