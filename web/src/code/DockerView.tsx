import {
  Box,
  ChevronDown,
  ChevronRight,
  FileCode,
  FileText,
  Layers,
  Play,
  RefreshCw,
  RotateCw,
  Square,
  SquareTerminal,
  Trash2,
} from 'lucide-react'
import { useCallback, useEffect, useRef, useState, type MouseEvent, type ReactNode } from 'react'

import { formatAgo, formatBytes } from '../components/format'
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
  onOpenFile,
  onHide,
}: {
  client: CodeClient
  onLogs: (c: Container) => void
  onShell: (c: Container) => void
  // onOpenFile opens a file, by its absolute path, as an editor tab.
  onOpenFile: (path: string) => void
  onHide: () => void
}) {
  const [data, setData] = useState<DockerOverview | null>(null)
  const [error, setError] = useState('')
  const [busy, setBusy] = useState<Set<string>>(new Set())
  const [imagesOpen, setImagesOpen] = useState(false)
  const [closed, setClosed] = useState<Set<string>>(new Set())
  // What the details below the list are of.
  const [selected, setSelected] = useState<{ kind: 'container' | 'project'; key: string } | null>(null)
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

  const projectAct = (name: string, action: 'start' | 'stop' | 'restart' | 'down') => {
    const key = `project:${name}`
    setBusy((b) => new Set(b).add(key))
    client
      .dockerProject(name, action)
      .then(load, (e: Error) => setError(`${name}: ${e.message}`))
      .finally(() =>
        setBusy((b) => {
          const n = new Set(b)
          n.delete(key)
          return n
        }),
      )
  }

  const projectMenu = (name: string, list: Container[]): MenuItem[] => {
    const files = list.find((c) => c.configFiles?.length)?.configFiles ?? []
    const dir = list.find((c) => c.workingDir)?.workingDir
    const anyUp = list.some((c) => c.state === 'running' || c.state === 'paused')
    return [
      {
        label: 'Start All',
        disabled: list.every((c) => c.state === 'running'),
        onSelect: () => projectAct(name, 'start'),
      },
      { label: 'Stop All', disabled: !anyUp, onSelect: () => projectAct(name, 'stop') },
      { label: 'Restart All', onSelect: () => projectAct(name, 'restart') },
      'separator',
      ...files.map((f): MenuItem => ({ label: `Open ${f.split('/').pop()}`, onSelect: () => onOpenFile(f) })),
      { label: 'Copy Working Directory', disabled: !dir, onSelect: () => dir && copyText(dir) },
      'separator',
      { label: 'Down (Remove Containers and Networks)', danger: true, onSelect: () => projectAct(name, 'down') },
    ]
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
  const toggleGroup = (g: string) =>
    setClosed((s) => {
      const n = new Set(s)
      if (n.has(g)) n.delete(g)
      else n.add(g)
      return n
    })
  const selectedContainer =
    selected?.kind === 'container' ? data?.containers.find((c) => c.id === selected.key) : undefined
  const selectedProject = selected?.kind === 'project' ? groups.get(selected.key) : undefined

  const row = (c: Container, nested: boolean) => {
    const isRunning = c.state === 'running'
    const isBusy = busy.has(c.id)
    return (
      <div
        key={c.id}
        className={`code-docker-row${nested ? ' code-docker-nested' : ''}${
          selected?.kind === 'container' && selected.key === c.id ? ' code-docker-row-on' : ''
        }`}
        onClick={() => setSelected({ kind: 'container', key: c.id })}
        onDoubleClick={() => onLogs(c)}
        onContextMenu={(e: MouseEvent) => {
          setSelected({ kind: 'container', key: c.id })
          menu.open(e, containerMenu(c))
        }}
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
      <div className="code-docker-split">
        <div
          className="code-docker"
          onContextMenu={(e) => {
            // Empty space has no menu of its own, and not the browser's.
            if (e.target === e.currentTarget) e.preventDefault()
          }}
        >
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
                  className={`code-docker-row code-docker-group${
                    selected?.kind === 'project' && selected.key === g ? ' code-docker-row-on' : ''
                  }`}
                  onClick={() => setSelected({ kind: 'project', key: g })}
                  onDoubleClick={() => toggleGroup(g)}
                  onContextMenu={(e: MouseEvent) => {
                    setSelected({ kind: 'project', key: g })
                    menu.open(e, projectMenu(g, list))
                  }}
                >
                  <span
                    className="code-docker-chevron"
                    onClick={(e) => {
                      e.stopPropagation()
                      toggleGroup(g)
                    }}
                  >
                    {open ? <ChevronDown size={13} /> : <ChevronRight size={13} />}
                  </span>
                  <Layers size={13} className="code-docker-icon" />
                  <span className="code-docker-name">{g}</span>
                  <span className="code-docker-meta">
                    {up} of {list.length} running
                  </span>
                  <span className="code-docker-actions">
                    {up > 0 ? (
                      <button
                        type="button"
                        className="code-tool-btn"
                        title="Stop all"
                        disabled={busy.has(`project:${g}`)}
                        onClick={(e) => {
                          e.stopPropagation()
                          projectAct(g, 'stop')
                        }}
                      >
                        <Square size={12} />
                      </button>
                    ) : (
                      <button
                        type="button"
                        className="code-tool-btn"
                        title="Start all"
                        disabled={busy.has(`project:${g}`)}
                        onClick={(e) => {
                          e.stopPropagation()
                          projectAct(g, 'start')
                        }}
                      >
                        <Play size={12} />
                      </button>
                    )}
                    <button
                      type="button"
                      className="code-tool-btn"
                      title="Restart all"
                      disabled={busy.has(`project:${g}`)}
                      onClick={(e) => {
                        e.stopPropagation()
                        projectAct(g, 'restart')
                      }}
                    >
                      <RotateCw size={12} />
                    </button>
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
        {selectedContainer && (
          <ContainerDetails
            c={selectedContainer}
            busy={busy.has(selectedContainer.id)}
            act={(a) => act(selectedContainer, a)}
            onLogs={() => onLogs(selectedContainer)}
            onShell={() => onShell(selectedContainer)}
            onOpenFile={onOpenFile}
          />
        )}
        {selected?.kind === 'project' && selectedProject && (
          <ProjectDetails
            name={selected.key}
            list={selectedProject}
            busy={busy.has(`project:${selected.key}`)}
            act={(a) => projectAct(selected.key, a)}
            onOpenFile={onOpenFile}
          />
        )}
      </div>
      {menu.menu}
    </ToolWindow>
  )
}

// Detail is one line of the details pane: what, and its value.
function Detail({ label, children }: { label: string; children: ReactNode }) {
  return (
    <>
      <span className="code-detail-label">{label}</span>
      <span className="code-detail-value">{children}</span>
    </>
  )
}

function ComposeFiles({ files, onOpenFile }: { files?: string[]; onOpenFile: (path: string) => void }) {
  if (!files?.length) return null
  return (
    <Detail label={files.length === 1 ? 'Compose file' : 'Compose files'}>
      {files.map((f) => (
        <button
          key={f}
          type="button"
          className="code-link code-docker-file"
          title="Open it"
          onClick={() => onOpenFile(f)}
        >
          <FileCode size={12} /> {f}
        </button>
      ))}
    </Detail>
  )
}

function ContainerDetails({
  c,
  busy,
  act,
  onLogs,
  onShell,
  onOpenFile,
}: {
  c: Container
  busy: boolean
  act: (a: DockerAction) => void
  onLogs: () => void
  onShell: () => void
  onOpenFile: (path: string) => void
}) {
  const running = c.state === 'running'
  return (
    <div className="code-details">
      <div className="code-details-head">
        <span className={`code-docker-dot code-docker-${c.state}`} />
        <span className="strong">{c.name}</span>
        <span className="muted small">{c.status}</span>
      </div>
      <div className="code-detail-grid">
        <Detail label="Image">{c.image}</Detail>
        <Detail label="ID">
          <button type="button" className="code-link" title="Copy it" onClick={() => copyText(c.id)}>
            {c.id.slice(0, 12)}
          </button>
        </Detail>
        {c.command && <Detail label="Command">{c.command}</Detail>}
        <Detail label="Ports">{c.ports.length ? c.ports.join(', ') : 'none published'}</Detail>
        <Detail label="Created">{formatAgo(new Date(c.created * 1000).toISOString())}</Detail>
        {c.project && (
          <Detail label="Compose">
            {c.project}
            {c.service ? ` / ${c.service}` : ''}
          </Detail>
        )}
        <ComposeFiles files={c.configFiles} onOpenFile={onOpenFile} />
      </div>
      <div className="code-details-actions">
        {running || c.state === 'paused' ? (
          <button type="button" className="btn btn-ghost btn-sm" disabled={busy} onClick={() => act('stop')}>
            <Square size={12} /> Stop
          </button>
        ) : (
          <button type="button" className="btn btn-ghost btn-sm" disabled={busy} onClick={() => act('start')}>
            <Play size={12} /> Start
          </button>
        )}
        <button type="button" className="btn btn-ghost btn-sm" disabled={busy} onClick={() => act('restart')}>
          <RotateCw size={12} /> Restart
        </button>
        <button type="button" className="btn btn-ghost btn-sm" onClick={onLogs}>
          <FileText size={12} /> Logs
        </button>
        <button type="button" className="btn btn-ghost btn-sm" disabled={!running} onClick={onShell}>
          <SquareTerminal size={12} /> Shell
        </button>
        <button
          type="button"
          className="btn btn-ghost btn-sm code-docker-danger"
          disabled={busy}
          onClick={() => act('remove')}
        >
          <Trash2 size={12} /> Remove
        </button>
      </div>
    </div>
  )
}

function ProjectDetails({
  name,
  list,
  busy,
  act,
  onOpenFile,
}: {
  name: string
  list: Container[]
  busy: boolean
  act: (a: 'start' | 'stop' | 'restart' | 'down') => void
  onOpenFile: (path: string) => void
}) {
  const up = list.filter((c) => c.state === 'running').length
  const dir = list.find((c) => c.workingDir)?.workingDir
  const files = list.find((c) => c.configFiles?.length)?.configFiles
  return (
    <div className="code-details">
      <div className="code-details-head">
        <Layers size={13} className="code-docker-icon" />
        <span className="strong">{name}</span>
        <span className="muted small">
          Compose project, {up} of {list.length} running
        </span>
      </div>
      <div className="code-detail-grid">
        {dir && <Detail label="Directory">{dir}</Detail>}
        <ComposeFiles files={files} onOpenFile={onOpenFile} />
        <Detail label="Services">{list.map((c) => `${c.service ?? c.name} (${c.state})`).join(', ')}</Detail>
      </div>
      <div className="code-details-actions">
        <button
          type="button"
          className="btn btn-ghost btn-sm"
          disabled={busy || up === list.length}
          onClick={() => act('start')}
        >
          <Play size={12} /> Start all
        </button>
        <button type="button" className="btn btn-ghost btn-sm" disabled={busy || up === 0} onClick={() => act('stop')}>
          <Square size={12} /> Stop all
        </button>
        <button type="button" className="btn btn-ghost btn-sm" disabled={busy} onClick={() => act('restart')}>
          <RotateCw size={12} /> Restart all
        </button>
        <button
          type="button"
          className="btn btn-ghost btn-sm code-docker-danger"
          disabled={busy}
          title="Remove its containers and networks, as docker compose down does"
          onClick={() => act('down')}
        >
          <Trash2 size={12} /> Down
        </button>
      </div>
    </div>
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
