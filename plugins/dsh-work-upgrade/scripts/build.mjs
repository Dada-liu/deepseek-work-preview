#!/usr/bin/env node
/**
 * 构建 dsh-work-upgrade（参照 dsh-version-plugin 的产物形态）。
 *
 * - src/index.ts   -> lib/index.js   (host 入口，ESM)
 * - src/client.tsx -> lib/client.js  (浏览器端，CJS 并包装成 DSH 客户端模块格式)
 */
import { readFileSync, writeFileSync, mkdirSync } from 'node:fs'
import { dirname, join, resolve } from 'node:path'
import { fileURLToPath, pathToFileURL } from 'node:url'

const root = join(dirname(fileURLToPath(import.meta.url)), '..')
const pkg = JSON.parse(readFileSync(join(root, 'package.json'), 'utf8'))
const id = pkg.name
const outDir = join(root, 'lib')

// 插件随宿主项目构建，不自带 node_modules：esbuild 从主项目的 pnpm
// 公共提升目录取（plugins/dsh-work-upgrade -> 项目根）。
const esbuildCandidates = [
  resolve(join(root, 'node_modules', 'esbuild', 'lib', 'main.js')),
  resolve(join(root, '..', '..', 'node_modules', '.pnpm', 'node_modules', 'esbuild', 'lib', 'main.js')),
]

async function loadEsbuild() {
  for (const path of esbuildCandidates) {
    try {
      // 动态 import 需要 file:// URL——Windows 下反斜杠路径会被当成 URL scheme
      return await import(pathToFileURL(path).href)
    } catch {
      // 尝试下一个候选
    }
  }
  return undefined
}

const esbuild = await loadEsbuild()
if (!esbuild) {
  console.error('[build] 错误：找不到 esbuild，无法构建 TypeScript/JSX 源码')
  process.exit(1)
}

mkdirSync(outDir, { recursive: true })

// 1. host 入口
const hostResult = await esbuild.build({
  entryPoints: [join(root, 'src', 'index.ts')],
  bundle: false,
  write: false,
  format: 'esm',
  platform: 'node',
  target: 'node18',
  sourcemap: true,
})
writeFileSync(join(outDir, 'index.js'), hostResult.outputFiles[0].text)
console.log(`[build] src/index.ts -> lib/index.js`)

// 2. 浏览器端
function indent(text, prefix) {
  return text
    .split('\n')
    .map((line) => (line.trimEnd() ? prefix + line : line))
    .join('\n')
}

function wrapModule(body) {
  return [
    `window.__ModuleLoader__.load({ id: ${JSON.stringify(id)}, factory: (require) => {`,
    '\tvar module = { exports: {} };',
    '\tvar exports = module.exports;',
    '',
    indent(body, '\t'),
    '',
    '\treturn module.exports;',
    '}});',
    '',
  ].join('\n')
}

const clientResult = await esbuild.build({
  entryPoints: [join(root, 'src', 'client.tsx')],
  bundle: true,
  write: false,
  format: 'cjs',
  platform: 'browser',
  target: 'es2022',
  external: ['react'],
  legalComments: 'none',
  define: { 'process.env.NODE_ENV': '"production"' },
})
writeFileSync(join(outDir, 'client.js'), wrapModule(clientResult.outputFiles[0].text))
console.log(`[build] src/client.tsx -> lib/client.js`)
