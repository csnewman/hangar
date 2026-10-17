import { useMutation, useQueryClient } from '@tanstack/react-query'
import {
  Code2,
  Copy,
  ExternalLink,
  GitBranch,
  Info,
  Monitor,
  MoreHorizontal,
  Pause,
  Play,
  Square,
  Trash2,
  Undo2,
} from 'lucide-react'
import { useEffect, useRef, useState, type ReactNode } from 'react'

import { api, type Environment, type EnvironmentSettings, type TemplateSetting } from '../api'
import { Conflicts, CopyChoice, fromWhat, useEnvironmentPacks } from '../components/EnvironmentPacks'
import { formatAgo, formatBytes, formatMemory, shortDigest } from '../components/format'
import { imageBuild, imageRisk, useImageChange } from '../components/ImageUpdate'
import { Banner, CopyField, Row, Rows, Section } from '../components/Pane'
import { PhaseDot } from '../components/Status'
import { desktop } from '../desktop'
import { openInVSCode, useEnvironmentActions } from '../environmentActions'
import { changeable, environmentsKey, longestWebName, sshCommand, webURL, withSize } from '../environments'
import { copyName, packPath } from '../pages/Packs'
import { useMe } from '../session'
import { homeTab, otherTab } from '../workspaceLinks'
import { copyText, useContextMenu, type MenuItem } from './ContextMenu'
import { EnvironmentActivity } from './EnvironmentActivity'

// EnvironmentView is an environment's own page in its workspace, laid out as
// an IDE lays out its settings: what it is and what it is doing at the
// head, with what can be done to it; notices across the top; then one page
// of sections, each a titled rule over rows of labels and values, with an
// index beside it that follows the scroll. Its machine's settings are
// changed where they are shown, and a bar at the foot applies them. lead
// comes before the sections: how a start is going, or why one failed.
// onDesktop, where given, shows the environment's desktop.
export function EnvironmentView({
  env,
  lead,
  onDesktop,
}: {
  env: Environment
  lead?: ReactNode
  onDesktop?: () => void
}) {
  const settings = useSettingsForm(env)
  const sections = [
    { id: 'overview', label: 'Overview' },
    { id: 'connect', label: 'Connect' },
    { id: 'machine', label: 'Machine' },
    ...(env.spec.repos.length > 0 ? [{ id: 'repositories', label: 'Repositories' }] : []),
    { id: 'files', label: 'Files' },
    { id: 'activity', label: 'Activity' },
    { id: 'delete', label: 'Delete' },
  ]
  const scroller = useRef<HTMLDivElement>(null)
  const [current, setCurrent] = useState('overview')
  // While a jump scrolls, the section jumped to stays chosen, though the
  // page may end before it reaches the top.
  const jumping = useRef(false)
  const ids = sections.map((s) => s.id).join(' ')

  useEffect(() => {
    const root = scroller.current
    if (!root) return
    const onScroll = () => {
      if (jumping.current) return
      const top = root.getBoundingClientRect().top
      const bottom = root.getBoundingClientRect().bottom
      const list = ids.split(' ').map((id) => ({ id, el: root.querySelector(`[data-section="${id}"]`) }))
      let at = list[0].id
      for (const { id, el } of list) {
        if (el && el.getBoundingClientRect().top - top <= 40) at = id
      }
      // At the end of the page the sections after it cannot reach the top:
      // one of them that is in view stays chosen.
      const end = root.scrollTop + root.clientHeight >= root.scrollHeight - 2
      setCurrent((was) => {
        const el = list.find((s) => s.id === was)?.el
        const shown = el && el.getBoundingClientRect().top < bottom
        return end && shown && list.findIndex((s) => s.id === was) > list.findIndex((s) => s.id === at) ? was : at
      })
    }
    const settled = () => {
      jumping.current = false
    }
    root.addEventListener('scroll', onScroll, { passive: true })
    root.addEventListener('scrollend', settled)
    return () => {
      root.removeEventListener('scroll', onScroll)
      root.removeEventListener('scrollend', settled)
    }
  }, [ids])

  const jump = (id: string) => {
    const root = scroller.current
    const el = root?.querySelector<HTMLElement>(`[data-section="${id}"]`)
    if (!root || !el) return
    const top = Math.min(el.offsetTop - 16, root.scrollHeight - root.clientHeight)
    setCurrent(id)
    if (Math.abs(root.scrollTop - top) < 1) return
    jumping.current = true
    // A browser without scrollend is given the time a smooth scroll takes.
    if (!('onscrollend' in window)) setTimeout(() => (jumping.current = false), 1000)
    root.scrollTo({ top, behavior: 'smooth' })
  }

  return (
    <div className="envtab">
      <Banners env={env} />
      <Head env={env} />
      <div className="envtab-body">
        <nav className="envtab-index" aria-label="Sections">
          {sections.map((s) => (
            <button
              key={s.id}
              type="button"
              className={s.id === current ? 'envtab-index-item envtab-index-on' : 'envtab-index-item'}
              aria-current={s.id === current ? 'true' : undefined}
              onClick={() => jump(s.id)}
            >
              {s.label}
            </button>
          ))}
        </nav>
        <div className="envtab-scroll" ref={scroller}>
          <div className="pane-page">
            {lead}
            <Section id="overview" title="Overview">
              <Overview env={env} />
            </Section>
            <Section id="connect" title="Connect">
              <Connect env={env} onDesktop={onDesktop} />
            </Section>
            <Section id="machine" title="Machine">
              <Machine env={env} form={settings} />
            </Section>
            {env.spec.repos.length > 0 && (
              <Section id="repositories" title="Repositories">
                <Repositories env={env} />
              </Section>
            )}
            <Section id="files" title="Files">
              <Files env={env} />
            </Section>
            <Section id="activity" title="Activity">
              <EnvironmentActivity env={env.id} />
            </Section>
            <Section id="delete" title="Delete">
              <Delete env={env} />
            </Section>
          </div>
        </div>
      </div>
      {settings.changes.length > 0 && <ApplyBar env={env} form={settings} />}
    </div>
  )
}

// stateLine says what the environment is doing, where, and why.
function stateLine(env: Environment): { label: string; detail?: string } {
  const label = env.phase.charAt(0).toUpperCase() + env.phase.slice(1)
  if (env.worker_online === false && env.worker && env.phase !== 'stopped') {
    return { label, detail: `as ${env.worker} last said; it has not reported for a while` }
  }
  if (env.desired === 'deleted') return { label: 'Deleting' }
  if (env.phase === 'running' && env.desired !== 'running') return { label, detail: `going to be ${env.desired}` }
  if (env.phase === 'running') return { label, detail: env.worker ? `on ${env.worker}` : undefined }
  return { label, detail: env.reason }
}

// Head names the environment, says what it is doing, and holds what is
// done to it: starting, suspending and stopping, and in More the rest.
function Head({ env }: { env: Environment }) {
  const me = useMe()
  const actions = useEnvironmentActions(env, '/environments')
  const menu = useContextMenu()
  const reset = useResetToTemplate(env)
  const [error, setError] = useState('')
  const state = stateLine(env)
  const busy = actions.busy || env.desired === 'deleted'
  const running = env.phase === 'running'
  const s = env.spec

  const more = (): MenuItem[] => {
    const out: MenuItem[] = [
      {
        label: 'Open in VS Code',
        detail: 'in a new tab',
        disabled: !running,
        onSelect: () => {
          setError('')
          openInVSCode(env.id).then(setError)
        },
      },
    ]
    if (me.ssh) {
      const ssh = me.ssh
      out.push({ label: 'Copy SSH Command', onSelect: () => copyText(sshCommand(ssh, env, env.owner_id === me.id)) })
    }
    if ((env.template_changes ?? []).length > 0 && env.template_id) {
      out.push('separator')
      out.push({
        label: 'Reset to Template',
        detail: changeable(env) ? 'keeps its disks' : 'stop it first',
        disabled: !changeable(env) || reset.isPending,
        onSelect: () => reset.mutate(),
      })
    }
    out.push('separator')
    out.push({
      label: 'Delete…',
      danger: true,
      disabled: busy,
      confirm: `Delete ${env.name}? This cannot be undone.`,
      onSelect: () => actions.remove.mutate(),
    })
    return out
  }

  const problem = error || actions.error?.message || reset.error?.message
  return (
    <header className="pane-head">
      <div className="pane-head-main">
        <div className="pane-head-title">
          <h2>{env.name}</h2>
          <span className="envtab-state">
            <PhaseDot phase={env.phase} workerOnline={env.worker_online} />
            <strong>{state.label}</strong>
            {state.detail && <span className="muted">{state.detail}</span>}
          </span>
        </div>
        <div className="pane-head-sub muted">
          {env.template} · {env.owner} · {s.cpus} vCPU{s.cpus === 1 ? '' : 's'}, {formatMemory(s.memory_mib)},{' '}
          {s.display === 'desktop' ? 'desktop' : 'headless'}
          {s.gpu !== 'none' && `, ${s.gpu} GPU`}
        </div>
        {problem && <div className="action-error">{problem}</div>}
      </div>
      <div className="pane-head-actions">
        {env.desired === 'running' ? (
          <>
            {running && (
              <button
                type="button"
                className="btn btn-ghost btn-sm"
                disabled={busy}
                title="Keeps its memory, to carry on where it was"
                onClick={() => actions.suspend.mutate()}
              >
                <Pause size={13} />
                Suspend
              </button>
            )}
            <button
              type="button"
              className="btn btn-ghost btn-sm"
              disabled={busy}
              onClick={() => actions.stop.mutate()}
            >
              <Square size={12} />
              Stop
            </button>
          </>
        ) : (
          env.desired !== 'deleted' && (
            <>
              <button
                type="button"
                className="btn btn-primary btn-sm"
                disabled={busy || env.phase === 'stopping' || env.phase === 'suspending'}
                onClick={() => actions.start.mutate()}
              >
                <Play size={12} />
                {env.phase === 'suspended' ? 'Resume' : env.phase === 'failed' ? 'Start Again' : 'Start'}
              </button>
              {env.desired === 'suspended' && (
                <button
                  type="button"
                  className="btn btn-ghost btn-sm"
                  disabled={busy}
                  title="Discards its memory"
                  onClick={() => actions.stop.mutate()}
                >
                  <Square size={12} />
                  Stop
                </button>
              )}
            </>
          )
        )}
        <button
          type="button"
          className="code-tool-btn"
          aria-label="More"
          title="VS Code, SSH, Reset to Template, Delete"
          aria-haspopup="menu"
          onClick={(e) => {
            const r = e.currentTarget.getBoundingClientRect()
            menu.openAt(r.right, r.bottom + 4, more(), 'right')
          }}
        >
          <MoreHorizontal size={15} />
        </button>
      </div>
      {menu.menu}
    </header>
  )
}

function useResetToTemplate(env: Environment) {
  const qc = useQueryClient()
  return useMutation({
    mutationFn: () => api.resetEnvironmentToTemplate(env.id),
    onSettled: () => qc.invalidateQueries({ queryKey: environmentsKey }),
  })
}

const settingNames: Record<TemplateSetting, string> = {
  image: 'image',
  cpus: 'vCPUs',
  memory: 'memory',
  display: 'display',
  gpu: 'GPU',
  dax: 'image mapping',
  repos: 'repositories',
  editor_path: 'editor folder',
  access: 'access',
  trusted_folders: 'trusted folders',
  packs: 'packs',
  web_names: 'web server names',
  placement: 'placement',
  name: 'accepted names',
}

function driftNames(env: Environment): string {
  return (env.template_changes ?? []).map((c) => settingNames[c]).join(', ')
}

// Banners are what needs deciding about the environment: a newer build of
// its image, an upgrade to keep or roll back, a change waiting for its next
// start, and how it differs from its template.
function Banners({ env }: { env: Environment }) {
  const image = useImageChange(env)
  const reset = useResetToTemplate(env)
  const [hidden, setHidden] = useState<string[]>([])
  const [details, setDetails] = useState(false)
  const hide = (key: string) => () => setHidden((h) => [...h, key])
  const stopFirst = image.stopped ? undefined : 'Stop the environment first'
  const u = env.image_update
  const rb = env.image_rollback
  const { risky, clean } = imageRisk(u)
  const conflicts = u?.conflicts ?? []
  const count = conflicts.length + (u?.more_conflicts ?? 0)
  const drift = driftNames(env)
  const driftKey = `drift:${drift}`
  const out: ReactNode[] = []

  if (env.image_change) {
    out.push(
      <Banner
        key="change"
        tone="info"
        actions={
          <button type="button" className="pane-link" disabled={image.busy} onClick={() => image.cancel.mutate()}>
            Cancel
          </button>
        }
      >
        {env.image_change === 'upgrade' ? (
          <>
            Its next start upgrades <strong>{env.spec.image}</strong>
            {u && <> to {imageBuild(u.digest)}</>}, keeping a copy of its writable disk to roll back to.
          </>
        ) : (
          <>
            Its next start rolls back to {imageBuild(rb?.digest ?? '')} and the writable disk it had then, losing what
            has been written to it since the upgrade.
          </>
        )}
      </Banner>,
    )
  } else {
    if (rb) {
      out.push(
        <Banner
          key="rollback"
          tone="info"
          actions={
            <>
              <button
                type="button"
                className="pane-link"
                disabled={image.busy || !image.stopped}
                title={stopFirst ?? 'Go back to the earlier image and writable disk at its next start'}
                onClick={() => image.rollback.mutate()}
              >
                Roll Back
              </button>
              <button
                type="button"
                className="pane-link"
                disabled={image.busy}
                title="Keep the upgrade and free the copy's space"
                onClick={() => image.keep.mutate()}
              >
                Keep Upgrade
              </button>
            </>
          }
        >
          Upgraded from {imageBuild(rb.digest)} {formatAgo(rb.at)}.{' '}
          <span className="muted">
            Its writable disk from before ({formatBytes(rb.size_bytes)}) is kept, to roll back to.
          </span>
        </Banner>,
      )
    }
    if (u && !hidden.includes(`image:${u.digest}`)) {
      out.push(
        <Banner
          key="image"
          tone="warn"
          onDismiss={hide(`image:${u.digest}`)}
          actions={
            <>
              {risky && conflicts.length > 0 && (
                <button type="button" className="pane-link" onClick={() => setDetails((d) => !d)}>
                  {details ? 'Hide Files' : 'Files…'}
                </button>
              )}
              <button
                type="button"
                className={risky || u.error ? 'pane-link pane-link-danger' : 'pane-link'}
                disabled={image.busy || !image.stopped || !u.checked}
                title={
                  stopFirst ?? 'At its next start, keeping its disks and a copy of its writable disk to roll back to'
                }
                onClick={() => image.upgrade.mutate(risky || !!u.error)}
              >
                {risky || u.error ? 'Upgrade Anyway' : 'Upgrade at Next Start'}
              </button>
            </>
          }
          below={
            details &&
            risky && (
              <ul className="pane-banner-list mono small">
                {conflicts.map((c) => (
                  <li key={c}>{c}</li>
                ))}
                {(u.more_conflicts ?? 0) > 0 && <li>and {u.more_conflicts} more</li>}
              </ul>
            )
          }
        >
          <strong>A newer build of {env.spec.image} is ready</strong>{' '}
          <span className="muted">
            ({imageBuild(u.digest)}; this has {imageBuild(env.image_digest ?? '')}).{' '}
            {!u.checked &&
              (image.stopped
                ? 'Checking what upgrading would leave hidden…'
                : 'Stop it to check what upgrading would leave hidden.')}
            {u.error && <>Its writable disk could not be compared with it: {u.error}</>}
            {clean && 'Nothing it has changed hides a file the newer build changes.'}
            {risky && (
              <>
                {u.packages && 'Packages have been installed in it, whose database would hide the newer one. '}
                {count > 0 && `${count} file${count === 1 ? '' : 's'} it has changed would hide the newer build's.`}
              </>
            )}
          </span>
        </Banner>,
      )
    }
  }
  if (drift && env.template_id && !hidden.includes(driftKey)) {
    const ok = changeable(env)
    out.push(
      <Banner
        key="drift"
        tone="info"
        onDismiss={hide(driftKey)}
        actions={
          <button
            type="button"
            className="pane-link"
            disabled={!ok || reset.isPending}
            title={ok ? 'Take the template’s settings, keeping this environment’s disks' : 'Stop the environment first'}
            onClick={() => reset.mutate()}
          >
            Reset to Template
          </button>
        }
      >
        {env.template_updated ? (
          <>
            Its template, <strong>{env.template}</strong>, has changed since it was made: {drift}.
          </>
        ) : (
          <>
            Changed from its template, <strong>{env.template}</strong>: {drift}.
          </>
        )}
      </Banner>,
    )
  }
  const error = image.error ?? reset.error
  if (error) {
    out.push(
      <div key="error" className="pane-banner pane-banner-error" role="alert">
        <div className="pane-banner-row">{error.message}</div>
      </div>,
    )
  }
  return <>{out}</>
}

function Overview({ env }: { env: Environment }) {
  const me = useMe()
  const drift = driftNames(env)
  return (
    <Rows>
      <Row label="Template">
        {env.template_id ? (
          <a href={`/templates/${env.template_id}`} target={otherTab(homeTab)}>
            {env.template}
          </a>
        ) : (
          <span>
            {env.template} <span className="muted">(deleted)</span>
          </span>
        )}
        {drift && <span className="pane-note">differs in {drift}</span>}
      </Row>
      <Row label="Image">
        <span className="mono">{env.image}</span>
        {env.image_digest && (
          <span className="mono muted" title={`Boots from this copy of the image: ${env.image_digest}`}>
            @ {shortDigest(env.image_digest).replace('sha256:', '')}
          </span>
        )}
        {env.image_update && <span className="pane-pill pane-pill-warn">newer build ready</span>}
      </Row>
      <Row label="Worker">
        {env.worker ?? <span className="muted">not placed yet</span>}
        {env.stats && <span className="muted">its disk takes {formatBytes(env.stats.disk_used_bytes)} there</span>}
      </Row>
      {env.gpu_renderer && (
        <Row label="Virtual GPU">
          <span className="mono">{env.gpu_renderer}</span>
          {env.gpu_software && (
            <span className="muted" title="Performance shows how much of the worker's CPU it takes">
              rendered on the worker's CPU
            </span>
          )}
        </Row>
      )}
      <Row label="Owner">
        {env.owner}
        {env.owner_id === me.id && <span className="muted">(you)</span>}
      </Row>
      <Row label="Created">
        <span title={new Date(env.created_at).toLocaleString()}>{formatAgo(env.created_at)}</span>
        <span className="muted">{new Date(env.created_at).toLocaleString()}</span>
      </Row>
      <Row label="ID">
        <CopyField text={env.id} />
      </Row>
    </Rows>
  )
}

function Connect({ env, onDesktop }: { env: Environment; onDesktop?: () => void }) {
  const me = useMe()
  const qc = useQueryClient()
  const [error, setError] = useState('')
  const ports = useMutation({
    mutationFn: (pub: boolean) => api.setEnvironmentPorts(env.id, pub),
    onSettled: () => qc.invalidateQueries({ queryKey: environmentsKey }),
  })
  const running = env.phase === 'running'
  const notRunning = running ? undefined : 'It is not running'
  const suffix = me.environment_host_suffix
  const pub = env.ports_public ?? false

  return (
    <Rows>
      {me.ssh && (
        <Row label="SSH">
          <CopyField text={sshCommand(me.ssh, env, env.owner_id === me.id)} />
          <span className="muted small">
            with a{' '}
            <a href="/account#sign-in-keys" target={otherTab(homeTab)}>
              sign-in key
            </a>
          </span>
        </Row>
      )}
      <Row label="Open">
        <button
          type="button"
          className="btn btn-ghost btn-sm"
          disabled={!running}
          title={notRunning ?? 'In a new tab'}
          onClick={() => {
            setError('')
            openInVSCode(env.id).then(setError)
          }}
        >
          <Code2 size={13} />
          VS Code
        </button>
        {desktop && (
          <button
            type="button"
            className="btn btn-ghost btn-sm"
            disabled={!running}
            title={notRunning}
            onClick={() => desktop?.openInVSCode(env.id)}
          >
            <Code2 size={13} />
            Desktop VS Code
          </button>
        )}
        {onDesktop && env.spec.display === 'desktop' && (
          <button
            type="button"
            className="btn btn-ghost btn-sm"
            disabled={!running}
            title={notRunning}
            onClick={onDesktop}
          >
            <Monitor size={13} />
            Desktop
          </button>
        )}
        {error && <span className="action-error">{error}</span>}
      </Row>
      {suffix !== undefined && (
        <>
          <Row label="Web servers">
            <div className="envtab-web">
              <table className="pane-table">
                <tbody>
                  {['', ...(env.spec.web_names ?? [])].map((n) => {
                    const url = webURL(n, env.short_id, suffix)
                    return (
                      <tr key={n}>
                        <td className="envtab-web-name">{n || 'default'}</td>
                        <td>
                          <a className="mono" href={url} target="_blank" rel="noreferrer" title={notRunning}>
                            {url.replace(/\/$/, '')}
                          </a>
                        </td>
                        <td className="envtab-web-acts">
                          <button
                            type="button"
                            className="code-tool-btn"
                            aria-label="Copy"
                            title="Copy"
                            onClick={() => copyText(url)}
                          >
                            <Copy size={12} />
                          </button>
                          <a
                            className="code-tool-btn"
                            href={url}
                            target="_blank"
                            rel="noreferrer"
                            aria-label="Open"
                            title="Open"
                          >
                            <ExternalLink size={12} />
                          </a>
                        </td>
                      </tr>
                    )
                  })}
                </tbody>
              </table>
              <span className="muted small">
                HTTP reaches its port 80, and HTTPS its 443 whatever its certificate. Any name of up to{' '}
                {longestWebName(env.short_id, suffix)} characters and a hyphen in front reaches the same server, for one
                that tells its sites apart by host.
              </span>
            </div>
          </Row>
          <Row label="Who can reach them">
            <span className="pane-segmented" role="radiogroup" aria-label="Who can reach its web servers">
              {[false, true].map((p) => (
                <button
                  key={String(p)}
                  type="button"
                  role="radio"
                  aria-checked={pub === p}
                  className={pub === p ? 'pane-segment pane-segment-on' : 'pane-segment'}
                  disabled={ports.isPending}
                  onClick={() => pub !== p && ports.mutate(p)}
                >
                  {p ? 'Public' : 'Private'}
                </button>
              ))}
            </span>
            <span className="muted small">
              {pub
                ? 'Anyone who can reach Hangar, without signing in'
                : 'People who may use this environment, signed in to Hangar'}
            </span>
            {ports.error && <span className="action-error">{ports.error.message}</span>}
          </Row>
        </>
      )}
    </Rows>
  )
}

function fromSpec(spec: Environment['spec']): EnvironmentSettings {
  return { cpus: spec.cpus, memory_mib: spec.memory_mib, display: spec.display, gpu: spec.gpu, dax: spec.dax ?? false }
}

// settingFields are the settings the machine section changes, each with
// the word for it in a sentence and its value in words.
const settingFields: { key: keyof EnvironmentSettings; word: string; show: (s: EnvironmentSettings) => string }[] = [
  { key: 'cpus', word: 'vCPUs', show: (s) => String(s.cpus) },
  { key: 'memory_mib', word: 'memory', show: (s) => formatMemory(s.memory_mib) },
  { key: 'display', word: 'display', show: (s) => (s.display === 'desktop' ? 'desktop' : 'headless') },
  { key: 'gpu', word: 'GPU', show: (s) => s.gpu },
  { key: 'dax', word: 'image files', show: (s) => (s.dax ? 'mapped' : 'read in') },
]

type SettingsForm = ReturnType<typeof useSettingsForm>

// useSettingsForm holds the machine's settings as they are being changed,
// against those it has. Settings saved elsewhere replace the form's.
function useSettingsForm(env: Environment) {
  const saved = fromSpec(env.spec)
  const [form, setForm] = useState(saved)
  const [base, setBase] = useState(saved)
  if (settingFields.some((f) => saved[f.key] !== base[f.key])) {
    setBase(saved)
    setForm(saved)
  }
  const changes = settingFields.filter((f) => form[f.key] !== saved[f.key])
  return {
    form,
    saved,
    changes,
    set: (patch: Partial<EnvironmentSettings>) => setForm((f) => ({ ...f, ...patch })),
    reset: () => setForm(saved),
  }
}

function Machine({ env, form: f }: { env: Environment; form: SettingsForm }) {
  const ok = changeable(env)
  const changed = (key: keyof EnvironmentSettings) => f.changes.some((c) => c.key === key)
  const memories = withSize(f.form.memory_mib)
  const placement = Object.entries(env.spec.placement ?? {})

  return (
    <>
      <fieldset className="pane-fields" disabled={!ok}>
        <Rows>
          <Row label="vCPUs" modified={changed('cpus')}>
            <input
              type="number"
              className="pane-number"
              min={1}
              max={64}
              required
              aria-label="vCPUs"
              value={f.form.cpus}
              onChange={(e) => f.set({ cpus: Number(e.target.value) })}
            />
          </Row>
          <Row label="Memory" modified={changed('memory_mib')}>
            <select
              aria-label="Memory"
              value={f.form.memory_mib}
              onChange={(e) => f.set({ memory_mib: Number(e.target.value) })}
            >
              {memories.map((m) => (
                <option key={m} value={m}>
                  {formatMemory(m)}
                </option>
              ))}
            </select>
          </Row>
          <Row label="Display" modified={changed('display')}>
            {(['desktop', 'none'] as const).map((d) => (
              <label key={d} className="check">
                <input
                  type="radio"
                  name={`display-${env.id}`}
                  checked={f.form.display === d}
                  onChange={() => f.set({ display: d })}
                />
                <span>{d === 'desktop' ? 'Desktop' : 'Headless'}</span>
              </label>
            ))}
          </Row>
          <Row label="GPU" modified={changed('gpu')}>
            <select
              aria-label="GPU"
              value={f.form.gpu}
              onChange={(e) => f.set({ gpu: e.target.value as EnvironmentSettings['gpu'] })}
            >
              <option value="none">None</option>
              <option value="virtual">Virtual</option>
              {env.spec.gpu === 'passthrough' && <option value="passthrough">Passthrough</option>}
            </select>
          </Row>
          <Row label="Image files" modified={changed('dax')}>
            <label
              className="check"
              title="Their memory is shared with other environments on the image, but programs run from large files, such as a browser, start slower"
            >
              <input type="checkbox" checked={f.form.dax} onChange={(e) => f.set({ dax: e.target.checked })} />
              <span>Map them from the worker's memory (DAX)</span>
            </label>
          </Row>
          {placement.length > 0 && (
            <Row label="Placement">
              <span className="mono">{placement.map(([k, v]) => `${k}=${v}`).join(', ')}</span>
              <span className="muted small">from its template</span>
            </Row>
          )}
        </Rows>
      </fieldset>
      <p className="pane-hint muted small">
        <Info size={12} />
        {ok
          ? 'They apply when it next starts.'
          : 'Stop the environment to change these. They apply when it next starts.'}
      </p>
    </>
  )
}

// ApplyBar sums up the machine's changed settings and applies them, or
// applies them and starts it.
function ApplyBar({ env, form: f }: { env: Environment; form: SettingsForm }) {
  const qc = useQueryClient()
  const save = useMutation({
    mutationFn: async (start: boolean) => {
      await api.updateEnvironmentSettings(env.id, f.form)
      if (start) await api.startEnvironment(env.id)
    },
    onSettled: () => qc.invalidateQueries({ queryKey: environmentsKey }),
  })
  const n = f.changes.length
  const ok = changeable(env)
  return (
    <div className="pane-apply">
      <strong className="pane-modified">
        {n} change{n === 1 ? '' : 's'}
      </strong>
      <span className="pane-apply-what muted">
        {save.error ? (
          <span className="action-error">{save.error.message}</span>
        ) : (
          <>
            {f.changes.map((c) => `${c.word} ${c.show(f.saved)} → ${c.show(f.form)}`).join(', ')}. Applied when it
            starts.
          </>
        )}
      </span>
      <button type="button" className="btn btn-ghost btn-sm" disabled={save.isPending} onClick={f.reset}>
        <Undo2 size={13} />
        Reset
      </button>
      <button
        type="button"
        className="btn btn-primary btn-sm"
        disabled={save.isPending || !ok}
        onClick={() => save.mutate(false)}
      >
        Apply
      </button>
      {env.desired === 'stopped' && (
        <button
          type="button"
          className="btn btn-ghost btn-sm"
          disabled={save.isPending || !ok}
          onClick={() => save.mutate(true)}
        >
          <Play size={12} />
          Apply and Start
        </button>
      )}
    </div>
  )
}

function Repositories({ env }: { env: Environment }) {
  return (
    <table className="pane-table">
      <thead>
        <tr>
          <th>Folder</th>
          <th>Repository</th>
          <th>Branch</th>
        </tr>
      </thead>
      <tbody>
        {env.spec.repos.map((r) => (
          <tr key={r.path}>
            <td>
              <span className="mono">{r.path}</span>
              {env.spec.editor_path === r.path && (
                <span className="pane-pill" title="The folder the editor opens on">
                  opens
                </span>
              )}
            </td>
            <td>
              <span className="mono">{r.url.replace(/^https:\/\//, '').replace(/\.git$/, '')}</span>
              {r.ref && <span className="muted"> at {r.ref}</span>}
            </td>
            <td>
              {r.branch && (
                <span className="branch">
                  <GitBranch size={12} />
                  <span className="mono">{r.branch}</span>
                </span>
              )}
            </td>
          </tr>
        ))}
      </tbody>
    </table>
  )
}

function Files({ env }: { env: Environment }) {
  const me = useMe()
  const own = env.owner_id === me.id
  const { packs, conflicts } = useEnvironmentPacks(env.id)
  if (packs.isPending) return <p className="muted">Loading…</p>
  if (packs.isError) return <div className="alert">{packs.error.message}</div>
  if (packs.data.length === 0) return <p className="muted">It has no packs.</p>
  return (
    <>
      <table className="pane-table">
        <thead>
          <tr>
            <th>Pack</th>
            <th>Attached</th>
            <th>Copy</th>
          </tr>
        </thead>
        <tbody>
          {packs.data.map((a) => (
            <tr key={a.pack.id}>
              <td>
                <a href={packPath(a.pack.id, a.copy.id)} target={otherTab(homeTab)}>
                  {a.pack.name}
                </a>
              </td>
              <td className="muted">{a.listed ? 'its template lists it' : 'attaches itself'}</td>
              <td title={fromWhat[a.from]}>
                <span className="envtab-copychoice">
                  {own ? <CopyChoice env={env.id} attached={a} /> : copyName(a.copy, me.id)}
                </span>
              </td>
            </tr>
          ))}
        </tbody>
      </table>
      {conflicts.data && conflicts.data.length > 0 && (
        <Conflicts env={env.id} own={own} conflicts={conflicts.data} names={packs.data} />
      )}
    </>
  )
}

function Delete({ env }: { env: Environment }) {
  const actions = useEnvironmentActions(env, '/environments')
  const [sure, setSure] = useState(false)
  return (
    <div className="envtab-delete">
      <p className="muted">
        Deleting removes its disks, and everything on them that is not pushed or in a pack. It cannot be undone.
      </p>
      {sure ? (
        <>
          <button
            type="button"
            className="btn btn-danger btn-sm"
            disabled={actions.busy || env.desired === 'deleted'}
            onClick={() => actions.remove.mutate()}
          >
            <Trash2 size={13} />
            Delete {env.name}
          </button>
          <button type="button" className="btn btn-ghost btn-sm" onClick={() => setSure(false)}>
            Cancel
          </button>
        </>
      ) : (
        <button
          type="button"
          className="btn btn-ghost btn-sm envtab-danger"
          disabled={env.desired === 'deleted'}
          onClick={() => setSure(true)}
        >
          <Trash2 size={13} />
          Delete {env.name}…
        </button>
      )}
      {actions.remove.error && <span className="action-error">{actions.remove.error.message}</span>}
    </div>
  )
}
