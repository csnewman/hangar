import RFB from '@novnc/novnc'
import { AppWindow, Clipboard, Eye, Maximize, MousePointer2, Scaling } from 'lucide-react'
import { useEffect, useRef, useState } from 'react'

import { api } from '../api'
import { useEnv } from '../pages/Environment'

type State = 'connecting' | 'live' | 'reconnecting' | 'ended'

// DesktopTab is the environment's desktop, over VNC: the image's own
// compositor, served by a VNC server the environment's agent starts when the
// tab first connects. Any number of windows may watch it at once; they share
// one desktop, pointer and all, as viewers of one screen do.
export function DesktopTab() {
  const env = useEnv()
  const running = env.phase === 'running'

  if (env.spec.display === 'none') {
    return (
      <Placeholder title="No desktop">
        {env.name} is headless: its template runs it without a graphical stack.
      </Placeholder>
    )
  }
  if (!running) {
    return <Placeholder title="Desktop">Start {env.name} to see its desktop.</Placeholder>
  }
  return <DesktopView key={env.id} env={env.id} name={env.name} />
}

function Placeholder({ title, children }: { title: string; children: React.ReactNode }) {
  return (
    <div className="console">
      <div className="console-empty">
        <AppWindow size={36} strokeWidth={1.4} />
        <h2>{title}</h2>
        <p>{children}</p>
      </div>
    </div>
  )
}

function DesktopView({ env, name }: { env: string; name: string }) {
  const host = useRef<HTMLDivElement>(null)
  const page = useRef<HTMLDivElement>(null)
  const rfbRef = useRef<RFB | null>(null)
  const [state, setState] = useState<State>('connecting')
  const [reason, setReason] = useState('')
  const [viewOnly, setViewOnly] = useState(false)
  // Whether the desktop follows this window's size. Everyone watching shares
  // one desktop, so the last window to resize it wins.
  const [fit, setFit] = useState(true)
  const [restart, setRestart] = useState(0)

  useEffect(() => {
    const el = host.current
    if (!el) return
    const scheme = location.protocol === 'https:' ? 'wss' : 'ws'
    const url = `${scheme}://${location.host}/api/frontend/environments/${env}/desktop`
    let rfb: RFB | null = null
    let retry: ReturnType<typeof setTimeout> | undefined
    let backoff = 500
    let disposed = false

    const connect = () => {
      rfb = new RFB(el, url, { shared: true, wsProtocols: ['binary'] })
      rfbRef.current = rfb
      // The picture is scaled to the tab. The desktop's own size is set
      // through Hangar, not by the viewer: see resizeDesktop.
      rfb.resizeSession = false
      rfb.scaleViewport = true
      rfb.focusOnClick = true
      rfb.background = 'var(--surface-2)'
      rfb.qualityLevel = 7
      rfb.compressionLevel = 2
      rfb.addEventListener('connect', () => {
        backoff = 500
        setState('live')
        setReason('')
      })
      rfb.addEventListener('disconnect', ((e: CustomEvent<{ clean: boolean }>) => {
        rfbRef.current = null
        if (disposed) return
        setState('reconnecting')
        setReason(e.detail.clean ? '' : 'the connection dropped')
        retry = setTimeout(connect, backoff)
        backoff = Math.min(backoff * 2, 10000)
      }) as EventListener)
      // Whatever is copied on the desktop reaches this browser's clipboard;
      // the browser may refuse while the page is not focused.
      rfb.addEventListener('clipboard', ((e: CustomEvent<{ text: string }>) => {
        navigator.clipboard?.writeText(e.detail.text).catch(() => {})
      }) as EventListener)
    }
    // On the next tick, so React's development double mount opens one
    // connection, not two.
    let start: ReturnType<typeof setTimeout> | undefined = setTimeout(() => {
      start = undefined
      connect()
    }, 0)

    return () => {
      disposed = true
      clearTimeout(start)
      clearTimeout(retry)
      rfb?.disconnect()
      rfbRef.current = null
    }
  }, [env, restart])

  useEffect(() => {
    if (rfbRef.current) rfbRef.current.viewOnly = viewOnly
  }, [viewOnly, state])

  // The desktop is sized to the tab, in CSS pixels, once connected and
  // whenever the tab changes size.
  useEffect(() => {
    const el = host.current
    if (!el || !fit || state !== 'live') return
    let timer: ReturnType<typeof setTimeout> | undefined
    let last = ''
    const resize = () => {
      clearTimeout(timer)
      timer = setTimeout(() => {
        const w = Math.round(el.clientWidth)
        const h = Math.round(el.clientHeight)
        const key = `${w}x${h}`
        if (w < 100 || h < 100 || key === last) return
        last = key
        api.resizeDesktop(env, w, h).catch(() => {})
      }, 300)
    }
    const obs = new ResizeObserver(resize)
    obs.observe(el)
    resize()
    return () => {
      clearTimeout(timer)
      obs.disconnect()
    }
  }, [env, fit, state])

  const paste = async () => {
    try {
      const text = await navigator.clipboard.readText()
      rfbRef.current?.clipboardPasteFrom(text)
      rfbRef.current?.focus()
    } catch {
      // Reading the clipboard needs the user's permission.
    }
  }

  return (
    <div className="desktop-page" ref={page}>
      <div className="desktop-bar">
        <span className={`desktop-dot desktop-dot-${state}`} />
        <span className="desktop-title">{name}</span>
        <span className="muted small">
          {state === 'connecting' && 'Connecting…'}
          {state === 'live' && 'Connected'}
          {state === 'reconnecting' && <>Reconnecting…{reason && ` (${reason})`}</>}
        </span>
        <span className="desktop-spacer" />
        <button
          type="button"
          className={viewOnly ? 'btn btn-ghost btn-on' : 'btn btn-ghost'}
          onClick={() => setViewOnly((v) => !v)}
          title={viewOnly ? 'Viewing only: click to control the desktop' : 'Watch without sending input'}
        >
          {viewOnly ? <Eye size={14} /> : <MousePointer2 size={14} />}
          {viewOnly ? 'View only' : 'Control'}
        </button>
        <button
          type="button"
          className={fit ? 'btn btn-ghost btn-on' : 'btn btn-ghost'}
          onClick={() => setFit((f) => !f)}
          title={fit ? 'The desktop follows this window’s size: click to keep its size' : 'Size the desktop to this window'}
        >
          <Scaling size={14} />
          Fit
        </button>
        <button type="button" className="btn btn-ghost" onClick={paste} title="Paste this browser's clipboard into the desktop">
          <Clipboard size={14} />
          Paste
        </button>
        <button
          type="button"
          className="btn btn-ghost"
          onClick={() => page.current?.requestFullscreen?.()}
          title="Full screen"
          aria-label="Full screen"
        >
          <Maximize size={14} />
        </button>
        {state === 'reconnecting' && (
          <button type="button" className="btn btn-ghost" onClick={() => setRestart((n) => n + 1)}>
            Retry now
          </button>
        )}
      </div>
      <div ref={host} className="desktop-host" />
    </div>
  )
}
