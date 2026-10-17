import { useMutation, useQueryClient } from '@tanstack/react-query'
import { useState } from 'react'

import { api, type Environment } from '../api'
import { changeable, environmentsKey } from '../environments'
import { formatAgo, formatBytes, shortDigest } from './format'

// imageBuild names a build of an image by its digest.
export function imageBuild(digest: string): string {
  return digest ? shortDigest(digest) : 'an earlier build'
}

// useImageChange is what can be done about an environment's image: upgrade
// it to the newer copy, roll back an upgrade or keep it, or cancel either
// before the start that would make it. Upgrading and rolling back need it
// stopped.
export function useImageChange(env: Environment) {
  const qc = useQueryClient()
  const done = () => qc.invalidateQueries({ queryKey: environmentsKey })
  const upgrade = useMutation({ mutationFn: (force: boolean) => api.upgradeEnvironmentImage(env.id, force), onSettled: done })
  const rollback = useMutation({ mutationFn: () => api.rollbackEnvironmentImage(env.id), onSettled: done })
  const keep = useMutation({ mutationFn: () => api.keepEnvironmentImage(env.id), onSettled: done })
  const cancel = useMutation({ mutationFn: () => api.cancelEnvironmentImageChange(env.id), onSettled: done })
  return {
    upgrade,
    rollback,
    keep,
    cancel,
    error: upgrade.error ?? rollback.error ?? keep.error ?? cancel.error,
    busy: upgrade.isPending || rollback.isPending || keep.isPending || cancel.isPending,
    stopped: changeable(env),
  }
}

// imageRisk is what upgrading would leave hiding the newer build's files:
// risky when the writable disk holds some, or the package database; clean
// when it was compared and holds none.
export function imageRisk(u: Environment['image_update']) {
  const risky = !!u && u.checked && !u.error && ((u.conflicts ?? []).length > 0 || !!u.packages)
  const clean = !!u && u.checked && !u.error && !risky
  return { risky, clean }
}

// ImageUpdate offers an environment the newer copy of its image its
// reference names, says what upgrading would leave hiding the newer image's files, and
// once upgraded, offers to roll back or keep it. Each change is made at the
// environment's next start.
export function ImageUpdate({ env }: { env: Environment }) {
  const [showAll, setShowAll] = useState(false)
  const { upgrade, rollback, keep, cancel, error, busy, stopped } = useImageChange(env)
  const stopFirst = stopped ? undefined : 'Stop the environment first'

  const u = env.image_update
  const rb = env.image_rollback
  const change = env.image_change
  if (!u && !rb && !change) return null

  if (change) {
    return (
      <div className="notice drift">
        <div>
          {change === 'upgrade' ? (
            <>
              Its next start upgrades <strong>{env.spec.image}</strong>
              {u && <> to {imageBuild(u.digest)}</>}, keeping a copy of its writable disk to roll back to.
            </>
          ) : (
            <>
              Its next start rolls back to {imageBuild(rb?.digest ?? '')} and the writable disk it had then, losing what has
              been written to it since the upgrade.
            </>
          )}
        </div>
        <div className="drift-actions">
          <button type="button" className="btn btn-ghost" disabled={busy} onClick={() => cancel.mutate()}>
            Cancel
          </button>
        </div>
        {error && <div className="alert">{error.message}</div>}
      </div>
    )
  }

  const conflicts = u?.conflicts ?? []
  const more = u?.more_conflicts ?? 0
  const { risky, clean } = imageRisk(u)
  const shown = showAll ? conflicts : conflicts.slice(0, 5)

  return (
    <>
      {rb && (
        <div className="notice drift">
          <div>
            Upgraded from {imageBuild(rb.digest)} {formatAgo(rb.at)}. Its writable disk from before ({formatBytes(rb.size_bytes)}) is
            kept, to roll back to.
          </div>
          <div className="drift-actions">
            <button
              type="button"
              className="btn btn-ghost"
              disabled={busy || !stopped}
              title={stopFirst ?? 'Go back to the earlier image and writable disk at its next start'}
              onClick={() => rollback.mutate()}
            >
              Roll back
            </button>
            <button
              type="button"
              className="btn btn-ghost"
              disabled={busy}
              title="Keep the upgrade and free the copy's space"
              onClick={() => keep.mutate()}
            >
              Keep upgrade
            </button>
          </div>
          {!u && error && <div className="alert">{error.message}</div>}
        </div>
      )}
      {u && (
        <div className="notice drift image-update">
          <div>
            <div>
              A newer build of <strong>{env.spec.image}</strong> is available: {imageBuild(u.digest)}, where this
              environment has {imageBuild(env.image_digest ?? '')}.
            </div>
            <div className="muted small">
              {!u.checked &&
                (stopped
                  ? 'Fetching it and checking what upgrading would leave hidden…'
                  : 'Stop it to check what upgrading would leave hidden.')}
              {u.error && <>Its writable disk could not be compared with the newer build: {u.error}</>}
              {clean && 'Nothing it has changed hides a file the newer build changes, so its disks carry over as they are.'}
              {risky && (
                <>
                  {u.packages && (
                    <>
                      Packages have been installed in it: its package database would hide the newer build&rsquo;s, and
                      describe neither.{' '}
                    </>
                  )}
                  {conflicts.length + more} file{conflicts.length + more === 1 ? '' : 's'} it has changed would keep
                  its version over the newer build&rsquo;s:
                </>
              )}
            </div>
            {risky && conflicts.length > 0 && (
              <ul className="image-conflicts mono small">
                {shown.map((c) => (
                  <li key={c}>{c}</li>
                ))}
                {!showAll && conflicts.length > shown.length && (
                  <li>
                    <button type="button" className="btn btn-ghost" onClick={() => setShowAll(true)}>
                      {conflicts.length - shown.length} more
                    </button>
                  </li>
                )}
                {(showAll || conflicts.length <= shown.length) && more > 0 && <li>and {more} more</li>}
              </ul>
            )}
          </div>
          <div className="drift-actions">
            <button
              type="button"
              className={risky || u.error ? 'btn btn-danger' : 'btn btn-ghost'}
              disabled={busy || !stopped || !u.checked}
              title={stopFirst ?? 'At its next start, keeping its disks and a copy of its writable disk to roll back to'}
              onClick={() => upgrade.mutate(risky || !!u.error)}
            >
              {risky || u.error ? 'Upgrade anyway' : 'Upgrade'}
            </button>
          </div>
          {error && <div className="alert">{error.message}</div>}
        </div>
      )}
    </>
  )
}
