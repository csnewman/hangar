import '@xterm/xterm/css/xterm.css'

import { ClipboardAddon } from '@xterm/addon-clipboard'
import { FitAddon } from '@xterm/addon-fit'
import { ImageAddon } from '@xterm/addon-image'
import { ProgressAddon, type IProgressState } from '@xterm/addon-progress'
import { UnicodeGraphemesAddon } from '@xterm/addon-unicode-graphemes'
import { WebLinksAddon } from '@xterm/addon-web-links'
import { WebglAddon } from '@xterm/addon-webgl'
import { Terminal } from '@xterm/xterm'
import { useEffect, useRef, useState } from 'react'

import { Connection, type SessionInfo } from './connection'
import { darkTheme, lightTheme, prefersDark } from './theme'

type State = 'connecting' | 'live' | 'reconnecting' | 'ended'

export interface PaneEvents {
  // attached reports the session once attached: for a new session, its ID.
  attached(session: SessionInfo): void
  // ended reports that the session's shell has exited.
  ended(): void
  title(title: string): void
  progress(state: IProgressState): void
}

// TerminalPane is one terminal session in the browser. It is the only place
// that knows which terminal emulator renders it, so replacing xterm.js is a
// change to this file.
//
// The session belongs to the environment, not the page: refreshing the page,
// or opening it in a second window, attaches to the same shell. A dropped
// connection is re-attached to the same session until it succeeds or the
// session is found to have ended.
export function TerminalPane({
  env,
  session,
  dir,
  events,
}: {
  env: string
  session: string | undefined
  // dir is where a new session's shell starts; absent, the home directory.
  dir?: string
  events: PaneEvents
}) {
  const host = useRef<HTMLDivElement>(null)
  const eventsRef = useRef(events)
  useEffect(() => {
    eventsRef.current = events
  })
  const [state, setState] = useState<State>('connecting')
  const [reason, setReason] = useState('')
  const [restart, setRestart] = useState(0)
  // The session to attach to is taken when the pane mounts: once a new
  // session has its ID the parent records it, and the pane carries on
  // rather than attaching again. Starting another forgets it.
  const initial = useRef(session)
  const initialDir = useRef(dir)

  useEffect(() => {
    const el = host.current
    if (!el) return

    const term = new Terminal({
      allowProposedApi: true,
      cursorBlink: true,
      fontFamily: '"JetBrains Mono", "SF Mono", Menlo, Consolas, "DejaVu Sans Mono", monospace',
      fontSize: 13,
      lineHeight: 1.15,
      scrollback: 10000,
      theme: prefersDark() ? darkTheme : lightTheme,
      macOptionIsMeta: true,
      rescaleOverlappingGlyphs: true,
      vtExtensions: { kittyKeyboard: true },
    })
    const fit = new FitAddon()
    term.loadAddon(fit)
    term.loadAddon(new UnicodeGraphemesAddon())
    term.unicode.activeVersion = '15-graphemes'
    term.loadAddon(
      new ImageAddon({ sixelSupport: true, iipSupport: true, kittySupport: true, enableSizeReports: true }),
    )
    term.loadAddon(
      new WebLinksAddon((e, uri) => {
        e.preventDefault()
        window.open(uri, '_blank', 'noopener,noreferrer')
      }),
    )
    // Programs in the environment may copy to the clipboard (OSC 52) but
    // never read it: a read would hand whatever the user last copied,
    // anywhere, to code running in the environment.
    term.loadAddon(
      new ClipboardAddon(undefined, {
        readText: () => '',
        writeText: (_selection, text) => navigator.clipboard.writeText(text),
      }),
    )
    const progress = new ProgressAddon()
    term.loadAddon(progress)
    progress.onChange((p) => eventsRef.current.progress(p))
    term.onTitleChange((t) => eventsRef.current.title(t))

    term.open(el)
    try {
      const webgl = new WebglAddon()
      // A lost GPU context falls back to the DOM renderer rather than
      // leaving a blank terminal.
      webgl.onContextLoss(() => webgl.dispose())
      term.loadAddon(webgl)
    } catch {
      // No WebGL: the DOM renderer draws instead.
    }
    fit.fit()

    let current = initial.current
    let conn: Connection | null = null
    let retry: ReturnType<typeof setTimeout> | undefined
    let backoff = 500
    let disposed = false

    const connect = () => {
      conn = new Connection(env, current, initialDir.current, term.cols, term.rows, {
        attached: (s) => {
          current = s.id
          initial.current = s.id
          backoff = 500
          // The replay that follows is the session's recent output from
          // wherever it starts, so the screen is cleared for it.
          term.reset()
          setState('live')
          eventsRef.current.attached(s)
        },
        output: (data) => term.write(data),
        exited: () => {
          setState('ended')
          setReason('')
          eventsRef.current.ended()
        },
        lost: (why) => {
          if (disposed) return
          if (why === 'no such session') {
            setState('ended')
            setReason('')
            eventsRef.current.ended()
            return
          }
          setState('reconnecting')
          setReason(why)
          retry = setTimeout(connect, backoff)
          backoff = Math.min(backoff * 2, 5000)
        },
      })
    }
    // On the next tick: a pane mounted and at once unmounted -- as React's
    // development mode does to every component -- must not open a
    // connection, since opening one may start a shell that nobody attaches
    // to again.
    let start: ReturnType<typeof setTimeout> | undefined = setTimeout(() => {
      start = undefined
      connect()
    }, 0)

    term.onData((d) => conn?.input(d))
    term.onBinary((d) => conn?.input(Uint8Array.from(d, (c) => c.charCodeAt(0))))
    term.onResize(({ cols, rows }) => conn?.resize(cols, rows))

    const resize = new ResizeObserver(() => {
      if (el.clientWidth > 0 && el.clientHeight > 0) fit.fit()
    })
    resize.observe(el)
    const scheme = window.matchMedia('(prefers-color-scheme: dark)')
    const onScheme = () => (term.options.theme = scheme.matches ? darkTheme : lightTheme)
    scheme.addEventListener('change', onScheme)
    term.focus()

    return () => {
      disposed = true
      clearTimeout(start)
      clearTimeout(retry)
      scheme.removeEventListener('change', onScheme)
      resize.disconnect()
      conn?.close()
      term.dispose()
    }
  }, [env, restart])

  return (
    <div className="term-pane">
      <div ref={host} className="term-host" />
      {state !== 'live' && (
        <div className={`term-status term-status-${state}`}>
          {state === 'connecting' && 'Connecting…'}
          {state === 'reconnecting' && <>Reconnecting… {reason && <span className="muted">({reason})</span>}</>}
          {state === 'ended' && (
            <>
              The session has ended.
              <button
                type="button"
                className="btn btn-ghost"
                onClick={() => {
                  initial.current = undefined
                  setRestart((n) => n + 1)
                }}
              >
                Start another
              </button>
            </>
          )}
        </div>
      )}
    </div>
  )
}
