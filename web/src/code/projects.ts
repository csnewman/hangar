import { useEffect, useMemo, useRef, useState } from 'react'

import { CodeClient, type Workspace } from './client'
import type { Languages } from './lsp'

// Project is one of the workspace's folders, as the Code tab works in it:
// its own connection to the Code service, which watches its files, reports
// its repositories and searches it, and its own language servers.
export type Project = { root: string; name: string; client: CodeClient; languages: Languages }

export const projectName = (root: string) => root.slice(root.lastIndexOf('/') + 1) || root

// useWorkspace is the environment's projects as the service keeps them,
// kept up as any tab changes them. Until they are first set they are the
// environment's folder and the template's other repositories (seed). A tab
// working in one folder alone shows that folder only. Folders that do not
// exist are kept apart (missing), to be shown as such and removed; a
// template's repository not there is left out.
export function useWorkspace(
  client: CodeClient,
  alone: boolean,
  seed: string[],
): { folders: string[]; present: string[]; missing: string[]; set: ((folders: string[]) => Promise<void>) | null } {
  const [w, setW] = useState<Workspace | null>(null)
  useEffect(() => {
    let live = true
    const load = () =>
      client.workspace().then(
        (x) => live && setW(x),
        () => {},
      )
    const offState = client.on('state', (s) => s === 'live' && load())
    const offChanged = client.on('workspace.changed', (x) => setW(x))
    client.connected().then(load, () => {})
    return () => {
      live = false
      offState()
      offChanged()
    }
  }, [client])
  const seedKey = seed.join('\n')
  const folders = useMemo(() => {
    if (alone || !w) return client.root ? [client.root] : []
    if (w.saved) return w.folders
    return [...new Set([...w.folders, ...seedKey.split('\n').filter(Boolean)])]
    // client.root is read once the workspace is, which follows the connection.
  }, [alone, w, client.root, seedKey])
  // Whether each folder is there, looked at as the list changes.
  const [there, setThere] = useState<Record<string, boolean>>({})
  const listed = folders.join('\n')
  useEffect(() => {
    let live = true
    const all = listed ? listed.split('\n') : []
    Promise.all(
      all.map((f) =>
        client.list(f).then(
          () => [f, true] as const,
          () => [f, false] as const,
        ),
      ),
    ).then((r) => live && setThere(Object.fromEntries(r)))
    return () => {
      live = false
    }
  }, [client, listed])
  const saved = !!w?.saved
  const present = useMemo(() => folders.filter((f) => there[f]), [folders, there])
  const missing = useMemo(() => (saved ? folders.filter((f) => there[f] === false) : []), [folders, there, saved])
  const set = useMemo(
    () =>
      alone
        ? null
        : async (next: string[]) => {
            setW(await client.setWorkspace(next))
          },
    [alone, client],
  )
  return { folders, present, missing, set }
}

// useProjects keeps a connection and language servers for each folder: the
// tab's own connection for its own folder, a new one for each other,
// closed once the folder leaves the workspace.
export function useProjects(
  primary: CodeClient,
  env: string,
  folders: string[],
  languagesFor: (client: CodeClient, root: string) => Languages,
): Project[] {
  const made = useRef(new Map<string, Project>())
  const languagesRef = useRef(languagesFor)
  languagesRef.current = languagesFor

  const key = folders.join('\n')
  const projects = useMemo(() => {
    const want = key ? key.split('\n') : []
    const out: Project[] = []
    for (const root of want) {
      let p = made.current.get(root)
      if (!p) {
        const client = root === primary.root ? primary : new CodeClient(env, root)
        p = { root, name: projectName(root), client, languages: languagesRef.current(client, root) }
        made.current.set(root, p)
      }
      out.push(p)
    }
    return out
  }, [key, primary, env])

  // Folders gone from the workspace: their connections closed.
  useEffect(() => {
    const keep = new Set(projects.map((p) => p.root))
    for (const [root, p] of made.current) {
      if (keep.has(root)) continue
      if (p.client !== primary) p.client.close()
      made.current.delete(root)
    }
  }, [projects, primary])
  useEffect(
    () => () => {
      for (const p of made.current.values()) if (p.client !== primary) p.client.close()
      made.current.clear()
    },
    [primary],
  )
  return projects
}

// locate finds the project an absolute path is in, the innermost if
// projects nest, and the path from its folder; a path in none is itself.
export function locate(projects: Project[], abs: string): { root?: string; path: string } {
  let best: Project | null = null
  for (const p of projects) {
    if ((abs === p.root || abs.startsWith(p.root + '/')) && (!best || p.root.length > best.root.length)) best = p
  }
  if (!best) return { path: abs }
  return { root: best.root, path: abs === best.root ? '' : abs.slice(best.root.length + 1) }
}
