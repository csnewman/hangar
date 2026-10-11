import { Grid2x2, Maximize, Minus, Plus, Scan } from 'lucide-react'
import { useEffect, useLayoutEffect, useRef, useState } from 'react'

import { formatBytes } from '../components/format'
import type { CodeClient } from './client'
import { imageType } from './views'

// The largest image read: the service's limit for bytes at once.
const maxImage = 32 << 20

// toURL makes base64 bytes an object URL of a type, for an <img>. An SVG
// shown this way runs no scripts.
export function toURL(data: string, type: string): string {
  const bin = atob(data)
  const bytes = new Uint8Array(bin.length)
  for (let i = 0; i < bin.length; i++) bytes[i] = bin.charCodeAt(i)
  return URL.createObjectURL(new Blob([bytes], { type }))
}

// ImageView shows an image file as IntelliJ's image viewer does: fitted
// to the view, or zoomed (the buttons, or the wheel with Ctrl or ⌘) and
// moved by dragging, over a chessboard that shows its transparency, with
// its size and type. It shows the file afresh whenever it changes.
export function ImageView({ client, path }: { client: CodeClient; path: string }) {
  const type = imageType(path) ?? 'application/octet-stream'
  const [url, setUrl] = useState<string | null>(null)
  const [size, setSize] = useState(0)
  const [dims, setDims] = useState<{ w: number; h: number } | null>(null)
  const [error, setError] = useState('')
  const [zoom, setZoom] = useState<'fit' | number>('fit')
  const [chess, setChess] = useState(true)
  const box = useRef<HTMLDivElement>(null)

  useEffect(() => {
    let live = true
    let current: string | null = null
    const dir = path.includes('/') ? path.slice(0, path.lastIndexOf('/')) : ''
    const load = async () => {
      try {
        const b = await client.bytes(path, 0, maxImage)
        if (!live) return
        if (b.size > maxImage) {
          setError(`${path.split('/').pop()} is ${formatBytes(b.size)}, too large to show here.`)
          return
        }
        const next = toURL(b.data, type)
        if (current) URL.revokeObjectURL(current)
        current = next
        setUrl(next)
        setSize(b.size)
        setError('')
      } catch (e) {
        if (live) setError(e instanceof Error ? e.message : String(e))
      }
    }
    load()
    let timer: ReturnType<typeof setTimeout> | undefined
    const off = client.on('fs.changed', ({ dirs }) => {
      if (dirs.some((d) => d === client.root + (dir ? '/' + dir : '') || d.endsWith('/' + dir))) {
        clearTimeout(timer)
        timer = setTimeout(load, 250)
      }
    })
    return () => {
      live = false
      clearTimeout(timer)
      off()
      if (current) URL.revokeObjectURL(current)
    }
  }, [client, path, type])

  // The zoom the view is at, fitted or not, for stepping from.
  const scale = () => {
    if (zoom !== 'fit') return zoom
    const el = box.current
    if (!el || !dims) return 1
    return Math.min(1, (el.clientWidth - 32) / dims.w, (el.clientHeight - 32) / dims.h)
  }
  // Zooming keeps one point of the image where it was: the one under the
  // pointer for the wheel, else the one in the middle of the view.
  const img = useRef<HTMLImageElement>(null)
  const focus = useRef<{ fx: number; fy: number; x: number; y: number } | null>(null)
  const step = (by: number, at?: { x: number; y: number }) => {
    const el = box.current
    const im = img.current
    if (el && im) {
      const b = el.getBoundingClientRect()
      const r = im.getBoundingClientRect()
      const x = at?.x ?? b.left + b.width / 2
      const y = at?.y ?? b.top + b.height / 2
      focus.current = { fx: (x - r.left) / r.width, fy: (y - r.top) / r.height, x, y }
    }
    setZoom(Math.max(0.05, Math.min(32, +(scale() * by).toFixed(3))))
  }
  useLayoutEffect(() => {
    const f = focus.current
    focus.current = null
    const el = box.current
    const im = img.current
    if (!f || !el || !im) return
    const r = im.getBoundingClientRect()
    el.scrollLeft += r.left + f.fx * r.width - f.x
    el.scrollTop += r.top + f.fy * r.height - f.y
  }, [zoom])

  // An image larger than the view is moved by dragging it, as with
  // IntelliJ's hand.
  const [pannable, setPannable] = useState(false)
  const [panning, setPanning] = useState(false)
  useLayoutEffect(() => {
    const el = box.current
    if (!el) return
    const measure = () =>
      setPannable(el.scrollWidth > el.clientWidth + 1 || el.scrollHeight > el.clientHeight + 1)
    measure()
    const ro = new ResizeObserver(measure)
    ro.observe(el)
    return () => ro.disconnect()
  }, [zoom, dims])
  const pan = (e: React.PointerEvent) => {
    const el = box.current
    if (e.button !== 0 || !el || !pannable) return
    e.preventDefault()
    const { clientX: x0, clientY: y0 } = e
    const { scrollLeft: sx, scrollTop: sy } = el
    setPanning(true)
    const move = (ev: PointerEvent) => {
      el.scrollLeft = sx - (ev.clientX - x0)
      el.scrollTop = sy - (ev.clientY - y0)
    }
    const up = () => {
      setPanning(false)
      document.removeEventListener('pointermove', move, true)
      document.removeEventListener('pointerup', up, true)
    }
    document.addEventListener('pointermove', move, true)
    document.addEventListener('pointerup', up, true)
  }
  // The wheel with Ctrl or ⌘ zooms the image, not the page: a listener
  // that may prevent the page's zoom, which React's may not.
  const stepRef = useRef(step)
  stepRef.current = step
  useEffect(() => {
    const el = box.current
    if (!el) return
    const wheel = (e: WheelEvent) => {
      if (!e.ctrlKey && !e.metaKey) return
      e.preventDefault()
      stepRef.current(e.deltaY < 0 ? 1.1 : 1 / 1.1, { x: e.clientX, y: e.clientY })
    }
    el.addEventListener('wheel', wheel, { passive: false })
    return () => el.removeEventListener('wheel', wheel)
  }, [])

  const pct = Math.round(scale() * 100)
  return (
    <div className="code-image">
      <div className="code-image-bar">
        <button type="button" className="code-tool-btn" title="Zoom out" onClick={() => step(1 / 1.25)}>
          <Minus size={13} />
        </button>
        <span className="code-image-zoom">{pct}%</span>
        <button type="button" className="code-tool-btn" title="Zoom in" onClick={() => step(1.25)}>
          <Plus size={13} />
        </button>
        <button type="button" className="code-tool-btn" title="Actual size" onClick={() => setZoom(1)}>
          <Scan size={13} />
        </button>
        <button
          type="button"
          className={zoom === 'fit' ? 'code-tool-btn code-tool-btn-on' : 'code-tool-btn'}
          title="Fit to the view"
          onClick={() => setZoom('fit')}
        >
          <Maximize size={13} />
        </button>
        <button
          type="button"
          className={chess ? 'code-tool-btn code-tool-btn-on' : 'code-tool-btn'}
          title="Show transparency as a chessboard"
          onClick={() => setChess((c) => !c)}
        >
          <Grid2x2 size={13} />
        </button>
        <span className="code-tool-spacer" />
        <span className="muted small">
          {dims && `${dims.w}×${dims.h} · `}
          {type.replace('image/', '').replace('+xml', '').toUpperCase()} · {formatBytes(size)}
        </span>
      </div>
      <div
        ref={box}
        className={[
          'code-image-body',
          zoom === 'fit' && 'code-image-fit',
          pannable && 'code-image-pan',
          panning && 'code-image-panning',
        ]
          .filter(Boolean)
          .join(' ')}
        onPointerDown={pan}
      >
        {error && <p className="code-note code-note-error">{error}</p>}
        {url && !error && (
          <img
            ref={img}
            draggable={false}
            src={url}
            alt={path}
            className={chess ? 'code-image-chess' : undefined}
            style={zoom === 'fit' || !dims ? undefined : { width: dims.w * zoom, height: dims.h * zoom }}
            onLoad={(e) => setDims({ w: e.currentTarget.naturalWidth, h: e.currentTarget.naturalHeight })}
          />
        )}
      </div>
    </div>
  )
}
