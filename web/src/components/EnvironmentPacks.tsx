import { useMutation, useQuery, useQueryClient } from '@tanstack/react-query'

import { api, type AttachedPack } from '../api'
import { copyName } from '../pages/Packs'
import { useMe } from '../session'

// fromWhat says how an environment came to use a copy.
export const fromWhat: Record<AttachedPack['from'], string> = {
  environment: 'chosen here',
  template: "the template's",
  kept: 'the one it was given',
  default: 'your default',
}

// useEnvironmentPacks is the packs an environment has, with the copy it
// uses of each, and the conflicts found as their paths became shared.
export function useEnvironmentPacks(env: string) {
  const packs = useQuery({
    queryKey: ['environment', env, 'packs'],
    queryFn: () => api.environmentPacks(env),
    refetchInterval: 10_000,
  })
  const conflicts = useQuery({
    queryKey: ['environment', env, 'conflicts'],
    queryFn: () => api.environmentConflicts(env),
    refetchInterval: 5_000,
  })
  return { packs, conflicts }
}

// CopyChoice chooses which copy of a pack an environment uses.
export function CopyChoice({ env, attached }: { env: string; attached: AttachedPack }) {
  const me = useMe()
  const qc = useQueryClient()
  const copies = useQuery({
    queryKey: ['pack', attached.pack.id, 'copies'],
    queryFn: () => api.copies(attached.pack.id),
  })
  const choose = useMutation({
    mutationFn: (copy: string) => api.chooseEnvironmentCopy(env, attached.pack.id, copy || undefined),
    onSettled: () => qc.invalidateQueries({ queryKey: ['environment', env, 'packs'] }),
  })
  const list = copies.data ?? [attached.copy]
  return (
    <>
      <select value={attached.copy.id} disabled={choose.isPending} onChange={(e) => choose.mutate(e.target.value)}>
        {list.map((c) => (
          <option key={c.id} value={c.id}>
            {copyName(c, me.id)}
          </option>
        ))}
        {!list.some((c) => c.id === attached.copy.id) && (
          <option value={attached.copy.id}>{copyName(attached.copy, me.id)}</option>
        )}
      </select>
      {attached.from === 'environment' && (
        <button type="button" className="btn btn-ghost" onClick={() => choose.mutate('')}>
          Reset
        </button>
      )}
      {choose.error && <span className="action-error">{choose.error.message}</span>}
    </>
  )
}

// Conflicts lists the files an environment had of its own where paths
// became shared, which its owner resolves.
export function Conflicts({
  env,
  own,
  conflicts,
  names,
}: {
  env: string
  own: boolean
  conflicts: { copy_id: string; path: string; resolution?: string }[]
  names: AttachedPack[]
}) {
  const qc = useQueryClient()
  const resolve = useMutation({
    mutationFn: (c: { copy: string; path: string; how: 'shared' | 'environment' }) =>
      api.resolveConflict(env, c.copy, c.path, c.how),
    onSettled: () => qc.invalidateQueries({ queryKey: ['environment', env, 'conflicts'] }),
  })
  const packOf = (copy: string) => names.find((a) => a.copy.id === copy)?.pack.name ?? 'a pack'
  return (
    <>
      <div className="notice">
        This environment had files of its own, differing from its copies', where paths became shared. The copies' are
        used; its own are kept aside until you choose.
      </div>
      <div className="panel">
        <table className="table">
          <tbody>
            {conflicts.map((c) => (
              <tr key={c.copy_id + c.path}>
                <td>
                  <span className="mono">{c.path}</span>
                  <div className="muted small">{packOf(c.copy_id)}</div>
                </td>
                <td className="num">
                  {c.resolution ? (
                    <span className="muted small">
                      {c.resolution === 'shared' ? 'Keeping the shared file…' : "Using this environment's…"}
                    </span>
                  ) : own ? (
                    <>
                      <button
                        type="button"
                        className="btn btn-ghost"
                        disabled={resolve.isPending}
                        onClick={() => resolve.mutate({ copy: c.copy_id, path: c.path, how: 'shared' })}
                      >
                        Keep the shared one
                      </button>
                      <button
                        type="button"
                        className="btn btn-ghost"
                        disabled={resolve.isPending}
                        title="Replaces the copy's file for everyone using the copy"
                        onClick={() => resolve.mutate({ copy: c.copy_id, path: c.path, how: 'environment' })}
                      >
                        Use this environment's
                      </button>
                    </>
                  ) : null}
                </td>
              </tr>
            ))}
          </tbody>
        </table>
      </div>
      {resolve.error && <div className="alert">{resolve.error.message}</div>}
    </>
  )
}
