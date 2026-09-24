import { X } from 'lucide-react'
import { useState, type FormEvent } from 'react'
import { useSearchParams } from 'react-router'

import { Activity } from '../../components/Activity'
import { PageHeader } from '../../components/PageHeader'

// AuditPage is the whole audit log, narrowed by the subjects in the URL:
// every link into it from an environment, a template, a worker, an image or
// a person is one of these.
export function AuditPage() {
  const [params, setParams] = useSearchParams()
  const subjects = params.getAll('subject')
  const [adding, setAdding] = useState('')

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

  return (
    <div className="page">
      <PageHeader
        title="Audit log"
        subtitle="Everything people did, and everything Hangar and its workers decided on their own, newest first."
      />
      <form className="inline-form audit-filters" onSubmit={add}>
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
          placeholder="Filter: environment:<id>, user:<id>, image:<ref>, worker:<id>, actor:<id>"
          value={adding}
          onChange={(e) => setAdding(e.target.value)}
        />
      </form>
      <Activity subjects={subjects} />
    </div>
  )
}
