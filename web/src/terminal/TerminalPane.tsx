import '@xterm/xterm/css/xterm.css'
// For the menu, which is the Code tab's.
import '../code/code.css'

import { ClipboardAddon } from '@xterm/addon-clipboard'
import { FitAddon } from '@xterm/addon-fit'
import { ImageAddon } from '@xterm/addon-image'
import { ProgressAddon, type IProgressState } from '@xterm/addon-progress'
import { UnicodeGraphemesAddon } from '@xterm/addon-unicode-graphemes'
import { WebLinksAddon } from '@xterm/addon-web-links'
import { WebglAddon } from '@xterm/addon-webgl'
import { Terminal } from '@xterm/xterm'
import { Eye } from 'lucide-react'
import { useEffect, useRef, useState } from 'react'

import { copyText, shortcut, useContextMenu } from '../code/ContextMenu'
import { Connection, type SessionInfo, type TerminalSize } from './connection'
import { onDarkChange } from '../theme'
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
// session is found to have ended. A session that was live here and is gone
// without its shell having exited went with the environment, which
// restarted: a new shell is started in its place.
//
// The terminal has one size, held by whichever window is using it (see
// internal/terminal). The pane draws it at that size, not its own: a
// window larger than it shows it in its corner, a smaller one scrolls, and
// either says so, offering to fit it to this window. The pane tells the
// server what size its window would have; typing into it takes the size.
// isMac is whether ⌘ is the platform's key for copy and paste.
const isMac = /Mac|iPhone|iPad/.test(navigator.platform) || /Mac OS X/.test(navigator.userAgent)

export function TerminalPane({
  env,
  session,
  dir,
  run,
  events,
}: {
  env: string
  session: string | undefined
  // dir is where a new session's shell starts; absent, the home directory.
  dir?: string
  // run is a command typed into a new session once it starts.
  run?: string
  events: PaneEvents
}) {
  const host = useRef<HTMLDivElement>(null)
  const termRef = useRef<Terminal | null>(null)
  const menu = useContextMenu()
  const eventsRef = useRef(events)
  useEffect(() => {
    eventsRef.current = events
  })
  const [state, setState] = useState<State>('connecting')
  const [reason, setReason] = useState('')
  const [restart, setRestart] = useState(0)
  // The terminal's size as the server has it, and this window's own: they
  // differ while another window holds the size.
  const [sizing, setSizing] = useState<{ size: TerminalSize; want: { cols: number; rows: number } | null } | null>(
    null,
  )
  const claimRef = useRef<() => void>(() => {})
  // typedRef marks input as a person's, for a paste from the menu.
  const typedRef = useRef<() => void>(() => {})
  // The session to attach to is taken when the pane mounts: once a new
  // session has its ID the parent records it, and the pane carries on
  // rather than attaching again. Starting another forgets it.
  const initial = useRef(session)
  const initialDir = useRef(dir)
  const toRun = useRef(initial.current ? undefined : run)

  useEffect(() => {
    const el = host.current
    if (!el) return

    // A link opens in a new tab or window: the browser's, or in the desktop
    // app, the default browser's. Only the web's, not file: or anything a
    // program might name.
    const openLink = (uri: string) => {
      if (/^https?:\/\//i.test(uri)) window.open(uri, '_blank', 'noopener,noreferrer')
    }
    const term = (termRef.current = new Terminal({
      // Hyperlinks a program writes (OSC 8), such as a sign-in page. Their
      // text need not be where they go, so the address is asked about
      // first, and shows as the pointer rests on one. The address itself
      // is opened: xterm's own handler opens an empty window and then sends
      // it there, which the desktop app, handing new windows to the default
      // browser, has nothing to do with.
      linkHandler: {
        activate: (e, uri) => {
          e.preventDefault()
          if (window.confirm(`Do you want to open ${uri}?\n\nA program in the environment wrote this link.`)) openLink(uri)
        },
        hover: (_, uri) => {
          el.title = uri
        },
        leave: () => {
          el.title = ''
        },
      },
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
    }))
    // On a Mac, ⌘C, ⌘X and ⌘V are the browser's copy, cut and paste, which
    // xterm answers from its selection and with the clipboard, and ⌘A
    // selects all of the terminal. Otherwise a program that asks for the
    // kitty keyboard protocol would be sent them as keys, with ⌘ as Super,
    // and nothing would be copied or pasted.
    if (isMac) {
      term.attachCustomKeyEventHandler((e) => {
        if (!e.metaKey || e.ctrlKey || e.altKey) return true
        const key = e.key.toLowerCase()
        if (key === 'a') {
          if (e.type === 'keydown') {
            e.preventDefault()
            term.selectAll()
          }
          return false
        }
        return !['c', 'x', 'v'].includes(key)
      })
    }
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
        openLink(uri)
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
    // What this window would have the terminal be, and what it is.
    let want = fit.proposeDimensions() ?? null
    if (want) term.resize(want.cols, want.rows)
    let size: TerminalSize | null = null
    const show = () => {
      if (size && (term.cols !== size.cols || term.rows !== size.rows)) term.resize(size.cols, size.rows)
      setSizing(size ? { size, want } : null)
    }

    let current = initial.current
    let conn: Connection | null = null
    // The replay's bytes still to come, and whether the terminal is still
    // reading them. What the terminal answers while it reads them -- its
    // size, its version, asked by a program that may be long gone -- is not
    // sent: it would be typed into whatever reads the input now.
    let replayLeft = 0
    let replaying = false
    // Whether this pane has been attached, and whether the shell it was
    // attached to went with a restart of the environment.
    let wasLive = false
    let restarted = false
    let retry: ReturnType<typeof setTimeout> | undefined
    let backoff = 500
    let disposed = false
    // When a person last typed or pasted here.
    let typedAt = 0

    const connect = () => {
      conn = new Connection(env, current, initialDir.current, want?.cols ?? term.cols, want?.rows ?? term.rows, {
        attached: (s, history, mine) => {
          replayLeft = history
          replaying = history > 0
          current = s.id
          initial.current = s.id
          backoff = 500
          wasLive = true
          // The replay that follows is the session's recent output from
          // wherever it starts, so the screen is cleared for it.
          term.reset()
          size = { cols: s.cols, rows: s.rows, viewers: s.clients, mine }
          show()
          if (restarted) {
            restarted = false
            term.write('\x1b[2m[The environment restarted; this is a new shell.]\x1b[0m\r\n')
          }
          setState('live')
          eventsRef.current.attached(s)
          if (toRun.current) {
            conn?.input(toRun.current + '\r')
            toRun.current = undefined
          }
        },
        size: (s) => {
          size = s
          show()
        },
        output: (data) => {
          if (replayLeft <= 0) {
            term.write(data)
            return
          }
          replayLeft -= data.length
          if (replayLeft > 0) term.write(data)
          else
            term.write(data, () => {
              replaying = false
            })
        },
        exited: () => {
          setState('ended')
          setReason('')
          eventsRef.current.ended()
        },
        lost: (why) => {
          if (disposed) return
          if (why === 'no such session' && wasLive && current) {
            current = undefined
            initial.current = undefined
            restarted = true
            setState('reconnecting')
            setReason('starting a new shell')
            connect()
            return
          }
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

    // What a person types or pastes is sent from any window, which asks for
    // the size as they do. What the terminal sends of itself -- answers to
    // a program's queries, focus and mouse reports -- is sent only from the
    // window holding the size: every window watching would answer, and the
    // program would read each answer.
    const typed = () => {
      typedAt = performance.now()
      if (size && !size.mine && want) conn?.claim(want.cols, want.rows)
    }
    for (const ev of ['keydown', 'paste', 'compositionend'] as const) el.addEventListener(ev, typed, true)
    typedRef.current = typed
    const sending = () => !replaying && (size === null || size.mine || performance.now() - typedAt < 100)
    term.onData((d) => sending() && conn?.input(d))
    term.onBinary((d) => sending() && conn?.input(Uint8Array.from(d, (c) => c.charCodeAt(0))))
    claimRef.current = () => {
      if (want) conn?.claim(want.cols, want.rows)
      term.focus()
    }

    // The window's size is told to the server, which makes it the
    // terminal's while this window holds it; until the server says so, a
    // holder fits at once.
    const resize = new ResizeObserver(() => {
      if (el.clientWidth === 0 || el.clientHeight === 0) return
      const p = fit.proposeDimensions()
      if (!p || (want && p.cols === want.cols && p.rows === want.rows)) return
      want = { cols: p.cols, rows: p.rows }
      conn?.prefer(p.cols, p.rows)
      if (size?.mine) size = { ...size, cols: p.cols, rows: p.rows }
      show()
    })
    resize.observe(el)
    const offScheme = onDarkChange((dark) => (term.options.theme = dark ? darkTheme : lightTheme))
    term.focus()

    return () => {
      disposed = true
      clearTimeout(start)
      clearTimeout(retry)
      offScheme()
      resize.disconnect()
      conn?.close()
      term.dispose()
    }
  }, [env, restart])

  // The terminal's own menu, in place of the browser's: the clipboard,
  // choosing everything, and clearing what has scrolled away.
  const openMenu = (e: React.MouseEvent) => {
    const term = termRef.current
    if (!term) return
    const selected = term.getSelection()
    menu.open(e, [
      { label: 'Copy', shortcut: shortcut('Mod+C'), disabled: !selected, onSelect: () => copyText(selected) },
      {
        label: 'Paste',
        shortcut: shortcut('Mod+V'),
        onSelect: () => {
          navigator.clipboard?.readText().then(
            (text) => {
              typedRef.current()
              term.paste(text)
              term.focus()
            },
            () => {},
          )
        },
      },
      'separator',
      { label: 'Select All', shortcut: shortcut('Mod+A'), onSelect: () => term.selectAll() },
      {
        label: 'Clear',
        onSelect: () => {
          term.clear()
          term.focus()
        },
      },
    ])
  }

  return (
    <div className="term-pane">
      <div ref={host} className="term-host" onContextMenu={openMenu} />
      {menu.menu}
      {state === 'live' && sizing && <SizeBar sizing={sizing} onFit={() => claimRef.current()} />}
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

// SizeBar says when the terminal is drawn at another window's size, with
// fitting it to this one, and how many windows are watching it.
function SizeBar({
  sizing: { size, want },
  onFit,
}: {
  sizing: { size: TerminalSize; want: { cols: number; rows: number } | null }
  onFit: () => void
}) {
  const other = !size.mine && want !== null && (want.cols !== size.cols || want.rows !== size.rows)
  if (!other && size.viewers < 2) return null
  return (
    <div className="term-sizebar">
      {size.viewers > 1 && (
        <span title={`${size.viewers} windows have this terminal open`}>
          <Eye size={12} /> {size.viewers}
        </span>
      )}
      {other && (
        <>
          <span>
            Sized for another window ({size.cols}×{size.rows})
          </span>
          <button type="button" className="btn btn-ghost btn-sm" onClick={onFit}>
            Fit to this window
          </button>
        </>
      )}
    </div>
  )
}
