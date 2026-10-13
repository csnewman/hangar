import { type ChildProcessWithoutNullStreams } from 'node:child_process'
import { accessSync, constants, existsSync } from 'node:fs'
import { homedir } from 'node:os'
import { delimiter, join } from 'node:path'
import { pathToFileURL } from 'node:url'

import { spawn } from './child.js'

// A language server Hangar knows how to start: its command, the arguments
// that have it speak LSP on stdin and stdout, the LSP language IDs it
// serves, and the usual way to install it. The first one installed for a
// language is the one used.
type ServerSpec = { command: string; args: string[]; languages: string[]; install?: string }

const servers: ServerSpec[] = [
	{ command: 'gopls', args: [], languages: ['go'], install: 'go install golang.org/x/tools/gopls@latest' },
	{
		command: 'typescript-language-server',
		args: ['--stdio'],
		languages: ['typescript', 'typescriptreact', 'javascript', 'javascriptreact'],
		install: 'npm install -g typescript-language-server typescript',
	},
	{ command: 'pyright-langserver', args: ['--stdio'], languages: ['python'], install: 'npm install -g pyright' },
	{ command: 'basedpyright-langserver', args: ['--stdio'], languages: ['python'], install: 'pip install basedpyright' },
	{ command: 'pylsp', args: [], languages: ['python'], install: 'pip install python-lsp-server' },
	{ command: 'rust-analyzer', args: [], languages: ['rust'], install: 'rustup component add rust-analyzer' },
	{ command: 'clangd', args: [], languages: ['c', 'cpp'], install: 'sudo apt-get install -y clangd' },
	{
		command: 'bash-language-server',
		args: ['start'],
		languages: ['shellscript'],
		install: 'npm install -g bash-language-server',
	},
	{
		command: 'yaml-language-server',
		args: ['--stdio'],
		languages: ['yaml'],
		install: 'npm install -g yaml-language-server',
	},
	{
		command: 'vscode-json-language-server',
		args: ['--stdio'],
		languages: ['json'],
		install: 'npm install -g vscode-langservers-extracted',
	},
	{ command: 'jdtls', args: [], languages: ['java'] },
	{ command: 'zls', args: [], languages: ['zig'] },
]

// Missing is why a language has no server, and the servers that would
// serve it, with how each is installed when Hangar knows.
export type Missing = { missing: string; install: { command: string; install?: string }[] }

// The service's PATH is the system's; language servers are as often in
// the user's own directories, where their installers put them.
function searchPath(): string[] {
	const home = homedir()
	return [
		...(process.env.PATH ?? '').split(delimiter),
		join(home, 'go', 'bin'),
		join(home, '.local', 'bin'),
		join(home, '.cargo', 'bin'),
		join(home, '.npm-global', 'bin'),
		'/usr/local/go/bin',
	].filter(Boolean)
}

function which(command: string): string | null {
	for (const dir of searchPath()) {
		const p = join(dir, command)
		try {
			accessSync(p, constants.X_OK)
			return p
		} catch {
			// Not here.
		}
	}
	return null
}

// ServerSetting is a language server named in the settings' languageServers,
// by language: the command and its arguments, which come before Hangar's own
// for that language.
type ServerSetting = { command?: unknown; args?: unknown }

function fromSettings(language: string, settings: Record<string, unknown>): ServerSpec[] {
	const own = (settings.languageServers as Record<string, ServerSetting> | undefined)?.[language]
	if (!own || typeof own.command !== 'string' || !own.command) return []
	const args = Array.isArray(own.args) ? own.args.filter((a): a is string => typeof a === 'string') : []
	return [{ command: own.command, args, languages: [language] }]
}

// find is the server to start for a language, with where its command is,
// or why there is none: the settings' choice first, then Hangar's.
export function find(language: string, settings: Record<string, unknown> = {}): { spec: ServerSpec; path: string } | Missing {
	const specs = [...fromSettings(language, settings), ...servers.filter((s) => s.languages.includes(language))]
	if (specs.length === 0) return { missing: `Hangar knows no language server for ${language}`, install: [] }
	for (const spec of specs) {
		const path = spec.command.includes('/') ? spec.command : which(spec.command)
		if (path) return { spec, path }
	}
	return {
		missing: `no language server for ${language} is installed (looked for ${specs.map((s) => s.command).join(', ')})`,
		install: specs.map((s) => ({ command: s.command, install: s.install })),
	}
}

// Message is a JSON-RPC message, less its version: a request (id and
// method), a notification (method) or a response (id, and result or error).
export type Message = { id?: string | number; method?: string; params?: any; result?: unknown; error?: unknown }

// Requests a server makes of its client that the browser's client does
// not answer, answered here so the server carries on: registering for
// capabilities (file changes are sent regardless), its settings (none),
// and progress reports (not shown).
const answered: Record<string, (params: unknown) => unknown> = {
	'client/registerCapability': () => null,
	'client/unregisterCapability': () => null,
	'window/workDoneProgress/create': () => null,
	'workspace/configuration': (params) => ((params as { items?: unknown[] })?.items ?? []).map(() => null),
	'workspace/workspaceFolders': () => null,
}

// Server is one language server process, speaking LSP's framing on its
// stdio. Messages are passed whole, as JSON text; the requests in answered
// are answered here.
export class Server {
	private proc: ChildProcessWithoutNullStreams
	private buf: Buffer = Buffer.alloc(0)
	// What it has written to stderr, its log, the latest 64 KiB.
	private stderr = ''
	private exited = false
	readonly path: string
	readonly args: string[]
	readonly started = Date.now()

	private onMessage: (msg: Message) => void
	private onExit: (why: string) => void

	constructor(root: string, found: { spec: ServerSpec; path: string }, onMessage: (msg: Message) => void, onExit: (why: string) => void) {
		this.onMessage = onMessage
		this.onExit = onExit
		this.path = found.path
		this.args = found.spec.args
		const env = { ...process.env, PATH: searchPath().join(delimiter) }
		this.proc = spawn(found.path, found.spec.args, { cwd: root, env, stdio: ['pipe', 'pipe', 'pipe'] })
		this.proc.stdout.on('data', (chunk: Buffer) => this.receive(chunk))
		this.proc.stderr.setEncoding('utf8')
		this.proc.stderr.on('data', (s: string) => {
			this.stderr = (this.stderr + s).slice(-(64 << 10))
		})
		this.proc.on('error', (e) => this.exit(e.message))
		this.proc.on('exit', (code, signal) => this.exit(signal ? `stopped by ${signal}` : `exited with ${code}`))
		this.proc.stdin.on('error', () => {})
	}

	private exit(why: string) {
		if (this.exited) return
		this.exited = true
		const tail = this.stderr.trim().split('\n').slice(-3).join('\n')
		this.onExit(tail ? `${why}: ${tail}` : why)
	}

	private receive(chunk: Buffer) {
		this.buf = this.buf.length ? Buffer.concat([this.buf, chunk]) : chunk
		for (;;) {
			const end = this.buf.indexOf('\r\n\r\n')
			if (end < 0) return
			const head = this.buf.subarray(0, end).toString('ascii')
			const len = Number(/Content-Length:\s*(\d+)/i.exec(head)?.[1])
			if (!Number.isFinite(len)) {
				// Not LSP: drop what was read, and hope to find a header.
				this.buf = this.buf.subarray(end + 4)
				continue
			}
			if (this.buf.length < end + 4 + len) return
			const body = this.buf.subarray(end + 4, end + 4 + len).toString('utf8')
			this.buf = this.buf.subarray(end + 4 + len)
			this.fromServer(body)
		}
	}

	private fromServer(body: string) {
		let msg: Message
		try {
			msg = JSON.parse(body)
		} catch {
			return
		}
		const answer = msg.method !== undefined && msg.id !== undefined ? answered[msg.method] : undefined
		if (answer) {
			this.send({ id: msg.id, result: answer(msg.params) })
			return
		}
		this.onMessage(msg)
	}

	// send writes a message to the server.
	send(msg: Message) {
		if (this.exited) return
		const body = Buffer.from(JSON.stringify({ jsonrpc: '2.0', ...msg }), 'utf8')
		this.proc.stdin.write(`Content-Length: ${body.length}\r\n\r\n`)
		this.proc.stdin.write(body)
	}

	// filesChanged tells the server of files changed on disk, which it may
	// not be watching itself: a checkout, another tab's saves.
	filesChanged(paths: string[]) {
		if (paths.length === 0) return
		this.send({
			method: 'workspace/didChangeWatchedFiles',
			params: { changes: paths.map((p) => ({ uri: pathToFileURL(p).href, type: existsSync(p) ? 2 : 3 })) },
		})
	}

	get pid(): number | undefined {
		return this.proc.pid
	}

	get log(): string {
		return this.stderr
	}

	// kill ends the server at once, for a service that is itself stopping.
	kill() {
		if (!this.exited) this.proc.kill('SIGTERM')
	}

	stop() {
		if (this.exited) return
		// Asked nicely first, as LSP has it; then told.
		this.send({ id: 'hangar-shutdown', method: 'shutdown' })
		this.send({ method: 'exit' })
		const t = setTimeout(() => this.proc.kill('SIGKILL'), 3000)
		this.proc.once('exit', () => clearTimeout(t))
	}
}
