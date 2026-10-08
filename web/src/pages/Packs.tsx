import { useMutation, useQuery, useQueryClient } from '@tanstack/react-query'
import { Plus, X } from 'lucide-react'
import { useMemo, useState, type FormEvent } from 'react'
import { Link, Navigate, useNavigate, useParams, useSearchParams } from 'react-router'

import { api, type Copy, type Pack, type PackInput, type PackPath, type PackRef } from '../api'
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

// packPath is where a pack's page is, on one of its copies if given.
export const packPath = (id: string, copy?: string) => `/files/packs/${id}` + (copy ? `?copy=${copy}` : '')

// ownerOf names who owns a pack or copy: a team, a person, or the server.
export function ownerOf(k: { team?: string; owner?: string; builtin?: boolean }): string {
  if (k.builtin) return 'Built in'
  return k.team ?? k.owner ?? ''
}

// reaches says which environments a pack reaches by itself.
function reaches(k: Pack, me: string): string {
  switch (k.attach) {
    case 'everyone':
      return "Everyone's environments"
    case 'team':
      return `${k.team}'s environments`
    case 'owner':
      return k.owner_id === me ? 'All your environments' : `${k.owner}'s environments`
    default:
      return 'Templates that list it'
  }
}

// copyName names a copy for a list of them.
export function copyName(c: Copy, me: string): string {
  if (c.personal) return c.owner_id === me ? 'Your own' : `${c.owner}'s own`
  return `${c.name} (${c.team ?? c.owner})`
}

// PackList lists the packs the caller may use, the built-in ones first, for
// the Files page.
export function PackList() {
  const me = useMe()
  const [creating, setCreating] = useState(false)
  const packs = usePacks()
  const list = packs.data ?? []
  return (
    <section className="section">
      <div className="section-head">
        <h2 className="section-title">Packs</h2>
        {!creating && (
          <button type="button" className="btn btn-ghost" onClick={() => setCreating(true)}>
            <Plus size={14} />
            New pack
          </button>
        )}
      </div>
      {creating && <CreateForm onDone={() => setCreating(false)} />}
      {packs.isError && <div className="alert">Could not load packs: {packs.error.message}</div>}
      {list.length > 0 && (
        <div className="panel">
          <table className="table">
            <thead>
              <tr>
                <th>Pack</th>
                <th>Owner</th>
                <th>Reaches</th>
                <th>Your default</th>
                <th>Paths</th>
              </tr>
            </thead>
            <tbody>
              {list.map((k) => {
                const shared = k.paths.filter((p) => !p.path.startsWith('!'))
                return (
                  <tr key={k.id}>
                    <td>
                      <Link to={packPath(k.id)} className="strong">
                        {k.name}
                      </Link>
                      {k.description && <div className="muted small">{k.description}</div>}
                    </td>
                    <td>{ownerOf(k)}</td>
                    <td>{reaches(k, me.id)}</td>
                    <td>{k.default_copy_id ? 'A shared copy' : 'Your own copy'}</td>
                    <td className="mono small">
                      {shared.slice(0, 3).map((p) => (
                        <div key={p.path}>{p.path}</div>
                      ))}
                      {shared.length > 3 && <div className="muted">and {shared.length - 3} more</div>}
                    </td>
                  </tr>
                )
              })}
            </tbody>
          </table>
        </div>
      )}
    </section>
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
  const [attach, setAttach] = useState<Pack['attach']>('owner')
  const [paths, setPaths] = useState<PackPath[]>([{ path: '', sensitive: false }])
  const create = useMutation({
    mutationFn: (body: PackInput) => api.createPack(body),
    onSuccess: (k) => {
      qc.invalidateQueries({ queryKey: packsKey })
      onDone()
      navigate(packPath(k.id))
    },
  })
  const submit = (e: FormEvent) => {
    e.preventDefault()
    create.mutate({ name, team_id: team || undefined, attach, paths: cleanPaths(paths) })
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
          <select
            value={team}
            onChange={(e) => {
              setTeam(e.target.value)
              setAttach(e.target.value ? 'team' : 'owner')
            }}
          >
            <option value="">You</option>
            {owners.map((t) => (
              <option key={t.id} value={t.id}>
                {t.name}
              </option>
            ))}
          </select>
        </label>
      </div>
      <AttachField value={attach} onChange={setAttach} team={team !== ''} builtin={false} admin={me.admin} />
      <PathsEditor value={paths} onChange={setPaths} />
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

// cleanPaths are the paths as they are sent: trimmed, without empty rows.
function cleanPaths(paths: PackPath[]): PackPath[] {
  return paths.map((p) => ({ ...p, path: p.path.trim() })).filter((p) => p.path !== '')
}

function AttachField({
  value,
  onChange,
  team,
  builtin,
  admin,
  disabled,
}: {
  value: Pack['attach']
  onChange: (a: Pack['attach']) => void
  team: boolean
  builtin: boolean
  admin: boolean
  disabled?: boolean
}) {
  const options: [Pack['attach'], string][] = [['listed', 'Only environments whose template lists it']]
  if (!team && !builtin) options.push(['owner', "All its owner's environments"])
  if (team) options.push(['team', "All its team's members' environments"])
  if (admin || value === 'everyone') options.push(['everyone', "Everyone's environments"])
  return (
    <label className="field">
      <span>Reaches</span>
      <select value={value} disabled={disabled} onChange={(e) => onChange(e.target.value as Pack['attach'])}>
        {options.map(([v, label]) => (
          <option key={v} value={v}>
            {label}
          </option>
        ))}
      </select>
      <small className="muted">
        A template keeps self-attaching packs out of its environments with its access settings.
      </small>
    </label>
  )
}

// PathsEditor edits a pack's paths, a row each.
function PathsEditor({
  value,
  onChange,
  disabled,
}: {
  value: PackPath[]
  onChange: (v: PackPath[]) => void
  disabled?: boolean
}) {
  const set = (i: number, p: Partial<PackPath>) => onChange(value.map((x, j) => (j === i ? { ...x, ...p } : x)))
  return (
    <div className="field">
      <span>Paths</span>
      {value.map((p, i) => (
        <div key={i} className="inline-form">
          <input
            className="mono grow"
            spellCheck={false}
            disabled={disabled}
            value={p.path}
            placeholder={i === 0 ? '~/.npmrc, /workspace/app/.env or ~/.config/tool/' : ''}
            onChange={(e) => set(i, { path: e.target.value })}
          />
          <label className="check">
            <input
              type="checkbox"
              checked={p.sensitive}
              disabled={disabled || p.path.startsWith('!')}
              onChange={(e) => set(i, { sensitive: e.target.checked })}
            />
            <span>Sensitive</span>
          </label>
          {!disabled && (
            <button
              type="button"
              className="btn btn-ghost"
              title="Remove"
              onClick={() => onChange(value.filter((_, j) => j !== i))}
            >
              <X size={14} />
            </button>
          )}
        </div>
      ))}
      {!disabled && (
        <div>
          <button
            type="button"
            className="btn btn-ghost"
            onClick={() => onChange([...value, { path: '', sensitive: false }])}
          >
            <Plus size={14} />
            Add a path
          </button>
        </div>
      )}
      <small className="muted">
        In the home directory with <code>~/</code>, or absolute. One ending in <code>/</code> shares a directory and
        everything in it; one starting with <code>!</code> leaves a path in such a directory to each environment. Every
        file under a sensitive path is kept from environments whose template withholds sensitive files. A file at an
        absolute path is written once the directory it is in exists, so a repository cloned there is cloned first.
      </small>
    </div>
  )
}

// PackRedirect sends a pack's address from before it was under Files to
// its page.
export function PackRedirect() {
  const { id = '' } = useParams()
  return <Navigate to={packPath(id)} replace />
}

// PackPage is one pack: the files of one of its copies -- the caller's
// default unless another is chosen -- and its paths and settings.
export function PackPage() {
  const me = useMe()
  const { id = '' } = useParams()
  const [search, setSearch] = useSearchParams()
  const pack = useQuery({ queryKey: ['pack', id], queryFn: () => api.pack(id) })
  const copies = useQuery({ queryKey: ['pack', id, 'copies'], queryFn: () => api.copies(id) })

  if (pack.isPending || copies.isPending) return <div className="page" />
  if (pack.isError) return <div className="page alert">{pack.error.message}</div>
  if (copies.isError) return <div className="page alert">{copies.error.message}</div>
  const k = pack.data
  const own = copies.data.find((c) => c.personal)
  const fallback = k.default_copy_id ?? own?.id ?? copies.data[0]?.id
  const chosen = copies.data.find((c) => c.id === search.get('copy'))?.id ?? fallback

  return (
    <div className="page">
      <PageHeader
        crumbs={[{ label: 'Files', to: '/files' }]}
        title={k.name}
        subtitle={[ownerOf(k), reaches(k, me.id), k.description].filter(Boolean).join(' · ')}
      />
      <CopyBar
        pack={k}
        copies={copies.data}
        chosen={chosen}
        onChoose={(c) => setSearch(!c || c === fallback ? {} : { copy: c }, { replace: true })}
      />
      {chosen && <CopyFiles key={chosen} copy={chosen} pack={k} />}
      <div className="page-narrow">
        <PackSettings key={k.updated_at} pack={k} />
      </div>
    </div>
  )
}

// CopyBar chooses the copy shown, sets it as the default, and makes and
// deletes shared copies.
function CopyBar({
  pack,
  copies,
  chosen,
  onChoose,
}: {
  pack: Pack
  copies: Copy[]
  chosen?: string
  onChoose: (id: string) => void
}) {
  const me = useMe()
  const qc = useQueryClient()
  const teams = useTeams()
  const owners = (teams.data ?? []).filter((t) => canGive(t, me.admin))
  const [making, setMaking] = useState(false)
  const [name, setName] = useState('')
  const [team, setTeam] = useState('')
  const refresh = () => {
    qc.invalidateQueries({ queryKey: ['pack', pack.id] })
    qc.invalidateQueries({ queryKey: packsKey })
  }
  const current = copies.find((c) => c.id === chosen)
  const isDefault = (c?: Copy) => (pack.default_copy_id ? pack.default_copy_id === c?.id : !!c?.personal)
  const setDefault = useMutation({
    mutationFn: () => api.setDefaultCopy(pack.id, current?.personal ? undefined : chosen),
    onSettled: refresh,
  })
  const create = useMutation({
    mutationFn: () => api.createCopy(pack.id, name.trim(), team || undefined),
    onSuccess: (c) => {
      setMaking(false)
      setName('')
      onChoose(c.id)
    },
    onSettled: refresh,
  })
  const remove = useMutation({
    mutationFn: (id: string) => api.deleteCopy(id),
    onSuccess: () => onChoose(''),
    onSettled: refresh,
  })
  const submit = (e: FormEvent) => {
    e.preventDefault()
    if (name.trim()) create.mutate()
  }
  return (
    <section className="section">
      <div className="inline-form">
        <label className="field-inline">
          <span>Copy</span>
          <select value={chosen} onChange={(e) => onChoose(e.target.value)}>
            {copies.map((c) => (
              <option key={c.id} value={c.id}>
                {copyName(c, me.id)}
                {isDefault(c) ? ' · your default' : ''}
              </option>
            ))}
          </select>
        </label>
        {current && !isDefault(current) && (
          <button type="button" className="btn btn-ghost" onClick={() => setDefault.mutate()}>
            Use by default
          </button>
        )}
        {current && !current.personal && current.can_delete && (
          <ConfirmButton
            label="Delete copy"
            confirmLabel="Delete its files?"
            onConfirm={() => remove.mutate(current.id)}
            disabled={remove.isPending}
          />
        )}
        {!making && (
          <button type="button" className="btn btn-ghost" onClick={() => setMaking(true)}>
            <Plus size={14} />
            Shared copy
          </button>
        )}
      </div>
      {making && (
        <form className="inline-form" onSubmit={submit}>
          <input
            autoFocus
            className="grow"
            placeholder="Name, such as staging"
            value={name}
            onChange={(e) => setName(e.target.value)}
          />
          <select value={team} onChange={(e) => setTeam(e.target.value)}>
            <option value="">Yours</option>
            {owners.map((t) => (
              <option key={t.id} value={t.id}>
                {t.name}'s
              </option>
            ))}
          </select>
          <button type="submit" className="btn btn-primary" disabled={!name.trim() || create.isPending}>
            Make
          </button>
          <button type="button" className="btn btn-ghost" onClick={() => setMaking(false)}>
            Cancel
          </button>
        </form>
      )}
      <p className="muted small">
        Your own copy follows you into every environment that has the pack, unless you choose another here or on an
        environment, or its template pins one. A shared copy is one set of files a team, or you, keep apart.
      </p>
      {[setDefault.error, create.error, remove.error].map(
        (e) =>
          e && (
            <div key={e.message} className="alert">
              {e.message}
            </div>
          ),
      )}
    </section>
  )
}

// CopyFiles is one copy's files, in the browser.
function CopyFiles({ copy, pack }: { copy: string; pack: Pack }) {
  const key = useMemo(() => ['copy', copy] as const, [copy])
  const files = useQuery({ queryKey: key, queryFn: () => api.copy(copy), refetchInterval: 3000 })
  const [open, setOpen] = useState<string | null>(null)
  const shared = useMemo(() => pack.paths.filter((p) => !p.path.startsWith('!')).map((p) => p.path), [pack.paths])
  const writable = files.data?.copy.can_write ?? false
  const source = useMemo<FileSource>(
    () => ({
      key,
      get: (path) => api.copyFile(copy, path),
      put: (path, content) => api.putCopyFile(copy, path, content),
      mode: (path, mode) => api.setCopyFileMode(copy, path, mode),
      remove: (path) => api.deleteCopyFile(copy, path),
      writable,
      empty: 'No files yet. Add one here, or write one at a pack path in an environment using this copy.',
      example: shared[0]?.endsWith('/') ? `${shared[0]}file` : (shared[0] ?? '~/.npmrc'),
    }),
    [copy, key, shared, writable],
  )
  if (files.isError) return <div className="alert">{files.error.message}</div>
  if (!files.data) return null
  return (
    <section className="section">
      <h2 className="section-title">Files</h2>
      <FileBrowser source={source} files={files.data.files} paths={shared} open={open} onOpen={setOpen} />
    </section>
  )
}

function PackSettings({ pack }: { pack: Pack }) {
  const me = useMe()
  const qc = useQueryClient()
  const navigate = useNavigate()
  const [name, setName] = useState(pack.name)
  const [description, setDescription] = useState(pack.description)
  const [attach, setAttach] = useState(pack.attach)
  const [paths, setPaths] = useState<PackPath[]>(pack.paths)
  const refresh = () => {
    qc.invalidateQueries({ queryKey: packsKey })
    qc.invalidateQueries({ queryKey: ['pack', pack.id] })
  }
  const save = useMutation({
    mutationFn: () => api.updatePack(pack.id, { name, description, attach, paths: cleanPaths(paths) }),
    onSettled: refresh,
  })
  const remove = useMutation({
    mutationFn: () => api.deletePack(pack.id),
    onSuccess: () => {
      qc.invalidateQueries({ queryKey: packsKey })
      navigate('/files')
    },
  })
  const disabled = !pack.can_change
  const submit = (e: FormEvent) => {
    e.preventDefault()
    save.mutate()
  }
  return (
    <section className="section">
      <h2 className="section-title">Paths and settings</h2>
      {disabled && (
        <p className="muted small">
          {pack.builtin
            ? 'An administrator changes the built-in packs.'
            : "Only the pack's owner, or its team's members, change it."}
        </p>
      )}
      <form className="panel form form-spaced" onSubmit={submit}>
        <label className="field">
          <span>Name</span>
          <input required maxLength={100} disabled={disabled} value={name} onChange={(e) => setName(e.target.value)} />
        </label>
        <label className="field">
          <span>Description</span>
          <input disabled={disabled} value={description} onChange={(e) => setDescription(e.target.value)} />
        </label>
        <AttachField
          value={attach}
          onChange={setAttach}
          team={!!pack.team_id}
          builtin={pack.builtin}
          admin={me.admin}
          disabled={disabled}
        />
        <PathsEditor value={paths} onChange={setPaths} disabled={disabled} />
        <p className="muted small">
          Copies keep their files under a path taken out, no longer shared; environments keep theirs as their own.
          Templates list this pack by its ID, <code>{pack.id}</code>.
        </p>
        {save.error && <div className="alert">{save.error.message}</div>}
        {!disabled && (
          <div className="form-actions">
            {pack.can_delete && (
              <ConfirmButton
                label="Delete pack"
                confirmLabel="Delete it and every copy?"
                onConfirm={() => remove.mutate()}
                disabled={remove.isPending}
              />
            )}
            <button type="submit" className="btn btn-primary" disabled={save.isPending}>
              Save
            </button>
          </div>
        )}
      </form>
    </section>
  )
}

// PacksField chooses the packs a template's environments have, in the
// order chosen -- a path two of them name is the first's -- and for each,
// the copy every environment uses, or each owner's own. A pack listed that
// the editor cannot see is kept, and may be taken out.
export function PacksField({ value, onChange }: { value: PackRef[]; onChange: (refs: PackRef[]) => void }) {
  const packs = usePacks()
  const all = packs.data ?? []
  const list = all.filter((k) => k.attach !== 'everyone' || value.some((r) => r.pack === k.id))
  const unseen = value.filter((r) => !all.some((k) => k.id === r.pack))
  const index = (id: string) => value.findIndex((r) => r.pack === id)
  const toggle = (id: string, on: boolean) =>
    onChange(on ? [...value, { pack: id }] : value.filter((r) => r.pack !== id))
  const pin = (id: string, copy: string) =>
    onChange(value.map((r) => (r.pack === id ? { pack: id, copy: copy || undefined } : r)))
  return (
    <div className="field">
      <span>Packs</span>
      {list.length === 0 && unseen.length === 0 && (
        <small className="muted">
          No packs. <Link to="/files">Make one</Link> for files environments made from this template should have, such
          as a project's <code>.env</code>.
        </small>
      )}
      {list.map((k) => {
        const i = index(k.id)
        return (
          <div key={k.id}>
            <label className="check">
              <input type="checkbox" checked={i >= 0} onChange={(e) => toggle(k.id, e.target.checked)} />
              <span>
                {k.name}
                {value.length > 1 && i >= 0 && <span className="muted"> · {i + 1}</span>}
                <small className="muted mono">
                  {k.paths
                    .filter((p) => !p.path.startsWith('!'))
                    .map((p) => p.path)
                    .join(', ')}
                </small>
              </span>
            </label>
            {i >= 0 && <PinField pack={k.id} value={value[i].copy ?? ''} onChange={(c) => pin(k.id, c)} />}
          </div>
        )
      })}
      {unseen.map((r) => (
        <label key={r.pack} className="check">
          <input type="checkbox" checked onChange={() => toggle(r.pack, false)} />
          <span>
            A pack you cannot see
            <small className="muted mono">{r.pack}</small>
          </span>
        </label>
      ))}
      {(list.length > 0 || unseen.length > 0) && (
        <small className="muted">
          An environment has each of these its owner may use, before the packs that attach themselves to it. A path two
          packs name is the first's.
        </small>
      )}
    </div>
  )
}

// PinField chooses the copy of a pack every environment uses: each owner's
// own, or one of its shared copies.
function PinField({ pack, value, onChange }: { pack: string; value: string; onChange: (copy: string) => void }) {
  const copies = useQuery({ queryKey: ['pack', pack, 'copies'], queryFn: () => api.copies(pack) })
  const shared = (copies.data ?? []).filter((c) => !c.personal)
  return (
    <label className="field-inline pin">
      <span>Copy</span>
      <select value={value} onChange={(e) => onChange(e.target.value)}>
        <option value="">Each person's own</option>
        {shared.map((c) => (
          <option key={c.id} value={c.id}>
            {c.name} ({c.team ?? c.owner})
          </option>
        ))}
        {value && !shared.some((c) => c.id === value) && <option value={value}>A copy you cannot see</option>}
      </select>
    </label>
  )
}
