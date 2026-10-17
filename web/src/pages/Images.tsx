import './images.css'

import { useMutation, useQuery, useQueryClient } from '@tanstack/react-query'
import { Boxes, UsersRound, X } from 'lucide-react'
import { useState, type ReactNode } from 'react'
import { Link, useNavigate, useParams } from 'react-router'

import { api, type ImageRepository } from '../api'
import { Activity } from '../components/Activity'
import { ConfirmButton } from '../components/ConfirmButton'
import { formatAgo, formatBytes, shortDigest } from '../components/format'
import {
  CopyField,
  DetailHead,
  DetailPage,
  ListGroup,
  ListItem,
  ListPane,
  MasterDetail,
  NoneChosen,
  PaneBar,
  PaneRoot,
  Row,
  Rows,
  SearchField,
  Section,
  Segmented,
} from '../components/Pane'
import { usePhone } from '../phone'
import { useMe } from '../session'
import { useTeams } from '../teams'
import { useTitle } from '../title'

const imagesKey = ['images'] as const

// ImagesPage lists the repositories in Hangar's registry the caller may
// pull, and says how to push one. The repositories are a list, and the push
// instructions sit beside it until one is chosen.
export function ImagesPage() {
  return (
    <ImagesLayout>
      <PushingPage />
    </ImagesLayout>
  )
}

function ownerName(x: ImageRepository): string {
  if (x.team) return `team ${x.team.name}`
  return x.owner ? x.owner.display_name || x.owner.username : ''
}

// ImagePage shows one repository: how to pull it, its tags, and who may use
// it, beside the list of repositories.
export function ImagePage() {
  const { id = '' } = useParams()
  return (
    <ImagesLayout chosen>
      <ImageDetail key={id} id={id} />
    </ImagesLayout>
  )
}

function Collaborators({
  repo: x,
  onChange,
  bare = false,
}: {
  repo: ImageRepository
  onChange: (x: ImageRepository) => void
  bare?: boolean
}) {
  const [adding, setAdding] = useState('')
  const people = useQuery({ queryKey: ['people'], queryFn: api.people, enabled: x.can_push, refetchInterval: false })
  const teams = useTeams()
  const set = useMutation({
    mutationFn: ({ users, teams }: { users: string[]; teams: string[] }) => api.setImageCollaborators(x.id, users, teams),
    onSuccess: (updated) => {
      onChange(updated)
      setAdding('')
    },
  })
  const ids = x.collaborators.map((c) => c.id)
  const teamIDs = x.collaborator_teams.map((c) => c.id)
  const candidates = (people.data ?? []).filter((p) => p.id !== x.owner?.id && !ids.includes(p.id))
  const teamCandidates = (teams.data ?? []).filter((t) => t.id !== x.team?.id && !teamIDs.includes(t.id))
  const add = () => {
    const [kind, id] = adding.split(':')
    if (kind === 'team') set.mutate({ users: ids, teams: [...teamIDs, id] })
    else set.mutate({ users: [...ids, id], teams: teamIDs })
  }

  const body = (
    <>
      <div className="people">
        {x.collaborator_teams.map((c) => (
          <span key={c.id} className="person">
            <UsersRound size={14} />
            <Link to={`/teams/${c.id}`}>{c.name}</Link>
            {x.can_push && (
              <button
                type="button"
                className="person-remove"
                title="Remove"
                aria-label={`Remove team ${c.slug}`}
                disabled={set.isPending}
                onClick={() => set.mutate({ users: ids, teams: teamIDs.filter((t) => t !== c.id) })}
              >
                <X size={13} />
              </button>
            )}
          </span>
        ))}
        {x.collaborators.map((c) => (
          <span key={c.id} className="person">
            {c.display_name || c.username}
            {x.can_push && (
              <button
                type="button"
                className="person-remove"
                title="Remove"
                aria-label={`Remove ${c.username}`}
                disabled={set.isPending}
                onClick={() => set.mutate({ users: ids.filter((u) => u !== c.id), teams: teamIDs })}
              >
                <X size={13} />
              </button>
            )}
          </span>
        ))}
        {x.collaborators.length === 0 && x.collaborator_teams.length === 0 && (
          <span className="muted small">No collaborators yet.</span>
        )}
      </div>
      {x.can_push && (
        <div className="inline-form">
          <select value={adding} onChange={(e) => setAdding(e.target.value)} aria-label="Add a collaborator">
            <option value="">Add a person or team…</option>
            {teamCandidates.length > 0 && (
              <optgroup label="Teams">
                {teamCandidates.map((t) => (
                  <option key={t.id} value={`team:${t.id}`}>
                    {t.name}
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
  if (bare) return body
  return (
    <section className="panel form section">
      <div className="form-section-head">
        <h2>Collaborators</h2>
        <span className="muted small">People push; a team's viewers pull and its members push.</span>
      </div>
      {body}
    </section>
  )
}

// ImagesLayout is the Images page: the repositories the caller may pull
// down the left, grouped by whose they are, and beside them the chosen one,
// or what children show. chosen says one is, which on a phone is a screen
// of its own; there the list ends with how to push an image, which on a
// computer stands beside it.
function ImagesLayout({ children, chosen = false }: { children: ReactNode; chosen?: boolean }) {
  const me = useMe()
  const phone = usePhone()
  const images = useQuery({ queryKey: imagesKey, queryFn: api.images, enabled: !!me.registry, refetchInterval: 15_000 })
  const [query, setQuery] = useState('')
  if (!me.registry) {
    return (
      <PaneRoot>
        <PaneBar title="Images" />
        <NoneChosen>This server runs no registry of its own.</NoneChosen>
      </PaneRoot>
    )
  }
  const q = query.trim().toLowerCase()
  const list = (images.data ?? []).filter((x) => !q || x.path.toLowerCase().includes(q))
  const groups: { title: string; repos: ImageRepository[] }[] = [
    { title: 'Yours', repos: list.filter((x) => !x.team && x.owner?.id === me.id) },
  ]
  const teams = [...new Set(list.filter((x) => x.team).map((x) => x.team!.name))].sort()
  for (const t of teams) groups.push({ title: t, repos: list.filter((x) => x.team?.name === t) })
  groups.push({ title: 'Shared with you', repos: list.filter((x) => !x.team && x.owner?.id !== me.id) })

  return (
    <PaneRoot>
      <MasterDetail
        chosen={chosen}
        back={{ to: '/images', label: 'Images' }}
        list={
          <ListPane
            title="Images"
            back={{ to: '/more', label: 'More' }}
            head={<SearchField value={query} onChange={setQuery} placeholder="Repositories" />}
          >
            {images.isError && <div className="md-empty-list">Could not load images: {images.error.message}</div>}
            {groups
              .filter((g) => g.repos.length > 0)
              .map((g) => (
                <ListGroup key={g.title} title={g.title}>
                  {g.repos.map((x) => (
                    <ListItem
                      key={x.id}
                      to={`/images/${x.id}`}
                      mark={<Boxes size={15} />}
                      title={<span className="mono images-path">{x.path}</span>}
                      sub={[
                        `${x.tag_count} ${x.tag_count === 1 ? 'tag' : 'tags'}`,
                        formatAgo(x.updated_at),
                        x.visibility === 'shared' ? 'shared' : 'private',
                      ].join(' · ')}
                    />
                  ))}
                </ListGroup>
              ))}
            {images.isSuccess && list.length === 0 && (
              <div className="md-empty-list">{q ? 'No repository has that name.' : 'No repositories yet.'}</div>
            )}
            {phone && (
              <div className="images-push">
                <Section title="Pushing an image">
                  <PushCommands
                    registry={me.registry}
                    refName={`${me.registry}/${me.username.toLowerCase()}/my-image:latest`}
                  />
                </Section>
              </div>
            )}
          </ListPane>
        }
      >
        {children}
      </MasterDetail>
    </PaneRoot>
  )
}

// PushCommands are how an image reaches the registry: signing in, then
// pushing to ref.
function PushCommands({ registry, refName }: { registry: string; refName: string }) {
  const me = useMe()
  return (
    <>
      <p className="pane-hint images-hint">
        Sign in with your username and an <Link to="/settings/access-tokens">access token</Link> as the password, then
        push. Build on one of Hangar's images, which boot as a machine, and push to your own namespace or a team's.
      </p>
      <Rows>
        <Row label="Sign in">
          <CopyField text={`docker login ${registry} -u ${me.username}`} />
        </Row>
        <Row label="Push">
          <CopyField text={`docker push ${refName}`} />
        </Row>
      </Rows>
    </>
  )
}

// PushingPage stands beside the list while no repository is chosen: what
// the registry is, and how an image gets there.
function PushingPage() {
  const me = useMe()
  const namespace = me.username.toLowerCase()
  const refName = `${me.registry}/${namespace}/my-image:latest`
  return (
    <>
      <DetailHead
        mark={<Boxes size={20} className="images-mark" />}
        title="Images"
        sub="Hangar's own registry: images for your templates, kept here and published nowhere else."
      />
      <DetailPage>
        <Section title="Pushing an image">
          <PushCommands registry={me.registry!} refName={refName} />
          <p className="pane-hint images-hint">
            Then name <code>{refName}</code> as a template's image. Choose a repository on the left for its tags and who
            may pull it.
          </p>
        </Section>
      </DetailPage>
    </>
  )
}

// VisibilityPill says who can pull a repository.
function VisibilityPill({ repo: x }: { repo: ImageRepository }) {
  return x.visibility === 'shared' ? (
    <span className="pane-pill pane-pill-good" title="Everyone can pull it">
      shared
    </span>
  ) : (
    <span className="pane-pill pane-pill-grey" title="Only its owner and collaborators can pull it">
      private
    </span>
  )
}

// ImageDetail is one repository beside the list: how to pull it, its tags,
// who may pull and push it, what has been done to it, and how to push a tag.
function ImageDetail({ id }: { id: string }) {
  const me = useMe()
  const qc = useQueryClient()
  const navigate = useNavigate()
  const image = useQuery({ queryKey: [...imagesKey, id], queryFn: () => api.image(id), refetchInterval: 15_000 })
  useTitle(image.data?.path)
  const done = (x: ImageRepository) => {
    qc.setQueryData([...imagesKey, id], x)
    qc.invalidateQueries({ queryKey: imagesKey })
  }
  const update = useMutation({
    mutationFn: (visibility: 'private' | 'shared') =>
      api.updateImage(id, { visibility, description: image.data?.description || undefined }),
    onSuccess: done,
  })
  const deleteTag = useMutation({ mutationFn: (tag: string) => api.deleteImageTag(id, tag), onSuccess: done })
  const remove = useMutation({
    mutationFn: () => api.deleteImage(id),
    onSuccess: () => {
      qc.invalidateQueries({ queryKey: imagesKey })
      navigate('/images')
    },
  })

  if (image.isPending) return null
  if (image.isError) {
    return <NoneChosen>This repository does not exist, or it is not shared with you.</NoneChosen>
  }
  const x = image.data
  const tags = x.tags ?? []
  const error = update.error ?? deleteTag.error ?? remove.error
  const tag = tags[0]?.name ?? 'latest'

  return (
    <>
      <DetailHead
        mark={<Boxes size={20} className="images-mark" />}
        title={<span className="mono">{x.path}</span>}
        sub={
          <>
            <VisibilityPill repo={x} />
            <span>
              {x.team ? (
                <>
                  team <Link to={`/teams/${x.team.id}`}>{x.team.name}</Link>
                </>
              ) : (
                ownerName(x)
              )}
            </span>
            <span>·</span>
            <span>
              {tags.length} {tags.length === 1 ? 'tag' : 'tags'}
            </span>
            {x.description && (
              <>
                <span>·</span>
                <span>{x.description}</span>
              </>
            )}
          </>
        }
        actions={
          x.can_manage && (
            <ConfirmButton label="Delete" confirmLabel="Delete the repository?" onConfirm={() => remove.mutate()} />
          )
        }
      />
      {error && (
        <div className="pane-banner pane-banner-error" role="alert">
          <div className="pane-banner-row">{error.message}</div>
        </div>
      )}
      <DetailPage>
        <Section title="Pull">
          <Rows>
            <Row label="Image">
              <CopyField text={`${x.name}:${tag}`} />
            </Row>
          </Rows>
          <p className="pane-hint">Name it as a template's image; workers pull it from Hangar themselves.</p>
        </Section>
        <Section title="Tags">
          {tags.length === 0 ? (
            <p className="pane-hint">No tags.</p>
          ) : (
            <table className="pane-table images-tags">
              <thead>
                <tr>
                  <th>Tag</th>
                  <th>Digest</th>
                  <th className="num">Size</th>
                  <th>Pushed</th>
                  <th />
                </tr>
              </thead>
              <tbody>
                {tags.map((t) => (
                  <tr key={t.name}>
                    <td className="mono strong nowrap">{t.name}</td>
                    <td className="mono muted small" title={t.digest}>
                      {shortDigest(t.digest)}
                    </td>
                    <td className="num nowrap">{formatBytes(t.size_bytes)}</td>
                    <td className="muted nowrap">{formatAgo(t.updated_at)}</td>
                    <td className="num">
                      {x.can_push && (
                        <ConfirmButton
                          label="Delete"
                          confirmLabel="Delete?"
                          disabled={deleteTag.isPending}
                          onConfirm={() => deleteTag.mutate(t.name)}
                        />
                      )}
                    </td>
                  </tr>
                ))}
              </tbody>
            </table>
          )}
        </Section>
        <Section title="Who can pull it">
          <Rows>
            <Row label="Pull">
              <Segmented
                label="Who can pull it"
                value={x.visibility}
                disabled={!x.can_manage || update.isPending}
                onChange={(v) => update.mutate(v)}
                options={[
                  { value: 'private', label: 'Owner and collaborators' },
                  { value: 'shared', label: 'Everyone' },
                ]}
              />
              {!x.can_manage && <span className="muted small">Only its owner can change who can pull it.</span>}
            </Row>
          </Rows>
        </Section>
        <Section
          title="Collaborators"
          extra={<span className="muted">People push; a team's viewers pull and its members push.</span>}
        >
          <Collaborators repo={x} onChange={done} bare />
        </Section>
        <Section title="Activity">
          <Activity subjects={[`image:${x.path}`]} pageSize={8} />
        </Section>
        {x.can_push && me.registry && (
          <Section title="Pushing a tag">
            <PushCommands registry={me.registry} refName={`${x.name}:${tag}`} />
          </Section>
        )}
      </DetailPage>
    </>
  )
}
