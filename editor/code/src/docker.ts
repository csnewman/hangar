import { execFile } from 'node:child_process'
import { request, type IncomingMessage } from 'node:http'

// The Docker Engine's socket: DOCKER_HOST's, when it names a unix socket,
// else the usual one.
function socketPath(): string {
	const host = process.env.DOCKER_HOST ?? ''
	return host.startsWith('unix://') ? host.slice('unix://'.length) : '/var/run/docker.sock'
}

// The Engine API version spoken; any engine since Docker 20.10 has it.
const API = '/v1.41'

export type Container = {
	id: string
	name: string
	image: string
	// running, exited, paused, created, restarting, removing or dead.
	state: string
	// Docker's own description: "Up 3 minutes", "Exited (0) 2 hours ago".
	status: string
	ports: string[]
	created: number
	// The command it runs.
	command: string
	// The Compose project and service it belongs to, if any, and where the
	// project's files are.
	project?: string
	service?: string
	workingDir?: string
	configFiles?: string[]
}

export type Image = { id: string; tags: string[]; size: number; created: number; containers: number }

export type Overview = { available: boolean; why?: string; containers: Container[]; images: Image[] }

type Res = { status: number; body: string }

function call(method: string, path: string, body?: unknown): Promise<Res> {
	return new Promise((resolve, reject) => {
		const req = request(
			{ socketPath: socketPath(), path: API + path, method, headers: body ? { 'content-type': 'application/json' } : {} },
			(res) => {
				let data = ''
				res.setEncoding('utf8')
				res.on('data', (c: string) => (data += c))
				res.on('end', () => resolve({ status: res.statusCode ?? 0, body: data }))
			},
		)
		req.on('error', reject)
		req.setTimeout(30_000, () => req.destroy(new Error('Docker did not answer')))
		if (body) req.write(JSON.stringify(body))
		req.end()
	})
}

async function json<T>(method: string, path: string): Promise<T> {
	const res = await call(method, path)
	if (res.status >= 300) throw new Error(engineError(res))
	return JSON.parse(res.body) as T
}

function engineError(res: Res): string {
	try {
		return JSON.parse(res.body).message ?? `Docker answered ${res.status}`
	} catch {
		return `Docker answered ${res.status}`
	}
}

// why says what stops the service reaching Docker, in words for a person.
function why(e: unknown): string {
	const code = (e as NodeJS.ErrnoException)?.code
	if (code === 'ENOENT' || code === 'ECONNREFUSED') return 'Docker is not running in this environment'
	if (code === 'EACCES') return 'this user may not use Docker (not in the docker group)'
	return e instanceof Error ? e.message : String(e)
}

type ApiContainer = {
	Id: string
	Names: string[]
	Image: string
	State: string
	Status: string
	Command: string
	Created: number
	Ports: { IP?: string; PrivatePort: number; PublicPort?: number; Type: string }[]
	Labels: Record<string, string>
}

type ApiImage = { Id: string; RepoTags: string[] | null; Size: number; Created: number; Containers: number }

function ports(c: ApiContainer): string[] {
	const out = new Set<string>()
	for (const p of c.Ports ?? []) {
		// IPv4 and IPv6 bindings of one port are one port to a person.
		if (p.PublicPort) out.add(`${p.PublicPort}→${p.PrivatePort}/${p.Type}`)
		else out.add(`${p.PrivatePort}/${p.Type}`)
	}
	return [...out]
}

export type Action = 'start' | 'stop' | 'restart' | 'pause' | 'unpause' | 'remove'

// Docker is the environment's Docker: its containers and images, what
// can be done to them, their logs, and word of their changes.
export class Docker {
	private listeners = new Set<() => void>()
	private events: IncomingMessage | undefined
	private retry: NodeJS.Timeout | undefined
	private timer: NodeJS.Timeout | undefined

	async overview(): Promise<Overview> {
		try {
			const [cs, is] = await Promise.all([
				json<ApiContainer[]>('GET', '/containers/json?all=1'),
				json<ApiImage[]>('GET', '/images/json'),
			])
			const containers = cs.map(
				(c): Container => ({
					id: c.Id,
					name: (c.Names?.[0] ?? c.Id.slice(0, 12)).replace(/^\//, ''),
					image: c.Image,
					state: c.State,
					status: c.Status,
					ports: ports(c),
					created: c.Created,
					command: c.Command ?? '',
					project: c.Labels?.['com.docker.compose.project'],
					service: c.Labels?.['com.docker.compose.service'],
					workingDir: c.Labels?.['com.docker.compose.project.working_dir'],
					configFiles: c.Labels?.['com.docker.compose.project.config_files']
						?.split(',')
						.map((f) => f.trim())
						.filter(Boolean),
				}),
			)
			containers.sort((a, b) => (a.project ?? '').localeCompare(b.project ?? '') || a.name.localeCompare(b.name))
			const images = is
				.map(
					(i): Image => ({
						id: i.Id,
						tags: (i.RepoTags ?? []).filter((t) => t !== '<none>:<none>'),
						size: i.Size,
						created: i.Created,
						containers: i.Containers,
					}),
				)
				.sort((a, b) => b.created - a.created)
			return { available: true, containers, images }
		} catch (e) {
			return { available: false, why: why(e), containers: [], images: [] }
		}
	}

	async act(id: string, action: Action) {
		if (!/^[0-9a-f]{12,64}$/.test(id)) throw new Error('not a container')
		const res =
			action === 'remove'
				? await call('DELETE', `/containers/${id}?force=1`)
				: await call('POST', `/containers/${id}/${action}`)
		// 304: already as asked.
		if (res.status >= 300 && res.status !== 304) throw new Error(engineError(res))
	}

	// project does to a Compose project what its containers can be done
	// to, each in turn, or takes it down as Compose does: its containers
	// and networks removed.
	async project(name: string, action: 'start' | 'stop' | 'restart' | 'down') {
		if (!/^[a-z0-9][a-z0-9_-]*$/.test(name)) throw new Error('not a Compose project')
		if (action === 'down') {
			await new Promise<void>((resolve, reject) =>
				execFile('docker', ['compose', '-p', name, 'down'], { timeout: 120_000 }, (err, _out, stderr) =>
					err ? reject(new Error(stderr.trim().split('\n').pop() || err.message)) : resolve(),
				),
			)
			return
		}
		const filters = encodeURIComponent(JSON.stringify({ label: [`com.docker.compose.project=${name}`] }))
		const cs = await json<ApiContainer[]>('GET', `/containers/json?all=1&filters=${filters}`)
		const errors: string[] = []
		for (const c of cs) {
			try {
				await this.act(c.Id, action)
			} catch (e) {
				errors.push(`${(c.Names?.[0] ?? c.Id).replace(/^\//, '')}: ${e instanceof Error ? e.message : String(e)}`)
			}
		}
		if (errors.length) throw new Error(errors.join('; '))
	}

	async removeImage(id: string) {
		if (!/^sha256:[0-9a-f]{64}$/.test(id)) throw new Error('not an image')
		const res = await call('DELETE', `/images/${id}`)
		if (res.status >= 300) throw new Error(engineError(res))
	}

	// logs follows a container's output: the last lines of it, then what it
	// writes, until the returned function is called.
	logs(id: string, tail: number, onText: (text: string) => void, onEnd: (why: string) => void): () => void {
		let closed = false
		let res: IncomingMessage | undefined
		const end = (why: string) => {
			if (closed) return
			closed = true
			res?.destroy()
			onEnd(why)
		}
		;(async () => {
			if (!/^[0-9a-f]{12,64}$/.test(id)) return end('not a container')
			let tty = false
			try {
				tty = (await json<{ Config: { Tty: boolean } }>('GET', `/containers/${id}/json`)).Config.Tty
			} catch (e) {
				return end(why(e))
			}
			const req = request(
				{
					socketPath: socketPath(),
					path: `${API}/containers/${id}/logs?stdout=1&stderr=1&follow=1&timestamps=0&tail=${Math.max(0, Math.min(tail, 10000))}`,
				},
				(r) => {
					res = r
					if (closed) return r.destroy()
					if ((r.statusCode ?? 0) >= 300) return end(`Docker answered ${r.statusCode}`)
					// Without a TTY, the stream is framed: a byte saying which
					// stream, three empty, the length, then that much output.
					let buf: Buffer = Buffer.alloc(0)
					r.on('data', (c: Buffer) => {
						if (tty) return onText(c.toString('utf8'))
						buf = buf.length ? Buffer.concat([buf, c]) : c
						let out = ''
						while (buf.length >= 8) {
							const len = buf.readUInt32BE(4)
							if (buf.length < 8 + len) break
							out += buf.subarray(8, 8 + len).toString('utf8')
							buf = buf.subarray(8 + len)
						}
						if (out) onText(out)
					})
					r.on('end', () => end('the container stopped'))
					r.on('error', (e) => end(why(e)))
				},
			)
			req.on('error', (e) => end(why(e)))
			req.end()
		})()
		return () => end('')
	}

	// onChange calls fn, a moment after Docker reports anything happening to
	// a container or image. Docker is watched while anyone listens.
	onChange(fn: () => void): () => void {
		this.listeners.add(fn)
		if (this.listeners.size === 1) this.watch()
		return () => {
			this.listeners.delete(fn)
			if (this.listeners.size === 0) {
				clearTimeout(this.retry)
				this.events?.destroy()
				this.events = undefined
			}
		}
	}

	private watch() {
		const filters = encodeURIComponent(JSON.stringify({ type: ['container', 'image'] }))
		const req = request({ socketPath: socketPath(), path: `${API}/events?filters=${filters}` }, (res) => {
			if (this.listeners.size === 0) return res.destroy()
			this.events = res
			res.on('data', () => {
				clearTimeout(this.timer)
				this.timer = setTimeout(() => {
					for (const fn of this.listeners) fn()
				}, 200)
			})
			res.on('end', () => this.again())
			res.on('error', () => this.again())
		})
		req.on('error', () => this.again())
		req.end()
	}

	// again watches anew after Docker went away, as it does on a restart.
	private again() {
		this.events = undefined
		if (this.listeners.size === 0) return
		clearTimeout(this.retry)
		this.retry = setTimeout(() => this.watch(), 5000)
	}
}
