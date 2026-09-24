import { useMutation, useQuery, useQueryClient } from '@tanstack/react-query'
import { Plus, Trash2, X } from 'lucide-react'
import { useState, type FormEvent, type ReactNode } from 'react'
import { Link, useNavigate, useParams } from 'react-router'

import { api, type Display, type GPU, type Repo, type Template, type TemplateInput } from '../api'
import { ConfirmButton } from '../components/ConfirmButton'
import { PageHeader } from '../components/PageHeader'
import { useMe } from '../session'
import { blankSpec, templatesKey } from '../templates'
import { VisibilityBadge } from './Templates'

const blank: TemplateInput = { name: '', description: '', visibility: 'private', spec: blankSpec }

// TemplateEditorPage creates a template, or shows one: editable for its
// owner and collaborators, read-only for anyone else who can see it.
export function TemplateEditorPage() {
  const { id } = useParams()
  const template = useQuery({
    queryKey: [...templatesKey, id],
    queryFn: () => api.template(id!),
    enabled: id !== undefined,
    refetchInterval: false,
  })

  if (id === undefined) return <Editor />
  if (template.isPending) return <div className="page" />
  if (template.isError) {
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
  return <Editor key={template.data.id} existing={template.data} />
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

function Editor({ existing }: { existing?: Template }) {
  const qc = useQueryClient()
  const navigate = useNavigate()
  const [form, setForm] = useState<TemplateInput>(() => (existing ? toInput(existing) : blank))
  const [placement, setPlacement] = useState<[string, string][]>(() =>
    Object.entries(existing?.spec.placement ?? {}),
  )
  const editable = existing ? existing.can_edit : true
  const manageable = existing ? existing.can_manage : true
  const spec = form.spec
  const setSpec = (patch: Partial<typeof spec>) => setForm({ ...form, spec: { ...spec, ...patch } })

  const save = useMutation({
    mutationFn: (body: TemplateInput) => (existing ? api.updateTemplate(existing.id, body) : api.createTemplate(body)),
    onSuccess: (t) => {
      qc.invalidateQueries({ queryKey: templatesKey })
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
    const body: TemplateInput = {
      ...form,
      spec: {
        ...spec,
        repos: spec.repos.map((r) => ({ ...r, ref: r.ref || undefined, branch: r.branch || undefined })),
        editor_path: spec.editor_path || undefined,
        untrusted: spec.untrusted || undefined,
        placement: Object.fromEntries(placement.filter(([k]) => k.trim() !== '').map(([k, v]) => [k.trim(), v.trim()])),
      },
      description: form.description || undefined,
      name_pattern: form.name_pattern || undefined,
      name_hint: form.name_hint || undefined,
    }
    save.mutate(body)
  }

  const setRepo = (i: number, patch: Partial<Repo>) =>
    setSpec({ repos: spec.repos.map((r, j) => (j === i ? { ...r, ...patch } : r)) })

  return (
    <div className="page page-narrow">
      <PageHeader
        crumbs={[{ label: 'Templates', to: '/templates' }]}
        title={existing ? existing.name : 'New template'}
        subtitle={
          existing && (
            <span className="subtitle-row">
              <VisibilityBadge template={existing} />
              <span>
                by {existing.owner.display_name || existing.owner.username} · used by {existing.environments}{' '}
                environment{existing.environments === 1 ? '' : 's'}
              </span>
            </span>
          )
        }
        actions={
          existing && (
            <>
              <Link to={`/environments/new?template=${existing.id}`} className="btn btn-primary">
                Use template
              </Link>
              {manageable && (
                <ConfirmButton label="Delete" confirmLabel="Delete?" onConfirm={() => remove.mutate()} />
              )}
            </>
          )
        }
      />

      {existing && !editable && (
        <div className="notice">You can use this template, but only its owner and collaborators can change it.</div>
      )}
      {remove.error && <div className="alert">{remove.error.message}</div>}

      <form onSubmit={submit}>
        <fieldset disabled={!editable} className="form-stack">
          <Section title="General">
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
                    {v === 'private' ? 'Owner and collaborators' : 'Everyone'}
                  </label>
                ))}
              </div>
              {!manageable && <small>Only the owner can change who can use it.</small>}
            </div>
          </Section>

          <Section title="Machine">
            <label className="field">
              <span>Image</span>
              <input
                required
                className="mono"
                value={spec.image}
                onChange={(e) => setSpec({ image: e.target.value })}
              />
              <small>Any OCI image built on a Hangar base.</small>
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
                A passthrough GPU pins the environment's memory and cannot be suspended. Add a placement rule below
                so it lands on a worker that has one.
              </small>
            )}
          </Section>

          <Section title="Workspace" hint="Cloned when the environment is created.">
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
              In a new branch, <code>{'{name}'}</code> becomes the environment's name.
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
            <label className="check">
              <input
                type="checkbox"
                checked={spec.untrusted ?? false}
                onChange={(e) => setSpec({ untrusted: e.target.checked })}
              />
              <span>
                Untrusted code
                <small className="muted">
                  The owner's credentials never reach it: no Claude sign-in and no SSH keys. The rest of their profile
                  still does.
                </small>
              </span>
            </label>
          </Section>

          <Section title="Naming" hint="Leave empty to allow any name.">
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

          <Section title="Placement" hint="Runs only on workers with every one of these labels.">
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
              <button type="button" className="btn btn-ghost btn-add" onClick={() => setPlacement([...placement, ['', '']])}>
                <Plus size={14} />
                Add rule
              </button>
            )}
          </Section>
        </fieldset>

        {save.error && <div className="alert">{save.error.message}</div>}
        {save.isSuccess && existing && <div className="notice notice-good">Saved.</div>}
        {editable && (
          <div className="form-actions form-footer">
            <Link to="/templates" className="btn btn-ghost">
              {existing ? 'Back' : 'Cancel'}
            </Link>
            <button type="submit" className="btn btn-primary" disabled={save.isPending}>
              {existing ? 'Save changes' : 'Create template'}
            </button>
          </div>
        )}
      </form>

      {existing && <Collaborators template={existing} />}
    </div>
  )
}

function Section({ title, hint, children }: { title: string; hint?: string; children: ReactNode }) {
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

// Collaborators can edit the template and change who else can.
function Collaborators({ template: t }: { template: Template }) {
  const me = useMe()
  const qc = useQueryClient()
  const [adding, setAdding] = useState('')
  const people = useQuery({ queryKey: ['people'], queryFn: api.people, enabled: t.can_edit, refetchInterval: false })
  const set = useMutation({
    mutationFn: (ids: string[]) => api.setCollaborators(t.id, ids),
    onSuccess: (updated) => {
      qc.setQueryData([...templatesKey, t.id], updated)
      qc.invalidateQueries({ queryKey: templatesKey })
      setAdding('')
    },
  })
  const ids = t.collaborators.map((c) => c.id)
  const candidates = (people.data ?? []).filter((p) => p.id !== t.owner.id && !ids.includes(p.id))

  return (
    <section className="panel form section">
      <div className="form-section-head">
        <h2>Collaborators</h2>
        <span className="muted small">Can edit this template and choose who else can.</span>
      </div>
      <div className="people">
        <span className="person person-owner">
          <span className="avatar avatar-sm">{initial(t.owner.display_name || t.owner.username)}</span>
          {t.owner.display_name || t.owner.username}
          <span className="muted small">owner</span>
        </span>
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
                onClick={() => set.mutate(ids.filter((x) => x !== c.id))}
              >
                <X size={13} />
              </button>
            )}
          </span>
        ))}
        {t.collaborators.length === 0 && <span className="muted small">No collaborators yet.</span>}
      </div>
      {t.can_edit && (
        <div className="inline-form">
          <select value={adding} onChange={(e) => setAdding(e.target.value)} aria-label="Add a collaborator">
            <option value="">Add a person…</option>
            {candidates.map((p) => (
              <option key={p.id} value={p.id}>
                {p.display_name ? `${p.display_name} (${p.username})` : p.username}
              </option>
            ))}
          </select>
          <button
            type="button"
            className="btn btn-ghost"
            disabled={!adding || set.isPending}
            onClick={() => set.mutate([...ids, adding])}
          >
            Add
          </button>
        </div>
      )}
      {set.error && <div className="alert">{set.error.message}</div>}
    </section>
  )
}

function initial(name: string): string {
  return name.slice(0, 1).toUpperCase()
}
