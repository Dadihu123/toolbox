#!/usr/bin/env node
/**
 * `pnpm i18n:check` — guard rails for the webview word lists.
 *
 * Checks, in order:
 *   1. every namespace has the same key set in every locale;
 *   2. no value is empty or whitespace;
 *   3. `en-US` values contain no CJK characters (a forgotten translation);
 *   4. every `t('ns:key')` / `t('key')` / `data-i18n*="key"` used in `ui/**`
 *      resolves to a key that exists (so a renamed key fails the build);
 *   5. reports keys that no source file uses (warning only — a key may be used
 *      by a page that is still being ported).
 *
 * The Rust word lists (`src-tauri/locales/*.json`) are covered by `cargo test`
 * (`l10n.rs` has the same first three rules); this script is the frontend half.
 */
import { readdirSync, readFileSync, statSync } from 'node:fs';
import { join, relative, resolve } from 'node:path';

const root = resolve(import.meta.dirname, '..');
const localesDir = join(root, 'ui', 'locales');
const sourceDirs = [join(root, 'ui')];
const strictLocales = ['zh-CN', 'en-US'];

const problems = [];
const warnings = [];

const fail = (message) => problems.push(message);
const warn = (message) => warnings.push(message);

/* ------------------------------------------------------------------ load */

const locales = readdirSync(localesDir, { withFileTypes: true })
  .filter((entry) => entry.isDirectory())
  .map((entry) => entry.name)
  .sort();

if (locales.length === 0) fail(`no locale directories under ${localesDir}`);
for (const locale of strictLocales) {
  if (!locales.includes(locale)) fail(`missing required locale directory: ${locale}`);
}

/** locale -> namespace -> flat key -> value */
const wordlists = new Map();

function flatten(value, prefix, out) {
  if (value !== null && typeof value === 'object' && !Array.isArray(value)) {
    for (const [key, child] of Object.entries(value)) {
      flatten(child, prefix ? `${prefix}.${key}` : key, out);
    }
    return;
  }
  if (typeof value !== 'string') {
    fail(`${prefix}: expected a string, found ${JSON.stringify(value)}`);
    return;
  }
  out.set(prefix, value);
}

for (const locale of locales) {
  const dir = join(localesDir, locale);
  const namespaces = new Map();
  for (const file of readdirSync(dir).filter((name) => name.endsWith('.json')).sort()) {
    const namespace = file.slice(0, -'.json'.length);
    const flat = new Map();
    let parsed;
    try {
      parsed = JSON.parse(readFileSync(join(dir, file), 'utf8'));
    } catch (error) {
      fail(`${locale}/${file}: invalid JSON (${error.message})`);
      continue;
    }
    flatten(parsed, '', flat);
    namespaces.set(namespace, flat);
  }
  if (namespaces.size === 0) fail(`${locale}: no namespace files`);
  wordlists.set(locale, namespaces);
}

/* -------------------------------------------------------------- 1-3 rules */

const namespaceNames = new Set();
for (const namespaces of wordlists.values()) {
  for (const namespace of namespaces.keys()) namespaceNames.add(namespace);
}

for (const namespace of [...namespaceNames].sort()) {
  const reference = wordlists.get('zh-CN')?.get(namespace);
  if (!reference) {
    fail(`namespace "${namespace}" is missing from zh-CN`);
    continue;
  }
  for (const [locale, namespaces] of wordlists) {
    const current = namespaces.get(namespace);
    if (!current) {
      fail(`${locale}: namespace "${namespace}" is missing`);
      continue;
    }
    for (const key of reference.keys()) {
      if (!current.has(key)) fail(`${locale}:${namespace}: missing key "${key}"`);
    }
    for (const key of current.keys()) {
      if (!reference.has(key)) fail(`${locale}:${namespace}: extra key "${key}" (not in zh-CN)`);
    }
    for (const [key, value] of current) {
      if (value.trim() === '') fail(`${locale}:${namespace}: empty value for "${key}"`);
    }
  }
}

const CJK = /[\u3000-\u303f\u4e00-\u9fff\uf900-\ufaff\uff00-\uffef]/;
const english = wordlists.get('en-US');
if (english) {
  for (const [namespace, flat] of english) {
    for (const [key, value] of flat) {
      if (CJK.test(value)) {
        fail(`en-US:${namespace}: "${key}" still contains CJK: ${JSON.stringify(value)}`);
      }
    }
  }
}

/* ------------------------------------------------------------ 4-5 usage */

const sourceExtensions = new Set(['.js', '.mjs', '.html']);
const sourceFiles = [];
const walk = (dir) => {
  for (const entry of readdirSync(dir, { withFileTypes: true })) {
    if (entry.name === 'node_modules' || entry.name === 'dist') continue;
    const path = join(dir, entry.name);
    if (entry.isDirectory()) walk(path);
    else if (sourceExtensions.has(path.slice(path.lastIndexOf('.')))) sourceFiles.push(path);
  }
};
for (const dir of sourceDirs) walk(dir);

const used = new Map(); // "ns:key" or "key" -> [files]
const record = (key, file) => {
  if (!key) return;
  if (!used.has(key)) used.set(key, []);
  used.get(key).push(relative(root, file));
};

for (const file of sourceFiles) {
  const text = readFileSync(file, 'utf8');
  // t("ns:key"), t('key', {...}), i18n.t(...)
  for (const match of text.matchAll(/\bt\(\s*["']([A-Za-z0-9_.:-]+)["']/g)) record(match[1], file);
  // data-i18n="ns:key" / data-i18n-placeholder="key" / data-i18n-title="key"
  for (const match of text.matchAll(/data-i18n(?:-placeholder|-title)?\s*=\s*["']([A-Za-z0-9_.:-]+)["']/g)) {
    record(match[1], file);
  }
}

const exists = (raw) => {
  const [maybeNs, ...rest] = raw.split(':');
  if (rest.length > 0) {
    const flat = english?.get(maybeNs) ?? wordlists.get('zh-CN')?.get(maybeNs);
    return Boolean(flat && flat.has(rest.join(':')));
  }
  // No namespace prefix: valid if any namespace defines the key.
  for (const namespaces of wordlists.values()) {
    for (const flat of namespaces.values()) if (flat.has(raw)) return true;
  }
  return false;
};

for (const [key, files] of used) {
  if (!exists(key)) fail(`used but not defined: "${key}" (${[...new Set(files)].join(', ')})`);
}

const referenced = new Set(used.keys());
for (const flat of wordlists.get('zh-CN')?.values() ?? []) {
  for (const key of flat.keys()) {
    const withNs = [...namespaceNames].map((ns) => `${ns}:${key}`);
    if (!referenced.has(key) && !withNs.some((candidate) => referenced.has(candidate))) {
      warn(`defined but unused: "${key}"`);
    }
  }
}

/* ---------------------------------------------------------------- report */

for (const message of warnings) console.warn(`warn  ${message}`);
for (const message of problems) console.error(`error ${message}`);

const keyCount = [...(wordlists.get('zh-CN')?.values() ?? [])].reduce(
  (sum, flat) => sum + flat.size,
  0
);
if (problems.length > 0) {
  console.error(`\ni18n:check failed — ${problems.length} problem(s), ${warnings.length} warning(s)`);
  process.exit(1);
}
console.log(
  `i18n:check ok — ${locales.length} locales, ${namespaceNames.size} namespaces, ${keyCount} keys, ` +
    `${used.size} referenced keys, ${warnings.length} warning(s)`
);
