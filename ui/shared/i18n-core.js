/**
 * i18next bootstrap shared by the host pages (panel, main window).
 *
 * Each page owns a small `*-i18n.js` that imports only the namespaces it needs
 * (the panel must never pull in the main window's word list) and calls
 * `initI18n` with its own resource bundle.
 *
 * Language selection, in order:
 *   1. `?lng=` query parameter — dev only, for reviewing a translation without
 *      touching the settings (`?lng=en-US`, `?lng=en-XA`).
 *   2. `window.__TOOLBOX_ENV__.locale`, injected by the Rust environment service
 *      before the page runs.
 *   3. the browser's own preference (`pnpm dev` outside Tauri).
 *
 * A live `locale` event from the host switches the language without a reload.
 * `navigator.language` is *not* updated by that switch — code inside a host page
 * must read `i18next.language`; inside a tool it is `__toolbox.locale`.
 */
import i18next from 'i18next';

const DEV = Boolean(import.meta.env && import.meta.env.DEV);

/** Wrap every string so untranslated literals stand out in a screenshot. */
function pseudoResources(resources) {
  const wrap = (value) => (typeof value === 'string' ? `⟦${value}⟧` : value);
  const walk = (node) => {
    if (typeof node === 'string') return wrap(node);
    if (Array.isArray(node)) return node.map(walk);
    if (node && typeof node === 'object') {
      const out = {};
      for (const [key, child] of Object.entries(node)) out[key] = walk(child);
      return out;
    }
    return node;
  };
  const out = {};
  for (const [locale, namespaces] of Object.entries(resources)) out[locale] = walk(namespaces);
  out['en-XA'] = walk(resources['en-US'] || resources['zh-CN']);
  return out;
}

export async function initI18n({ resources, namespaces, defaultNS }) {
  const env = window.__TOOLBOX_ENV__;
  const override = DEV ? new URLSearchParams(location.search).get('lng') : null;
  const browserLocale = (navigator.language || '').toLowerCase().startsWith('zh') ? 'zh-CN' : 'en-US';
  let language = override || (env && env.locale) || browserLocale;

  const usePseudo = DEV && language === 'en-XA';
  const bundle = usePseudo ? pseudoResources(resources) : resources;

  await i18next.init({
    lng: usePseudo ? 'en-XA' : language,
    // zh-CN is the source language: a missing key shows Chinese, never a raw key.
    fallbackLng: 'zh-CN',
    supportedLngs: ['zh-CN', 'en-US', 'en-XA'],
    nonExplicitSupportedLngs: false,
    ns: namespaces,
    defaultNS,
    resources: bundle,
    interpolation: { escapeValue: false },
    returnNull: false,
  });

  if (window.__hostOn) {
    window.__hostOn('locale', (locale) => {
      if (locale === 'zh-CN' || locale === 'en-US') {
        i18next.changeLanguage(locale);
      }
    });
  }

  // Debug handle: `__i18n.language` / `__i18n.hasResourceBundle(...)` from the
  // devtools console or a CDP probe. Cheap, and it keeps the live language
  // discoverable without importing the module again.
  window.__i18n = i18next;

  return i18next;
}

/** Translate the `data-i18n` / `data-i18n-placeholder` / `data-i18n-title` attributes. */
export function translateDom(root = document) {
  root.querySelectorAll('[data-i18n]').forEach((el) => {
    el.textContent = i18next.t(el.getAttribute('data-i18n'));
  });
  root.querySelectorAll('[data-i18n-placeholder]').forEach((el) => {
    el.setAttribute('placeholder', i18next.t(el.getAttribute('data-i18n-placeholder')));
  });
  root.querySelectorAll('[data-i18n-title]').forEach((el) => {
    el.setAttribute('title', i18next.t(el.getAttribute('data-i18n-title')));
  });
}

export default i18next;
