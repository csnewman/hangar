import { useCallback, useEffect, useRef, useState } from 'react'

import { formatBytes } from '../components/format'
import type { CodeClient } from './client'
import { copyText, isMac, shortcut, useContextMenu } from './ContextMenu'

const perRow = 16
const rowHeight = 20
// The file is read in chunks as its rows come into view.
const chunk = 64 << 10

const hex2 = (b: number) => b.toString(16).padStart(2, '0')
const shown = (b: number) => (b >= 0x20 && b < 0x7f ? String.fromCharCode(b) : '.')

const toBase64 = (bytes: number[]) => btoa(String.fromCharCode(...bytes))

// HexView shows a file's bytes, sixteen to a row: the offset, each byte in
// hex, and as text where it is printable. A byte chosen in either column
// is marked in both, shift extending the choice; the bar says where it is
// and what it reads as, Go to finds an offset, and the choice copies as
// hex. Large files are read a part at a time as they are scrolled.
//
// A file the user may write is edited in place: hex digits typed in the
// hex column, a byte's high half then its low, or characters in the text
// column, overwrite the bytes chosen. The changes are kept here, marked,
// until saved, and saving writes only them, and only to the version of the
// file they were made to: a file changed on disk meanwhile is reloaded,
// not overwritten. Unchanged, the view reads the file afresh whenever it
// changes.
export function HexView({ client, path }: { client: CodeClient; path: string }) {
  const [size, setSize] = useState<number | null>(null)
  const [writable, setWritable] = useState(false)
  const [error, setError] = useState('')
  // Chunks read, by index, and the file's version they were read from; a
  // tick to redraw on.
  const chunks = useRef(new Map<number, Uint8Array>())
  const asked = useRef(new Set<number>())
  const version = useRef('')
  const [, setTick] = useState(0)
  // Counts the reloads, so the chunks in view are read again after each.
  const [gen, setGen] = useState(0)
  const redraw = () => setTick((t) => t + 1)
  // The changes not saved, by offset, and how to undo each in turn.
  const edits = useRef(new Map<number, number>())
  const history = useRef<{ off: number; prev: number | undefined }[]>([])
  const [dirty, setDirty] = useState(0)
  // The file changed on disk while there were changes here.
  const [stale, setStale] = useState(false)
  const [saving, setSaving] = useState(false)
  // Saving changes the file too; that is no news.
  const writing = useRef(false)
  const [sel, setSel] = useState<{ anchor: number; at: number } | null>(null)
  // Which column was clicked last, and a byte's high half typed in hex,
  // waiting for its low.
  const [column, setColumn] = useState<'hex' | 'text'>('hex')
  const [half, setHalf] = useState<number | null>(null)
  const [goto, setGoto] = useState('')
  const list = useRef<HTMLDivElement>(null)
  const [view, setView] = useState({ top: 0, height: 400 })
  const menu = useContextMenu()

  // reload reads the file from the start, dropping the changes.
  const reload = useCallback(() => {
    chunks.current.clear()
    asked.current.clear()
    // Whichever answers first, the probe or a chunk, says the version.
    version.current = ''
    setGen((g) => g + 1)
    edits.current.clear()
    history.current = []
    setDirty(0)
    setStale(false)
    setHalf(null)
    client.probe(path).then(
      (p) => {
        version.current ||= p.version
        setSize(p.size)
        setWritable(p.writable)
        setError('')
        redraw()
      },
      (e: Error) => setError(e.message),
    )
  }, [client, path])

  const fetchChunk = useCallback(
    (i: number) => {
      if (asked.current.has(i)) return
      asked.current.add(i)
      client.bytes(path, i * chunk, chunk).then(
        (b) => {
          // Read from a later version than the rest: the file changed.
          if (version.current && b.version !== version.current) {
            asked.current.delete(i)
            if (edits.current.size) setStale(true)
            else reload()
            return
          }
          version.current ||= b.version
          const bin = atob(b.data)
          const bytes = new Uint8Array(bin.length)
          for (let k = 0; k < bin.length; k++) bytes[k] = bin.charCodeAt(k)
          chunks.current.set(i, bytes)
          setSize(b.size)
          redraw()
        },
        (e: Error) => {
          asked.current.delete(i)
          setError(e.message)
        },
      )
    },
    [client, path, reload],
  )

  // From the start, and again when the file changes: at once with nothing
  // changed here, else only said.
  useEffect(() => {
    reload()
    const dir = path.includes('/') ? path.slice(0, path.lastIndexOf('/')) : ''
    let timer: ReturnType<typeof setTimeout> | undefined
    const off = client.on('fs.changed', ({ dirs }) => {
      if (!dirs.some((d) => d === client.root + (dir ? '/' + dir : '') || d.endsWith('/' + dir))) return
      clearTimeout(timer)
      timer = setTimeout(() => {
        if (writing.current) return
        client.probe(path).then(
          (p) => {
            if (p.version === version.current) return
            if (edits.current.size) setStale(true)
            else reload()
          },
          () => {},
        )
      }, 250)
    })
    return () => {
      clearTimeout(timer)
      off()
    }
  }, [client, path, reload])

  // Leaving the page with changes not saved asks first.
  useEffect(() => {
    if (!dirty) return
    const ask = (e: BeforeUnloadEvent) => e.preventDefault()
    window.addEventListener('beforeunload', ask)
    return () => window.removeEventListener('beforeunload', ask)
  }, [dirty])

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
  }, [first, last, size, fetchChunk, gen])

  // A byte as on disk, and as it is here, changed or not.
  const original = (off: number): number | undefined => chunks.current.get(Math.floor(off / chunk))?.[off % chunk]
  const byte = (off: number): number | undefined => edits.current.get(off) ?? original(off)

  const lo = sel ? Math.min(sel.anchor, sel.at) : -1
  const hi = sel ? Math.max(sel.anchor, sel.at) : -1
  const choose = (off: number, extend: boolean) => {
    setHalf(null)
    setSel((s) => (extend && s ? { anchor: s.anchor, at: off } : { anchor: off, at: off }))
  }

  const reveal = (off: number) => {
    const el = list.current
    if (!el) return
    const top = Math.floor(off / perRow) * rowHeight
    if (top < el.scrollTop) el.scrollTop = top
    else if (top + rowHeight > el.scrollTop + el.clientHeight) el.scrollTop = top + rowHeight - el.clientHeight
    setView({ top: el.scrollTop, height: el.clientHeight })
  }

  // set changes a byte, remembering what it was for undoing; a byte set
  // back to its own value is no change.
  const set = (off: number, v: number) => {
    const o = original(off)
    if (o === undefined) return
    history.current.push({ off, prev: edits.current.get(off) })
    if (v === o) edits.current.delete(off)
    else edits.current.set(off, v)
    setDirty(edits.current.size)
    redraw()
  }

  const undo = () => {
    const h = history.current.pop()
    if (!h) return
    if (h.prev === undefined) edits.current.delete(h.off)
    else edits.current.set(h.off, h.prev)
    setDirty(edits.current.size)
    setHalf(null)
    setSel({ anchor: h.off, at: h.off })
    reveal(h.off)
    redraw()
  }

  // save writes the changes, a run of neighbouring bytes at a time.
  const save = async () => {
    if (!edits.current.size || saving) return
    const offs = [...edits.current.keys()].sort((a, b) => a - b)
    const runs: { offset: number; data: string }[] = []
    let start = offs[0]
    let bytes: number[] = []
    offs.forEach((o, i) => {
      if (i > 0 && o !== offs[i - 1] + 1) {
        runs.push({ offset: start, data: toBase64(bytes) })
        start = o
        bytes = []
      }
      bytes.push(edits.current.get(o)!)
    })
    runs.push({ offset: start, data: toBase64(bytes) })
    setSaving(true)
    writing.current = true
    try {
      const r = await client.patch(path, version.current, runs)
      // What was written is now the file's own.
      for (const [o, v] of edits.current) {
        const c = chunks.current.get(Math.floor(o / chunk))
        if (c) c[o % chunk] = v
      }
      version.current = r.version
      edits.current.clear()
      history.current = []
      setDirty(0)
      setError('')
      redraw()
    } catch (e) {
      const why = e instanceof Error ? e.message : String(e)
      if (/changed on disk/.test(why)) setStale(true)
      else setError(why)
    } finally {
      setSaving(false)
      writing.current = false
    }
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
    choose(n, false)
    const el = list.current
    if (!el) return
    el.scrollTop = Math.max(0, Math.floor(n / perRow) * rowHeight - el.clientHeight / 2)
    setView({ top: el.scrollTop, height: el.clientHeight })
    el.focus()
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

  const type = (e: React.KeyboardEvent) => {
    if (!writable || !sel || size === null || e.metaKey || e.ctrlKey || e.altKey) return false
    const at = sel.at
    const cur = byte(at)
    if (cur === undefined) return false
    const advance = () => {
      if (at + 1 < size) {
        choose(at + 1, false)
        reveal(at + 1)
      } else setHalf(null)
    }
    if (column === 'hex' && /^[0-9a-f]$/i.test(e.key)) {
      const d = parseInt(e.key, 16)
      if (half === null) {
        set(at, (d << 4) | (cur & 0x0f))
        setHalf(d)
      } else {
        set(at, (half << 4) | d)
        advance()
      }
      return true
    }
    if (column === 'text' && e.key.length === 1 && e.key >= ' ' && e.key <= '~') {
      set(at, e.key.charCodeAt(0))
      advance()
      return true
    }
    return false
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
      const cls = [
        off >= lo && off <= hi && 'code-hex-on',
        edits.current.has(off) && 'code-hex-mod',
        half !== null && off === sel?.at && 'code-hex-half',
      ]
        .filter(Boolean)
        .join(' ')
      const pick = (col: 'hex' | 'text') => (e: React.MouseEvent) => {
        e.preventDefault()
        list.current?.focus()
        setColumn(col)
        choose(off, e.shiftKey)
      }
      hexes.push(
        <span key={k} className={cls || undefined} onMouseDown={pick('hex')} data-gap={k === 8 ? '' : undefined}>
          {b === undefined ? '··' : hex2(b)}
        </span>,
      )
      chars.push(
        <span key={k} className={cls || undefined} onMouseDown={pick('text')}>
          {b === undefined ? ' ' : shown(b)}
        </span>,
      )
    }
    rowsShown.push(
      <div key={r} className="code-hex-row" style={{ top: r * rowHeight }}>
        <span className="code-hex-off">{base.toString(16).padStart(8, '0')}</span>
        <span className={column === 'hex' ? 'code-hex-bytes code-hex-col-on' : 'code-hex-bytes'}>{hexes}</span>
        <span className={column === 'text' ? 'code-hex-chars code-hex-col-on' : 'code-hex-chars'}>{chars}</span>
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
        {dirty > 0 && (
          <>
            <span className="small code-hex-dirty">
              {dirty} byte{dirty === 1 ? '' : 's'} changed
            </span>
            <button type="button" className="btn btn-sm" onClick={reload} title="Drop the changes">
              Revert
            </button>
            <button
              type="button"
              className="btn btn-sm btn-primary"
              disabled={saving}
              onClick={save}
              title={`Write the changes to the file (${shortcut('Mod+S')})`}
            >
              {saving ? 'Saving…' : 'Save'}
            </button>
          </>
        )}
        <span className="muted small">
          {size !== null && `${formatBytes(size)}${writable ? '' : ' · read-only'}`}
        </span>
      </div>
      {stale && (
        <div className="code-diff-about code-hex-stale">
          The file changed on disk since it was read. Reloading drops the {dirty} changed byte
          {dirty === 1 ? '' : 's'} here.{' '}
          <button type="button" className="code-link" onClick={reload}>
            Reload
          </button>
        </div>
      )}
      {error && <p className="code-note code-note-error">{error}</p>}
      <div
        ref={list}
        className="code-hex-list"
        tabIndex={0}
        onScroll={(e) => setView({ top: e.currentTarget.scrollTop, height: e.currentTarget.clientHeight })}
        onContextMenu={(e) =>
          sel &&
          menu.open(e, [
            { label: 'Copy as Hex', shortcut: shortcut('Mod+C'), onSelect: () => copyText(selectedHex()) },
            { label: 'Copy Offset', onSelect: () => copyText(`0x${lo.toString(16)}`) },
            ...(dirty ? ['separator' as const, { label: 'Undo', shortcut: shortcut('Mod+Z'), onSelect: undo }] : []),
          ])
        }
        onKeyDown={(e) => {
          const mod = isMac ? e.metaKey : e.ctrlKey
          if (mod && e.key === 'c' && sel) {
            e.preventDefault()
            copyText(selectedHex())
            return
          }
          if (mod && e.key === 's') {
            e.preventDefault()
            save()
            return
          }
          if (mod && e.key === 'z') {
            e.preventDefault()
            undo()
            return
          }
          if (type(e)) {
            e.preventDefault()
            return
          }
          const by = {
            ArrowRight: 1,
            ArrowLeft: -1,
            ArrowDown: perRow,
            ArrowUp: -perRow,
            PageDown: perRow * 20,
            PageUp: -perRow * 20,
          }[e.key]
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
