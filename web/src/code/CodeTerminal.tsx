import { useQuery, useQueryClient } from '@tanstack/react-query'
import { Plus, SquareTerminal, X } from 'lucide-react'
import { useEffect, useState } from 'react'

import { api } from '../api'
import { TerminalPane } from '../terminal/TerminalPane'
import { ToolWindow } from './ToolWindow'

// CodeTerminal is the Terminal tool window: the environment's terminal
// sessions, the same ones the Terminal tab and VS Code show.
export function CodeTerminal({
  env,
  openIn,
  onHide,
}: {
  env: string
  // openIn asks for a new session in a folder; n changes with each ask.
  openIn: { dir: string; n: number } | null
  onHide: () => void
}) {
  const qc = useQueryClient()
  const key = ['terminals', env]
  const sessions = useQuery({ queryKey: key, queryFn: () => api.terminals(env), refetchInterval: 3000 })
  const [pane, setPane] = useState<{ key: number; session: string | undefined; chosen: boolean; dir?: string }>({
    key: 0,
    session: undefined,
    chosen: false,
  })
  useEffect(() => {
    if (openIn) setPane((p) => ({ key: p.key + 1, session: undefined, chosen: true, dir: openIn.dir }))
  }, [openIn])
  const [titles, setTitles] = useState<Record<string, string>>({})

  const list = sessions.data ?? []
  // Until one is chosen, the first session there is; with none, a new one.
  const attachTo = pane.chosen ? pane.session : pane.session ?? list[0]?.id
  const active = pane.session ?? attachTo
  const choose = (session: string | undefined) => setPane((p) => ({ key: p.key + 1, session, chosen: true }))

  return (
    <ToolWindow
      title="Terminal"
      extra={
        <div className="code-term-tabs">
          {list.map((s, i) => {
            const title = titles[s.id] || s.title || `Shell ${i + 1}`
            return (
              <span key={s.id} className={s.id === active ? 'code-term-tab code-term-tab-on' : 'code-term-tab'}>
                <button type="button" onClick={() => choose(s.id)} title={title}>
                  <SquareTerminal size={12} />
                  <span>{title}</span>
                </button>
                <button
                  type="button"
                  className="code-term-close"
                  title="End this session"
                  onClick={() => {
                    api.closeTerminal(env, s.id).finally(() => qc.invalidateQueries({ queryKey: key }))
                    if (s.id === active) choose(list.find((o) => o.id !== s.id)?.id)
                  }}
                >
                  <X size={11} />
                </button>
              </span>
            )
          })}
          <button type="button" className="code-tool-btn" title="New session" onClick={() => choose(undefined)}>
            <Plus size={14} />
          </button>
        </div>
      }
      onHide={onHide}
    >
      {!sessions.isPending && (
        <div className="code-term">
          <TerminalPane
            key={pane.key}
            env={env}
            session={attachTo}
            dir={pane.dir}
            events={{
              attached: (s) => {
                setPane((p) => (p.session === s.id ? p : { ...p, session: s.id }))
                qc.invalidateQueries({ queryKey: key })
              },
              ended: () => qc.invalidateQueries({ queryKey: key }),
              title: (t) => active && setTitles((all) => ({ ...all, [active]: t })),
              progress: () => {},
            }}
          />
        </div>
      )}
    </ToolWindow>
  )
}
