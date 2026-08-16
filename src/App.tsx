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

export default function App() {
  const [message, setMessage] = useState('正在初始化…')
  const [error, setError] = useState<string | null>(null)

  useEffect(() => {
    if (!isTauri) {
      setError('本页面只能在 DeepSeek Work 桌面应用窗口中打开')
      return
    }

    const unlistenStatus = listen<string>('boot-status', (event) => {
      setMessage(event.payload)
    })

    // Navigate from the frontend itself: the Rust-side window.navigate can
    // race with a slow page load in dev and get lost.
    const unlistenReady = listen<string>('dsh-ready', (event) => {
      setMessage('正在加载 DeepSeek Work…')
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

    return () => {
      window.clearInterval(timer)
      void unlistenStatus.then((f) => { f() })
      void unlistenReady.then((f) => { f() })
      void unlistenError.then((f) => { f() })
    }
  }, [])

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
                  setMessage('正在重试…')
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
