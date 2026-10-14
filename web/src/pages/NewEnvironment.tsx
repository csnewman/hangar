import { useMutation, useQueryClient } from '@tanstack/react-query'
import { GitBranch } from 'lucide-react'
import { useState, type FormEvent } from 'react'
import { Link, useNavigate, useSearchParams } from 'react-router'

import { api, type Template } from '../api'
import { PageHeader } from '../components/PageHeader'
import { PhoneFoot, PhoneHead, PhoneNav, PhoneRow, PhoneSection, PhoneValue } from '../components/Phone'
import { SpecChips } from '../components/SpecChips'
import { environmentsKey } from '../environments'
import { usePhone } from '../phone'
import { useMe } from '../session'
import {
  checkName,
  fillTemplate,
  groupTemplates,
  ownerLabel,
  patternGroups,
  repoName,
  specLine,
  templateVars,
  useTemplates,
} from '../templates'
import { useTeams } from '../teams'

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

  const teams = useTeams()
  const groups = groupTemplates(templates.data ?? [], me, teams.data)
  const ordered = [...groups.mine, ...groups.teams.flatMap((g) => g.templates), ...groups.collaborating, ...groups.others]

  // On a phone, choosing and naming are two screens: the name's is the
  // template's, with step=name.
  const phone = usePhone()
  if (phone) {
    if (chosen && params.get('step') === 'name') {
      return (
        <form className="phone-screen" onSubmit={submit}>
          <PhoneNav
            back={{ to: `/environments/new?template=${chosen.id}`, label: 'templates' }}
            title="Name it"
            sub={chosen.name}
          />
          <div className="phone-screen-body">
            <label className="field">
              <span>Name</span>
              <input
                autoFocus
                required
                autoCapitalize="none"
                autoCorrect="off"
                value={name}
                onChange={(e) => setName(e.target.value)}
                placeholder={chosen.name_hint ? undefined : 'my-agent'}
                aria-invalid={problem !== null}
              />
              <small className={problem ? 'field-error' : undefined}>
                {problem ?? chosen.name_hint ?? 'It becomes the environment’s hostname, in lower case.'}
              </small>
            </label>
            <PhonePreview template={chosen} name={name} />
            {create.error && <div className="alert">{create.error.message}</div>}
          </div>
          <PhoneFoot>
            <button type="submit" className="btn btn-primary" disabled={create.isPending || !name || problem !== null}>
              {create.isPending ? 'Creating…' : 'Create environment'}
            </button>
          </PhoneFoot>
        </form>
      )
    }
    const section = (title: string, list: Template[]) =>
      list.length > 0 && (
        <PhoneSection key={title} title={title}>
          <div role="radiogroup" aria-label={title} className="phone-radios">
            {list.map((t) => (
              <button
                key={t.id}
                type="button"
                role="radio"
                aria-checked={t.id === chosenID}
                className="phone-row phone-radio"
                onClick={() => choose(t)}
              >
                <span className="phone-radio-mark" aria-hidden="true" />
                <span className="phone-row-main">
                  <span className="phone-radio-head">
                    <span className="strong">{t.name}</span>
                    <span className="phone-row-sub">{ownerLabel(t, me)}</span>
                  </span>
                  {t.description && <span className="phone-radio-desc">{t.description}</span>}
                  <span className="phone-row-sub">{specLine(t.spec, t.name_pattern)}</span>
                </span>
              </button>
            ))}
          </div>
        </PhoneSection>
      )
    return (
      <div className="phone-screen">
        <div className="phone-screen-body">
          <PhoneHead back={{ to: '/environments', label: 'Environments' }} title="New environment" />
          {templates.isSuccess && ordered.length === 0 && (
            <p className="phone-note">
              There are no templates yet. <Link to="/templates/new">Make one</Link> first.
            </p>
          )}
          {section('Yours', groups.mine)}
          {groups.teams.map((g) => section(`Team ${g.team.name}`, g.templates))}
          {section('Shared with you to edit', groups.collaborating)}
          {section(me.admin ? 'Everyone else’s' : 'Shared by others', groups.others)}
        </div>
        <PhoneFoot>
          <button
            type="button"
            className="btn btn-primary"
            disabled={!chosen}
            onClick={() => setParams({ template: chosen!.id, step: 'name' })}
          >
            Next
          </button>
        </PhoneFoot>
      </div>
    )
  }

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
                <span className="muted small">{ownerLabel(t, me)}</span>
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
                onChange={(e) => setName(e.target.value)}
                placeholder={chosen.name_hint ? undefined : 'my-agent'}
                aria-invalid={problem !== null}
              />
              <small className={problem ? 'field-error' : undefined}>
                {problem ?? chosen.name_hint ?? 'It becomes the environment’s hostname, in lower case.'}
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

// Preview shows what the environment will be, with what is known of it filled
// into its template: all but its short ID, which it is given when made.
function Preview({ template: t, name }: { template: Template; name: string }) {
  const me = useMe()
  const vars = templateVars(t, name, me.username)
  const groups = patternGroups(t.name_pattern)
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
            <span className="mono">{repoName(r.url)}</span>
            {r.ref && (
              <>
                {' '}
                <span className="muted">at</span> <span className="mono">{fillTemplate(r.ref, vars)}</span>
              </>
            )}{' '}
            <span className="muted">into</span> <span className="mono">{fillTemplate(r.path, vars)}</span>
            {r.branch && (
              <span className="branch">
                <GitBranch size={12} />
                <span className="mono">{fillTemplate(r.branch, vars)}</span>
              </span>
            )}
          </span>
        </div>
      ))}
      {t.spec.editor_path && (
        <div className="preview-row">
          <span className="muted">Editor opens</span>
          <span className="mono">{fillTemplate(t.spec.editor_path, vars)}</span>
        </div>
      )}
      {groups.length > 0 && (
        <div className="preview-row">
          <span className="muted">From the name</span>
          <span className="mono">
            {groups.map((g) => `{${g}} = ${vars[g] === undefined ? '…' : vars[g] || '(empty)'}`).join(', ')}
          </span>
        </div>
      )}
      <div className="preview-row">
        <span className="muted">Runs as</span>
        <SpecChips spec={t.spec} />
      </div>
    </div>
  )
}

// PhonePreview is Preview as a phone's list.
function PhonePreview({ template: t, name }: { template: Template; name: string }) {
  const me = useMe()
  const vars = templateVars(t, name, me.username)
  const groups = patternGroups(t.name_pattern)
  return (
    <PhoneSection title={`${t.name} gives it`}>
      <PhoneRow label="Image" sub={t.spec.image} mono />
      {t.spec.repos.map((r) => (
        <PhoneRow
          key={r.path}
          label="Clones"
          mono
          sub={
            `${repoName(r.url)}${r.ref ? ` at ${fillTemplate(r.ref, vars)}` : ''} into ${fillTemplate(r.path, vars)}` +
            (r.branch ? `, on ${fillTemplate(r.branch, vars)}` : '')
          }
        />
      ))}
      {t.spec.editor_path && <PhoneRow label="Editor opens" sub={fillTemplate(t.spec.editor_path, vars)} mono />}
      {groups.length > 0 && (
        <PhoneRow
          label="From the name"
          mono
          sub={groups.map((g) => `{${g}} = ${vars[g] === undefined ? '…' : vars[g] || '(empty)'}`).join(', ')}
        />
      )}
      <PhoneRow label="Runs as" end={<PhoneValue>{specLine(t.spec)}</PhoneValue>} />
    </PhoneSection>
  )
}
