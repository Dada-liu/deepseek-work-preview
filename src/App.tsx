import { useEffect, useState } from 'react'
import { invoke } from '@tauri-apps/api/core'
import { listen } from '@tauri-apps/api/event'
import './App.css'

interface DshStatus {
  running: boolean
  url: string | null
}

// `invoke`/`listen` throw when this page is opened outside the Tauri webview
// (e.g. the vite dev URL in a regular browser).
const isTauri = '__TAURI_INTERNALS__' in window

// First boot unpacks ~450 MB of runtime, which can take minutes. Once the
// wait passes 30s, rotate reassurance copy every 10s so the user knows the
// app is not stuck.
const ROTATE_AFTER_SECONDS = 30
const ROTATE_INTERVAL_SECONDS = 10
const ROTATING_MESSAGES = [
  '首次启动需要解压运行环境，请耐心等待…',
  '正在准备依赖组件，可能需要几分钟…',
  '初始化仍在进行中，请勿关闭窗口…',
  '即将完成，感谢你的耐心等待…',
]

export default function App() {
  const [statusMessage, setStatusMessage] = useState('正在初始化…')
  const [elapsed, setElapsed] = useState(0)
  const [error, setError] = useState<string | null>(null)

  useEffect(() => {
    if (!isTauri) {
      setError('本页面只能在 DeepSeek Work 桌面应用窗口中打开')
      return
    }

    const unlistenStatus = listen<string>('boot-status', (event) => {
      setStatusMessage(event.payload)
    })

    // Navigate from the frontend itself: the Rust-side window.navigate can
    // race with a slow page load in dev and get lost.
    const unlistenReady = listen<string>('dsh-ready', (event) => {
      setStatusMessage('正在加载 DeepSeek Work…')
      window.location.href = event.payload
    })

    const unlistenError = listen<string>('dsh-error', (event) => {
      setError(event.payload)
    })

    // Ask Rust to start the boot sequence (idempotent; re-emits dsh-ready if
    // the service is already up).
    void invoke('start_dsh_service').catch((err: unknown) => {
      setError(err instanceof Error ? err.message : String(err))
    })

    // Fallback polling in case the ready event was fired before this page
    // finished loading.
    const timer = window.setInterval(() => {
      void invoke<DshStatus>('get_dsh_status').then((s) => {
        if (s.running && s.url) {
          window.location.href = s.url
        }
      }).catch(() => { /* ignore transient failures */ })
    }, 500)

    // Track how long the boot has taken so the copy can switch to rotating
    // reassurance messages after ROTATE_AFTER_SECONDS.
    const clock = window.setInterval(() => {
      setElapsed((s) => s + 1)
    }, 1000)

    return () => {
      window.clearInterval(timer)
      window.clearInterval(clock)
      void unlistenStatus.then((f) => { f() })
      void unlistenReady.then((f) => { f() })
      void unlistenError.then((f) => { f() })
    }
  }, [])

  const message =
    elapsed >= ROTATE_AFTER_SECONDS
      ? ROTATING_MESSAGES[
          Math.floor((elapsed - ROTATE_AFTER_SECONDS) / ROTATE_INTERVAL_SECONDS) %
            ROTATING_MESSAGES.length
        ]
      : statusMessage

  return (
    <main className="boot-screen">
      <div className="boot-card">
        <h1>DeepSeek Work</h1>
        {!error && <div className="spinner" />}
        <p className="status-message">{message}</p>
        {error && (
          <>
            <p className="error-message">{error}</p>
            {isTauri && (
              <button
                className="btn"
                onClick={() => {
                  setError(null)
                  setElapsed(0)
                  setStatusMessage('正在重试…')
                  void invoke('restart_dsh_service')
                }}
              >
                重试
              </button>
            )}
          </>
        )}
      </div>
    </main>
  )
}
