import { useMutation, useQueryClient } from '@tanstack/react-query'
import { GitBranch } from 'lucide-react'
import { useState, type FormEvent } from 'react'
import { Link, useNavigate, useSearchParams } from 'react-router'

import { api, type Template } from '../api'
import { PageHeader } from '../components/PageHeader'
import { SpecChips } from '../components/SpecChips'
import { environmentsKey } from '../environments'
import { useMe } from '../session'
import { branchFor, checkName, groupTemplates, repoName, useTemplates } from '../templates'

export function NewEnvironmentPage() {
  const me = useMe()
  const qc = useQueryClient()
  const navigate = useNavigate()
  const [params, setParams] = useSearchParams()
  const templates = useTemplates()
  const [name, setName] = useState('')

  const chosenID = params.get('template')
  const chosen = templates.data?.find((t) => t.id === chosenID)
  const choose = (t: Template) => setParams({ template: t.id }, { replace: true })

  const create = useMutation({
    mutationFn: () => api.createEnvironment({ template_id: chosen!.id, name }),
    onSuccess: (env) => {
      qc.invalidateQueries({ queryKey: environmentsKey })
      navigate(`/environments/${env.id}`)
    },
  })

  const problem = chosen ? checkName(name, chosen) : null
  const submit = (e: FormEvent) => {
    e.preventDefault()
    if (chosen && !problem) create.mutate()
  }

  const { mine, collaborating, others } = groupTemplates(templates.data ?? [], me)
  const ordered = [...mine, ...collaborating, ...others]

  return (
    <div className="page page-narrow">
      <PageHeader crumbs={[{ label: 'Environments', to: '/environments' }]} title="New environment" />

      <section className="section">
        <h2 className="section-title">1. Choose a template</h2>
        {templates.isSuccess && ordered.length === 0 && (
          <div className="panel empty-state">
            There are no templates yet. <Link to="/templates/new">Make one</Link> first.
          </div>
        )}
        <div className="choices" role="radiogroup">
          {ordered.map((t) => (
            <button
              key={t.id}
              type="button"
              role="radio"
              aria-checked={t.id === chosenID}
              className={t.id === chosenID ? 'choice choice-on' : 'choice'}
              onClick={() => choose(t)}
            >
              <div className="choice-head">
                <span className="strong">{t.name}</span>
                <span className="muted small">
                  {t.owner.id === me.id ? 'yours' : `by ${t.owner.display_name || t.owner.username}`}
                </span>
              </div>
              {t.description && <p className="choice-desc">{t.description}</p>}
              <SpecChips spec={t.spec} namePattern={t.name_pattern} />
            </button>
          ))}
        </div>
      </section>

      {chosen && (
        <section className="section">
          <h2 className="section-title">2. Name it</h2>
          <form className="panel form" onSubmit={submit}>
            <label className="field">
              <span>Name</span>
              <input
                autoFocus
                required
                value={name}
                onChange={(e) => setName(e.target.value.toLowerCase())}
                placeholder={chosen.name_hint ? undefined : 'my-agent'}
                aria-invalid={problem !== null}
              />
              <small className={problem ? 'field-error' : undefined}>
                {problem ?? chosen.name_hint ?? 'It becomes the environment’s hostname.'}
              </small>
            </label>

            <Preview template={chosen} name={name} />

            {create.error && <div className="alert">{create.error.message}</div>}
            <div className="form-actions">
              <Link to="/environments" className="btn btn-ghost">
                Cancel
              </Link>
              <button type="submit" className="btn btn-primary" disabled={create.isPending || !name || problem !== null}>
                {create.isPending ? 'Creating…' : 'Create environment'}
              </button>
            </div>
          </form>
        </section>
      )}
    </div>
  )
}

// Preview shows what the environment will be, with its name filled in.
function Preview({ template: t, name }: { template: Template; name: string }) {
  return (
    <div className="preview">
      <div className="preview-row">
        <span className="muted">Image</span>
        <span className="mono">{t.spec.image}</span>
      </div>
      {t.spec.repos.map((r) => (
        <div key={r.path} className="preview-row">
          <span className="muted">Clones</span>
          <span>
            <span className="mono">{repoName(r.url)}</span> <span className="muted">into</span>{' '}
            <span className="mono">{r.path}</span>
            {r.branch && (
              <span className="branch">
                <GitBranch size={12} />
                <span className="mono">{branchFor(r.branch, name)}</span>
              </span>
            )}
          </span>
        </div>
      ))}
      {t.spec.editor_path && (
        <div className="preview-row">
          <span className="muted">Editor opens</span>
          <span className="mono">{t.spec.editor_path}</span>
        </div>
      )}
      <div className="preview-row">
        <span className="muted">Runs as</span>
        <SpecChips spec={t.spec} />
      </div>
    </div>
  )
}
