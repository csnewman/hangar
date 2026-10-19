import './files.css'

import { useMutation, useQuery, useQueryClient } from '@tanstack/react-query'
import { FolderSync, Lock, Plus, Undo2, X } from 'lucide-react'
import { useMemo, useState, type FormEvent, type ReactNode } from 'react'
import { Link, useNavigate, useParams, useSearchParams } from 'react-router'

import { api, type Copy, type Pack, type PackInput, type PackPath, type PackRef } from '../api'
import { ConfirmButton } from '../components/ConfirmButton'
import { FileBrowser, type FileSource } from '../components/FileBrowser'
import {
  ApplyBar,
  CopyField,
  DetailHead,
  DetailPage,
  ListGroup,
  ListItem,
  ListPane,
  MasterDetail,
  NoneChosen,
  PaneRoot,
  Row,
  Rows,
  SearchField,
  Section,
} from '../components/Pane'
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

// cleanPaths are the paths as they are sent: trimmed, without empty rows.
function cleanPaths(paths: PackPath[]): PackPath[] {
  return paths.map((p) => ({ ...p, path: p.path.trim() })).filter((p) => p.path !== '')
}

// attachOptions are the environments a pack may reach by itself: a team's
// pack its team's, a person's their own, and everyone's for an
// administrator.
function attachOptions(value: Pack['attach'], team: boolean, builtin: boolean, admin: boolean) {
  const options: [Pack['attach'], string][] = [['listed', 'Only environments whose template lists it']]
  if (!team && !builtin) options.push(['owner', "All its owner's environments"])
  if (team) options.push(['team', "All its team's members' environments"])
  if (admin || value === 'everyone') options.push(['everyone', "Everyone's environments"])
  return options
}

// PackPage is one pack: the files of one of its copies -- the caller's
// default unless another is chosen -- and its paths and settings, beside
// the list of packs.
export function PackPage() {
  const { id = '' } = useParams()
  return (
    <PacksLayout chosen>
      <PackDetail key={id} id={id} />
    </PacksLayout>
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
  const c = files.data.copy
  return (
    <>
      {!c.personal && c.can_write && c.team && (
        <div className="notice notice-warn">
          This is {c.team}'s shared copy, {c.name}: a file you change, add or remove here changes in the environments of
          everyone in {c.team} who uses it, not just yours.
        </div>
      )}
      <FileBrowser source={source} files={files.data.files} paths={shared} open={open} onOpen={setOpen} />
    </>
  )
}

// ReachWarning says, to someone who may change a pack, who else a change
// reaches: everyone, for a built-in pack or one attached to everyone, and a
// team's members, for a team's pack.
function ReachWarning({ pack }: { pack: Pack }) {
  let who = ''
  if (pack.builtin || pack.attach === 'everyone') who = "every user's environments"
  else if (pack.team) who = `the environments of everyone in ${pack.team} who uses it`
  if (!who) return null
  return (
    <div className="notice notice-warn">
      {pack.builtin ? 'This is a built-in pack: changes' : 'Changes'} to its paths and settings apply to {who}, not just
      yours. A path taken out stops being shared for all of them, and one added starts sharing what each of them has
      there.
      {pack.default_paths && <BuiltinDefaults pack={pack} defaults={pack.default_paths} />}
    </div>
  )
}

// pathKey names a path and whether it is sensitive, for comparing lists.
const pathKey = (p: PackPath) => `${p.path}${p.sensitive ? ' (sensitive)' : ''}`

// BuiltinDefaults is a built-in pack's paths as Hangar ships them: whether
// its own differ, how, and a way back to them.
function BuiltinDefaults({ pack, defaults }: { pack: Pack; defaults: PackPath[] }) {
  const qc = useQueryClient()
  const now = pack.paths.map(pathKey)
  const shipped = defaults.map(pathKey)
  const added = now.filter((p) => !shipped.includes(p))
  const removed = shipped.filter((p) => !now.includes(p))
  const customized = added.length > 0 || removed.length > 0
  const reset = useMutation({
    mutationFn: () =>
      api.updatePack(pack.id, {
        name: pack.name,
        description: pack.description,
        attach: pack.attach,
        squash: pack.squash,
        paths: defaults,
      }),
    onSettled: () => {
      qc.invalidateQueries({ queryKey: packsKey })
      qc.invalidateQueries({ queryKey: ['pack', pack.id] })
    },
  })
  return (
    <div className="builtin-defaults">
      <div className="builtin-defaults-head">
        <strong>{customized ? "Its paths differ from Hangar's defaults." : "Its paths are Hangar's defaults."}</strong>
        {customized && (
          <ConfirmButton
            label="Reset to the defaults"
            confirmLabel="Reset for every user?"
            onConfirm={() => reset.mutate()}
            disabled={reset.isPending}
          />
        )}
      </div>
      {customized && (
        <ul className="mono small builtin-defaults-diff">
          {added.map((p) => (
            <li key={`+${p}`}>+ {p}</li>
          ))}
          {removed.map((p) => (
            <li key={`-${p}`}>− {p}</li>
          ))}
        </ul>
      )}
      <details>
        <summary>Hangar's defaults ({defaults.length} paths)</summary>
        <ul className="mono small">
          {defaults.map((p) => (
            <li key={p.path}>{pathKey(p)}</li>
          ))}
        </ul>
      </details>
      {reset.error && <div className="alert">{reset.error.message}</div>}
    </div>
  )
}

// squashOptions are who environments' users act as on a pack's files, as
// an NFS export's squash options say.
const squashOptions: [Pack['squash'], string][] = [
  ['all_squash', "One owner: everyone acts as the environment's user"],
  ['root_squash', 'Real owners, root acting as nobody'],
  ['no_root_squash', 'Real owners, root included'],
]

// lowerFirst starts a phrase in lower case, for the middle of a line.
const lowerFirst = (t: string) => t.charAt(0).toLowerCase() + t.slice(1)

// PacksLayout is the Files page: the packs the caller may use down the
// left, grouped by whose they are, and beside them the chosen one, or what
// children show. chosen says a pack, or a new one's form, is shown, which
// on a phone is a screen of its own.
export function PacksLayout({ children, chosen = false }: { children: ReactNode; chosen?: boolean }) {
  const me = useMe()
  const packs = usePacks()
  const [query, setQuery] = useState('')
  const q = query.trim().toLowerCase()
  const list = (packs.data ?? []).filter(
    (k) => !q || k.name.toLowerCase().includes(q) || k.paths.some((p) => p.path.toLowerCase().includes(q)),
  )
  const groups: { title: string; packs: Pack[] }[] = [
    { title: 'Built in', packs: list.filter((k) => k.builtin) },
    { title: 'Yours', packs: list.filter((k) => !k.builtin && !k.team_id && k.owner_id === me.id) },
  ]
  const teams = [...new Set(list.filter((k) => !k.builtin && k.team).map((k) => k.team!))].sort()
  for (const t of teams) groups.push({ title: t, packs: list.filter((k) => !k.builtin && k.team === t) })
  groups.push({ title: "Others'", packs: list.filter((k) => !k.builtin && !k.team_id && k.owner_id !== me.id) })

  return (
    <PaneRoot>
      <MasterDetail
        chosen={chosen}
        back={{ to: '/files', label: 'Files' }}
        list={
          <ListPane
            title="Files"
            back={{ to: '/more', label: 'More' }}
            head={
              <>
                <SearchField value={query} onChange={setQuery} placeholder="Packs and paths" />
                <Link to="/files?new" className="pane-icon-btn files-new" title="New pack" aria-label="New pack">
                  <Plus size={15} />
                </Link>
              </>
            }
          >
            {packs.isError && <div className="md-empty-list">Could not load packs: {packs.error.message}</div>}
            {groups
              .filter((g) => g.packs.length > 0)
              .map((g) => (
                <ListGroup key={g.title} title={g.title}>
                  {g.packs.map((k) => (
                    <ListItem
                      key={k.id}
                      to={packPath(k.id)}
                      mark={k.builtin ? <Lock size={14} /> : <FolderSync size={15} />}
                      title={k.name}
                      sub={lowerFirst(reaches(k, me.id))}
                    />
                  ))}
                </ListGroup>
              ))}
            {packs.isSuccess && list.length === 0 && (
              <div className="md-empty-list">{q ? 'No pack has that name or path.' : 'No packs yet.'}</div>
            )}
          </ListPane>
        }
      >
        {children}
      </MasterDetail>
    </PaneRoot>
  )
}

// PathRows edits a pack's paths, a row each, as a table.
function PathRows({ value, onChange }: { value: PackPath[]; onChange: (v: PackPath[]) => void }) {
  const set = (i: number, p: Partial<PackPath>) => onChange(value.map((x, j) => (j === i ? { ...x, ...p } : x)))
  return (
    <>
      <table className="pane-table files-paths">
        <thead>
          <tr>
            <th>Path</th>
            <th className="files-sensitive">Sensitive</th>
            <th className="files-remove" />
          </tr>
        </thead>
        <tbody>
          {value.map((p, i) => (
            <tr key={i}>
              <td>
                <input
                  className="mono files-path"
                  aria-label="Path"
                  spellCheck={false}
                  value={p.path}
                  placeholder={i === 0 ? '~/.npmrc, /workspace/app/.env or ~/.config/tool/' : ''}
                  onChange={(e) => set(i, { path: e.target.value })}
                />
              </td>
              <td className="files-sensitive">
                <input
                  type="checkbox"
                  aria-label={`${p.path || 'This path'} is sensitive`}
                  checked={p.sensitive}
                  disabled={p.path.startsWith('!')}
                  onChange={(e) => set(i, { sensitive: e.target.checked })}
                />
              </td>
              <td className="files-remove">
                <button
                  type="button"
                  className="pane-icon-btn"
                  title="Remove"
                  aria-label={`Remove ${p.path || 'this path'}`}
                  onClick={() => onChange(value.filter((_, j) => j !== i))}
                >
                  <X size={14} />
                </button>
              </td>
            </tr>
          ))}
        </tbody>
      </table>
      <div>
        <button
          type="button"
          className="btn btn-ghost btn-sm"
          onClick={() => onChange([...value, { path: '', sensitive: false }])}
        >
          <Plus size={13} />
          Add a Path
        </button>
      </div>
      <p className="pane-hint files-hint">
        In the home directory with <code>~/</code>, or absolute. One ending in <code>/</code> shares a directory and
        everything in it; one starting with <code>!</code> leaves a path in such a directory to each environment. Every
        file under a sensitive path is kept from environments whose template withholds sensitive files. A file at an
        absolute path is written once the directory it is in exists, so a repository cloned there is cloned first.
      </p>
    </>
  )
}

// NewPack makes a pack: its name, owner, reach and paths, then its page.
export function NewPack() {
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
      navigate(packPath(k.id))
    },
  })
  const submit = (e: FormEvent) => {
    e.preventDefault()
    create.mutate({ name, team_id: team || undefined, attach, paths: cleanPaths(paths) })
  }
  return (
    <form className="files-form" onSubmit={submit}>
      <DetailHead
        mark={<FolderSync size={20} className="files-mark" />}
        title="New Pack"
        sub="A list of paths to share between environments"
      />
      <DetailPage>
        <Section title="Pack">
          <fieldset className="pane-fields files-fields">
            <Rows>
              <Row label="Name">
                <input
                  autoFocus
                  required
                  maxLength={100}
                  className="pane-wide"
                  value={name}
                  onChange={(e) => setName(e.target.value)}
                />
              </Row>
              <Row label="Owner">
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
              </Row>
              <Row label="Attaches to">
                <select value={attach} onChange={(e) => setAttach(e.target.value as Pack['attach'])}>
                  {attachOptions(attach, team !== '', false, me.admin).map(([v, label]) => (
                    <option key={v} value={v}>
                      {label}
                    </option>
                  ))}
                </select>
              </Row>
            </Rows>
          </fieldset>
        </Section>
        <Section title="Paths">
          <div className="pane-fields">
            <PathRows value={paths} onChange={setPaths} />
          </div>
        </Section>
      </DetailPage>
      <div className="pane-apply">
        <span className="pane-apply-what">
          {create.error ? (
            <span className="action-error">{create.error.message}</span>
          ) : (
            'Your own copy of it starts empty.'
          )}
        </span>
        <Link to="/files" className="btn btn-ghost btn-sm">
          Cancel
        </Link>
        <button type="submit" className="btn btn-primary btn-sm" disabled={!name.trim() || create.isPending}>
          Make Pack
        </button>
      </div>
    </form>
  )
}

// PackDetail is one pack beside the list: who has it and where it reaches,
// then its paths and settings, its copies, and the files of the copy chosen.
function PackDetail({ id }: { id: string }) {
  const me = useMe()
  const qc = useQueryClient()
  const navigate = useNavigate()
  const [search, setSearch] = useSearchParams()
  const pack = useQuery({ queryKey: ['pack', id], queryFn: () => api.pack(id) })
  const copies = useQuery({ queryKey: ['pack', id, 'copies'], queryFn: () => api.copies(id) })
  const remove = useMutation({
    mutationFn: () => api.deletePack(id),
    onSuccess: () => {
      qc.invalidateQueries({ queryKey: packsKey })
      navigate('/files')
    },
  })
  if (pack.isError) return <NoneChosen>{pack.error.message}</NoneChosen>
  if (copies.isError) return <NoneChosen>{copies.error.message}</NoneChosen>
  if (pack.isPending || copies.isPending) return null
  const k = pack.data
  const own = copies.data.find((c) => c.personal)
  const fallback = k.default_copy_id ?? own?.id ?? copies.data[0]?.id
  const chosen = copies.data.find((c) => c.id === search.get('copy'))?.id ?? fallback
  const choose = (c: string) => setSearch(!c || c === fallback ? {} : { copy: c }, { replace: true })
  return (
    <>
      <DetailHead
        mark={k.builtin ? <Lock size={20} className="files-mark" /> : <FolderSync size={20} className="files-mark" />}
        title={k.name}
        sub={
          <>
            <span>{ownerOf(k) || 'yours'}</span>
            <span>·</span>
            <span>{lowerFirst(reaches(k, me.id))}</span>
            {k.description && (
              <>
                <span>·</span>
                <span>{k.description}</span>
              </>
            )}
          </>
        }
        actions={
          k.can_delete && (
            <ConfirmButton
              label="Delete Pack"
              confirmLabel="Delete it and every copy?"
              onConfirm={() => remove.mutate()}
              disabled={remove.isPending}
            />
          )
        }
      />
      {remove.error && (
        <div className="pane-banner pane-banner-error" role="alert">
          <div className="pane-banner-row">{remove.error.message}</div>
        </div>
      )}
      <PackBody key={k.updated_at} pack={k} copies={copies.data} chosen={chosen} onChoose={choose} />
    </>
  )
}

// PackBody is a pack's sections, its paths and settings changed where they
// are shown and saved from the bar at the foot.
function PackBody({
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
  const [name, setName] = useState(pack.name)
  const [description, setDescription] = useState(pack.description)
  const [attach, setAttach] = useState(pack.attach)
  const [squash, setSquash] = useState(pack.squash)
  const [paths, setPaths] = useState<PackPath[]>(pack.paths)
  const save = useMutation({
    mutationFn: () => api.updatePack(pack.id, { name, description, attach, squash, paths: cleanPaths(paths) }),
    onSettled: () => {
      qc.invalidateQueries({ queryKey: packsKey })
      qc.invalidateQueries({ queryKey: ['pack', pack.id] })
    },
  })
  const reset = () => {
    setName(pack.name)
    setDescription(pack.description)
    setAttach(pack.attach)
    setSquash(pack.squash)
    setPaths(pack.paths)
  }
  const samePaths = cleanPaths(paths).map(pathKey).join('\n') === pack.paths.map(pathKey).join('\n')
  const changed = [
    name !== pack.name && 'name',
    description !== pack.description && 'description',
    attach !== pack.attach && 'where it attaches',
    squash !== pack.squash && 'ownership',
    !samePaths && 'paths',
  ].filter((c): c is string => !!c)
  const disabled = !pack.can_change
  const current = copies.find((c) => c.id === chosen)

  return (
    <>
      <DetailPage
        sections={[
          { id: 'paths', label: 'Paths' },
          { id: 'settings', label: 'Settings' },
          { id: 'copies', label: 'Copies' },
          { id: 'files', label: 'Files' },
        ]}
      >
        {!disabled && <ReachWarning pack={pack} />}
        {disabled && (
          <p className="pane-hint">
            {pack.builtin
              ? 'An administrator changes the built-in packs.'
              : "Only the pack's owner, or its team's members, change it."}
          </p>
        )}
        <Section id="paths" title="Paths">
          <fieldset className="pane-fields files-fields" disabled={disabled}>
            <PathRows value={paths} onChange={setPaths} />
          </fieldset>
        </Section>
        <Section id="settings" title="Settings">
          <fieldset className="pane-fields files-fields" disabled={disabled}>
            <Rows>
              <Row label="Name" modified={name !== pack.name}>
                <input
                  required
                  maxLength={100}
                  className="pane-wide"
                  value={name}
                  onChange={(e) => setName(e.target.value)}
                />
              </Row>
              <Row label="Description" modified={description !== pack.description}>
                <input className="pane-wide" value={description} onChange={(e) => setDescription(e.target.value)} />
              </Row>
              <Row label="Attaches to" modified={attach !== pack.attach}>
                <select value={attach} onChange={(e) => setAttach(e.target.value as Pack['attach'])}>
                  {attachOptions(attach, !!pack.team_id, pack.builtin, me.admin).map(([v, label]) => (
                    <option key={v} value={v}>
                      {label}
                    </option>
                  ))}
                </select>
              </Row>
              <Row label="Ownership" modified={squash !== pack.squash}>
                <select value={squash} onChange={(e) => setSquash(e.target.value as Pack['squash'])}>
                  {squashOptions.map(([v, label]) => (
                    <option key={v} value={v}>
                      {label}
                    </option>
                  ))}
                </select>
              </Row>
            </Rows>
          </fieldset>
          <Rows>
            <Row label="ID">
              <CopyField text={pack.id} />
              <span className="muted small">templates list it by this</span>
            </Row>
          </Rows>
          <p className="pane-hint files-hint">
            A template keeps self-attaching packs out of its environments with its access settings. With one owner,
            every file is the environment's user's, whoever makes it; with real owners, a file made by root, or another
            user, is theirs, as on any disk. Copies keep their files under a path taken out, unshared; environments keep
            theirs as their own.
          </p>
        </Section>
        <Section id="copies" title="Copies">
          <Copies pack={pack} copies={copies} chosen={chosen} onChoose={onChoose} />
        </Section>
        <Section
          id="files"
          title="Files"
          extra={
            copies.length > 1 && (
              <select
                className="pane-input"
                aria-label="Copy shown"
                value={chosen}
                onChange={(e) => onChoose(e.target.value)}
              >
                {copies.map((c) => (
                  <option key={c.id} value={c.id}>
                    {copyName(c, me.id)}
                  </option>
                ))}
              </select>
            )
          }
        >
          {current && copies.length <= 1 && <p className="pane-hint">{copyName(current, me.id)} copy</p>}
          {chosen && <CopyFiles key={chosen} copy={chosen} pack={pack} />}
        </Section>
      </DetailPage>
      {!disabled && (changed.length > 0 || save.error) && (
        <ApplyBar
          count={changed.length}
          actions={
            <>
              <button type="button" className="btn btn-ghost btn-sm" disabled={save.isPending} onClick={reset}>
                <Undo2 size={13} />
                Reset
              </button>
              <button
                type="button"
                className="btn btn-primary btn-sm"
                disabled={save.isPending || !name.trim() || changed.length === 0}
                onClick={() => save.mutate()}
              >
                Save
              </button>
            </>
          }
        >
          {save.error ? (
            <span className="action-error">{save.error.message}</span>
          ) : (
            <>Its {changed.join(', ')}. Saved for everyone it reaches.</>
          )}
        </ApplyBar>
      )}
    </>
  )
}

// Copies lists a pack's copies: which is the caller's default, the one whose
// files are shown, and making and deleting shared ones.
function Copies({
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
  const isDefault = (c: Copy) => (pack.default_copy_id ? pack.default_copy_id === c.id : c.personal)
  const setDefault = useMutation({
    mutationFn: (c: Copy) => api.setDefaultCopy(pack.id, c.personal ? undefined : c.id),
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
  const error = setDefault.error ?? create.error ?? remove.error
  return (
    <>
      <table className="pane-table">
        <thead>
          <tr>
            <th>Copy</th>
            <th>Whose</th>
            <th />
          </tr>
        </thead>
        <tbody>
          {copies.map((c) => (
            <tr key={c.id}>
              <td>
                <span className="files-copy">
                  <strong>{c.personal ? copyName(c, me.id) : c.name}</strong>
                  {isDefault(c) && <span className="pane-pill">your default</span>}
                  {c.id === chosen && <span className="pane-pill pane-pill-grey">files shown below</span>}
                </span>
              </td>
              <td className="muted">{c.team ?? c.owner}</td>
              <td className="num">
                <span className="files-copy-actions">
                  {c.id !== chosen && (
                    <button type="button" className="pane-link" onClick={() => onChoose(c.id)}>
                      Show Files
                    </button>
                  )}
                  {!isDefault(c) && (
                    <button
                      type="button"
                      className="pane-link"
                      disabled={setDefault.isPending}
                      onClick={() => setDefault.mutate(c)}
                    >
                      Use by Default
                    </button>
                  )}
                  {!c.personal && c.can_delete && (
                    <ConfirmButton
                      label="Delete"
                      confirmLabel="Delete its files?"
                      onConfirm={() => remove.mutate(c.id)}
                      disabled={remove.isPending}
                    />
                  )}
                </span>
              </td>
            </tr>
          ))}
        </tbody>
      </table>
      {making ? (
        <form className="files-make pane-fields" onSubmit={submit}>
          <input
            autoFocus
            className="pane-wide"
            aria-label="Name of the shared copy"
            placeholder="Name, such as staging"
            value={name}
            onChange={(e) => setName(e.target.value)}
          />
          <select aria-label="Whose" value={team} onChange={(e) => setTeam(e.target.value)}>
            <option value="">Yours</option>
            {owners.map((t) => (
              <option key={t.id} value={t.id}>
                {t.name}'s
              </option>
            ))}
          </select>
          <button type="submit" className="btn btn-primary btn-sm" disabled={!name.trim() || create.isPending}>
            Make
          </button>
          <button type="button" className="btn btn-ghost btn-sm" onClick={() => setMaking(false)}>
            Cancel
          </button>
        </form>
      ) : (
        <div>
          <button type="button" className="btn btn-ghost btn-sm" onClick={() => setMaking(true)}>
            <Plus size={13} />
            New Shared Copy
          </button>
        </div>
      )}
      <p className="pane-hint files-hint">
        Your own copy follows you into every environment that has the pack, unless you choose another here or on an
        environment, or its template pins one. A shared copy is one set of files a team, or you, keep apart.
      </p>
      {error && <div className="alert">{error.message}</div>}
    </>
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
