import { File, GitCompare, X } from 'lucide-react'
import { useEffect, useState } from 'react'

import type { CodeClient, SharedFile } from './client'
import { CodeEditor } from './CodeEditor'
import { DiffView } from './DiffView'

export type Tab = { kind: 'file' | 'diff'; path: string }

export const tabKey = (t: Tab) => `${t.kind}:${t.path}`

// EditorArea is the Code tab's centre: a strip of tabs and the chosen one, a
// shared file or a diff.
export function EditorArea({
  client,
  tabs,
  active,
  user,
  onActivate,
  onClose,
}: {
  client: CodeClient
  tabs: Tab[]
  active: string | null
  user: { name: string; color: string }
  onActivate: (key: string) => void
  onClose: (key: string) => void
}) {
  const current = tabs.find((t) => tabKey(t) === active) ?? null
  return (
    <div className="code-editors">
      {tabs.length > 0 && (
        <div className="code-tabs" role="tablist">
          {tabs.map((t) => {
            const key = tabKey(t)
            const name = t.path.split('/').pop() ?? t.path
            return (
              <div
                key={key}
                role="tab"
                aria-selected={key === active}
                className={key === active ? 'code-tab code-tab-on' : 'code-tab'}
                title={t.kind === 'diff' ? `Changes to ${t.path}` : t.path}
                onClick={() => onActivate(key)}
                onAuxClick={(e) => {
                  if (e.button === 1) onClose(key)
                }}
              >
                {t.kind === 'diff' ? <GitCompare size={13} className="code-icon-diff" /> : <File size={13} className="code-icon-file" />}
                <span>{name}</span>
                <button
                  type="button"
                  className="code-tab-close"
                  aria-label={`Close ${name}`}
                  onClick={(e) => {
                    e.stopPropagation()
                    onClose(key)
                  }}
                >
                  <X size={12} />
                </button>
              </div>
            )
          })}
        </div>
      )}
      <div className="code-editor-body">
        {current?.kind === 'file' && <FileTab key={tabKey(current)} client={client} path={current.path} user={user} />}
        {current?.kind === 'diff' && <DiffView key={tabKey(current)} client={client} path={current.path} />}
        {!current && (
          <div className="code-empty">
            <p>Open a file from Project, or a change from Changes.</p>
            <p className="muted small">Everyone with a file open edits it together; changes are saved as you type.</p>
          </div>
        )}
      </div>
    </div>
  )
}

function FileTab({ client, path, user }: { client: CodeClient; path: string; user: { name: string; color: string } }) {
  const [file, setFile] = useState<SharedFile | null>(null)
  const [error, setError] = useState('')

  // A file deleted while open is shown as deleted rather than edited, which
  // would bring it back.
  useEffect(
    () =>
      client.on('doc.deleted', ({ path: gone }) => {
        if (gone === `${client.root}/${path}` || gone === path) setError(`${path.split('/').pop()} was deleted.`)
      }),
    [client, path],
  )

  useEffect(() => {
    let f: SharedFile | null = null
    let live = true
    setFile(null)
    setError('')
    client.open(path).then(
      async (opened) => {
        f = opened
        if (!live) {
          client.release(opened)
          return
        }
        await opened.whenSynced()
        if (live) setFile(opened)
      },
      (e: Error) => live && setError(e.message),
    )
    return () => {
      live = false
      if (f) client.release(f)
    }
  }, [client, path])

  if (error) return <div className="code-empty"><p className="code-note-error">{error}</p></div>
  if (!file) return <div className="code-empty"><p className="muted">Opening {path}…</p></div>
  return <CodeEditor file={file} user={user} />
}
