import { mkdir, readFile, rename, writeFile } from 'node:fs/promises'
import { homedir } from 'node:os'
import { dirname, join } from 'node:path'

// The Code tab's settings are a file in the user's home, which the profile
// shares by default, so they follow the person to each environment.
export const settingsPath = join(homedir(), '.config', 'hangar', 'code.json')

// The largest settings file kept.
const MAX_SIZE = 64 << 10

export type Settings = Record<string, unknown>

// SettingsFile reads and writes the settings, telling each listener when
// any tab changes them.
export class SettingsFile {
	private listeners = new Set<(s: Settings) => void>()

	constructor(private path = settingsPath) {}

	async get(): Promise<Settings> {
		try {
			const v = JSON.parse(await readFile(this.path, 'utf8'))
			return v && typeof v === 'object' && !Array.isArray(v) ? v : {}
		} catch {
			return {}
		}
	}

	// set replaces the settings, written beside the file and renamed over
	// it, so a reader never sees half of them.
	async set(s: unknown): Promise<void> {
		if (!s || typeof s !== 'object' || Array.isArray(s)) throw new Error('settings are an object')
		const text = JSON.stringify(s, null, 2) + '\n'
		if (text.length > MAX_SIZE) throw new Error('the settings are too large')
		await mkdir(dirname(this.path), { recursive: true })
		const tmp = `${this.path}.${process.pid}.tmp`
		await writeFile(tmp, text, { mode: 0o644 })
		await rename(tmp, this.path)
		for (const fn of this.listeners) fn(s as Settings)
	}

	onChange(fn: (s: Settings) => void): () => void {
		this.listeners.add(fn)
		return () => this.listeners.delete(fn)
	}
}
