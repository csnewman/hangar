import { mkdir, readFile, rename, stat, writeFile } from 'node:fs/promises'
import { homedir } from 'node:os'
import { dirname, isAbsolute, resolve } from 'node:path'

import { scan } from './repos.js'

// The workspace is the environment's list of projects: the folders the
// Code tab shows side by side, each with its own files, repositories and
// language servers. It is the environment's, not the person's, so it is
// kept in the guest outside what the profile shares, and everyone using
// the environment sees the same list. It is a VS Code workspace file, which
// VS Code opens too, and the worker writes it first with the projects the
// environment's template names (api.WorkspaceFile); HANGAR_WORKSPACE names
// another, as the tests do.
export const workspacePath = process.env.HANGAR_WORKSPACE || '/var/lib/hangar-workspace/projects.code-workspace'

// The most projects a workspace holds.
const MAX_FOLDERS = 32

export type WorkspaceState = {
	folders: string[]
	// saved is whether the list has been set, by the worker from the
	// template's projects or by a tab; until it has, it is the service's
	// own folder.
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
				// A folder's path may be relative to the file, as VS Code
				// allows.
				const folders = v.folders
					.map((f: { path?: unknown }) => f?.path)
					.filter((f: unknown): f is string => typeof f === 'string' && f !== '')
					.map((f: string) => resolve(dirname(this.path), f))
				if (folders.length) return { folders, saved: true }
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
		// Whatever else VS Code keeps in the file, its settings say, stays.
		let kept: Record<string, unknown> = {}
		try {
			const v = JSON.parse(await readFile(this.path, 'utf8'))
			if (v && typeof v === 'object' && !Array.isArray(v)) kept = v
		} catch {
			// A new file.
		}
		await mkdir(dirname(this.path), { recursive: true })
		const tmp = `${this.path}.${process.pid}.tmp`
		const body = { ...kept, folders: out.map((path) => ({ path })) }
		await writeFile(tmp, JSON.stringify(body, null, 2) + '\n', { mode: 0o644 })
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
