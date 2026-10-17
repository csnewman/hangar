import { Activity, BellDot, ChevronUp } from 'lucide-react'
import { useState, type ReactNode } from 'react'

import type { Environment } from '../api'
import { formatMemory, formatPercent } from '../components/format'
import { Logo } from '../components/Logo'
import { PhaseDot } from '../components/Status'
import { desktop } from '../desktop'
import { useEnvironmentActions, vscodeItems } from '../environmentActions'
import { splitByOwner, sshCommand, useEnvironments } from '../environments'
import { useMe } from '../session'
import { environmentTab, homeTab, otherTab } from '../workspaceLinks'
import type { CodeClient } from './client'
import { copyText, isMac, shortcut, useContextMenu, type MenuItem } from './ContextMenu'
import { useEditorStatus, type EditorStatusStore } from './editorStatus'
import type { Languages } from './lsp'
import { ServerChip } from './LspPanel'

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
  // onEnvironment shows its details and settings, where they are not shown
  // already.
  onEnvironment?: () => void
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
    if (onEnvironment) out.push({ label: 'Environment', detail: 'details and settings', onSelect: onEnvironment })
    out.push(...vscodeItems(env.id, running, setError))
    if (desktop) {
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
      {env.image_update && onEnvironment && (
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

// EditorItems are the status bar's account of the file being edited: where
// the caret is, which goes to a line when clicked, the file's language, and
// what knows its code.
export function EditorItems({
  store,
  client,
  languages,
}: {
  store: EditorStatusStore
  client: CodeClient
  languages: Languages
}) {
  const s = useEditorStatus(store)
  if (!s) return null
  const selection =
    s.selected > 0
      ? ` (${s.selected} ${s.selected === 1 ? 'char' : 'chars'}${s.selectedLines > 1 ? `, ${s.selectedLines} lines` : ''})`
      : ''
  return (
    <>
      <button
        type="button"
        className="code-statusbar-item"
        title={`Go to Line (${shortcut(isMac ? 'Mod+Alt+G' : 'Mod+G')})`}
        onClick={s.goto}
      >
        {`${s.line}:${s.col}${selection}`}
      </button>
      {s.language && <span className="code-statusbar-info">{s.language}</span>}
      <ServerChip server={s.server} readOnly={s.readOnly} client={client} languages={languages} />
    </>
  )
}
