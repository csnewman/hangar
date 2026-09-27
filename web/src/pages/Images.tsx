import { useMutation, useQuery, useQueryClient } from '@tanstack/react-query'
import { Globe, Lock, UsersRound, X } from 'lucide-react'
import { useState } from 'react'
import { Link, useNavigate, useParams } from 'react-router'

import { api, type ImageRepository } from '../api'
import { Activity } from '../components/Activity'
import { ConfirmButton } from '../components/ConfirmButton'
import { CopyCode } from '../components/CopyCode'
import { formatAgo, formatBytes, shortDigest } from '../components/format'
import { PageHeader } from '../components/PageHeader'
import { useMe } from '../session'
import { useTeams } from '../teams'
import { useTitle } from '../title'

const imagesKey = ['images'] as const

// ImagesPage lists the repositories in Hangar's registry the caller may
// pull, and says how to push one.
export function ImagesPage() {
  const me = useMe()
  const images = useQuery({ queryKey: imagesKey, queryFn: api.images, enabled: !!me.registry, refetchInterval: 15_000 })

  if (!me.registry) {
    return (
      <div className="page">
        <PageHeader title="Images" />
        <div className="panel empty-state">This server runs no registry of its own.</div>
      </div>
    )
  }
  const list = images.data ?? []
  const namespace = me.username.toLowerCase()

  return (
    <div className="page">
      <PageHeader
        title="Images"
        subtitle="Hangar's own registry: images for your templates, kept here and published nowhere else."
      />

      <section className="panel form section">
        <div className="form-section-head">
          <h2>Pushing an image</h2>
        </div>
        <p className="muted small">
          Sign in with your username and an <Link to="/account">access token</Link> as the password. Build on one of
          Hangar's images, which boot as a machine, and push to your own namespace or a team's.
        </p>
        <CopyCode text={`docker login ${me.registry} -u ${me.username}`} />
        <CopyCode text={`docker push ${me.registry}/${namespace}/my-image:latest`} />
        <p className="muted small">
          Then name <code>{`${me.registry}/${namespace}/my-image:latest`}</code> as a template's image.
        </p>
      </section>

      {images.isError && <div className="alert">Could not load images: {images.error.message}</div>}
      <section className="section">
        <h2 className="section-title">
          Repositories <span className="section-count">{list.length}</span>
        </h2>
        {images.isSuccess && list.length === 0 ? (
          <div className="panel empty-state">No repositories yet. Push an image to make one.</div>
        ) : (
          <div className="panel">
            <table className="table">
              <thead>
                <tr>
                  <th>Repository</th>
                  <th>Owner</th>
                  <th className="num">Tags</th>
                  <th>Updated</th>
                </tr>
              </thead>
              <tbody>
                {list.map((x) => (
                  <tr key={x.id}>
                    <td>
                      <Link to={`/images/${x.id}`} className="strong mono">
                        {x.path}
                      </Link>{' '}
                      <VisibilityBadge repo={x} />
                      {x.description && <div className="muted small">{x.description}</div>}
                    </td>
                    <td>{ownerName(x)}</td>
                    <td className="num">{x.tag_count}</td>
                    <td className="muted nowrap">{formatAgo(x.updated_at)}</td>
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

function ownerName(x: ImageRepository): string {
  if (x.team) return `team ${x.team.name}`
  return x.owner ? x.owner.display_name || x.owner.username : ''
}

function VisibilityBadge({ repo: x }: { repo: ImageRepository }) {
  if (x.visibility === 'shared') {
    return (
      <span className="badge badge-good" title="Everyone can pull it">
        <Globe size={11} />
        shared
      </span>
    )
  }
  return (
    <span className="badge badge-idle" title="Only its owner and collaborators can pull it">
      <Lock size={11} />
      private
    </span>
  )
}

// ImagePage shows one repository: how to pull it, its tags, and who may use
// it.
export function ImagePage() {
  const { id } = useParams()
  const qc = useQueryClient()
  const navigate = useNavigate()
  const image = useQuery({ queryKey: [...imagesKey, id], queryFn: () => api.image(id!), refetchInterval: 15_000 })
  useTitle(image.data?.path)
  const done = (x: ImageRepository) => {
    qc.setQueryData([...imagesKey, id], x)
    qc.invalidateQueries({ queryKey: imagesKey })
  }
  const update = useMutation({
    mutationFn: (visibility: 'private' | 'shared') =>
      api.updateImage(id!, { visibility, description: image.data?.description || undefined }),
    onSuccess: done,
  })
  const deleteTag = useMutation({ mutationFn: (tag: string) => api.deleteImageTag(id!, tag), onSuccess: done })
  const remove = useMutation({
    mutationFn: () => api.deleteImage(id!),
    onSuccess: () => {
      qc.invalidateQueries({ queryKey: imagesKey })
      navigate('/images')
    },
  })

  if (image.isPending) return <div className="page" />
  if (image.isError) {
    return (
      <div className="page">
        <PageHeader crumbs={[{ label: 'Images', to: '/images' }]} title="Not found" />
        <div className="panel empty-state">
          This repository does not exist, or it is not shared with you. <Link to="/images">Back to images</Link>
        </div>
      </div>
    )
  }
  const x = image.data
  const tags = x.tags ?? []
  const error = update.error ?? deleteTag.error ?? remove.error

  return (
    <div className="page page-narrow">
      <PageHeader
        crumbs={[{ label: 'Images', to: '/images' }]}
        title={<span className="mono">{x.path}</span>}
        subtitle={
          <span className="subtitle-row">
            <VisibilityBadge repo={x} />
            <span>
              {x.team ? (
                <>
                  team <Link to={`/teams/${x.team.id}`}>{x.team.name}</Link>
                </>
              ) : (
                ownerName(x)
              )}
            </span>
          </span>
        }
        actions={
          x.can_manage && <ConfirmButton label="Delete" confirmLabel="Delete?" onConfirm={() => remove.mutate()} />
        }
      />
      {error && <div className="alert">{error.message}</div>}
      {x.description && <p className="page-lead">{x.description}</p>}

      <section className="panel form section">
        <div className="field">
          <span>Pull</span>
          <CopyCode text={`${x.name}:${tags[0]?.name ?? 'latest'}`} />
          <small>Name it as a template's image; workers pull it from Hangar themselves.</small>
        </div>
        <div className="field">
          <span>Who can pull it</span>
          <div className="segmented" role="radiogroup">
            {(['private', 'shared'] as const).map((v) => (
              <label key={v} className={x.visibility === v ? 'seg seg-on' : 'seg'}>
                <input
                  type="radio"
                  name="visibility"
                  checked={x.visibility === v}
                  disabled={!x.can_manage || update.isPending}
                  onChange={() => update.mutate(v)}
                />
                {v === 'private' ? 'Owner and collaborators' : 'Everyone'}
              </label>
            ))}
          </div>
          {!x.can_manage && <small>Only its owner can change who can pull it.</small>}
        </div>
      </section>

      <section className="section">
        <h2 className="section-title">
          Tags <span className="section-count">{tags.length}</span>
        </h2>
        {tags.length === 0 ? (
          <div className="panel empty-state">No tags.</div>
        ) : (
          <div className="panel">
            <table className="table">
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
                    <td className="mono strong">{t.name}</td>
                    <td className="mono muted small" title={t.digest}>
                      {shortDigest(t.digest)}
                    </td>
                    <td className="num nowrap">{formatBytes(t.size_bytes)}</td>
                    <td className="muted nowrap">{formatAgo(t.updated_at)}</td>
                    <td>
                      {x.can_push && (
                        <div className="actions">
                          <ConfirmButton
                            label="Delete"
                            confirmLabel="Delete?"
                            disabled={deleteTag.isPending}
                            onConfirm={() => deleteTag.mutate(t.name)}
                          />
                        </div>
                      )}
                    </td>
                  </tr>
                ))}
              </tbody>
            </table>
          </div>
        )}
      </section>

      <Collaborators repo={x} onChange={done} />

      <section className="section">
        <h2 className="section-title">Activity</h2>
        <Activity subjects={[`image:${x.path}`]} />
      </section>
    </div>
  )
}

// Collaborators are people, who push, and teams, whose viewers pull and
// whose members push.
function Collaborators({ repo: x, onChange }: { repo: ImageRepository; onChange: (x: ImageRepository) => void }) {
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

  return (
    <section className="panel form section">
      <div className="form-section-head">
        <h2>Collaborators</h2>
        <span className="muted small">People push; a team's viewers pull and its members push.</span>
      </div>
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
    </section>
  )
}
