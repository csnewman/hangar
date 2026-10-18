import { RotateCw, SquareArrowOutUpRight } from 'lucide-react'
import { useEffect, useState } from 'react'

import { webURL } from '../environments'
import { useEnv } from '../pages/Environment'
import { useMe } from '../session'
import type { CodeClient } from './client'

// isHtml is whether a file is a web page, by its name.
export const isHtml = (path: string) => /\.html?$/i.test(path)

// HtmlPreview shows an HTML file as a browser does, served from the
// environment by the Code service through its numbered host
// (<port>-env<short>), so the styles, scripts and images it loads from
// beside it load too. It reloads when anything in the file's folder
// changes, typing in it included.
export function HtmlPreview({ client, path }: { client: CodeClient; path: string }) {
  const env = useEnv()
  const me = useMe()
  const suffix = me.environment_host_suffix
  const [server, setServer] = useState<{ port: number; secret: string } | null>(null)
  const [error, setError] = useState('')
  const [n, setN] = useState(0)
  useEffect(() => {
    client.previewServer().then(setServer, (e: Error) => setError(e.message))
  }, [client])

  const abs = path.startsWith('/') ? path : `${client.root}/${path}`
  const dir = abs.slice(0, abs.lastIndexOf('/'))
  useEffect(() => {
    let timer: ReturnType<typeof setTimeout> | undefined
    const off = client.on('fs.changed', ({ dirs }) => {
      if (!dirs.some((d) => d === dir || d.startsWith(dir + '/'))) return
      clearTimeout(timer)
      timer = setTimeout(() => setN((x) => x + 1), 400)
    })
    return () => {
      clearTimeout(timer)
      off()
    }
  }, [client, dir])

  if (suffix === undefined) {
    return (
      <div className="code-empty">
        This Hangar has no hosts for environments&rsquo; web servers, which the preview is served on.
      </div>
    )
  }
  if (error)
    return (
      <div className="code-empty">
        <p className="code-note-error">{error}</p>
      </div>
    )
  if (!server)
    return (
      <div className="code-empty">
        <p className="muted">Starting the preview…</p>
      </div>
    )
  const url =
    webURL(String(server.port), env.short_id, suffix) + server.secret + abs.split('/').map(encodeURIComponent).join('/')
  return (
    <div className="code-html">
      <div className="code-image-bar">
        <button
          type="button"
          className="code-tool-btn"
          title="Reload"
          aria-label="Reload"
          onClick={() => setN((x) => x + 1)}
        >
          <RotateCw size={13} />
        </button>
        <span className="code-html-url muted small" title={url}>
          {abs}
        </span>
        <span className="code-tool-spacer" />
        <a
          className="code-tool-btn"
          href={url}
          target="_blank"
          rel="noopener"
          title="Open in a new tab"
          aria-label="Open in a new tab"
        >
          <SquareArrowOutUpRight size={13} />
        </a>
      </div>
      <iframe
        key={n}
        src={url}
        title={`Preview of ${abs}`}
        sandbox="allow-scripts allow-same-origin allow-forms allow-popups allow-modals"
      />
    </div>
  )
}
