import { useCallback, useEffect, useRef, useState } from 'react'

import { formatBytes } from '../components/format'
import type { CodeClient } from './client'
import { copyText, useContextMenu } from './ContextMenu'

const perRow = 16
const rowHeight = 20
// The file is read in chunks as its rows come into view.
const chunk = 64 << 10

const hex2 = (b: number) => b.toString(16).padStart(2, '0')
const shown = (b: number) => (b >= 0x20 && b < 0x7f ? String.fromCharCode(b) : '.')

// HexView shows a file's bytes, sixteen to a row: the offset, each byte in
// hex, and as text where it is printable. A byte chosen in either column
// is marked in both, shift extending the choice; the bar says where it is
// and what it reads as, Go to finds an offset, and the choice copies as
// hex. Large files are read a part at a time as they are scrolled, and
// read afresh when the file changes.
export function HexView({ client, path }: { client: CodeClient; path: string }) {
  const [size, setSize] = useState<number | null>(null)
  const [error, setError] = useState('')
  // Chunks read, by index; a version to redraw on.
  const chunks = useRef(new Map<number, Uint8Array>())
  const asked = useRef(new Set<number>())
  const [, setVersion] = useState(0)
  const [sel, setSel] = useState<{ anchor: number; at: number } | null>(null)
  const [goto, setGoto] = useState('')
  const list = useRef<HTMLDivElement>(null)
  const [view, setView] = useState({ top: 0, height: 400 })
  const menu = useContextMenu()

  const fetchChunk = useCallback(
    (i: number) => {
      if (asked.current.has(i)) return
      asked.current.add(i)
      client.bytes(path, i * chunk, chunk).then(
        (b) => {
          const bin = atob(b.data)
          const bytes = new Uint8Array(bin.length)
          for (let k = 0; k < bin.length; k++) bytes[k] = bin.charCodeAt(k)
          chunks.current.set(i, bytes)
          setSize(b.size)
          setVersion((v) => v + 1)
        },
        (e: Error) => {
          asked.current.delete(i)
          setError(e.message)
        },
      )
    },
    [client, path],
  )

  // From the start, and again when the file changes.
  useEffect(() => {
    const reset = () => {
      chunks.current.clear()
      asked.current.clear()
      client.probe(path).then(
        (p) => {
          setSize(p.size)
          setError('')
          setVersion((v) => v + 1)
        },
        (e: Error) => setError(e.message),
      )
    }
    reset()
    const dir = path.includes('/') ? path.slice(0, path.lastIndexOf('/')) : ''
    let timer: ReturnType<typeof setTimeout> | undefined
    const off = client.on('fs.changed', ({ dirs }) => {
      if (dirs.some((d) => d === client.root + (dir ? '/' + dir : '') || d.endsWith('/' + dir))) {
        clearTimeout(timer)
        timer = setTimeout(reset, 250)
      }
    })
    return () => {
      clearTimeout(timer)
      off()
    }
  }, [client, path])

  useEffect(() => {
    const el = list.current
    if (!el) return
    const ro = new ResizeObserver(() => setView({ top: el.scrollTop, height: el.clientHeight }))
    ro.observe(el)
    return () => ro.disconnect()
  }, [])

  const rows = size === null ? 0 : Math.ceil(size / perRow)
  const first = Math.max(0, Math.floor(view.top / rowHeight) - 5)
  const last = Math.min(rows, Math.ceil((view.top + view.height) / rowHeight) + 5)

  // The chunks the rows in view need.
  useEffect(() => {
    if (size === null || last <= first) return
    const from = Math.floor((first * perRow) / chunk)
    const to = Math.floor((last * perRow - 1) / chunk)
    for (let i = from; i <= to; i++) fetchChunk(i)
  }, [first, last, size, fetchChunk])

  const byte = (off: number): number | undefined => chunks.current.get(Math.floor(off / chunk))?.[off % chunk]

  const lo = sel ? Math.min(sel.anchor, sel.at) : -1
  const hi = sel ? Math.max(sel.anchor, sel.at) : -1
  const choose = (off: number, extend: boolean) =>
    setSel((s) => (extend && s ? { anchor: s.anchor, at: off } : { anchor: off, at: off }))

  const reveal = (off: number) => {
    const el = list.current
    if (!el) return
    const top = Math.floor(off / perRow) * rowHeight
    if (top < el.scrollTop) el.scrollTop = top
    else if (top + rowHeight > el.scrollTop + el.clientHeight) el.scrollTop = top + rowHeight - el.clientHeight
    setView({ top: el.scrollTop, height: el.clientHeight })
  }

  const selectedHex = () => {
    const out: string[] = []
    for (let o = lo; o <= hi; o++) {
      const b = byte(o)
      if (b === undefined) break
      out.push(hex2(b))
    }
    return out.join(' ')
  }

  const jump = () => {
    const t = goto.trim().toLowerCase()
    const n = t.startsWith('0x') ? parseInt(t.slice(2), 16) : /[a-f]/.test(t) ? parseInt(t, 16) : parseInt(t, 10)
    if (!Number.isFinite(n) || size === null || n < 0 || n >= size) return
    setSel({ anchor: n, at: n })
    const el = list.current
    if (!el) return
    el.scrollTop = Math.max(0, Math.floor(n / perRow) * rowHeight - el.clientHeight / 2)
    setView({ top: el.scrollTop, height: el.clientHeight })
  }

  // What the chosen bytes read as, little-endian, as far as there are
  // bytes for.
  const reads = () => {
    if (!sel) return ''
    const at = sel.at
    const bs: number[] = []
    for (let k = 0; k < 4; k++) {
      const b = byte(at + k)
      if (b === undefined) break
      bs.push(b)
    }
    if (!bs.length) return ''
    const parts = [`u8 ${bs[0]}`, `i8 ${(bs[0] << 24) >> 24}`]
    if (bs.length >= 2) parts.push(`u16 ${bs[0] | (bs[1] << 8)}`)
    if (bs.length >= 4) parts.push(`u32 ${(bs[0] | (bs[1] << 8) | (bs[2] << 16) | (bs[3] << 24)) >>> 0}`)
    return parts.join(' · ')
  }

  const rowsShown = []
  for (let r = first; r < last; r++) {
    const base = r * perRow
    const hexes = []
    const chars = []
    for (let k = 0; k < perRow; k++) {
      const off = base + k
      if (size !== null && off >= size) break
      const b = byte(off)
      const on = off >= lo && off <= hi
      const props = {
        className: on ? 'code-hex-on' : undefined,
        onMouseDown: (e: React.MouseEvent) => {
          e.preventDefault()
          list.current?.focus()
          choose(off, e.shiftKey)
        },
      }
      hexes.push(
        <span key={k} {...props} data-gap={k === 8 ? '' : undefined}>
          {b === undefined ? '··' : hex2(b)}
        </span>,
      )
      chars.push(
        <span key={k} {...props}>
          {b === undefined ? ' ' : shown(b)}
        </span>,
      )
    }
    rowsShown.push(
      <div key={r} className="code-hex-row" style={{ top: r * rowHeight }}>
        <span className="code-hex-off">{base.toString(16).padStart(8, '0')}</span>
        <span className="code-hex-bytes">{hexes}</span>
        <span className="code-hex-chars">{chars}</span>
      </div>,
    )
  }

  return (
    <div className="code-hex">
      <div className="code-image-bar">
        <form
          className="code-hex-goto"
          onSubmit={(e) => {
            e.preventDefault()
            jump()
          }}
        >
          <input
            placeholder="Go to offset (0x1f or 31)"
            spellCheck={false}
            value={goto}
            onChange={(e) => setGoto(e.target.value)}
          />
        </form>
        <span className="muted small">
          {sel &&
            (lo === hi
              ? `Offset 0x${lo.toString(16)} (${lo}) · ${reads()}`
              : `0x${lo.toString(16)}–0x${hi.toString(16)} · ${hi - lo + 1} bytes`)}
        </span>
        <span className="code-tool-spacer" />
        <span className="muted small">
          {size !== null && `${formatBytes(size)} · read-only`}
        </span>
      </div>
      {error && <p className="code-note code-note-error">{error}</p>}
      <div
        ref={list}
        className="code-hex-list"
        tabIndex={0}
        onScroll={(e) => setView({ top: e.currentTarget.scrollTop, height: e.currentTarget.clientHeight })}
        onContextMenu={(e) =>
          sel &&
          menu.open(e, [
            { label: 'Copy as Hex', onSelect: () => copyText(selectedHex()) },
            { label: 'Copy Offset', onSelect: () => copyText(`0x${lo.toString(16)}`) },
          ])
        }
        onKeyDown={(e) => {
          if ((e.metaKey || e.ctrlKey) && e.key === 'c' && sel) {
            e.preventDefault()
            copyText(selectedHex())
            return
          }
          const by = { ArrowRight: 1, ArrowLeft: -1, ArrowDown: perRow, ArrowUp: -perRow, PageDown: perRow * 20, PageUp: -perRow * 20 }[e.key]
          if (by === undefined || size === null || !size) return
          e.preventDefault()
          const next = Math.max(0, Math.min(size - 1, (sel?.at ?? -by) + by))
          choose(next, e.shiftKey)
          reveal(next)
        }}
      >
        <div style={{ height: rows * rowHeight, position: 'relative' }}>{rowsShown}</div>
        {size === 0 && <p className="code-note">The file is empty.</p>}
      </div>
      {menu.menu}
    </div>
  )
}
