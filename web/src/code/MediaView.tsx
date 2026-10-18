import { useEffect, useState } from 'react'

import { formatBytes } from '../components/format'
import type { CodeClient } from './client'
import { mediaType } from './views'

// The largest file played or shown, read a chunk at a time: the whole of
// it is held in the page.
const maxMedia = 512 << 20
const chunk = 16 << 20

// MediaView shows a PDF in the browser's own viewer, or plays a sound or a
// video, read from the environment. It shows the file afresh whenever it
// changes.
export function MediaView({ client, path, kind }: { client: CodeClient; path: string; kind: 'pdf' | 'media' }) {
  const type = mediaType(path) ?? (kind === 'pdf' ? 'application/pdf' : 'video/mp4')
  const [url, setUrl] = useState<string | null>(null)
  const [read, setRead] = useState<{ done: number; size: number } | null>(null)
  const [error, setError] = useState('')

  useEffect(() => {
    let live = true
    let current: string | null = null
    const dir = path.includes('/') ? path.slice(0, path.lastIndexOf('/')) : ''
    const load = async () => {
      try {
        const parts: BlobPart[] = []
        let size = Infinity
        for (let at = 0; at < size;) {
          const b = await client.bytes(path, at, chunk)
          if (!live) return
          size = b.size
          if (size > maxMedia) {
            setError(`${path.split('/').pop()} is ${formatBytes(size)}, too large to show here.`)
            return
          }
          const bin = atob(b.data)
          if (!bin.length) break
          const bytes = new Uint8Array(bin.length)
          for (let i = 0; i < bin.length; i++) bytes[i] = bin.charCodeAt(i)
          parts.push(bytes)
          at += bytes.length
          setRead({ done: at, size })
        }
        const next = URL.createObjectURL(new Blob(parts, { type }))
        if (current) URL.revokeObjectURL(current)
        current = next
        setUrl(next)
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
        timer = setTimeout(load, 500)
      }
    })
    return () => {
      live = false
      clearTimeout(timer)
      off()
      if (current) URL.revokeObjectURL(current)
    }
  }, [client, path, type])

  if (error)
    return (
      <div className="code-empty">
        <p className="code-note-error">{error}</p>
      </div>
    )
  if (!url) {
    return (
      <div className="code-empty">
        <p className="muted">
          Reading {path.split('/').pop()}
          {read && read.size > chunk ? ` · ${formatBytes(read.done)} of ${formatBytes(read.size)}` : '…'}
        </p>
      </div>
    )
  }
  return (
    <div className="code-media">
      {kind === 'pdf' ? (
        <iframe src={url} title={path} />
      ) : type.startsWith('audio/') ? (
        <audio src={url} controls />
      ) : (
        <video src={url} controls />
      )}
    </div>
  )
}
