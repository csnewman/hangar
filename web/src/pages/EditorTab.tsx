import { Code2 } from 'lucide-react'
import { useEffect, useState } from 'react'

import { api } from '../api'
import { useEnv } from './Environment'

// EditorTab is the environment's editor: VS Code, served by its own server in
// the environment, on an origin of the environment's own. Opening the tab
// signs this browser in to that origin with a one-time ticket, so the editor
// never sees Hangar's session.
export function EditorTab() {
  const env = useEnv()
  const running = env.phase === 'running'
  const [src, setSrc] = useState<string>()
  const [error, setError] = useState<string>()

  useEffect(() => {
    if (!running) return
    let current = true
    setSrc(undefined)
    setError(undefined)
    api.openEditor(env.id).then(
      (r) => current && setSrc(r.url),
      (e: Error) => current && setError(e.message),
    )
    return () => {
      current = false
    }
  }, [env.id, running])

  if (!running || error) {
    return (
      <div className="console">
        <div className="console-empty">
          <Code2 size={36} strokeWidth={1.4} />
          <h2>Editor</h2>
          <p>{error ?? `Start ${env.name} to open its editor.`}</p>
        </div>
      </div>
    )
  }
  return (
    <div className="editor-page">
      {src && (
        <iframe
          className="editor-frame"
          title={`${env.name} editor`}
          src={src}
          // VS Code reads and writes the clipboard itself, which a
          // cross-origin frame may only do when the page allows it.
          allow="clipboard-read; clipboard-write; fullscreen"
        />
      )}
    </div>
  )
}
