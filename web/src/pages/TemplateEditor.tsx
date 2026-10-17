import { useMutation, useQuery, useQueryClient } from '@tanstack/react-query'
import { Plus, Trash2, Undo2, UsersRound, X } from 'lucide-react'
import { createContext, useContext, useState, type FormEvent, type ReactNode } from 'react'
import { Link, useNavigate, useParams, useSearchParams } from 'react-router'

import { api, type Access, type Display, type GPU, type Repo, type Template, type TemplateInput } from '../api'
import { ConfirmButton } from '../components/ConfirmButton'
import { formatMemory } from '../components/format'
import { PageHeader } from '../components/PageHeader'
import {
  ApplyBar,
  DetailHead,
  DetailPage,
  NoneChosen,
  Row,
  Rows,
  Section as PaneSection,
  Segmented,
  Tile,
} from '../components/Pane'
import { PhoneFoot, PhoneHead, PhoneNav, PhoneRow, PhoneSection } from '../components/Phone'
import { usePhone } from '../phone'
import { useMe } from '../session'
import { blankSpec, builtinVariables, hangarImages, patternGroups, specLine, templatesKey } from '../templates'
import { canGive, teamsKey, useTeams } from '../teams'
import { TemplatesLayout, VisibilityBadge } from './Templates'
import { Activity } from '../components/Activity'
import { PacksField } from './Packs'

const blank: TemplateInput = { name: '', description: '', visibility: 'private', spec: blankSpec }

// TemplateEditorPage creates a template, or shows one: editable for its
// owner, its team's members and its collaborators, read-only for anyone else
// who can see it. ?team= makes a new template that team's. On a computer it
// is the chosen one beside the list of templates.
export function TemplateEditorPage() {
  const { id } = useParams()
  const phone = usePhone()
  const template = useQuery({
    queryKey: [...templatesKey, id],
    queryFn: () => api.template(id!),
    enabled: id !== undefined,
    refetchInterval: false,
  })
  const frame = (children: ReactNode) => (phone ? children : <TemplatesLayout>{children}</TemplatesLayout>)

  if (id === undefined) return frame(<Editor />)
  if (template.isPending) return phone ? <div className="page" /> : <TemplatesLayout />
  if (template.isError) {
    if (!phone) {
      return frame(
        <NoneChosen>
          This template does not exist, or it is not shared with you. <Link to="/templates">Back to templates</Link>
        </NoneChosen>,
      )
    }
    return (
      <div className="page">
        <PageHeader crumbs={[{ label: 'Templates', to: '/templates' }]} title="Not found" />
        <div className="panel empty-state">
          This template does not exist, or it is not shared with you. <Link to="/templates">Back to templates</Link>
        </div>
      </div>
    )
  }
  // Keyed so that switching templates starts the form afresh.
  return frame(<Editor key={template.data.id} existing={template.data} />)
}

function toInput(t: Template): TemplateInput {
  return {
    name: t.name,
    description: t.description,
    visibility: t.visibility,
    spec: t.spec,
    name_pattern: t.name_pattern ?? '',
    name_hint: t.name_hint ?? '',
  }
}

// toBody is the template as it is sent: empty settings left out, and the
// placement rules as labels.
function toBody(form: TemplateInput, placement: [string, string][]): TemplateInput {
  const spec = form.spec
  return {
    ...form,
    spec: {
      ...spec,
      repos: spec.repos.map((r) => ({ ...r, ref: r.ref || undefined, branch: r.branch || undefined })),
      editor_path: spec.editor_path || undefined,
      access: compactAccess(spec.access),
      web_names: (() => {
        const names = (spec.web_names ?? []).map((n) => n.trim()).filter((n) => n !== '')
        return names.length > 0 ? names : undefined
      })(),
      dax: spec.dax || undefined,
      packs: spec.packs?.length ? spec.packs : undefined,
      trusted_folders: spec.access?.no_editor_trust
        ? undefined
        : (spec.trusted_folders ?? []).map((f) => f.trim()).filter((f) => f !== '') || undefined,
      placement: Object.fromEntries(placement.filter(([k]) => k.trim() !== '').map(([k, v]) => [k.trim(), v.trim()])),
    },
    description: form.description || undefined,
    name_pattern: form.name_pattern || undefined,
    name_hint: form.name_hint || undefined,
  }
}

// stable is a value as JSON with its objects' keys in order, so two that
// differ only in the order of their keys compare equal.
const stable = (v: unknown) =>
  JSON.stringify(v, (_, x) =>
    x && typeof x === 'object' && !Array.isArray(x)
      ? Object.fromEntries(Object.entries(x).sort(([a], [b]) => a.localeCompare(b)))
      : x,
  )

// templateFields are what a change to a template is told by: each setting's
// name, and its value in words where that is short.
const templateFields: {
  key: string
  label: string
  get: (b: TemplateInput) => unknown
  show?: (b: TemplateInput) => string
}[] = [
  { key: 'name', label: 'Name', get: (b) => b.name },
  { key: 'description', label: 'Description', get: (b) => b.description },
  {
    key: 'visibility',
    label: 'Who can use it',
    get: (b) => b.visibility,
    show: (b) => (b.visibility === 'shared' ? 'everyone' : 'private'),
  },
  { key: 'image', label: 'Image', get: (b) => b.spec.image },
  { key: 'cpus', label: 'vCPUs', get: (b) => b.spec.cpus, show: (b) => String(b.spec.cpus) },
  { key: 'memory', label: 'Memory', get: (b) => b.spec.memory_mib, show: (b) => formatMemory(b.spec.memory_mib) },
  {
    key: 'display',
    label: 'Display',
    get: (b) => b.spec.display,
    show: (b) => (b.spec.display === 'desktop' ? 'desktop' : 'headless'),
  },
  { key: 'gpu', label: 'GPU', get: (b) => b.spec.gpu, show: (b) => b.spec.gpu },
  { key: 'dax', label: 'Image files', get: (b) => !!b.spec.dax },
  { key: 'repos', label: 'Repositories', get: (b) => b.spec.repos },
  { key: 'editor_path', label: 'Editor folder', get: (b) => b.spec.editor_path },
  { key: 'access', label: 'What it is given', get: (b) => b.spec.access },
  { key: 'trusted_folders', label: 'Trusted folders', get: (b) => b.spec.trusted_folders },
  { key: 'packs', label: 'Packs', get: (b) => b.spec.packs },
  { key: 'web_names', label: 'Web servers', get: (b) => b.spec.web_names },
  { key: 'name_pattern', label: 'Name pattern', get: (b) => b.name_pattern },
  { key: 'name_hint', label: 'Name hint', get: (b) => b.name_hint },
  { key: 'placement', label: 'Placement', get: (b) => b.spec.placement },
]

function Editor({ existing }: { existing?: Template }) {
  const qc = useQueryClient()
  const navigate = useNavigate()
  const me = useMe()
  const [params] = useSearchParams()
  const teams = useTeams()
  const [form, setForm] = useState<TemplateInput>(() => (existing ? toInput(existing) : blank))
  // owner is the team to own the template, '' for its user.
  const initialOwner = existing ? (existing.team?.id ?? '') : (params.get('team') ?? '')
  const [owner, setOwner] = useState(initialOwner)
  const ownerChoices = (teams.data ?? []).filter((t) => canGive(t, me.admin) || t.id === initialOwner)
  const [placement, setPlacement] = useState<[string, string][]>(() => Object.entries(existing?.spec.placement ?? {}))
  const phone = usePhone()
  const editable = existing ? existing.can_edit : true
  const manageable = existing ? existing.can_manage : true
  const spec = form.spec
  const setSpec = (patch: Partial<typeof spec>) => setForm({ ...form, spec: { ...spec, ...patch } })

  const save = useMutation({
    mutationFn: (body: TemplateInput) => (existing ? api.updateTemplate(existing.id, body) : api.createTemplate(body)),
    onSuccess: (t) => {
      if (existing) qc.setQueryData([...templatesKey, existing.id], t)
      qc.invalidateQueries({ queryKey: templatesKey })
      qc.invalidateQueries({ queryKey: teamsKey })
      if (!existing) navigate(`/templates/${t.id}`, { replace: true })
    },
  })
  const remove = useMutation({
    mutationFn: () => api.deleteTemplate(existing!.id),
    onSuccess: () => {
      qc.invalidateQueries({ queryKey: templatesKey })
      navigate('/templates')
    },
  })

  const submit = (e: FormEvent) => {
    e.preventDefault()
    save.mutate({
      ...toBody(form, placement),
      // An existing template's owner is sent only to change it.
      team_id: existing && owner === initialOwner ? undefined : owner,
    })
  }

  // What has changed from the template as it is saved.
  const placementOf = (t: Template): [string, string][] => Object.entries(t.spec.placement ?? {})
  const now = toBody(form, placement)
  const was = existing ? toBody(toInput(existing), placementOf(existing)) : now
  const changes = [
    ...(existing && owner !== initialOwner ? [{ key: 'owner', text: 'Owner' }] : []),
    ...templateFields
      .filter((f) => stable(f.get(now)) !== stable(f.get(was)))
      .map((f) => ({ key: f.key, text: f.show ? `${f.label} ${f.show(was)} → ${f.show(now)}` : f.label })),
  ]
  const changed = (key: string) => existing !== undefined && changes.some((c) => c.key === key)
  const reset = () => {
    if (!existing) return
    setForm(toInput(existing))
    setOwner(initialOwner)
    setPlacement(placementOf(existing))
    save.reset()
  }

  const setRepo = (i: number, patch: Partial<Repo>) =>
    setSpec({ repos: spec.repos.map((r, j) => (j === i ? { ...r, ...patch } : r)) })

  const sections = (
    <>
      <Section id="general" title="General">
        <label className="field">
          <span>Name</span>
          <input
            required
            maxLength={100}
            value={form.name}
            onChange={(e) => setForm({ ...form, name: e.target.value })}
            placeholder="Monorepo with desktop"
          />
        </label>
        <label className="field">
          <span>Description</span>
          <textarea
            rows={2}
            maxLength={2000}
            value={form.description ?? ''}
            onChange={(e) => setForm({ ...form, description: e.target.value })}
            placeholder="What it is for, and who should use it."
          />
        </label>
        <label className="field">
          <span>Owner</span>
          <select value={owner} disabled={!manageable} onChange={(e) => setOwner(e.target.value)}>
            <option value="">
              {existing && existing.team
                ? 'Me — take it out of the team'
                : existing && existing.owner.id !== me.id
                  ? existing.owner.display_name || existing.owner.username
                  : `Me (${me.username})`}
            </option>
            {ownerChoices.map((t) => (
              <option key={t.id} value={t.id}>
                {t.name} (team)
              </option>
            ))}
          </select>
          {owner ? (
            <small>The team's viewers use it, its members edit it, and its admins decide the rest.</small>
          ) : (
            !manageable && <small>Only the owner can give it to another.</small>
          )}
        </label>
        <div className="field">
          <span>Who can use it</span>
          <div className="segmented" role="radiogroup">
            {(['private', 'shared'] as const).map((v) => (
              <label key={v} className={form.visibility === v ? 'seg seg-on' : 'seg'}>
                <input
                  type="radio"
                  name="visibility"
                  checked={form.visibility === v}
                  disabled={!manageable}
                  onChange={() => setForm({ ...form, visibility: v })}
                />
                {v === 'private' ? (owner ? 'The team and collaborators' : 'Owner and collaborators') : 'Everyone'}
              </label>
            ))}
          </div>
          {!manageable && <small>Only the owner can change who can use it.</small>}
        </div>
      </Section>

      <Section id="machine" title="Machine">
        <label className="field">
          <span>Image</span>
          <input
            required
            className="mono"
            list="hangar-images"
            value={spec.image}
            onChange={(e) => setSpec({ image: e.target.value })}
          />
          <datalist id="hangar-images">
            {hangarImages.map((i) => (
              <option key={i.ref} value={i.ref}>
                {i.label}
              </option>
            ))}
          </datalist>
          <small>
            One of Hangar's images, or an OCI image built on one. <code>base</code> has Docker, GPU drivers, a desktop
            and a browser; <code>minimal</code> has what an environment needs and a package manager for the rest.
          </small>
        </label>
        <div className="field-row">
          <label className="field">
            <span>vCPUs</span>
            <input
              type="number"
              min={1}
              max={64}
              required
              value={spec.cpus}
              onChange={(e) => setSpec({ cpus: Number(e.target.value) })}
            />
          </label>
          <label className="field">
            <span>Memory (MiB)</span>
            <input
              type="number"
              min={512}
              max={262144}
              step={512}
              required
              value={spec.memory_mib}
              onChange={(e) => setSpec({ memory_mib: Number(e.target.value) })}
            />
          </label>
        </div>
        <div className="field-row">
          <label className="field">
            <span>Display</span>
            <select value={spec.display} onChange={(e) => setSpec({ display: e.target.value as Display })}>
              <option value="desktop">Desktop</option>
              <option value="none">Headless — lighter, no desktop</option>
            </select>
            {spec.display === 'desktop' && <small>Needs an image with a desktop, such as a base image.</small>}
          </label>
          <label className="field">
            <span>GPU</span>
            <select value={spec.gpu} onChange={(e) => setSpec({ gpu: e.target.value as GPU })}>
              <option value="none">None</option>
              <option value="virtual">Virtual — shared with other environments</option>
              <option value="passthrough">Passthrough — a whole physical GPU</option>
            </select>
          </label>
        </div>
        {spec.gpu === 'passthrough' && (
          <small className="muted">
            A passthrough GPU pins the environment's memory and cannot be suspended. Add a placement rule below so it
            lands on a worker that has one.
          </small>
        )}
        <label className="check">
          <input type="checkbox" checked={spec.dax ?? false} onChange={(e) => setSpec({ dax: e.target.checked })} />
          <span>
            Map image files from the host (DAX)
            <small className="muted">
              Shares the image's files in memory with other environments instead of each keeping a copy. Programs that
              run from large files, such as a browser, can be many times slower with it.
            </small>
          </span>
        </label>
      </Section>

      <Section id="workspace" title="Workspace" hint="Cloned when the environment is created.">
        {spec.repos.map((r, i) => (
          <div key={i} className="repo">
            <div className="field-row">
              <label className="field grow">
                <span>Repository URL</span>
                <input
                  required
                  className="mono"
                  value={r.url}
                  onChange={(e) => setRepo(i, { url: e.target.value })}
                  placeholder="https://github.com/org/repo.git"
                />
              </label>
              <button
                type="button"
                className="icon-btn repo-remove"
                title="Remove repository"
                aria-label="Remove repository"
                onClick={() => setSpec({ repos: spec.repos.filter((_, j) => j !== i) })}
              >
                <Trash2 size={15} />
              </button>
            </div>
            <div className="field-row">
              <label className="field">
                <span>Clone to</span>
                <input
                  required
                  className="mono"
                  value={r.path}
                  onChange={(e) => setRepo(i, { path: e.target.value })}
                  placeholder="/workspace/repo"
                />
              </label>
              <label className="field">
                <span>Check out</span>
                <input
                  className="mono"
                  value={r.ref ?? ''}
                  onChange={(e) => setRepo(i, { ref: e.target.value })}
                  placeholder="default branch"
                />
              </label>
              <label className="field">
                <span>New branch</span>
                <input
                  className="mono"
                  value={r.branch ?? ''}
                  onChange={(e) => setRepo(i, { branch: e.target.value })}
                  placeholder="agent/{name}"
                />
              </label>
            </div>
          </div>
        ))}
        {editable && (
          <button
            type="button"
            className="btn btn-ghost btn-add"
            onClick={() => setSpec({ repos: [...spec.repos, { url: '', path: '/workspace/' }] })}
          >
            <Plus size={14} />
            Add repository
          </button>
        )}
        <small className="muted">
          Paths, refs, new branches and the editor folder may use{' '}
          {[...builtinVariables, ...patternGroups(form.name_pattern)].map((v, i, all) => (
            <span key={v}>
              <code>{`{${v}}`}</code>
              {i < all.length - 2 ? ', ' : i === all.length - 2 ? ' and ' : ''}
            </span>
          ))}
          , filled in as each environment is made; <code>{'{name:lower}'}</code> and <code>{'{name:upper}'}</code>{' '}
          change the case. A named group in the name pattern, such as <code>{'(?P<ticket>[A-Z]+-[0-9]+)'}</code>, is a
          variable of its own.
        </small>
        <label className="field">
          <span>Open the editor in</span>
          <input
            className="mono"
            value={spec.editor_path ?? ''}
            onChange={(e) => setSpec({ editor_path: e.target.value })}
            placeholder={spec.repos[0]?.path || '/workspace'}
          />
        </label>
        <AccessField value={spec.access ?? {}} onChange={(access) => setSpec({ access })} />
        {!spec.access?.no_editor_trust && (
          <label className="field">
            <span>VS Code also trusts</span>
            <textarea
              className="mono"
              rows={2}
              value={(spec.trusted_folders ?? []).join('\n')}
              onChange={(e) => setSpec({ trusted_folders: e.target.value.split('\n') })}
              placeholder="/workspace/shared"
            />
            <small className="muted">
              One folder per line. The repositories and the editor's folder are trusted without being listed.
            </small>
          </label>
        )}
        <PacksField value={spec.packs ?? []} onChange={(packs) => setSpec({ packs })} />
        <label className="field">
          <span>Web servers</span>
          <input
            className="mono"
            value={(spec.web_names ?? []).join(', ')}
            onChange={(e) => setSpec({ web_names: e.target.value.split(/[\s,]+/) })}
            placeholder="dashboard, customer-ui"
          />
          <small className="muted">
            Names the environment's page links to, each reaching its web server at &lt;name&gt;-env&lt;id&gt; on
            Hangar's host: HTTPS to its port 443, HTTP to its port 80.
          </small>
        </label>
      </Section>

      <Section id="naming" title="Naming" hint="Leave empty to allow any name.">
        <div className="field-row">
          <label className="field">
            <span>Names must match</span>
            <input
              className="mono"
              value={form.name_pattern ?? ''}
              onChange={(e) => setForm({ ...form, name_pattern: e.target.value })}
              placeholder="proj-[0-9]+"
            />
          </label>
          <label className="field">
            <span>Hint shown to people</span>
            <input
              value={form.name_hint ?? ''}
              onChange={(e) => setForm({ ...form, name_hint: e.target.value })}
              placeholder="A ticket, like proj-123"
            />
          </label>
        </div>
        <small className="muted">A regular expression the whole name must match.</small>
      </Section>

      <Section id="placement" title="Placement" hint="Runs only on workers with every one of these labels.">
        {placement.map(([k, v], i) => (
          <div key={i} className="field-row kv">
            <input
              className="mono"
              placeholder="label"
              value={k}
              onChange={(e) => setPlacement(placement.map((p, j) => (j === i ? [e.target.value, p[1]] : p)))}
            />
            <span className="kv-eq">=</span>
            <input
              className="mono"
              placeholder="value"
              value={v}
              onChange={(e) => setPlacement(placement.map((p, j) => (j === i ? [p[0], e.target.value] : p)))}
            />
            <button
              type="button"
              className="icon-btn"
              title="Remove rule"
              aria-label="Remove rule"
              onClick={() => setPlacement(placement.filter((_, j) => j !== i))}
            >
              <Trash2 size={15} />
            </button>
          </div>
        ))}
        {editable && (
          <button
            type="button"
            className="btn btn-ghost btn-add"
            onClick={() => setPlacement([...placement, ['', '']])}
          >
            <Plus size={14} />
            Add rule
          </button>
        )}
      </Section>
    </>
  )

  // On a phone an existing template is an index of its sections, each a
  // screen of its own (?section=); a new one is every section on one screen.
  if (phone) {
    const open = existing ? params.get('section') : 'all'
    if (open) {
      return (
        <OpenSection.Provider value={open}>
          <form className="phone-screen" onSubmit={submit}>
            <PhoneNav
              back={
                existing
                  ? { to: `/templates/${existing.id}`, label: existing.name }
                  : { to: '/templates', label: 'Templates' }
              }
              title={existing ? (sectionTitles[open] ?? existing.name) : 'New template'}
              sub={existing?.name}
            />
            <div className="phone-screen-body">
              {existing && !editable && (
                <div className="notice">
                  You can use this template, but only its {existing.team ? "team's members" : 'owner'} and collaborators
                  can change it.
                </div>
              )}
              <fieldset disabled={!editable} className="form-stack">
                {sections}
              </fieldset>
              {save.error && <div className="alert">{save.error.message}</div>}
              {save.isSuccess && existing && <div className="notice notice-good">Saved.</div>}
            </div>
            {editable && (
              <PhoneFoot>
                <button type="submit" className="btn btn-primary" disabled={save.isPending}>
                  {existing ? 'Save changes' : 'Create template'}
                </button>
              </PhoneFoot>
            )}
          </form>
        </OpenSection.Provider>
      )
    }
    const t = existing!
    const placed = Object.entries(t.spec.placement ?? {})
    return (
      <div className="page phone-page">
        <PhoneHead back={{ to: '/templates', label: 'Templates' }} title={t.name}>
          <p className="phone-note">
            {t.visibility} · {t.team ? `team ${t.team.name}` : `by ${t.owner.display_name || t.owner.username}`} · used
            by {t.environments} environment{t.environments === 1 ? '' : 's'}
          </p>
        </PhoneHead>
        <Link to={`/environments/new?template=${t.id}`} className="btn btn-primary">
          Use template
        </Link>
        <PhoneSection>
          <PhoneRow to="?section=general" label="General" sub={t.description || t.name} />
          <PhoneRow to="?section=machine" label="Machine" sub={`${t.spec.image} · ${specLine(t.spec)}`} />
          <PhoneRow
            to="?section=workspace"
            label="Workspace"
            sub={[
              t.spec.repos.length === 0
                ? 'No repositories'
                : t.spec.repos.length === 1
                  ? t.spec.repos[0].url
                  : `${t.spec.repos.length} repositories`,
              t.spec.packs?.length ? `${t.spec.packs.length} packs` : '',
            ]
              .filter(Boolean)
              .join(' · ')}
          />
          <PhoneRow to="?section=naming" label="Naming" sub={t.name_pattern || 'Any name'} />
          <PhoneRow
            to="?section=placement"
            label="Placement"
            sub={placed.length ? placed.map(([k, v]) => `${k}=${v}`).join(', ') : 'Any worker'}
          />
        </PhoneSection>
        <Collaborators template={t} />
        <section className="phone-section">
          <h2 className="phone-heading">Activity</h2>
          <Activity subjects={[`template:${t.id}`]} pageSize={5} />
        </section>
        {manageable && (
          <ConfirmButton label="Delete template" confirmLabel="Delete it?" onConfirm={() => remove.mutate()} />
        )}
        {remove.error && <div className="alert">{remove.error.message}</div>}
      </div>
    )
  }

  const hint = (text: ReactNode) => <small className="tpl-hint">{text}</small>
  const label = (text: string, id: string) => <label htmlFor={id}>{text}</label>
  const variables = [...builtinVariables, ...patternGroups(form.name_pattern)]

  return (
    <form className="tpl-form" onSubmit={submit}>
      <DetailHead
        mark={<Tile name={existing?.name || form.name || 'New template'} />}
        title={existing ? existing.name : 'New template'}
        sub={
          existing ? (
            <>
              <VisibilityBadge template={existing} />
              <span>
                {existing.team ? (
                  <>
                    team <Link to={`/teams/${existing.team.id}`}>{existing.team.name}</Link>
                  </>
                ) : (
                  <>by {existing.owner.display_name || existing.owner.username}</>
                )}
              </span>
              <span>
                · used by {existing.environments} environment{existing.environments === 1 ? '' : 's'}
              </span>
            </>
          ) : (
            <span>Settings every environment made from it starts with.</span>
          )
        }
        actions={
          existing && (
            <>
              <Link to={`/environments/new?template=${existing.id}`} className="btn btn-primary btn-sm">
                <Plus size={14} />
                New Environment
              </Link>
              {manageable && (
                <ConfirmButton label="Delete" confirmLabel="Delete it?" onConfirm={() => remove.mutate()} />
              )}
            </>
          )
        }
      />
      <DetailPage>
        {existing && !editable && (
          <div className="notice">
            You can use this template, but only its {existing.team ? "team's members" : 'owner'} and collaborators can
            change it.
          </div>
        )}
        {remove.error && <div className="alert">{remove.error.message}</div>}
        <fieldset disabled={!editable} className="pane-fields tpl-fields">
          <PaneSection title="General">
            <Rows>
              <Row label={label('Name', 'tpl-name')} modified={changed('name')}>
                <input
                  id="tpl-name"
                  className="pane-wide"
                  required
                  maxLength={100}
                  value={form.name}
                  onChange={(e) => setForm({ ...form, name: e.target.value })}
                  placeholder="Monorepo with desktop"
                />
              </Row>
              <Row label={label('Description', 'tpl-description')} modified={changed('description')}>
                <textarea
                  id="tpl-description"
                  className="pane-wide"
                  rows={2}
                  maxLength={2000}
                  value={form.description ?? ''}
                  onChange={(e) => setForm({ ...form, description: e.target.value })}
                  placeholder="What it is for, and who should use it."
                />
              </Row>
              <Row label={label('Owner', 'tpl-owner')} modified={changed('owner')}>
                <select id="tpl-owner" value={owner} disabled={!manageable} onChange={(e) => setOwner(e.target.value)}>
                  <option value="">
                    {existing && existing.team
                      ? 'Me — take it out of the team'
                      : existing && existing.owner.id !== me.id
                        ? existing.owner.display_name || existing.owner.username
                        : `Me (${me.username})`}
                  </option>
                  {ownerChoices.map((t) => (
                    <option key={t.id} value={t.id}>
                      {t.name} (team)
                    </option>
                  ))}
                </select>
                {owner
                  ? hint("The team's viewers use it, its members edit it, and its admins decide the rest.")
                  : !manageable && hint('Only the owner can give it to another.')}
              </Row>
              <Row label="Who can use it" modified={changed('visibility')}>
                <Segmented
                  label="Who can use it"
                  value={form.visibility}
                  disabled={!manageable}
                  options={[
                    { value: 'private', label: owner ? 'The team and collaborators' : 'Owner and collaborators' },
                    { value: 'shared', label: 'Everyone' },
                  ]}
                  onChange={(v) => setForm({ ...form, visibility: v })}
                />
                {!manageable && hint('Only the owner can change who can use it.')}
              </Row>
            </Rows>
          </PaneSection>

          <PaneSection title="Machine">
            <Rows>
              <Row label={label('Image', 'tpl-image')} modified={changed('image')}>
                <input
                  id="tpl-image"
                  required
                  className="mono pane-wide"
                  list="hangar-images"
                  value={spec.image}
                  onChange={(e) => setSpec({ image: e.target.value })}
                />
                <datalist id="hangar-images">
                  {hangarImages.map((i) => (
                    <option key={i.ref} value={i.ref}>
                      {i.label}
                    </option>
                  ))}
                </datalist>
                {hint(
                  <>
                    One of Hangar's images, or an OCI image built on one. <code>base</code> has Docker, GPU drivers, a
                    desktop and a browser; <code>minimal</code> has what an environment needs and a package manager for
                    the rest.
                  </>,
                )}
              </Row>
              <Row label={label('vCPUs', 'tpl-cpus')} modified={changed('cpus')}>
                <input
                  id="tpl-cpus"
                  type="number"
                  className="pane-number"
                  min={1}
                  max={64}
                  required
                  value={spec.cpus}
                  onChange={(e) => setSpec({ cpus: Number(e.target.value) })}
                />
              </Row>
              <Row label={label('Memory', 'tpl-memory')} modified={changed('memory')}>
                <input
                  id="tpl-memory"
                  type="number"
                  className="tpl-memory"
                  min={512}
                  max={262144}
                  step={512}
                  required
                  value={spec.memory_mib}
                  onChange={(e) => setSpec({ memory_mib: Number(e.target.value) })}
                />
                <span className="muted">MiB{spec.memory_mib >= 1024 && `, ${formatMemory(spec.memory_mib)}`}</span>
              </Row>
              <Row label={label('Display', 'tpl-display')} modified={changed('display')}>
                <select
                  id="tpl-display"
                  value={spec.display}
                  onChange={(e) => setSpec({ display: e.target.value as Display })}
                >
                  <option value="desktop">Desktop</option>
                  <option value="none">Headless — lighter, no desktop</option>
                </select>
                {spec.display === 'desktop' && hint('Needs an image with a desktop, such as a base image.')}
              </Row>
              <Row label={label('GPU', 'tpl-gpu')} modified={changed('gpu')}>
                <select id="tpl-gpu" value={spec.gpu} onChange={(e) => setSpec({ gpu: e.target.value as GPU })}>
                  <option value="none">None</option>
                  <option value="virtual">Virtual — shared with other environments</option>
                  <option value="passthrough">Passthrough — a whole physical GPU</option>
                </select>
                {spec.gpu === 'passthrough' &&
                  hint(
                    'A passthrough GPU pins the environment’s memory and cannot be suspended. Add a placement rule below so it lands on a worker that has one.',
                  )}
              </Row>
              <Row label="Image files" modified={changed('dax')}>
                <label className="check">
                  <input
                    type="checkbox"
                    checked={spec.dax ?? false}
                    onChange={(e) => setSpec({ dax: e.target.checked })}
                  />
                  <span>Map them from the host (DAX)</span>
                </label>
                {hint(
                  'Shares the image’s files in memory with other environments rather than each keeping a copy. Programs that run from large files, such as a browser, can be many times slower with it.',
                )}
              </Row>
            </Rows>
          </PaneSection>

          <PaneSection title="Workspace" extra={<span className="muted">Cloned when the environment is created.</span>}>
            <Rows>
              <Row label="Repositories" modified={changed('repos')}>
                <div className="tpl-block">
                  {spec.repos.length > 0 && (
                    <table className="pane-table tpl-repos">
                      <thead>
                        <tr>
                          <th>Repository URL</th>
                          <th>Clone to</th>
                          <th>Check out</th>
                          <th>New branch</th>
                          <th aria-label="Remove" />
                        </tr>
                      </thead>
                      <tbody>
                        {spec.repos.map((r, i) => (
                          <tr key={i}>
                            <td>
                              <input
                                required
                                aria-label="Repository URL"
                                className="mono"
                                value={r.url}
                                onChange={(e) => setRepo(i, { url: e.target.value })}
                                placeholder="https://github.com/org/repo.git"
                              />
                            </td>
                            <td>
                              <input
                                required
                                aria-label="Clone to"
                                className="mono"
                                value={r.path}
                                onChange={(e) => setRepo(i, { path: e.target.value })}
                                placeholder="/workspace/repo"
                              />
                            </td>
                            <td>
                              <input
                                aria-label="Check out"
                                className="mono"
                                value={r.ref ?? ''}
                                onChange={(e) => setRepo(i, { ref: e.target.value })}
                                placeholder="default branch"
                              />
                            </td>
                            <td>
                              <input
                                aria-label="New branch"
                                className="mono"
                                value={r.branch ?? ''}
                                onChange={(e) => setRepo(i, { branch: e.target.value })}
                                placeholder="agent/{name}"
                              />
                            </td>
                            <td>
                              <button
                                type="button"
                                className="pane-icon-btn"
                                title="Remove repository"
                                aria-label="Remove repository"
                                onClick={() => setSpec({ repos: spec.repos.filter((_, j) => j !== i) })}
                              >
                                <Trash2 size={14} />
                              </button>
                            </td>
                          </tr>
                        ))}
                      </tbody>
                    </table>
                  )}
                  {editable && (
                    <button
                      type="button"
                      className="btn btn-ghost btn-sm"
                      onClick={() => setSpec({ repos: [...spec.repos, { url: '', path: '/workspace/' }] })}
                    >
                      <Plus size={14} />
                      Add Repository
                    </button>
                  )}
                  {hint(
                    <>
                      Paths, refs, new branches and the editor folder may use{' '}
                      {variables.map((v, i) => (
                        <span key={v}>
                          <code>{`{${v}}`}</code>
                          {i < variables.length - 2 ? ', ' : i === variables.length - 2 ? ' and ' : ''}
                        </span>
                      ))}
                      , filled in as each environment is made; <code>{'{name:lower}'}</code> and{' '}
                      <code>{'{name:upper}'}</code> change the case. A named group in the name pattern, such as{' '}
                      <code>{'(?P<ticket>[A-Z]+-[0-9]+)'}</code>, is a variable of its own.
                    </>,
                  )}
                </div>
              </Row>
              <Row label={label('The editor opens', 'tpl-editor')} modified={changed('editor_path')}>
                <input
                  id="tpl-editor"
                  className="mono pane-wide"
                  value={spec.editor_path ?? ''}
                  onChange={(e) => setSpec({ editor_path: e.target.value })}
                  placeholder={spec.repos[0]?.path || '/workspace'}
                />
              </Row>
              <Row label="Given of its owner's" modified={changed('access')}>
                <div className="tpl-block">
                  <AccessChecks value={spec.access ?? {}} onChange={(access) => setSpec({ access })} />
                </div>
              </Row>
              {!spec.access?.no_editor_trust && (
                <Row label={label('VS Code also trusts', 'tpl-trusted')} modified={changed('trusted_folders')}>
                  <textarea
                    id="tpl-trusted"
                    className="mono pane-wide"
                    rows={2}
                    value={(spec.trusted_folders ?? []).join('\n')}
                    onChange={(e) => setSpec({ trusted_folders: e.target.value.split('\n') })}
                    placeholder="/workspace/shared"
                  />
                  {hint(
                    'One folder per line. The repositories and the editor’s folder are trusted without being listed.',
                  )}
                </Row>
              )}
              <Row label="Packs" modified={changed('packs')}>
                <div className="tpl-block tpl-packs">
                  <PacksField value={spec.packs ?? []} onChange={(packs) => setSpec({ packs })} />
                </div>
              </Row>
              <Row label={label('Web servers', 'tpl-web')} modified={changed('web_names')}>
                <input
                  id="tpl-web"
                  className="mono pane-wide"
                  value={(spec.web_names ?? []).join(', ')}
                  onChange={(e) => setSpec({ web_names: e.target.value.split(/[\s,]+/) })}
                  placeholder="dashboard, customer-ui"
                />
                {hint(
                  <>
                    Names the environment's page links to, each reaching its web server at &lt;name&gt;-env&lt;id&gt; on
                    Hangar's host: HTTPS to its port 443, HTTP to its port 80.
                  </>,
                )}
              </Row>
            </Rows>
          </PaneSection>

          <PaneSection title="Naming" extra={<span className="muted">Leave empty to allow any name.</span>}>
            <Rows>
              <Row label={label('Names must match', 'tpl-pattern')} modified={changed('name_pattern')}>
                <input
                  id="tpl-pattern"
                  className="mono pane-wide"
                  value={form.name_pattern ?? ''}
                  onChange={(e) => setForm({ ...form, name_pattern: e.target.value })}
                  placeholder="proj-[0-9]+"
                />
                {hint('A regular expression the whole name must match.')}
              </Row>
              <Row label={label('Hint shown to people', 'tpl-hint')} modified={changed('name_hint')}>
                <input
                  id="tpl-hint"
                  className="pane-wide"
                  value={form.name_hint ?? ''}
                  onChange={(e) => setForm({ ...form, name_hint: e.target.value })}
                  placeholder="A ticket, like proj-123"
                />
              </Row>
            </Rows>
          </PaneSection>

          <PaneSection title="Placement">
            <Rows>
              <Row label="Worker labels" modified={changed('placement')}>
                <div className="tpl-block">
                  {placement.map(([k, v], i) => (
                    <div key={i} className="tpl-kv">
                      <input
                        className="mono"
                        aria-label="Label"
                        placeholder="label"
                        value={k}
                        onChange={(e) => setPlacement(placement.map((p, j) => (j === i ? [e.target.value, p[1]] : p)))}
                      />
                      <span className="kv-eq">=</span>
                      <input
                        className="mono"
                        aria-label="Value"
                        placeholder="value"
                        value={v}
                        onChange={(e) => setPlacement(placement.map((p, j) => (j === i ? [p[0], e.target.value] : p)))}
                      />
                      <button
                        type="button"
                        className="pane-icon-btn"
                        title="Remove rule"
                        aria-label="Remove rule"
                        onClick={() => setPlacement(placement.filter((_, j) => j !== i))}
                      >
                        <Trash2 size={14} />
                      </button>
                    </div>
                  ))}
                  {editable && (
                    <button
                      type="button"
                      className="btn btn-ghost btn-sm"
                      onClick={() => setPlacement([...placement, ['', '']])}
                    >
                      <Plus size={14} />
                      Add Rule
                    </button>
                  )}
                  {hint(
                    placement.length > 0
                      ? 'It runs only on workers with every one of these labels.'
                      : 'It runs on any worker.',
                  )}
                </div>
              </Row>
            </Rows>
          </PaneSection>
        </fieldset>

        {existing && (
          <PaneSection
            title="Collaborators"
            extra={<span className="muted">People edit it; a team's viewers use it and its members edit it.</span>}
          >
            <Collaborators template={existing} bare />
          </PaneSection>
        )}
        {existing && (
          <PaneSection title="Activity">
            <Activity subjects={[`template:${existing.id}`]} pageSize={10} />
          </PaneSection>
        )}
      </DetailPage>
      {!existing ? (
        <div className="pane-apply">
          <strong className="pane-modified">New template</strong>
          <span className="pane-apply-what">
            {save.error ? (
              <span className="action-error">{save.error.message}</span>
            ) : (
              'Fill in its settings, then create it.'
            )}
          </span>
          <Link to="/templates" className="btn btn-ghost btn-sm">
            Cancel
          </Link>
          <button type="submit" className="btn btn-primary btn-sm" disabled={save.isPending}>
            {save.isPending ? 'Creating…' : 'Create'}
          </button>
        </div>
      ) : (
        editable &&
        changes.length > 0 && (
          <ApplyBar
            count={changes.length}
            actions={
              <>
                <button type="button" className="btn btn-ghost btn-sm" disabled={save.isPending} onClick={reset}>
                  <Undo2 size={13} />
                  Reset
                </button>
                <button type="submit" className="btn btn-primary btn-sm" disabled={save.isPending}>
                  Apply
                </button>
              </>
            }
          >
            {save.error ? (
              <span className="action-error">{save.error.message}</span>
            ) : (
              <>
                {changes.map((c) => c.text).join(', ')}. Environments made from it after Apply get it; those made before
                say they differ.
              </>
            )}
          </ApplyBar>
        )
      )}
    </form>
  )
}

// compactAccess is access as a template sends it: only what it keeps from
// the environment, or nothing.
function compactAccess(access: Access | undefined): Access | undefined {
  const out = Object.fromEntries(Object.entries(access ?? {}).filter(([, v]) => v)) as Access
  return Object.keys(out).length > 0 ? out : undefined
}

// What of its owner's an environment is given, each a setting of its own.
const accessSettings: { key: keyof Access; label: string; hint: string }[] = [
  {
    key: 'no_self_attached',
    label: 'Self-attaching packs',
    hint: "Packs that reach environments by themselves: the profile, and the owner's and their teams'. The packs listed below arrive either way.",
  },
  {
    key: 'no_sensitive_files',
    label: 'Sensitive files',
    hint: "Every file under a pack's sensitive paths: Claude's sign-in, git and registry logins, and the like.",
  },
  { key: 'no_ssh_keys', label: 'SSH keys', hint: "Signatures from the owner's SSH keys, for git and ssh." },
  { key: 'no_registry', label: 'Registry sign-in', hint: "Docker signs in to Hangar's registry as the owner." },
  {
    key: 'no_editor_trust',
    label: 'Editor trust',
    hint: 'VS Code trusts the repositories and the folders below; without it, it opens everything in Restricted Mode.',
  },
]

// untrusted is the preset for code the owner does not trust: their files,
// but none of their credentials, and an editor that trusts nothing.
const untrusted: Access = { no_sensitive_files: true, no_ssh_keys: true, no_registry: true, no_editor_trust: true }

function AccessField({ value, onChange }: { value: Access; onChange: (a: Access) => void }) {
  return (
    <div className="field">
      <span>What it is given of its owner's</span>
      <AccessChecks value={value} onChange={onChange} />
    </div>
  )
}

// AccessChecks are what an environment is given of its owner's, each a
// box to tick, and the preset for untrusted code.
function AccessChecks({ value, onChange }: { value: Access; onChange: (a: Access) => void }) {
  const isUntrusted = accessSettings.every(({ key }) => !!value[key] === !!untrusted[key])
  return (
    <>
      {accessSettings.map(({ key, label, hint }) => (
        <label key={key} className="check">
          <input
            type="checkbox"
            checked={!value[key]}
            onChange={(e) => onChange({ ...value, [key]: !e.target.checked })}
          />
          <span>
            {label}
            <small className="muted">{hint}</small>
          </span>
        </label>
      ))}
      <div>
        <button type="button" className="btn btn-ghost" disabled={isUntrusted} onClick={() => onChange(untrusted)}>
          Untrusted code
        </button>
        <small className="muted"> Their files, but none of their credentials, and an editor that trusts nothing.</small>
      </div>
    </>
  )
}

// OpenSection is the section a phone shows: one by its id, or 'all'. Off
// a phone it is null, and every section shows.
const OpenSection = createContext<string | null>(null)

const sectionTitles: Record<string, string> = {
  general: 'General',
  machine: 'Machine',
  workspace: 'Workspace',
  naming: 'Naming',
  placement: 'Placement',
}

function Section({ id, title, hint, children }: { id: string; title: string; hint?: string; children: ReactNode }) {
  const open = useContext(OpenSection)
  if (open && open !== 'all' && open !== id) return null
  if (open && open !== 'all') {
    return (
      <section className="form-stack">
        {hint && <p className="phone-note">{hint}</p>}
        {children}
      </section>
    )
  }
  return (
    <section className="panel form">
      <div className="form-section-head">
        <h2>{title}</h2>
        {hint && <span className="muted small">{hint}</span>}
      </div>
      {children}
    </section>
  )
}

// Collaborators are people, who edit the template and choose who else can,
// and teams, whose viewers use it and whose members edit it.
// bare leaves out its panel and heading, for a section that has its own.
function Collaborators({ template: t, bare = false }: { template: Template; bare?: boolean }) {
  const me = useMe()
  const qc = useQueryClient()
  const [adding, setAdding] = useState('')
  const people = useQuery({ queryKey: ['people'], queryFn: api.people, enabled: t.can_edit, refetchInterval: false })
  const teams = useTeams()
  const set = useMutation({
    mutationFn: ({ users, teams }: { users: string[]; teams: string[] }) => api.setCollaborators(t.id, users, teams),
    onSuccess: (updated) => {
      qc.setQueryData([...templatesKey, t.id], updated)
      qc.invalidateQueries({ queryKey: templatesKey })
      setAdding('')
    },
  })
  const ids = t.collaborators.map((c) => c.id)
  const teamIDs = t.collaborator_teams.map((c) => c.id)
  const candidates = (people.data ?? []).filter((p) => p.id !== t.owner.id && !ids.includes(p.id))
  const teamCandidates = (teams.data ?? []).filter((x) => x.id !== t.team?.id && !teamIDs.includes(x.id))
  const add = () => {
    const [kind, id] = adding.split(':')
    if (kind === 'team') set.mutate({ users: ids, teams: [...teamIDs, id] })
    else set.mutate({ users: [...ids, id], teams: teamIDs })
  }

  const body = (
    <>
      <div className="people">
        {t.team ? (
          <span className="person person-owner">
            <UsersRound size={14} />
            {t.team.name}
            <span className="muted small">owner</span>
          </span>
        ) : (
          <span className="person person-owner">
            <span className="avatar avatar-sm">{initial(t.owner.display_name || t.owner.username)}</span>
            {t.owner.display_name || t.owner.username}
            <span className="muted small">owner</span>
          </span>
        )}
        {t.collaborator_teams.map((c) => (
          <span key={c.id} className="person">
            <UsersRound size={14} />
            <Link to={`/teams/${c.id}`}>{c.name}</Link>
            {t.can_edit && (
              <button
                type="button"
                className="person-remove"
                title="Remove"
                aria-label={`Remove team ${c.slug}`}
                disabled={set.isPending}
                onClick={() => set.mutate({ users: ids, teams: teamIDs.filter((x) => x !== c.id) })}
              >
                <X size={13} />
              </button>
            )}
          </span>
        ))}
        {t.collaborators.map((c) => (
          <span key={c.id} className="person">
            <span className="avatar avatar-sm">{initial(c.display_name || c.username)}</span>
            {c.display_name || c.username}
            {c.id === me.id && <span className="muted small">you</span>}
            {t.can_edit && (
              <button
                type="button"
                className="person-remove"
                title={c.id === me.id ? 'Leave' : 'Remove'}
                aria-label={`Remove ${c.username}`}
                disabled={set.isPending}
                onClick={() => set.mutate({ users: ids.filter((x) => x !== c.id), teams: teamIDs })}
              >
                <X size={13} />
              </button>
            )}
          </span>
        ))}
        {t.collaborators.length === 0 && t.collaborator_teams.length === 0 && (
          <span className="muted small">No collaborators yet.</span>
        )}
      </div>
      {t.can_edit && (
        <div className="inline-form">
          <select value={adding} onChange={(e) => setAdding(e.target.value)} aria-label="Add a collaborator">
            <option value="">Add a person or team…</option>
            {teamCandidates.length > 0 && (
              <optgroup label="Teams">
                {teamCandidates.map((x) => (
                  <option key={x.id} value={`team:${x.id}`}>
                    {x.name}
                  </option>
                ))}
              </optgroup>
            )}
            <optgroup label="People">
              {candidates.map((p) => (
                <option key={p.id} value={`user:${p.id}`}>
                  {p.display_name ? `${p.display_name} (${p.username})` : p.username}
                </option>
              ))}
            </optgroup>
          </select>
          <button type="button" className="btn btn-ghost" disabled={!adding || set.isPending} onClick={add}>
            Add
          </button>
        </div>
      )}
      {set.error && <div className="alert">{set.error.message}</div>}
    </>
  )
  if (bare) return <div className="tpl-collaborators">{body}</div>
  return (
    <section className="panel form section">
      <div className="form-section-head">
        <h2>Collaborators</h2>
        <span className="muted small">People edit it; a team's viewers use it and its members edit it.</span>
      </div>
      {body}
    </section>
  )
}

function initial(name: string): string {
  return name.slice(0, 1).toUpperCase()
}
