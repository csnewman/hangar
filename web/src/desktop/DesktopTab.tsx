import RFB from '@novnc/novnc'
import { AppWindow, Clipboard, Eye, Keyboard, Maximize, MousePointer2, Scaling } from 'lucide-react'
import { useEffect, useRef, useState } from 'react'

import { api } from '../api'
import { Heartbeat, onReturn } from '../heartbeat'
import { useEnv } from '../pages/Environment'
import { usePhone } from '../phone'

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
  const phone = usePhone()
  // Whether the desktop follows this window's size. Everyone watching shares
  // one desktop, so the last window to resize it wins; a phone leaves it as
  // it is unless asked, and shows it scaled down.
  const [fit, setFit] = useState(!phone)
  const keys = useRef<HTMLTextAreaElement>(null)
  const [restart, setRestart] = useState(0)

  useEffect(() => {
    const el = host.current
    if (!el) return
    const scheme = location.protocol === 'https:' ? 'wss' : 'ws'
    const url = `${scheme}://${location.host}/api/frontend/environments/${env}/desktop`
    let rfb: RFB | null = null
    let beat: Heartbeat | undefined
    let retry: ReturnType<typeof setTimeout> | undefined
    let backoff = 500
    let disposed = false

    const connect = () => {
      // The socket is made here, not by noVNC, so that it can carry the
      // heartbeat's text messages beside noVNC's own.
      const ws = new WebSocket(url, ['binary'])
      const current = new RFB(el, ws, { shared: true })
      rfb = current
      rfbRef.current = rfb
      const vnc = ws.onmessage
      const live = new Heartbeat(ws, (why) => {
        setReason(why)
        current.disconnect()
      })
      beat = live
      ws.onmessage = (e) => {
        if (typeof e.data === 'string') live.answered(e.data)
        else vnc?.call(ws, e)
      }
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
        live.stop()
        rfbRef.current = null
        if (disposed) return
        setState('reconnecting')
        if (!e.detail.clean) setReason((r) => r || 'the connection dropped')
        retry = setTimeout(() => {
          retry = undefined
          connect()
        }, backoff)
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
    // Coming back to the page, or the network coming back, a dropped
    // connection is made again at once, not when its backoff runs out.
    const offReturn = onReturn(() => {
      if (retry === undefined) return
      clearTimeout(retry)
      retry = undefined
      backoff = 500
      connect()
    })

    return () => {
      disposed = true
      clearTimeout(start)
      clearTimeout(retry)
      offReturn()
      beat?.stop()
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

  // A phone's keyboard comes up for a text field, not for the desktop's
  // picture: what is typed into a hidden one is sent on as keys.
  const typed = (e: React.FormEvent<HTMLTextAreaElement>) => {
    const rfb = rfbRef.current
    const ev = e.nativeEvent as InputEvent
    if (rfb) {
      if (ev.inputType === 'deleteContentBackward') rfb.sendKey(0xff08, 'Backspace')
      else if (ev.inputType === 'insertLineBreak') rfb.sendKey(0xff0d, 'Enter')
      else for (const c of ev.data ?? '') rfb.sendKey(keysym(c), null)
    }
    e.currentTarget.value = ''
  }

  if (phone) {
    return (
      <div className="desktop-page desktop-phone" ref={page}>
        {state !== 'live' && (
          <div className="desktop-phone-state">
            <span className={`desktop-dot desktop-dot-${state}`} />
            {state === 'connecting' && 'Connecting…'}
            {state === 'reconnecting' && <>Reconnecting…{reason && ` (${reason})`}</>}
            {state === 'reconnecting' && (
              <button type="button" className="btn btn-ghost" onClick={() => setRestart((n) => n + 1)}>
                Retry now
              </button>
            )}
          </div>
        )}
        <div ref={host} className="desktop-host" />
        <textarea
          ref={keys}
          className="desktop-keys"
          aria-label="Type to the desktop"
          autoCapitalize="off"
          autoCorrect="off"
          spellCheck={false}
          onInput={typed}
        />
        <div className="desktop-tools" role="toolbar" aria-label="Desktop">
          <div className="phone-choice" role="radiogroup" aria-label="Input">
            {([false, true] as const).map((v) => (
              <button
                key={String(v)}
                type="button"
                role="radio"
                aria-checked={viewOnly === v}
                className={viewOnly === v ? 'phone-choice-on' : undefined}
                onClick={() => setViewOnly(v)}
              >
                {v ? 'View' : 'Touch'}
              </button>
            ))}
          </div>
          <span className="desktop-spacer" />
          <button
            type="button"
            className="phone-icon-btn"
            aria-label="Keyboard"
            disabled={viewOnly}
            onClick={() => keys.current?.focus()}
          >
            <Keyboard size={20} />
          </button>
          <button type="button" className="phone-icon-btn" aria-label="Paste" disabled={viewOnly} onClick={paste}>
            <Clipboard size={20} />
          </button>
          <button
            type="button"
            className={fit ? 'phone-icon-btn phone-icon-btn-on' : 'phone-icon-btn'}
            aria-label={fit ? 'Stop sizing the desktop to this phone' : 'Size the desktop to this phone'}
            aria-pressed={fit}
            onClick={() => setFit((f) => !f)}
          >
            <Scaling size={20} />
          </button>
          <button
            type="button"
            className="phone-icon-btn"
            aria-label="Full screen"
            onClick={() => page.current?.requestFullscreen?.()}
          >
            <Maximize size={20} />
          </button>
        </div>
      </div>
    )
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
        <div className="segmented" role="radiogroup" aria-label="Input">
          {([false, true] as const).map((v) => (
            <label
              key={String(v)}
              className={viewOnly === v ? 'seg seg-on' : 'seg'}
              title={v ? 'Watch the desktop without sending it input' : 'Send the desktop your mouse and keyboard'}
            >
              <input type="radio" name="desktop-input" checked={viewOnly === v} onChange={() => setViewOnly(v)} />
              {v ? <Eye size={14} /> : <MousePointer2 size={14} />}
              {v ? 'View' : 'Control'}
            </label>
          ))}
        </div>
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

// keysym is the X keysym that types a character: Latin-1 as itself, the
// rest of Unicode offset into the keysyms kept for it.
function keysym(c: string): number {
  const code = c.codePointAt(0) ?? 0
  return code < 0x100 ? code : 0x01000000 + code
}
