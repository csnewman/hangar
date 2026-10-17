import { useMutation, useQueryClient } from '@tanstack/react-query'

import { api, type Environment } from '../api'
import { changeable, environmentsKey } from '../environments'
import { shortDigest } from './format'

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
  const upgrade = useMutation({
    mutationFn: (force: boolean) => api.upgradeEnvironmentImage(env.id, force),
    onSettled: done,
  })
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
