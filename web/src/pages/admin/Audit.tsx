import './admin.css'

import { X } from 'lucide-react'
import { useState, type FormEvent } from 'react'
import { useSearchParams } from 'react-router'

import { Activity } from '../../components/Activity'
import { PageHeader } from '../../components/PageHeader'
import { PaneBar, PaneRoot } from '../../components/Pane'
import { usePhone } from '../../phone'

// AuditPage is the whole audit log, narrowed by the subjects in the URL:
// every link into it from an environment, a template, a worker, an image or
// a person is one of these. On a computer it fills the window, the filters
// under its bar and the events scrolling beneath them.
export function AuditPage() {
  const [params, setParams] = useSearchParams()
  const subjects = params.getAll('subject')
  const [adding, setAdding] = useState('')
  const phone = usePhone()

  const set = (next: string[]) => {
    const p = new URLSearchParams()
    for (const s of next) p.append('subject', s)
    setParams(p)
  }
  const add = (e: FormEvent) => {
    e.preventDefault()
    const s = adding.trim()
    if (s && s.includes(':') && !subjects.includes(s)) set([...subjects, s])
    setAdding('')
  }

  const filters = (
    <>
      {subjects.map((s) => (
        <span key={s} className="label mono">
          {s}
          <button
            type="button"
            className="icon-btn"
            aria-label={`Stop filtering on ${s}`}
            onClick={() => set(subjects.filter((x) => x !== s))}
          >
            <X size={12} />
          </button>
        </span>
      ))}
      <input
        className="mono grow"
        aria-label="Filter by subject"
        placeholder="Filter: environment:<id>, user:<id>, image:<ref>, worker:<id>, actor:<id>"
        value={adding}
        onChange={(e) => setAdding(e.target.value)}
      />
    </>
  )

  if (!phone) {
    return (
      <PaneRoot>
        <PaneBar title="Audit log">
          <span className="muted">
            What people did, and what Hangar and its workers decided on their own, newest first.
          </span>
        </PaneBar>
        <form className="admin-filters pane-fields" onSubmit={add}>
          {filters}
        </form>
        <div className="pane-scroll">
          <div className="admin-audit">
            <Activity subjects={subjects} />
          </div>
        </div>
      </PaneRoot>
    )
  }

  return (
    <div className="page">
      <PageHeader
        title="Audit log"
        subtitle="Everything people did, and everything Hangar and its workers decided on their own, newest first."
      />
      <form className="inline-form audit-filters" onSubmit={add}>
        {filters}
      </form>
      <Activity subjects={subjects} />
    </div>
  )
}
