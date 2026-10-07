import { useMutation, useQuery, useQueryClient } from '@tanstack/react-query'
import { Plus } from 'lucide-react'
import { useMemo, useState, type FormEvent } from 'react'
import { Link, useNavigate, useParams } from 'react-router'

import { api, type FilePack, type FilePackInput } from '../api'
import { ConfirmButton } from '../components/ConfirmButton'
import { FileBrowser, type FileSource } from '../components/FileBrowser'
import { PageHeader } from '../components/PageHeader'
import { useMe } from '../session'
import { canGive, useTeams } from '../teams'

export const packsKey = ['packs'] as const

export function usePacks() {
  return useQuery({
    queryKey: packsKey,
    queryFn: api.packs,
    refetchInterval: 30_000,
  })
}

// ownerOf names who owns a pack: a team, or a person.
export function ownerOf(k: FilePack): string {
  return k.team ?? k.owner ?? ''
}

// pathsOf reads paths written one to a line.
function pathsOf(text: string): string[] {
  return text
    .split('\n')
    .map((p) => p.trim())
    .filter((p) => p !== '')
}

// PacksPage lists the file packs the caller may use. A pack is files at
// absolute paths that templates give their environments, beside each
// owner's profile.
export function PacksPage() {
  const [creating, setCreating] = useState(false)
  const packs = usePacks()
  const list = packs.data ?? []

  return (
    <div className="page">
      <PageHeader
        title="File packs"
        subtitle="Files at fixed paths, such as a project's .env, that the templates listing a pack give their environments, kept in step as they change there or here."
        actions={
          !creating && (
            <button type="button" className="btn btn-primary" onClick={() => setCreating(true)}>
              <Plus size={15} />
              New pack
            </button>
          )
        }
      />
      {creating && <CreateForm onDone={() => setCreating(false)} />}
      {packs.isError && <div className="alert">Could not load packs: {packs.error.message}</div>}
      <section className="section">
        {!packs.isPending && list.length === 0 && (
          <div className="panel empty-state">
            No packs. Make one for files a project's environments need that are not in its repository.
          </div>
        )}
        {list.length > 0 && (
          <div className="panel">
            <table className="table">
              <thead>
                <tr>
                  <th>Pack</th>
                  <th>Owner</th>
                  <th>Copies</th>
                  <th>Paths</th>
                </tr>
              </thead>
              <tbody>
                {list.map((k) => (
                  <tr key={k.id}>
                    <td>
                      <Link to={`/packs/${k.id}`} className="strong">
                        {k.name}
                      </Link>
                      {k.description && <div className="muted small">{k.description}</div>}
                    </td>
                    <td>{ownerOf(k)}</td>
                    <td>{k.personal ? 'One per person' : 'One, shared'}</td>
                    <td className="mono small">
                      {k.paths.slice(0, 3).map((p) => (
                        <div key={p}>{p}</div>
                      ))}
                      {k.paths.length > 3 && <div className="muted">and {k.paths.length - 3} more</div>}
                    </td>
                  </tr>
                ))}
              </tbody>
            </table>
          </div>
        )}
      </section>
    </div>
  )
}

function CreateForm({ onDone }: { onDone: () => void }) {
  const me = useMe()
  const qc = useQueryClient()
  const navigate = useNavigate()
  const teams = useTeams()
  const owners = (teams.data ?? []).filter((t) => canGive(t, me.admin))
  const [name, setName] = useState('')
  const [team, setTeam] = useState('')
  const [personal, setPersonal] = useState(false)
  const [paths, setPaths] = useState('')
  const create = useMutation({
    mutationFn: (body: FilePackInput) => api.createPack(body),
    onSuccess: (k) => {
      qc.invalidateQueries({ queryKey: packsKey })
      onDone()
      navigate(`/packs/${k.id}`)
    },
  })
  const submit = (e: FormEvent) => {
    e.preventDefault()
    create.mutate({
      name,
      team_id: team || undefined,
      personal,
      paths: pathsOf(paths),
    })
  }
  return (
    <form className="panel form form-spaced" onSubmit={submit}>
      <div className="field-row">
        <label className="field grow">
          <span>Name</span>
          <input autoFocus required maxLength={100} value={name} onChange={(e) => setName(e.target.value)} />
        </label>
        <label className="field">
          <span>Owner</span>
          <select value={team} onChange={(e) => setTeam(e.target.value)}>
            <option value="">You</option>
            {owners.map((t) => (
              <option key={t.id} value={t.id}>
                {t.name}
              </option>
            ))}
          </select>
        </label>
      </div>
      <PathsField value={paths} onChange={setPaths} />
      <label className="check">
        <input type="checkbox" checked={personal} onChange={(e) => setPersonal(e.target.checked)} />
        <span>
          Personal
          <small className="muted">
            Each person who uses it has their own copy of its files, as for a token of their own. Otherwise everyone
            shares one copy. This cannot be changed later.
          </small>
        </span>
      </label>
      {create.error && <div className="alert">{create.error.message}</div>}
      <div className="form-actions">
        <button type="button" className="btn btn-ghost" onClick={onDone}>
          Cancel
        </button>
        <button type="submit" className="btn btn-primary" disabled={!name.trim() || create.isPending}>
          Make pack
        </button>
      </div>
    </form>
  )
}

function PathsField({
  value,
  onChange,
  disabled,
}: {
  value: string
  onChange: (v: string) => void
  disabled?: boolean
}) {
  return (
    <label className="field">
      <span>Paths, one to a line</span>
      <textarea
        className="mono"
        rows={4}
        spellCheck={false}
        disabled={disabled}
        value={value}
        onChange={(e) => onChange(e.target.value)}
        placeholder={'/workspace/app/.env\n/workspace/app/config/secrets/'}
      />
      <small className="muted">
        Absolute. One ending in <code>/</code> holds a directory and everything in it. A file is written once the
        directory its path is in exists, so a repository cloned there is cloned first.
      </small>
    </label>
  )
}

// PackPage is one pack: its files, and its settings for those who may
// change it.
export function PackPage() {
  const { id = '' } = useParams()
  const key = useMemo(() => ['pack', id] as const, [id])
  const pack = useQuery({
    queryKey: key,
    queryFn: () => api.pack(id),
    refetchInterval: 3000,
  })
  const [open, setOpen] = useState<string | null>(null)
  const source = useMemo<FileSource | null>(() => {
    if (!pack.data) return null
    const k = pack.data.pack
    return {
      key,
      get: (path) => api.packFile(id, path),
      put: (path, content) => api.putPackFile(id, path, content),
      settings: (path, mode, trustedOnly) => api.setPackFileSettings(id, path, mode, trustedOnly),
      remove: (path) => api.deletePackFile(id, path),
      writable: k.can_write,
      empty: 'No files yet. Add one here, or write one at a pack path in an environment that has the pack.',
      example: k.paths[0]?.endsWith('/') ? `${k.paths[0]}file` : (k.paths[0] ?? '/workspace/app/.env'),
    }
  }, [pack.data, id, key])

  if (pack.isPending) return <div className="page" />
  if (pack.isError) return <div className="page alert">{pack.error.message}</div>
  const k = pack.data.pack

  return (
    <div className="page">
      <PageHeader
        title={k.name}
        subtitle={[
          `Owned by ${ownerOf(k)}`,
          k.personal ? 'each person has their own copy of its files; these are yours' : 'one copy, shared by everyone',
          k.description,
        ]
          .filter(Boolean)
          .join(' · ')}
      />
      <section className="section">
        <h2 className="section-title">Files</h2>
        {source && <FileBrowser source={source} files={pack.data.files} paths={k.paths} open={open} onOpen={setOpen} />}
      </section>
      <div className="page-narrow">
        <PackSettings key={k.updated_at} pack={k} />
      </div>
    </div>
  )
}

function PackSettings({ pack }: { pack: FilePack }) {
  const qc = useQueryClient()
  const navigate = useNavigate()
  const [name, setName] = useState(pack.name)
  const [description, setDescription] = useState(pack.description)
  const [paths, setPaths] = useState(pack.paths.join('\n'))
  const refresh = () => {
    qc.invalidateQueries({ queryKey: packsKey })
    qc.invalidateQueries({ queryKey: ['pack', pack.id] })
  }
  const save = useMutation({
    mutationFn: () => api.updatePack(pack.id, { name, description, paths: pathsOf(paths) }),
    onSettled: refresh,
  })
  const remove = useMutation({
    mutationFn: () => api.deletePack(pack.id),
    onSuccess: () => {
      qc.invalidateQueries({ queryKey: packsKey })
      navigate('/packs')
    },
  })
  const disabled = !pack.can_change
  const submit = (e: FormEvent) => {
    e.preventDefault()
    save.mutate()
  }
  return (
    <section className="section">
      <h2 className="section-title">Settings</h2>
      {disabled && <p className="muted small">Only the pack's owner, or its team's members, change it.</p>}
      <form className="panel form form-spaced" onSubmit={submit}>
        <label className="field">
          <span>Name</span>
          <input required maxLength={100} disabled={disabled} value={name} onChange={(e) => setName(e.target.value)} />
        </label>
        <label className="field">
          <span>Description</span>
          <input disabled={disabled} value={description} onChange={(e) => setDescription(e.target.value)} />
        </label>
        <PathsField value={paths} onChange={setPaths} disabled={disabled} />
        <p className="muted small">
          Files under a path taken out are dropped from the pack; environments keep their copies. Templates list this
          pack by its ID, <code>{pack.id}</code>.
        </p>
        {save.error && <div className="alert">{save.error.message}</div>}
        {remove.error && <div className="alert">{remove.error.message}</div>}
        <div className="form-actions">
          {pack.can_delete && (
            <ConfirmButton
              label="Delete pack"
              confirmLabel="Delete it and every copy of its files?"
              onConfirm={() => remove.mutate()}
              disabled={remove.isPending}
            />
          )}
          {!disabled && (
            <button type="submit" className="btn btn-primary" disabled={save.isPending}>
              Save
            </button>
          )}
        </div>
      </form>
    </section>
  )
}

// FilePacksField chooses the packs a template's environments have, in the
// order they are chosen: a file two of them name is the first's. A pack
// listed that the editor cannot see is kept, and may be taken out.
export function FilePacksField({ value, onChange }: { value: string[]; onChange: (ids: string[]) => void }) {
  const packs = usePacks()
  const list = packs.data ?? []
  const unseen = value.filter((id) => !list.some((k) => k.id === id))
  const toggle = (id: string, on: boolean) => onChange(on ? [...value, id] : value.filter((v) => v !== id))
  return (
    <div className="field">
      <span>File packs</span>
      {list.length === 0 && unseen.length === 0 && (
        <small className="muted">
          No packs. <Link to="/packs">Make one</Link> for files at fixed paths that environments made from this template
          should have, such as a project's <code>.env</code>.
        </small>
      )}
      {list.map((k) => (
        <label key={k.id} className="check">
          <input type="checkbox" checked={value.includes(k.id)} onChange={(e) => toggle(k.id, e.target.checked)} />
          <span>
            {k.name}
            {value.length > 1 && value.includes(k.id) && <span className="muted"> · {value.indexOf(k.id) + 1}</span>}
            <small className="muted mono">{k.paths.join(', ')}</small>
          </span>
        </label>
      ))}
      {unseen.map((id) => (
        <label key={id} className="check">
          <input type="checkbox" checked onChange={() => toggle(id, false)} />
          <span>
            A pack you cannot see
            <small className="muted mono">{id}</small>
          </span>
        </label>
      ))}
      {(list.length > 0 || unseen.length > 0) && (
        <small className="muted">
          An environment has each of these its owner may use. A file two packs name is the first's.
        </small>
      )}
    </div>
  )
}
