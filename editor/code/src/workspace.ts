import { mkdir, readFile, rename, stat, writeFile } from 'node:fs/promises'
import { homedir } from 'node:os'
import { dirname, isAbsolute, join, resolve } from 'node:path'

import { scan } from './repos.js'

// The workspace is the environment's list of projects: the folders the
// Code tab shows side by side, each with its own files, repositories and
// language servers. It is the environment's, not the person's, so it is
// kept in the guest outside what the profile shares, and everyone using
// the environment sees the same list.
export const workspacePath = join(homedir(), '.local', 'state', 'hangar', 'workspace.json')

// The most projects a workspace holds.
const MAX_FOLDERS = 32

export type WorkspaceState = {
	folders: string[]
	// saved is whether the list has been set; until it has, it is the
	// service's own folder and the browser offers the template's
	// repositories.
	saved: boolean
}

// WorkspaceFile reads and writes the workspace, telling each listener when
// any tab changes it.
export class WorkspaceFile {
	private listeners = new Set<(w: WorkspaceState) => void>()

	constructor(
		private root: string,
		private path = workspacePath,
	) {}

	async get(): Promise<WorkspaceState> {
		try {
			const v = JSON.parse(await readFile(this.path, 'utf8'))
			if (Array.isArray(v?.folders)) {
				const folders = v.folders.filter((f: unknown): f is string => typeof f === 'string' && isAbsolute(f))
				return { folders, saved: true }
			}
		} catch {
			// None yet, or unreadable: the service's own folder.
		}
		return { folders: [this.root], saved: false }
	}

	// set replaces the list: each an absolute path to a folder, once each.
	async set(folders: unknown): Promise<WorkspaceState> {
		if (!Array.isArray(folders)) throw new Error('folders is a list')
		const out: string[] = []
		for (const f of folders) {
			if (typeof f !== 'string' || !isAbsolute(f)) throw new Error(`${String(f)} is not an absolute path`)
			const dir = resolve(f)
			let st
			try {
				st = await stat(dir)
			} catch {
				throw new Error(`${dir} does not exist`)
			}
			if (!st.isDirectory()) throw new Error(`${dir} is not a folder`)
			if (!out.includes(dir)) out.push(dir)
		}
		if (out.length > MAX_FOLDERS) throw new Error(`a workspace holds at most ${MAX_FOLDERS} folders`)
		await mkdir(dirname(this.path), { recursive: true })
		const tmp = `${this.path}.${process.pid}.tmp`
		await writeFile(tmp, JSON.stringify({ folders: out }, null, 2) + '\n', { mode: 0o644 })
		await rename(tmp, this.path)
		const state = { folders: out, saved: true }
		for (const fn of this.listeners) fn(state)
		return state
	}

	onChange(fn: (w: WorkspaceState) => void): () => void {
		this.listeners.add(fn)
		return () => this.listeners.delete(fn)
	}

	// repositories are the git repositories in the home folder, for adding
	// to the workspace: those a few folders down, not inside another.
	repositories(): Promise<string[]> {
		return scan(homedir())
	}
}
