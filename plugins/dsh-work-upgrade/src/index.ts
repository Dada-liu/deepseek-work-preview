/**
 * dsh-work-upgrade 宿主入口。
 *
 * 通过 RPC channel 提供 DeepSeek Work 桌面应用的版本查询与升级包下载：
 * 国内源（官网 www.hotpotliuyu.com/ds-work）优先，GitHub Releases 兜底。
 *
 * 已知限制：官网 js/main.js 中的版本号由官网发版流程维护，滞后于 GitHub
 * 时国内渠道会返回偏旧的版本（不报错，只是可升级版本旧）。
 */
import { createWriteStream } from 'node:fs'
import { homedir, platform } from 'node:os'
import { join } from 'node:path'
import { spawn } from 'node:child_process'
import { Readable } from 'node:stream'
import { pipeline } from 'node:stream/promises'

const DOMESTIC_MAIN_JS = 'https://www.hotpotliuyu.com/ds-work/js/main.js'
const DOMESTIC_DOWNLOAD_BASE = 'https://www.hotpotliuyu.com/ds-work/downloads'
const GITHUB_LATEST =
  'https://api.github.com/repos/Dada-liu/deepseek-work-preview/releases/latest'

/** 允许下载的域名白名单（防 SSRF 滥用本端点探测内网）。 */
const ALLOWED_HOSTS = new Set([
  'www.hotpotliuyu.com',
  'api.github.com',
  'github.com',
  'objects.githubusercontent.com',
])

const TIMEOUT_MS = 10_000

/** 带超时的 fetch。 */
function fetchWithTimeout(url: string, init?: RequestInit): Promise<Response> {
  const controller = new AbortController()
  const timer = setTimeout(() => controller.abort(), TIMEOUT_MS)
  return fetch(url, { ...init, signal: controller.signal }).finally(() => clearTimeout(timer))
}

interface VersionInfo {
  version: string
  source: 'domestic' | 'github'
}

/** 国内渠道：从官网 main.js 的 DOMESTIC_URLS 解析版本号。 */
async function getDomesticVersion(): Promise<string> {
  const res = await fetchWithTimeout(DOMESTIC_MAIN_JS)
  if (!res.ok) throw new Error(`官网返回 ${res.status}`)
  const text = await res.text()
  const match = /DeepSeek\.Work_(\d+\.\d+\.\d+)_macOS\.zip/.exec(text)
  if (!match) throw new Error('官网 main.js 中未找到版本号')
  return match[1]
}

interface GithubAsset {
  name: string
  browser_download_url: string
}

/** GitHub 渠道：最新 release 的版本号与资产列表。 */
async function getGithubRelease(): Promise<{ version: string; assets: GithubAsset[] }> {
  const res = await fetchWithTimeout(GITHUB_LATEST, {
    headers: { 'user-agent': 'dsh-work-upgrade', accept: 'application/vnd.github+json' },
  })
  if (!res.ok) throw new Error(`GitHub API 返回 ${res.status}`)
  const data = (await res.json()) as { tag_name?: string; assets?: GithubAsset[] }
  if (typeof data.tag_name !== 'string') throw new Error('GitHub 响应缺少 tag_name')
  return { version: data.tag_name.replace(/^v/, ''), assets: data.assets ?? [] }
}

/** 最新版本：国内优先，GitHub 兜底。 */
async function getLatestVersion(): Promise<VersionInfo> {
  try {
    return { version: await getDomesticVersion(), source: 'domestic' }
  } catch {
    return { version: (await getGithubRelease()).version, source: 'github' }
  }
}

/** 当前平台的国内 zip 文件名。不支持的平台返回 undefined。 */
function domesticFileName(version: string): string | undefined {
  if (platform() === 'darwin') return `DeepSeek.Work_${version}_macOS.zip`
  if (platform() === 'win32') return `DeepSeek.Work_${version}_Windows.zip`
  return undefined
}

/** 当前平台的 GitHub release 资产匹配规则。 */
function githubAssetPattern(): RegExp | undefined {
  if (platform() === 'darwin') return /aarch64\.dmg$/
  if (platform() === 'win32') return /x64-setup\.exe$/
  return undefined
}

function assertAllowedUrl(url: string): void {
  const { hostname } = new URL(url)
  if (!ALLOWED_HOSTS.has(hostname)) throw new Error(`不允许的下载域名: ${hostname}`)
}

/** 下载 url 到 ~/Downloads 并用系统默认方式打开/定位。 */
async function downloadAndOpen(url: string): Promise<string> {
  assertAllowedUrl(url)
  const fileName = decodeURIComponent(url.split('/').pop() ?? 'download')
  const dest = join(homedir(), 'Downloads', fileName)
  const res = await fetchWithTimeout(url, { headers: { 'user-agent': 'dsh-work-upgrade' } })
  if (!res.ok || !res.body) throw new Error(`下载失败：HTTP ${res.status}`)
  await pipeline(Readable.fromWeb(res.body as import('node:stream/web').ReadableStream), createWriteStream(dest))
  if (platform() === 'darwin') {
    // dmg 挂载 / zip 调起归档工具解压
    spawn('open', [dest], { detached: true, stdio: 'ignore' }).unref()
  } else if (platform() === 'win32') {
    if (dest.endsWith('.exe')) {
      spawn(dest, [], { detached: true, stdio: 'ignore' }).unref()
    } else {
      spawn('explorer', [`/select,${dest}`], { detached: true, stdio: 'ignore' }).unref()
    }
  }
  return dest
}

/**
 * 下载指定版本的升级包：国内 zip 优先（HEAD 探测存在性），失败回退 GitHub
 * release 资产。返回保存路径与渠道。
 */
async function downloadUpdate(version: string): Promise<{ path: string; source: VersionInfo['source'] }> {
  const domesticName = domesticFileName(version)
  if (domesticName !== undefined) {
    const domesticUrl = `${DOMESTIC_DOWNLOAD_BASE}/${domesticName}`
    try {
      const probe = await fetchWithTimeout(domesticUrl, { method: 'HEAD' })
      if (probe.ok) return { path: await downloadAndOpen(domesticUrl), source: 'domestic' }
    } catch {
      // 探测失败，落 GitHub
    }
  }
  const pattern = githubAssetPattern()
  if (pattern === undefined) throw new Error(`不支持的平台: ${platform()}`)
  const { assets } = await getGithubRelease()
  const asset = assets.find((a) => pattern.test(a.name))
  if (!asset) throw new Error(`最新 release 中没有匹配 ${pattern} 的资产`)
  return { path: await downloadAndOpen(asset.browser_download_url), source: 'github' }
}

type RpcOk = { ok: true; value: unknown }
type RpcErr = { ok: false; error: { code: string; message: string; details: Record<string, never> } }

function ok(value: unknown): RpcOk {
  return { ok: true, value }
}

function err(error: unknown): RpcErr {
  return {
    ok: false,
    error: {
      code: 'internal-error',
      message: error instanceof Error ? error.message : String(error),
      details: {},
    },
  }
}

async function rpcHandler(endpoint: string, payload: unknown): Promise<RpcOk | RpcErr> {
  try {
    switch (endpoint) {
      case 'getLatestVersion':
        return ok(await getLatestVersion())
      case 'downloadUpdate': {
        const version = (payload as { version?: unknown } | undefined)?.version
        if (typeof version !== 'string' || !/^\d+\.\d+\.\d+$/.test(version)) {
          throw new Error('downloadUpdate 需要形如 x.y.z 的 version 参数')
        }
        return ok(await downloadUpdate(version))
      }
      default:
        return err(new Error(`未知端点: ${endpoint}`))
    }
  } catch (error) {
    return err(error)
  }
}

interface ConnectionFace {
  rpc: {
    handle(
      channel: string,
      handler: (endpoint: string, payload: unknown, signal: AbortSignal) => Promise<unknown>,
      options: { authority: string },
    ): Promise<() => void> | (() => void)
  }
}

export const name = 'dsh-work-upgrade'
export const inject = ['connection']

export async function apply(ctx: { connection: ConnectionFace }): Promise<(() => void) | void> {
  const dispose = await ctx.connection.rpc.handle('/dsh-work-upgrade', rpcHandler, {
    authority: 'loopback',
  })
  return dispose
}

// 测试用导出
export { getLatestVersion, getDomesticVersion, domesticFileName, githubAssetPattern }
