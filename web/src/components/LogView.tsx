import { useEffect, useRef, useState } from 'react'

import type { LogPart } from '../api'

// How often a log is read on from where it was, while the page is shown.
const pollMs = 2000
// The most kept on the page; older text is dropped from the top.
const maxChars = 1_000_000

// Colour and cursor codes, which a console and systemd write, and carriage
// returns that redraw a line in a terminal.
// eslint-disable-next-line no-control-regex
const controls = /\x1b\[[0-9;?]*[ -/]*[@-~]|\x1b[()][0-9A-Za-z]|\r(?!\n)/g

// LogView shows a log that grows: its end first, then what follows as it is
// written. load reads from an offset, or the end without one. Another log
// is another LogView: give each its own key.
export function LogView({ load }: { load: (offset?: number) => Promise<LogPart> }) {
  const [text, setText] = useState('')
  const [earlier, setEarlier] = useState(false)
  const [missing, setMissing] = useState(false)
  const [error, setError] = useState<string | null>(null)
  const pre = useRef<HTMLPreElement>(null)
  const follow = useRef(true)

  useEffect(() => {
    let stopped = false
    let next: number | undefined
    let timer: ReturnType<typeof setTimeout> | undefined

    const read = async () => {
      if (stopped) return
      // The first read always; after that only while the page is seen.
      if (next === undefined || document.visibilityState === 'visible') {
        try {
          const part = await load(next)
          if (stopped) return
          if (next === undefined) setEarlier(part.start > 0)
          else if (part.start !== next) {
            // The log started over, or its oldest went, since the last
            // read: what was shown no longer runs on into this.
            setText('')
            setEarlier(part.start > 0)
          }
          next = part.next
          setMissing(!!part.missing)
          setError(null)
          if (part.text) {
            const clean = part.text.replace(controls, '')
            setText((t) => {
              const all = t + clean
              return all.length > maxChars ? all.slice(all.length - maxChars) : all
            })
          }
        } catch (e) {
          if (!stopped) setError(e instanceof Error ? e.message : String(e))
        }
      }
      timer = setTimeout(read, pollMs)
    }
    read()
    return () => {
      stopped = true
      clearTimeout(timer)
    }
  }, [load])

  // Kept at the bottom while it is there; scrolled up, it stays put.
  useEffect(() => {
    const el = pre.current
    if (el && follow.current) el.scrollTop = el.scrollHeight
  }, [text])

  const onScroll = () => {
    const el = pre.current
    if (el) follow.current = el.scrollTop + el.clientHeight >= el.scrollHeight - 16
  }

  return (
    <div className="log-view">
      {error && <div className="alert">{error}</div>}
      {missing && !text && <p className="muted small">There is no such log here yet.</p>}
      {earlier && <p className="muted small">Earlier lines are no longer kept.</p>}
      {(text || !missing) && (
        <pre ref={pre} className="log-text mono" onScroll={onScroll}>
          {text || (!error ? 'Nothing logged yet.' : '')}
        </pre>
      )}
    </div>
  )
}
