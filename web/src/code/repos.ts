import type { Change, CodeClient, Repo } from './client'

// repoFor is the repository a path in the folder is in, its innermost: a
// repository's own folder is that repository.
export function repoFor(repos: Repo[], path: string): Repo | undefined {
  let best: Repo | undefined
  for (const r of repos) {
    if (r.uninitialised || r.error) continue
    if (r.path !== '' && path !== r.path && !path.startsWith(r.path + '/')) continue
    if (!best || r.path.length > best.path.length) best = r
  }
  return best
}

// usable are the repositories git can be asked about.
export const usable = (repos: Repo[]) => repos.filter((r) => !r.uninitialised && !r.error)

export const isStaged = (c: Change) => c.index !== '.' && c.index !== '?' && c.index !== 'U'

// commitAll commits what is staged in each repository with the message,
// a submodule before the repository naming it, so its parent sees its
// new commit to record. Amending amends the one repository with anything
// staged, or the only one.
export async function commitAll(client: CodeClient, repos: Repo[], message: string, amend: boolean) {
  const staged = repos.filter((r) => r.changes.some(isStaged))
  if (amend) {
    const one = staged.length === 1 ? staged[0] : usable(repos).length === 1 ? usable(repos)[0] : undefined
    if (!one) throw new Error('amend one repository at a time')
    await client.commit(message, true, one.path)
    return
  }
  const deepest = [...staged].sort((a, b) => depth(b.path) - depth(a.path))
  for (const r of deepest) {
    try {
      await client.commit(message, false, r.path)
    } catch (e) {
      throw new Error(`${r.name}: ${e instanceof Error ? e.message : String(e)}`)
    }
  }
}

const depth = (path: string) => (path === '' ? 0 : path.split('/').length)

// amendable is the repository Amend would amend, if there is one.
export function amendable(repos: Repo[]): Repo | undefined {
  const staged = repos.filter((r) => r.changes.some(isStaged))
  if (staged.length === 1) return staged[0]
  const ok = usable(repos)
  return staged.length === 0 && ok.length === 1 ? ok[0] : undefined
}

// within names a change from its repository's folder, as a list grouped
// by repository shows it.
export function within<T extends { path: string; from?: string }>(repo: Repo, c: T): T {
  if (!repo.path) return c
  const strip = (p: string) => (p.startsWith(repo.path + '/') ? p.slice(repo.path.length + 1) : p)
  return { ...c, path: strip(c.path), ...(c.from !== undefined && { from: strip(c.from) }) }
}
