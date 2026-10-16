import { Copy, Play, RefreshCw, Settings, SquareTerminal } from 'lucide-react'
import { useEffect, useRef, useState } from 'react'

import { formatAgo } from '../components/format'
import type { CodeClient, ServerInfo } from './client'
import { copyText, shortcut } from './ContextMenu'
import type { Languages, ServerState } from './lsp'

// ServerChip says, in the status bar, what knows the file's code: its
// language server, starting, running, or missing. Clicked, it opens the
// server's panel above it.
export function ServerChip({
  server,
  readOnly,
  client,
  languages,
}: {
  server: ServerState | null
  readOnly: boolean
  client: CodeClient
  languages: Languages | null
}) {
  const [open, setOpen] = useState(false)
  const box = useRef<HTMLDivElement>(null)
  useEffect(() => {
    if (!open) return
    const away = (e: MouseEvent) => {
      if (box.current && !box.current.contains(e.target as Node)) setOpen(false)
    }
    const key = (e: KeyboardEvent) => e.key === 'Escape' && setOpen(false)
    document.addEventListener('mousedown', away, true)
    document.addEventListener('keydown', key, true)
    return () => {
      document.removeEventListener('mousedown', away, true)
      document.removeEventListener('keydown', key, true)
    }
  }, [open])

  if (!server && !readOnly) return null
  let text = ''
  let tip = ''
  let cls = 'code-lsp'
  if (server?.state === 'starting') text = 'Starting language server…'
  else if (server?.state === 'ready') {
    text = server.command
    tip = `${server.command} knows this file's code: ${shortcut('Mod+B')} or ${shortcut('Mod')}-click goes to a definition, ${shortcut('Alt+F7')} finds usages, ${shortcut('Shift+F6')} renames. Click for its details.`
  } else if (server?.state === 'missing') {
    text = server.stopped ? `${server.language} server stopped` : 'No language server'
    tip = `${server.why}. Click for what to do.`
    cls += ' code-lsp-missing'
  }
  const clickable = !!languages && (server?.state === 'ready' || server?.state === 'missing')
  return (
    <div className="code-lsp-box" ref={box}>
      {open && server && languages && server.state !== 'starting' && (
        <LspPanel server={server} client={client} languages={languages} onClose={() => setOpen(false)} />
      )}
      <div className={cls}>
        {readOnly && <span className="code-lsp-ro">Read-only</span>}
        {text &&
          (clickable ? (
            <button type="button" className="code-lsp-btn" title={tip} onClick={() => setOpen((o) => !o)}>
              {text}
            </button>
          ) : (
            <span title={tip || undefined}>{text}</span>
          ))}
      </div>
    </div>
  )
}

// LspPanel is a language server's panel: for one running, what it is, how
// long it has run, who uses it, its log and the messages passed, and
// restarting it; for none, why, and how to install one or name another.
function LspPanel({
  server,
  client,
  languages,
  onClose,
}: {
  server: Exclude<ServerState, { state: 'starting' }>
  client: CodeClient
  languages: Languages
  onClose: () => void
}) {
  const [info, setInfo] = useState<ServerInfo | null>(null)
  const [error, setError] = useState('')
  const [tab, setTab] = useState<'log' | 'messages'>('messages')
  const [busy, setBusy] = useState(false)

  // Kept current while open: the log and messages move.
  useEffect(() => {
    let live = true
    const load = () =>
      languages.info(server.language).then(
        (i) => live && setInfo(i),
        (e: Error) => live && setError(e.message),
      )
    load()
    const t = setInterval(load, 2000)
    return () => {
      live = false
      clearInterval(t)
    }
  }, [languages, server.language])

  const restart = async () => {
    setBusy(true)
    try {
      await languages.restart(server.language)
      onClose()
    } catch (e) {
      setError(e instanceof Error ? e.message : String(e))
    } finally {
      setBusy(false)
    }
  }

  // The settings file, made if there is none, for naming a server.
  const openSettings = async () => {
    try {
      await client.saveSettings(await client.settings())
      if (!client.settingsPath) throw new Error('the environment did not say where the settings are')
      languages.openFile(client.settingsPath)
      onClose()
    } catch (e) {
      setError(e instanceof Error ? e.message : String(e))
    }
  }

  const running = info?.running ? info : null
  const name = running?.serverInfo?.name ?? (server.state === 'ready' ? server.command : server.language)

  return (
    <div className="code-lsp-panel" role="dialog" aria-label={`${server.language} language server`}>
      <div className="code-lsp-panel-head">
        <span className={running ? 'code-lsp-dot code-lsp-dot-on' : 'code-lsp-dot'} />
        <strong>{running || server.state === 'ready' ? name : `No ${server.language} language server`}</strong>
        {running?.serverInfo?.version && (
          <span className="muted small" title={running.serverInfo.version}>
            {shortVersion(running.serverInfo.version)}
          </span>
        )}
        {running && <span className="muted small">· {server.language}</span>}
        <span className="code-tool-spacer" />
        {running && (
          <button type="button" className="btn btn-sm" disabled={busy} onClick={restart}>
            <RefreshCw size={12} /> Restart
          </button>
        )}
      </div>
      {error && <p className="code-note code-note-error">{error}</p>}

      {running && (
        <>
          <dl className="code-lsp-facts">
            <dt>Running</dt>
            <dd>
              since {formatAgo(new Date(running.started).toISOString())}
              {running.pid !== undefined && <span className="muted"> · pid {running.pid}</span>}
            </dd>
            <dt>Command</dt>
            <dd className="code-lsp-cmd">
              <code>{[running.path, ...running.args].join(' ')}</code>
              <button
                type="button"
                className="code-tool-btn"
                title="Copy the command"
                onClick={() => copyText([running.path, ...running.args].join(' '))}
              >
                <Copy size={11} />
              </button>
            </dd>
            <dt>Folder</dt>
            <dd>
              <code>{running.root}</code>
            </dd>
            <dt>Used by</dt>
            <dd>
              {running.tabs} browser tab{running.tabs === 1 ? '' : 's'}, {running.documents} file
              {running.documents === 1 ? '' : 's'} open
            </dd>
          </dl>
          <div className="code-changes-mode" role="tablist">
            <button type="button" role="tab" aria-selected={tab === 'messages'} onClick={() => setTab('messages')}>
              Messages
            </button>
            <button type="button" role="tab" aria-selected={tab === 'log'} onClick={() => setTab('log')}>
              Log
            </button>
          </div>
          {tab === 'log' ? (
            <pre className="code-lsp-log">{running.log.trim() || 'The server has written nothing to its log.'}</pre>
          ) : (
            <div className="code-lsp-log code-lsp-trace">
              {[...running.trace].reverse().map((m, i) => (
                <div key={i} className={m.error ? 'code-lsp-trace-error' : undefined} title={m.error}>
                  <span className="muted">{new Date(m.t).toLocaleTimeString()}</span>
                  <span className="code-lsp-dir">{m.dir === 'to' ? '→' : '←'}</span>
                  <span>{m.method ?? `answer ${m.id}`}</span>
                  {m.kind === 'response' && m.method && <span className="muted">answered</span>}
                  {m.ms !== undefined && <span className="muted">{m.ms} ms</span>}
                  {m.error && <span className="code-lsp-err">{m.error}</span>}
                </div>
              ))}
              {running.trace.length === 0 && <p className="code-note">No messages yet.</p>}
            </div>
          )}
        </>
      )}

      {!running && (
        <div className="code-lsp-missing-body">
          <p>
            {server.state === 'missing'
              ? server.why.charAt(0).toUpperCase() + server.why.slice(1) + '.'
              : `The ${server.language} server is not running.`}
          </p>
          {info && !info.running && 'install' in info && info.install.length > 0 && (
            <>
              <p className="muted small">Install one, then try again:</p>
              {info.install.map((s) => (
                <div key={s.command} className="code-lsp-install">
                  <strong>{s.command}</strong>
                  {s.install ? (
                    <>
                      <code>{s.install}</code>
                      <button
                        type="button"
                        className="code-tool-btn"
                        title="Run it in a new terminal session"
                        onClick={() => {
                          languages.run(s.install!)
                          onClose()
                        }}
                      >
                        <SquareTerminal size={13} />
                      </button>
                      <button type="button" className="code-tool-btn" title="Copy" onClick={() => copyText(s.install!)}>
                        <Copy size={12} />
                      </button>
                    </>
                  ) : (
                    <span className="muted small">see its documentation</span>
                  )}
                </div>
              ))}
            </>
          )}
          {info && !info.running && !('install' in info) && (
            <p className="muted small">
              <code>{info.command}</code> is installed at <code>{info.path}</code>.
            </p>
          )}
          <div className="code-lsp-actions">
            <button
              type="button"
              className="btn btn-sm btn-primary"
              onClick={() => {
                languages.retry(server.language)
                onClose()
              }}
            >
              <Play size={12} /> {server.state === 'missing' && server.stopped ? 'Start again' : 'Try again'}
            </button>
            <button
              type="button"
              className="btn btn-sm"
              title={`Name a server for ${server.language} in the settings' languageServers`}
              onClick={openSettings}
            >
              <Settings size={12} /> Choose a server…
            </button>
          </div>
        </div>
      )}
    </div>
  )
}

// shortVersion is a server's version for its panel's head. gopls gives its
// whole build as JSON; its module's version is what a person means.
function shortVersion(v: string): string {
  if (v.startsWith('{')) {
    try {
      const b = JSON.parse(v) as { Version?: string; Main?: { Version?: string } }
      const found = b.Main?.Version ?? b.Version
      if (found) return found
    } catch {
      // Not JSON after all.
    }
  }
  return v.length > 40 ? `${v.slice(0, 40)}…` : v
}
