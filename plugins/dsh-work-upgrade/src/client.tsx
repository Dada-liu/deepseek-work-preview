/**
 * dsh-work-upgrade 浏览器端：在「设置 → 通用设置」挂一行 DeepSeek Work
 * 桌面应用版本卡片——当前版本（Tauri 壳内经 window.__TAURI__ 读宿主应用
 * 版本）、检查更新、一键下载升级包（国内源优先，GitHub 兜底，由宿主侧
 * RPC 执行）。
 */
import * as React from 'react'

const PLUGIN_NAME = 'dsh-work-upgrade'
const NS = 'dsh-work-upgrade'

const zh = {
  title: '应用版本',
  desktopOnly: '仅桌面端可用',
  current: '当前版本',
  latest: '最新版本',
  checkUpdate: '检查更新',
  update: '升级到新版本',
  checking: '正在检查更新…',
  downloading: '正在下载升级包…',
  downloaded: '已下载并打开安装包，请按提示完成安装',
  alreadyLatest: '已经是最新版本',
  error: '失败',
}
const en = {
  title: 'App Version',
  desktopOnly: 'Desktop app only',
  current: 'Current',
  latest: 'Latest',
  checkUpdate: 'Check for updates',
  update: 'Download update',
  checking: 'Checking for updates…',
  downloading: 'Downloading update…',
  downloaded: 'Installer downloaded and opened — follow its prompts to finish',
  alreadyLatest: 'Already up to date',
  error: 'Failed',
}

interface RpcResult<T> {
  ok: boolean
  value?: T
  error?: { message?: string }
}

interface Rpc {
  call(channel: string, endpoint: string, payload: Record<string, unknown>): Promise<RpcResult<never>>
}

interface VersionInfo {
  version: string
  source: 'domestic' | 'github'
}

interface DownloadInfo {
  path: string
  source: 'domestic' | 'github'
}

interface TauriGlobal {
  core?: { invoke?: (command: string) => Promise<unknown> }
}

/** Tauri 壳内由 withGlobalTauri 注入；纯浏览器（dsh web）没有。 */
function tauriInvoke(command: string): Promise<string> | undefined {
  const tauri = (window as unknown as { __TAURI__?: TauriGlobal }).__TAURI__
  const invoke = tauri?.core?.invoke
  if (typeof invoke !== 'function') return undefined
  return invoke(command) as Promise<string>
}

/** x.y.z 数字段比较：a > b 返回正数。 */
function compareVersions(a: string, b: string): number {
  const pa = a.split('.').map(Number)
  const pb = b.split('.').map(Number)
  for (let i = 0; i < 3; i++) {
    const diff = (pa[i] || 0) - (pb[i] || 0)
    if (diff !== 0) return diff
  }
  return 0
}

interface RowProps {
  t: (key: keyof typeof zh) => string
  rpc: Rpc
}

function UpgradeRow({ t, rpc }: RowProps) {
  const [current, setCurrent] = React.useState('')
  const [latest, setLatest] = React.useState('')
  const [status, setStatus] = React.useState('')
  const [busy, setBusy] = React.useState(false)

  React.useEffect(() => {
    const invoked = tauriInvoke('app_version')
    if (!invoked) {
      setStatus(t('desktopOnly'))
      return
    }
    invoked
      .then((version) => setCurrent(String(version)))
      .catch((error) => {
        setStatus(`${t('error')}: ${error instanceof Error ? error.message : String(error)}`)
      })
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [])

  const isDesktop = tauriInvoke('app_version') !== undefined
  const canUpdate = !busy && isDesktop && current !== '' && latest !== '' && compareVersions(latest, current) > 0

  const handleCheckUpdate = async () => {
    setBusy(true)
    setLatest('')
    setStatus(t('checking'))
    const result = await rpc.call('/dsh-work-upgrade', 'getLatestVersion', {})
    setBusy(false)
    if (result.ok) {
      const info = result.value as unknown as VersionInfo
      setLatest(info.version)
      setStatus(isDesktop && current !== '' && compareVersions(info.version, current) <= 0 ? t('alreadyLatest') : '')
    } else {
      setStatus(`${t('error')}: ${result.error?.message || ''}`)
    }
  }

  const handleUpdate = async () => {
    setBusy(true)
    setStatus(t('downloading'))
    const result = await rpc.call('/dsh-work-upgrade', 'downloadUpdate', { version: latest })
    setBusy(false)
    if (result.ok) {
      const info = result.value as unknown as DownloadInfo
      setStatus(`${t('downloaded')}: ${info.path}`)
    } else {
      setStatus(`${t('error')}: ${result.error?.message || ''}`)
    }
  }

  const buttonStyle: React.CSSProperties = {
    flex: 'none',
    height: '28px',
    padding: '0 12px',
    border: '1px solid var(--dsw-alias-border-l2)',
    borderRadius: '8px',
    background: 'transparent',
    color: 'var(--dsw-alias-label-primary)',
    fontSize: '13px',
    lineHeight: '18px',
    cursor: 'pointer',
  }
  const buttonDisabledStyle: React.CSSProperties = {
    ...buttonStyle,
    opacity: 0.5,
    cursor: 'not-allowed',
  }

  return (
    <div
      style={{
        display: 'flex',
        alignItems: 'flex-start',
        justifyContent: 'space-between',
        gap: '12px',
        padding: '12px 0',
      }}
    >
      <div style={{ minWidth: 0 }}>
        <div
          style={{
            fontSize: '14px',
            lineHeight: '20px',
            color: 'var(--dsw-alias-label-primary)',
          }}
        >
          {t('title')}
        </div>
        <div
          style={{
            fontSize: '13px',
            lineHeight: '18px',
            color: 'var(--dsw-alias-label-tertiary)',
          }}
        >
          {t('current')}: {current || '-'}
        </div>
        {latest && (
          <div
            style={{
              fontSize: '13px',
              lineHeight: '18px',
              color: 'var(--dsw-alias-label-tertiary)',
            }}
          >
            {t('latest')}: {latest}
          </div>
        )}
        {status && (
          <div
            style={{
              marginTop: '4px',
              fontSize: '12px',
              lineHeight: '18px',
              color: 'var(--dsw-alias-label-tertiary)',
              whiteSpace: 'pre-wrap',
              wordBreak: 'break-word',
            }}
          >
            {status}
          </div>
        )}
      </div>
      <div style={{ display: 'flex', gap: '8px', flexShrink: 0 }}>
        <button
          type="button"
          style={busy || !isDesktop ? buttonDisabledStyle : buttonStyle}
          disabled={busy || !isDesktop}
          onClick={handleCheckUpdate}
        >
          {t('checkUpdate')}
        </button>
        <button
          type="button"
          style={canUpdate ? buttonStyle : buttonDisabledStyle}
          disabled={!canUpdate}
          onClick={handleUpdate}
        >
          {t('update')}
        </button>
      </div>
    </div>
  )
}

interface SlotsFace {
  inject(name: string, register: () => unknown): void
  register(meta: Record<string, unknown>, component: unknown): unknown
}

interface LocaleFace {
  register(ns: string, dicts: { zh: typeof zh; en: typeof en }): unknown
}

interface ClientCtx {
  slots: SlotsFace
  locale: LocaleFace
  connection: { rpc: Rpc }
  effect(execute: () => unknown, label?: string): void
}

export const name = PLUGIN_NAME
export const inject = ['slots', 'locale', 'connection']

export function apply(ctx: ClientCtx): void {
  ctx.effect(() => ctx.locale.register(NS, { zh, en }), `${NS}: dictionaries`)
  ctx.slots.inject('settings.general.item', () =>
    ctx.slots.register(
      {
        name: 'settings.general.item',
        id: 'dsh-work-upgrade',
        order: 100,
        locale: NS,
        inject: () => ({ rpc: ctx.connection.rpc }),
      },
      UpgradeRow,
    ),
  )
}
