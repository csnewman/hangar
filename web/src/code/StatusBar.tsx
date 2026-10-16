import { Activity, BellDot, ChevronUp } from 'lucide-react'
import { useState, type ReactNode } from 'react'

import { api, type Environment } from '../api'
import { formatMemory, formatPercent } from '../components/format'
import { Logo } from '../components/Logo'
import { PhaseDot } from '../components/Status'
import { desktop } from '../desktop'
import { useEnvironmentActions } from '../environmentActions'
import { splitByOwner, sshCommand, useEnvironments } from '../environments'
import { useMe } from '../session'
import { environmentTab, homeTab, otherTab } from '../workspaceLinks'
import { copyText, useContextMenu, type MenuItem } from './ContextMenu'

// StatusBar is the foot of an environment's workspace, as an editor's is:
// the way back to Hangar's own pages, the environment and what it is doing,
// then what the workspace adds (children), and at the far end its usage
// and anything else the workspace puts there (right). The environment's
// name opens its menu, which holds what is done to it: switching to
// another, opening it elsewhere, starting, stopping and deleting it.
export function StatusBar({
  env,
  onEnvironment,
  onPerformance,
  children,
  right,
}: {
  env: Environment
  // onEnvironment shows its details and settings.
  onEnvironment: () => void
  // onPerformance shows what it is using, while that can be shown.
  onPerformance?: () => void
  children?: ReactNode
  right?: ReactNode
}) {
  const me = useMe()
  const envs = useEnvironments()
  const menu = useContextMenu()
  const actions = useEnvironmentActions(env, '/environments')
  const [error, setError] = useState('')
  const running = env.phase === 'running'
  const s = env.stats

  const openVSCode = () => {
    setError('')
    // The tab is opened in the click, which a browser allows, and sent on
    // once the editor has its ticket.
    const w = window.open('', '_blank')
    api.openEditor(env.id).then(
      (r) => {
        if (w) w.location.href = r.url
        else window.open(r.url, '_blank')
      },
      (e: Error) => {
        w?.close()
        setError(e.message)
      },
    )
  }

  const items = (): MenuItem[] => {
    const others = splitByOwner(envs.data ?? [], me).mine.filter((e) => e.id !== env.id)
    const out: MenuItem[] = []
    if (others.length > 0) {
      out.push({ heading: 'Switch to' })
      for (const e of others) {
        out.push({
          label: e.name,
          detail: e.phase,
          onSelect: () => window.open(`/environments/${e.id}`, otherTab(environmentTab(e.id))),
        })
      }
      out.push('separator')
    }
    out.push({ label: 'Hangar Home', onSelect: () => window.open('/environments', otherTab(homeTab)) })
    out.push('separator')
    out.push({ label: 'Environment', detail: 'details and settings', onSelect: onEnvironment })
    out.push({ label: 'Open in VS Code', detail: 'in a new tab', disabled: !running, onSelect: openVSCode })
    if (desktop) {
      out.push({
        label: 'Open in Desktop VS Code',
        disabled: !running,
        onSelect: () => desktop?.openInVSCode(env.id),
      })
      out.push({
        label: 'Open a Terminal Here',
        detail: 'over SSH',
        disabled: !running,
        onSelect: async () => setError((await desktop?.openTerminal(env.id)) ?? ''),
      })
    }
    if (me.ssh) {
      const ssh = me.ssh
      out.push({ label: 'Copy SSH Command', onSelect: () => copyText(sshCommand(ssh, env, env.owner_id === me.id)) })
    }
    out.push('separator')
    const busy = actions.busy || env.desired === 'deleted'
    if (env.desired === 'running') {
      out.push({
        label: 'Suspend',
        detail: 'keeps its memory',
        disabled: busy || !running,
        onSelect: () => actions.suspend.mutate(),
      })
      out.push({ label: 'Stop', disabled: busy, onSelect: () => actions.stop.mutate() })
    } else {
      out.push({
        label: env.phase === 'suspended' ? 'Resume' : 'Start',
        disabled: busy,
        onSelect: () => actions.start.mutate(),
      })
      if (env.desired === 'suspended') {
        out.push({
          label: 'Stop',
          detail: 'discards its memory',
          disabled: busy,
          onSelect: () => actions.stop.mutate(),
        })
      }
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

  const problem = error || actions.error?.message
  return (
    <footer className="code-statusbar">
      <a
        className="code-statusbar-home"
        href="/environments"
        target={otherTab(homeTab)}
        title="Hangar: every environment, templates, images and files"
        aria-label="Hangar home"
      >
        <Logo size={16} />
      </a>
      <button
        type="button"
        className="code-statusbar-item code-statusbar-env"
        title={`${env.name}: Suspend, Stop, VS Code and more`}
        aria-haspopup="menu"
        onClick={(e) => {
          const r = e.currentTarget.getBoundingClientRect()
          menu.openAt(r.left, r.top - 4, items(), 'left', true)
        }}
      >
        <PhaseDot phase={env.phase} workerOnline={env.worker_online} />
        <span className="code-statusbar-name">{env.name}</span>
        <span className="code-statusbar-phase">
          {env.phase}
          {env.desired !== 'running' && env.phase === 'running' && ` → ${env.desired}`}
        </span>
        <ChevronUp size={11} />
      </button>
      {children}
      {env.image_update && (
        <button
          type="button"
          className="code-statusbar-item code-statusbar-notice"
          title="A newer build of its image is ready"
          onClick={onEnvironment}
        >
          <BellDot size={12} />
          Image update
        </button>
      )}
      {problem && <span className="code-statusbar-error">{problem}</span>}
      <span className="code-statusbar-spacer" />
      {right}
      {running && s && (
        <button
          type="button"
          className="code-statusbar-item"
          title="What it is using: show Performance"
          onClick={onPerformance}
          disabled={!onPerformance}
        >
          <Activity size={12} />
          CPU {formatPercent(s.cpu_percent)} · {formatMemory(s.memory_used_mib)} of {formatMemory(s.memory_total_mib)}
        </button>
      )}
      {menu.menu}
    </footer>
  )
}
