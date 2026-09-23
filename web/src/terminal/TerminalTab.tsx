import { useMutation, useQuery, useQueryClient } from '@tanstack/react-query'
import type { IProgressState } from '@xterm/addon-progress'
import { Plus, SquareTerminal, X } from 'lucide-react'
import { useState } from 'react'
import { useOutletContext, useSearchParams } from 'react-router'

import { api, type Environment } from '../api'
import { TerminalPane } from './TerminalPane'

// TerminalTab is an environment's terminal sessions: a strip of them, and
// the chosen one filling the rest of the tab.
//
// The chosen session is in the URL, so a refresh re-attaches to the same
// shell and a link to it opens it in another window -- which shares it,
// keystrokes and all.
export function TerminalTab() {
  const env = useOutletContext<Environment>()
  const qc = useQueryClient()
  const [params, setParams] = useSearchParams()
  const chosen = params.get('session') ?? undefined
  // Remounting the pane is what attaches it to a different session; a new
  // session learning its ID does not, so the pane has a key of its own.
  const [pane, setPane] = useState(() => ({ key: 0, session: chosen }))
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

  return (
    <div className="term-page">
      <div className="term-sessions" role="tablist">
        {list.map((s, i) => {
          const title = titles[s.id] || s.title || `Shell ${i + 1}`
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
      <TerminalPane
        key={pane.key}
        env={env.id}
        session={attachTo}
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
    </div>
  )
}
