import {
  Box,
  ChevronDown,
  ChevronRight,
  FileText,
  Layers,
  Play,
  RefreshCw,
  RotateCw,
  Square,
  SquareTerminal,
  Trash2,
} from 'lucide-react'
import { useCallback, useEffect, useRef, useState, type MouseEvent } from 'react'

import { formatBytes } from '../components/format'
import type { CodeClient, Container, DockerAction, DockerOverview } from './client'
import { copyText, useContextMenu, type MenuItem } from './ContextMenu'
import { ToolWindow } from './ToolWindow'

// The command a shell in a container runs: bash where the image has it.
export const containerShell = (name: string) =>
  `docker exec -it ${name} sh -c 'command -v bash >/dev/null && exec bash || exec sh'`

// DockerView is the Docker tool window: the environment's containers,
// grouped by Compose project, and its images, kept current as Docker
// reports changes; what can be done to each, a container's logs, and a
// shell in it.
export function DockerView({
  client,
  onLogs,
  onShell,
  onHide,
}: {
  client: CodeClient
  onLogs: (c: Container) => void
  onShell: (c: Container) => void
  onHide: () => void
}) {
  const [data, setData] = useState<DockerOverview | null>(null)
  const [error, setError] = useState('')
  const [busy, setBusy] = useState<Set<string>>(new Set())
  const [imagesOpen, setImagesOpen] = useState(false)
  const [closed, setClosed] = useState<Set<string>>(new Set())
  const menu = useContextMenu()

  const load = useCallback(() => {
    client.dockerOverview().then(
      (o) => {
        setData(o)
        setError('')
      },
      (e: Error) => setError(e.message),
    )
  }, [client])
  useEffect(() => {
    load()
    let timer: ReturnType<typeof setTimeout> | undefined
    const later = () => {
      clearTimeout(timer)
      timer = setTimeout(load, 150)
    }
    const offs = [client.on('docker.changed', later), client.on('state', (s) => s === 'live' && later())]
    // Statuses ("Up 3 minutes") move on without Docker saying so.
    const tick = setInterval(load, 30_000)
    return () => {
      clearTimeout(timer)
      clearInterval(tick)
      offs.forEach((off) => off())
    }
  }, [client, load])

  const act = (c: Container, action: DockerAction) => {
    setBusy((b) => new Set(b).add(c.id))
    client
      .dockerAct(c.id, action)
      .then(load, (e: Error) => setError(`${c.name}: ${e.message}`))
      .finally(() =>
        setBusy((b) => {
          const n = new Set(b)
          n.delete(c.id)
          return n
        }),
      )
  }

  const containerMenu = (c: Container): MenuItem[] => {
    const running = c.state === 'running'
    const paused = c.state === 'paused'
    return [
      { label: 'Show Logs', onSelect: () => onLogs(c) },
      { label: 'Open Shell', disabled: !running, onSelect: () => onShell(c) },
      'separator',
      running || paused
        ? { label: 'Stop', onSelect: () => act(c, 'stop') }
        : { label: 'Start', onSelect: () => act(c, 'start') },
      { label: 'Restart', onSelect: () => act(c, 'restart') },
      paused
        ? { label: 'Unpause', onSelect: () => act(c, 'unpause') }
        : { label: 'Pause', disabled: !running, onSelect: () => act(c, 'pause') },
      'separator',
      { label: 'Copy Name', onSelect: () => copyText(c.name) },
      { label: 'Copy ID', onSelect: () => copyText(c.id) },
      'separator',
      { label: running ? 'Stop and Remove' : 'Remove', danger: true, onSelect: () => act(c, 'remove') },
    ]
  }

  // Containers by Compose project, those of none last.
  const groups = new Map<string, Container[]>()
  for (const c of data?.containers ?? []) {
    const g = c.project ?? ''
    groups.set(g, [...(groups.get(g) ?? []), c])
  }
  const order = [...groups.keys()].sort((a, b) => (a === '' ? 1 : b === '' ? -1 : a.localeCompare(b)))
  const running = data?.containers.filter((c) => c.state === 'running').length ?? 0

  const row = (c: Container, nested: boolean) => {
    const isRunning = c.state === 'running'
    const isBusy = busy.has(c.id)
    return (
      <div
        key={c.id}
        className={`code-docker-row${nested ? ' code-docker-nested' : ''}`}
        onDoubleClick={() => onLogs(c)}
        onContextMenu={(e: MouseEvent) => menu.open(e, containerMenu(c))}
      >
        <span className={`code-docker-dot code-docker-${c.state}`} title={c.status} />
        <Box size={13} className="code-docker-icon" />
        <span className="code-docker-name" title={`${c.name}\n${c.image}\n${c.status}`}>
          {c.service && nested ? c.service : c.name}
        </span>
        <span className="code-docker-meta">{c.ports.length ? c.ports.join(', ') : c.image}</span>
        <span className="code-docker-actions">
          {isRunning || c.state === 'paused' ? (
            <button
              type="button"
              className="code-tool-btn"
              title="Stop"
              disabled={isBusy}
              onClick={() => act(c, 'stop')}
            >
              <Square size={12} />
            </button>
          ) : (
            <button
              type="button"
              className="code-tool-btn"
              title="Start"
              disabled={isBusy}
              onClick={() => act(c, 'start')}
            >
              <Play size={12} />
            </button>
          )}
          <button
            type="button"
            className="code-tool-btn"
            title="Restart"
            disabled={isBusy}
            onClick={() => act(c, 'restart')}
          >
            <RotateCw size={12} />
          </button>
          <button type="button" className="code-tool-btn" title="Show logs" onClick={() => onLogs(c)}>
            <FileText size={12} />
          </button>
          <button
            type="button"
            className="code-tool-btn"
            title="Open a shell in it"
            disabled={!isRunning}
            onClick={() => onShell(c)}
          >
            <SquareTerminal size={12} />
          </button>
        </span>
      </div>
    )
  }

  return (
    <ToolWindow
      title="Docker"
      onHide={onHide}
      extra={
        data?.available && <span className="muted small">{`${running} of ${data.containers.length} running`}</span>
      }
      actions={
        <button type="button" className="code-tool-btn" title="Refresh" onClick={load}>
          <RefreshCw size={13} />
        </button>
      }
    >
      <div className="code-docker">
        {data && !data.available && <div className="code-note">{data.why}</div>}
        {error && <div className="code-note code-note-error">{error}</div>}
        {data?.available && data.containers.length === 0 && (
          <div className="code-note">No containers. Start one with docker run or docker compose up.</div>
        )}
        {order.map((g) => {
          const list = groups.get(g)!
          if (!g) return list.map((c) => row(c, false))
          const open = !closed.has(g)
          const up = list.filter((c) => c.state === 'running').length
          return (
            <div key={`project:${g}`}>
              <div
                className="code-docker-row code-docker-group"
                onClick={() =>
                  setClosed((s) => {
                    const n = new Set(s)
                    if (open) n.add(g)
                    else n.delete(g)
                    return n
                  })
                }
              >
                {open ? <ChevronDown size={13} /> : <ChevronRight size={13} />}
                <Layers size={13} className="code-docker-icon" />
                <span className="code-docker-name">{g}</span>
                <span className="code-docker-meta">
                  {up} of {list.length} running
                </span>
              </div>
              {open && list.map((c) => row(c, true))}
            </div>
          )
        })}
        {data?.available && (
          <>
            <div
              className="code-docker-row code-docker-group code-docker-section"
              onClick={() => setImagesOpen((o) => !o)}
            >
              {imagesOpen ? <ChevronDown size={13} /> : <ChevronRight size={13} />}
              <span className="code-docker-name">Images</span>
              <span className="code-docker-meta">
                {data.images.length}, {formatBytes(data.images.reduce((n, i) => n + i.size, 0))}
              </span>
            </div>
            {imagesOpen &&
              data.images.map((i) => (
                <div
                  key={i.id}
                  className="code-docker-row code-docker-nested"
                  onContextMenu={(e: MouseEvent) =>
                    menu.open(e, [
                      { label: 'Copy ID', onSelect: () => copyText(i.id) },
                      {
                        label: 'Remove',
                        danger: true,
                        disabled: i.containers > 0,
                        onSelect: () => client.dockerRemoveImage(i.id).then(load, (e: Error) => setError(e.message)),
                      },
                    ])
                  }
                >
                  <span className="code-docker-name">{i.tags[0] ?? i.id.slice(7, 19)}</span>
                  <span className="code-docker-meta">{formatBytes(i.size)}</span>
                  <span className="code-docker-actions">
                    <button
                      type="button"
                      className="code-tool-btn"
                      title={i.containers > 0 ? 'In use by a container' : 'Remove'}
                      disabled={i.containers > 0}
                      onClick={() => client.dockerRemoveImage(i.id).then(load, (e: Error) => setError(e.message))}
                    >
                      <Trash2 size={12} />
                    </button>
                  </span>
                </div>
              ))}
          </>
        )}
      </div>
      {menu.menu}
    </ToolWindow>
  )
}

// Escape sequences a terminal would interpret, which a plain view shows
// as noise: colours, cursor movement, titles.
// eslint-disable-next-line no-control-regex
const ansi = /\x1b\[[0-9;?]*[ -/]*[@-~]|\x1b\][^\x07\x1b]*(?:\x07|\x1b\\)|\x1b[@-Z\\-_]/g

// The most of a container's output a logs tab keeps.
const MAX_LOG = 2 << 20

// DockerLogs follows a container's output in an editor tab: its last
// thousand lines, then what it writes, kept at the bottom unless scrolled
// up.
export function DockerLogs({ client, id, name }: { client: CodeClient; id: string; name: string }) {
  const [text, setText] = useState('')
  const [ended, setEnded] = useState('')
  const [follow, setFollow] = useState(true)
  const [n, setN] = useState(0)
  const pre = useRef<HTMLPreElement>(null)

  useEffect(() => {
    let stream = -1
    let live = true
    setText('')
    setEnded('')
    const offLog = client.on('docker.log', (p) => {
      if (p.stream !== stream) return
      setText((t) => {
        const next = t + p.text.replace(ansi, '').replace(/\r\n/g, '\n')
        return next.length > MAX_LOG ? next.slice(next.length - MAX_LOG) : next
      })
    })
    const offEnd = client.on('docker.logEnd', (p) => {
      if (p.stream === stream) setEnded(p.why || 'ended')
    })
    client.dockerLogs(id).then(
      (r) => {
        stream = r.stream
        if (!live) client.dockerLogsStop(r.stream).catch(() => {})
      },
      (e: Error) => setEnded(e.message),
    )
    return () => {
      live = false
      offLog()
      offEnd()
      if (stream >= 0) client.dockerLogsStop(stream).catch(() => {})
    }
  }, [client, id, n])

  useEffect(() => {
    const el = pre.current
    if (el && follow) el.scrollTop = el.scrollHeight
  }, [text, follow])

  return (
    <div className="code-logs">
      <div className="code-logs-head">
        <span className="strong">{name}</span>
        <span className="muted small">{ended ? `Not following: ${ended}` : 'Following its output'}</span>
        <span className="code-tool-spacer" />
        <label className="code-find-mask">
          <input type="checkbox" checked={follow} onChange={(e) => setFollow(e.target.checked)} />
          Scroll to the end
        </label>
        <button type="button" className="code-tool-btn" title="Clear" onClick={() => setText('')}>
          <Trash2 size={13} />
        </button>
        {ended && (
          <button type="button" className="code-tool-btn" title="Follow again" onClick={() => setN((x) => x + 1)}>
            <RotateCw size={13} />
          </button>
        )}
      </div>
      <pre
        ref={pre}
        className="code-logs-body"
        onScroll={(e) => {
          const el = e.currentTarget
          const atEnd = el.scrollHeight - el.scrollTop - el.clientHeight < 8
          if (atEnd !== follow) setFollow(atEnd)
        }}
      >
        {text || (ended ? '' : 'Waiting for output…')}
      </pre>
    </div>
  )
}
