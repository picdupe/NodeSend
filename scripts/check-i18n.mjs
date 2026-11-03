import { readFile, readdir } from 'node:fs/promises';

const locales = ['zh-CN', 'zh-TW', 'en-US', 'ja-JP'];
const namespaces = ['common', 'transfer', 'settings', 'errors'];
const root = new URL('../src/i18n/locales/', import.meta.url);

function flatten(value, prefix = '', output = {}) {
  for (const [key, entry] of Object.entries(value)) {
    const path = prefix ? `${prefix}.${key}` : key;
    if (entry && typeof entry === 'object' && !Array.isArray(entry)) flatten(entry, path, output);
    else output[path] = String(entry);
  }
  return output;
}

function params(value) {
  return [...value.matchAll(/\{\{\s*([^,}\s]+)(?:,[^}]*)?\s*\}\}/g)].map(match => match[1]).sort();
}

let errors = 0;
const catalogs = new Map();
for (const namespace of namespaces) {
  const base = flatten(JSON.parse(await readFile(new URL(`${locales[0]}/${namespace}.json`, root), 'utf8')));
  catalogs.set(`${locales[0]}:${namespace}`, base);
  for (const locale of locales.slice(1)) {
    const translations = flatten(JSON.parse(await readFile(new URL(`${locale}/${namespace}.json`, root), 'utf8')));
    catalogs.set(`${locale}:${namespace}`, translations);
    for (const key of Object.keys(base)) {
      if (!(key in translations)) {
        console.error(`[${locale}/${namespace}] Missing key: ${key}`);
        errors++;
      } else if (JSON.stringify(params(base[key])) !== JSON.stringify(params(translations[key]))) {
        console.error(`[${locale}/${namespace}] Placeholder mismatch: ${key}`);
        errors++;
      }
    }
    for (const key of Object.keys(translations)) {
      if (!(key in base)) {
        console.error(`[${locale}/${namespace}] Extra key: ${key}`);
        errors++;
      }
    }
  }
}

async function sourceFiles(directory) {
  const files = [];
  for (const entry of await readdir(directory, { withFileTypes: true })) {
    const path = new URL(`${entry.name}${entry.isDirectory() ? '/' : ''}`, directory);
    if (entry.isDirectory()) files.push(...await sourceFiles(path));
    else if (/\.(?:ts|tsx)$/.test(entry.name) && !entry.name.endsWith('.d.ts')) files.push(path);
  }
  return files;
}

const sources = await sourceFiles(new URL('../src/', import.meta.url));
const baseCatalogs = namespaces.map(namespace => catalogs.get(`${locales[0]}:${namespace}`));
const checkedKeys = new Set();
for (const file of sources) {
  const source = await readFile(file, 'utf8');
  for (const match of source.matchAll(/\bt\(\s*['"]([^'"]+)['"]/g)) {
    const raw = match[1];
    if (raw.includes('{{') || raw.includes('}')) continue;
    const separator = raw.indexOf(':');
    const namespace = separator >= 0 ? raw.slice(0, separator) : null;
    const key = separator >= 0 ? raw.slice(separator + 1) : raw;
    const exists = namespace
      ? baseCatalogs[namespaces.indexOf(namespace)]?.[key] !== undefined
      : baseCatalogs.some(catalog => catalog?.[key] !== undefined);
    if (!exists && !checkedKeys.has(raw)) {
      console.error(`[source] Missing translation key: ${raw} (${file.pathname})`);
      checkedKeys.add(raw);
      errors++;
    }
  }
  for (const match of source.matchAll(/\bt\(\s*['"]([^'"]+)['"]\s*,\s*\{([^}]*)\}/g)) {
    const raw = match[1];
    const separator = raw.indexOf(':');
    const namespace = separator >= 0 ? raw.slice(0, separator) : null;
    const key = separator >= 0 ? raw.slice(separator + 1) : raw;
    const catalog = namespace
      ? baseCatalogs[namespaces.indexOf(namespace)]
      : baseCatalogs.find(candidate => candidate?.[key] !== undefined);
    const expected = catalog?.[key];
    if (expected === undefined) continue;
    const actual = [...match[2].matchAll(/(?:^|,)\s*([A-Za-z_$][\w$]*)\s*(?=:|,|$)/g)]
      .map(item => item[1]).filter(name => !['ns', 'defaultValue'].includes(name)).sort();
    if (JSON.stringify(actual) !== JSON.stringify(params(expected))) {
      console.error(`[source] Interpolation mismatch: ${raw}; expected ${params(expected).join(', ') || 'none'}, got ${actual.join(', ') || 'none'}`);
      errors++;
    }
  }
}

const cliSource = await readFile(new URL('../src-tauri/src/cli.rs', import.meta.url), 'utf8');
for (const match of cliSource.matchAll(/\bcli_text(?:_args)?\(\s*"([^"]+)"/g)) {
  const key = `cli.${match[1]}`;
  if (!baseCatalogs[0][key]) {
    console.error(`[source] Missing CLI translation key: ${key}`);
    if (!checkedKeys.has(key)) { checkedKeys.add(key); errors++; }
  }
}
const entrySource = await readFile(new URL('../src-tauri/src/main.rs', import.meta.url), 'utf8');
for (const match of entrySource.matchAll(/cli_translate\(\s*"([^"]+)"/g)) {
  const key = `cli.${match[1]}`;
  if (!baseCatalogs[0][key]) {
    console.error(`[source] Missing CLI translation key: ${key}`);
    if (!checkedKeys.has(key)) { checkedKeys.add(key); errors++; }
  }
}

const taskEventSource = await readFile(new URL('../src/components/transfer/taskEvent.ts', import.meta.url), 'utf8');
const exactEventMap = taskEventSource.match(/const exactEvents:[^{]+\{([\s\S]*?)\n\};/);
if (exactEventMap) {
  for (const match of exactEventMap[1].matchAll(/:\s*'([A-Za-z]\w*)'/g)) {
    const key = `tasks.events.${match[1]}`;
    if (!baseCatalogs[1][key]) {
      console.error(`[source] Missing transfer translation key: ${key}`);
      errors++;
    }
  }
}

if (errors) process.exitCode = 1;
else console.log(`i18n check passed: ${locales.length} locales × ${namespaces.length} namespaces`);
