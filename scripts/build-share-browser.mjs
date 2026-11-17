import { existsSync } from 'node:fs'
import { spawnSync } from 'node:child_process'
import { dirname, resolve } from 'node:path'
import { fileURLToPath } from 'node:url'

const projectRoot = resolve(dirname(fileURLToPath(import.meta.url)), '..')
const sourceRoot = resolve(projectRoot, 'src-tauri/resources/share-browser-src')
const viteEntry = resolve(sourceRoot, 'node_modules/vite/bin/vite.js')
const vuePlugin = resolve(sourceRoot, 'node_modules/@vitejs/plugin-vue/package.json')
const npmCli = process.env.npm_execpath

if (!npmCli) {
  throw new Error('Run this script through npm so npm_execpath is available.')
}

function run(args) {
  const result = spawnSync(process.execPath, [npmCli, ...args], {
    cwd: projectRoot,
    stdio: 'inherit',
  })
  if (result.error) throw result.error
  if (result.status !== 0) process.exit(result.status ?? 1)
}

if (!existsSync(viteEntry) || !existsSync(vuePlugin)) {
  run(['ci', '--prefix', 'src-tauri/resources/share-browser-src', '--no-audit', '--no-fund'])
}

run(['--prefix', 'src-tauri/resources/share-browser-src', 'run', 'build'])
