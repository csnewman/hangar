import { useMutation, useQuery, useQueryClient } from '@tanstack/react-query'
import type { IProgressState } from '@xterm/addon-progress'
import { Check, ChevronLeft, GitCompare, Layers, Plus, SquareTerminal, X } from 'lucide-react'
import { useState } from 'react'
import { Link, useOutletContext, useSearchParams } from 'react-router'

import { api, type Environment, type TerminalSession } from '../api'
import { usePhone } from '../phone'
import { TerminalPane } from './TerminalPane'
import { useChangeCount } from './useChangeCount'

// TerminalTab is an environment's terminal sessions: a strip of them, and
// the chosen one filling the rest of the tab.
//
// The chosen session is in the URL, so a refresh re-attaches to the same
// shell and a link to it opens it in another window -- which shares it,
// keystrokes and all.
export function TerminalTab() {
  const env = useOutletContext<Environment>()
  const phone = usePhone()
  const qc = useQueryClient()
  const [params, setParams] = useSearchParams()
  const chosen = params.get('session') ?? undefined
  // Remounting the pane is what attaches it to a different session; a new
  // session learning its ID does not, so the pane has a key of its own.
  // ?new opens a new session, as the New session button does.
  const [pane, setPane] = useState(() => ({ key: params.has('new') && !chosen ? 1 : 0, session: chosen }))
  const [titles, setTitles] = useState<Record<string, string>>({})
  const [progress, setProgress] = useState<IProgressState | null>(null)

  const running = env.phase === 'running'
  const key = ['terminals', env.id]
  const sessions = useQuery({ queryKey: key, queryFn: () => api.terminals(env.id), enabled: running, refetchInterval: 3000 })
  const close = useMutation({
    mutationFn: (id: string) => api.closeTerminal(env.id, id),
    onSettled: () => qc.invalidateQueries({ queryKey: key }),
  })

  const choose = (session: string | undefined) => {
    setPane((p) => ({ key: p.key + 1, session }))
    setProgress(null)
    setParams(session ? { session } : {}, { replace: true })
  }

  if (!running) {
    return (
      <div className="console">
        <div className="console-empty">
          <SquareTerminal size={36} strokeWidth={1.4} />
          <h2>Terminal</h2>
          <p className="muted small">Start the environment to open a terminal.</p>
        </div>
      </div>
    )
  }

  const list = sessions.data ?? []
  // With nothing chosen, the first session there is; with none, a new one.
  const attachTo = pane.session ?? (pane.key === 0 && !chosen ? list[0]?.id : undefined)
  if (pane.key === 0 && !chosen && sessions.isPending) {
    return <div className="term-page" />
  }
  const active = chosen ?? attachTo
  const titleOf = (s: TerminalSession, i: number) => titles[s.id] || s.title || `Shell ${i + 1}`
  const at = list.findIndex((s) => s.id === active)
  const title = at >= 0 ? titleOf(list[at], at) : 'New session'

  const terminal = (
    <TerminalPane
      key={pane.key}
      env={env.id}
      session={attachTo}
      keys={phone}
      name={at >= 0 ? title : undefined}
      events={{
        attached: (s) => {
          if (s.id !== chosen) setParams({ session: s.id }, { replace: true })
          qc.invalidateQueries({ queryKey: key })
        },
        ended: () => qc.invalidateQueries({ queryKey: key }),
        title: (t) => active && setTitles((all) => ({ ...all, [active]: t })),
        progress: setProgress,
      }}
    />
  )

  if (phone) {
    return (
      <div className="term-page">
        <PhoneBar
          env={env}
          title={title}
          windows={at >= 0 ? list[at].clients : 0}
          sessions={list.map((s, i) => ({ ...s, title: titleOf(s, i) }))}
          active={active}
          onChoose={choose}
        />
        {terminal}
      </div>
    )
  }

  return (
    <div className="term-page">
      <div className="term-sessions" role="tablist">
        {list.map((s, i) => {
          const title = titleOf(s, i)
          return (
            <div key={s.id} className={s.id === active ? 'term-tab term-tab-on' : 'term-tab'} role="tab" aria-selected={s.id === active}>
              <button type="button" className="term-tab-name" onClick={() => choose(s.id)} title={title}>
                <SquareTerminal size={13} />
                <span>{title}</span>
                {s.clients > 1 && (
                  <span className="term-tab-shared" title={`${s.clients} windows are attached`}>
                    {s.clients}
                  </span>
                )}
              </button>
              <button
                type="button"
                className="term-tab-close"
                aria-label={`End ${title}`}
                title="End this session"
                onClick={() => {
                  close.mutate(s.id)
                  if (s.id === active) choose(list.find((o) => o.id !== s.id)?.id)
                }}
              >
                <X size={12} />
              </button>
            </div>
          )
        })}
        <button type="button" className="term-new" onClick={() => choose(undefined)} title="New session" aria-label="New session">
          <Plus size={14} />
        </button>
        {progress && progress.state !== 0 && (
          <div className={`term-progress term-progress-${progress.state}`} title="Progress reported by the program">
            <div style={{ width: `${progress.state === 3 ? 100 : progress.value}%` }} />
          </div>
        )}
      </div>
      {terminal}
    </div>
  )
}

// PhoneBar is a terminal's one bar on a phone: back to the environment,
// the session and how many windows share it, the uncommitted changes --
// what the program in it has been doing, a tap from their diffs -- and the
// other sessions, chosen from a list it opens.
function PhoneBar({
  env,
  title,
  windows,
  sessions,
  active,
  onChoose,
}: {
  env: Environment
  title: string
  windows: number
  sessions: TerminalSession[]
  active: string | undefined
  onChoose: (session: string | undefined) => void
}) {
  const [open, setOpen] = useState(false)
  const changes = useChangeCount(env.id, true)
  const choose = (session: string | undefined) => {
    setOpen(false)
    if (session !== active || !session) onChoose(session)
  }
  return (
    <>
      <div className="phone-nav phone-nav-wide">
        <Link to={`/environments/${env.id}`} className="phone-icon-btn" aria-label={`Back to ${env.name}`}>
          <ChevronLeft size={24} />
        </Link>
        <div className="phone-nav-title">
          <span>{title}</span>
          <span className="phone-row-sub">
            {env.name}
            {windows > 1 && ` · ${windows} windows`}
          </span>
        </div>
        <div className="phone-nav-end">
          <Link
            to={`/environments/${env.id}/code`}
            state={{ from: title }}
            className="phone-icon-btn"
            aria-label={changes ? `Changes, ${changes} uncommitted` : 'Changes'}
            title="Changes"
          >
            <GitCompare size={20} />
            {!!changes && <span className="phone-icon-count">{changes}</span>}
          </Link>
          <button
            type="button"
            className={open ? 'phone-icon-btn phone-icon-btn-on' : 'phone-icon-btn'}
            aria-label="Switch session"
            aria-expanded={open}
            title="Switch session"
            onClick={() => setOpen((o) => !o)}
          >
            <Layers size={20} />
          </button>
        </div>
      </div>
      {open && (
        <>
          <div className="term-switch-backdrop" onClick={() => setOpen(false)} />
          <div className="phone-list term-switch" role="menu">
            {sessions.map((s) => (
              <button key={s.id} type="button" role="menuitem" className="phone-row" onClick={() => choose(s.id)}>
                <SquareTerminal size={18} />
                <span className="phone-row-main">
                  <span>{s.title}</span>
                  {s.clients > 0 && (
                    <span className="phone-row-sub">
                      {s.clients} {s.clients === 1 ? 'window' : 'windows'}
                    </span>
                  )}
                </span>
                {s.id === active && <Check size={18} className="phone-row-accent" />}
              </button>
            ))}
            <button type="button" role="menuitem" className="phone-row phone-row-accent" onClick={() => choose(undefined)}>
              <Plus size={18} />
              <span className="phone-row-main">New session</span>
            </button>
          </div>
        </>
      )}
    </>
  )
}
