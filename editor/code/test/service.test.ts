// Runs the built service as the agent does and talks to it as two browser
// windows would: files, git, and one shared document edited from both sides
// and from the disk.
import { execFileSync, spawn } from 'node:child_process'
import { existsSync, mkdirSync, mkdtempSync, readFileSync, renameSync, rmSync, writeFileSync } from 'node:fs'
import { createServer } from 'node:http'
import { connect, type Socket } from 'node:net'
import { tmpdir } from 'node:os'
import { join } from 'node:path'
import { after, before, test } from 'node:test'
import assert from 'node:assert/strict'

import * as decoding from 'lib0/decoding'
import * as encoding from 'lib0/encoding'
import * as syncProtocol from 'y-protocols/sync'
import * as Y from 'yjs'

// Git here, and in the service the test starts, sees none of the machine's
// own configuration.
Object.assign(process.env, {
	GIT_CONFIG_GLOBAL: '/dev/null',
	GIT_CONFIG_NOSYSTEM: '1',
	GIT_AUTHOR_NAME: 'test',
	GIT_AUTHOR_EMAIL: 'test@localhost',
	GIT_COMMITTER_NAME: 'test',
	GIT_COMMITTER_EMAIL: 'test@localhost',
})

const dir = mkdtempSync(join(tmpdir(), 'hangar-code-'))
const root = join(dir, 'repo')
const sock = join(dir, 'code.sock')
const dockerSock = join(dir, 'docker.sock')
const git = (...args: string[]) =>
	execFileSync('git', ['-C', root, ...args])

let proc: ReturnType<typeof spawn>

before(async () => {
	execFileSync('git', ['init', '-q', '-b', 'main', root])
	writeFileSync(join(root, 'a.txt'), 'one\r\ntwo\r\n')
	git('add', '.')
	git('commit', '-q', '-m', 'first')
	proc = spawn(process.execPath, [join(import.meta.dirname, '..', 'dist', 'code.mjs'), '--socket', sock, '--root', root], {
		stdio: 'inherit',
		// The settings file is in the home directory; Docker is the fake
		// engine the Docker test serves.
		env: { ...process.env, HOME: dir, DOCKER_HOST: `unix://${dockerSock}` },
	})
	for (let i = 0; i < 100; i++) {
		try {
			await new Promise<void>((res, rej) => connect(sock).once('connect', function (this: Socket) { this.destroy(); res() }).once('error', rej))
			return
		} catch {
			await new Promise((r) => setTimeout(r, 50))
		}
	}
	throw new Error('the service did not start')
})

after(() => proc.kill())

// Client is one window: framed messages, JSON requests, and Yjs documents.
class Client {
	private sock: Socket
	private buf = Buffer.alloc(0)
	private next = 1
	private waiting = new Map<number, (m: any) => void>()
	events: any[] = []
	docs = new Map<number, Y.Doc>()
	// Language servers' messages, by server.
	lsp: { server: number; msg: any }[] = []

	constructor() {
		this.sock = connect(sock)
		this.sock.on('data', (c) => {
			this.buf = Buffer.concat([this.buf, c])
			while (this.buf.length >= 4 && this.buf.length >= 4 + this.buf.readUInt32BE(0)) {
				const f = this.buf.subarray(4, 4 + this.buf.readUInt32BE(0))
				this.buf = this.buf.subarray(4 + f.length)
				this.frame(f)
			}
		})
	}

	private write(frame: Buffer) {
		const len = Buffer.alloc(4)
		len.writeUInt32BE(frame.length)
		this.sock.write(Buffer.concat([len, frame]))
	}

	private frame(f: Buffer) {
		if (f[0] === 0x72) {
			const m = JSON.parse(f.subarray(1).toString())
			if (m.id) this.waiting.get(m.id)?.(m)
			else this.events.push(m)
		} else if (f[0] === 0x6c) {
			this.lsp.push({ server: f.readUInt32BE(1), msg: JSON.parse(f.subarray(5).toString()) })
		} else if (f[0] === 0x79) {
			const n = f.readUInt32BE(1)
			const doc = this.docs.get(n)
			if (!doc) return
			const dec = decoding.createDecoder(f.subarray(5))
			if (decoding.readVarUint(dec) !== 0) return
			const enc = encoding.createEncoder()
			encoding.writeVarUint(enc, 0)
			syncProtocol.readSyncMessage(dec, enc, doc, 'server')
			if (encoding.length(enc) > 1) this.sendYjs(n, encoding.toUint8Array(enc))
		}
	}

	private sendYjs(n: number, msg: Uint8Array) {
		const hdr = Buffer.alloc(5)
		hdr[0] = 0x79
		hdr.writeUInt32BE(n, 1)
		this.write(Buffer.concat([hdr, msg]))
	}

	call(method: string, params: Record<string, unknown> = {}): Promise<any> {
		const id = this.next++
		this.write(Buffer.concat([Buffer.from([0x72]), Buffer.from(JSON.stringify({ id, method, params }))]))
		return new Promise((res, rej) =>
			this.waiting.set(id, (m) => (m.error ? rej(new Error(m.error)) : res(m.result))),
		)
	}

	sendLsp(server: number, msg: unknown) {
		const hdr = Buffer.alloc(5)
		hdr[0] = 0x6c
		hdr.writeUInt32BE(server, 1)
		this.write(Buffer.concat([hdr, Buffer.from(JSON.stringify(msg))]))
	}

	async open(path: string): Promise<Y.Doc> {
		const { doc: n } = await this.call('doc.open', { path })
		const doc = new Y.Doc()
		this.docs.set(n, doc)
		doc.on('update', (u: Uint8Array, origin: unknown) => {
			if (origin === 'server') return
			const enc = encoding.createEncoder()
			encoding.writeVarUint(enc, 0)
			syncProtocol.writeUpdate(enc, u)
			this.sendYjs(n, encoding.toUint8Array(enc))
		})
		const enc = encoding.createEncoder()
		encoding.writeVarUint(enc, 0)
		syncProtocol.writeSyncStep1(enc, doc)
		this.sendYjs(n, encoding.toUint8Array(enc))
		return doc
	}

	close() {
		this.sock.destroy()
	}
}

async function until(what: string, ok: () => boolean) {
	for (let i = 0; i < 100; i++) {
		if (ok()) return
		await new Promise((r) => setTimeout(r, 50))
	}
	assert.fail(`timed out waiting for ${what}`)
}

test('files, git and a shared document', async () => {
	const a = new Client()
	const b = new Client()
	try {
		assert.deepEqual(await a.call('hello'), { root, home: root })
		const list = await a.call('fs.list', { path: '.' })
		assert.ok(list.some((e: any) => e.name === 'a.txt' && !e.dir))

		const st = await a.call('git.status')
		assert.equal(st.repo, true)
		assert.equal(st.branch, 'main')
		assert.equal(st.changes.length, 0)

		// Both windows open the file and see it, line endings as LF.
		const da = await a.open('a.txt')
		const db = await b.open('a.txt')
		await until('the document to load', () => da.getText('text').toString() === 'one\ntwo\n')
		await until('the second window to load', () => db.getText('text').toString() === 'one\ntwo\n')

		// An edit in one window reaches the other and, shortly, the disk,
		// with the file's CRLF endings kept.
		da.getText('text').insert(4, 'middle\n')
		await until('the edit to reach the other window', () => db.getText('text').toString() === 'one\nmiddle\ntwo\n')
		await until('the edit to reach the disk', () => readFileSync(join(root, 'a.txt'), 'utf8') === 'one\r\nmiddle\r\ntwo\r\n')

		// A change on disk -- an agent editing the file -- reaches both.
		writeFileSync(join(root, 'a.txt'), 'one\r\nmiddle\r\ntwo\r\nthree\r\n')
		await until('the disk change to reach a window', () => da.getText('text').toString() === 'one\nmiddle\ntwo\nthree\n')
		await until('the disk change to reach the other', () => db.getText('text').toString() === 'one\nmiddle\ntwo\nthree\n')

		const st2 = await a.call('git.status')
		assert.deepEqual(st2.changes.map((c: any) => [c.path, c.worktree]), [['a.txt', 'M']])
		const diff = await a.call('git.diff', { path: 'a.txt' })
		assert.equal(diff.head, 'one\r\ntwo\r\n')
		assert.equal(diff.working, 'one\r\nmiddle\r\ntwo\r\nthree\r\n')

		await a.call('git.stage', { path: 'a.txt' })
		await a.call('git.commit', { message: 'second' })
		assert.equal((await a.call('git.status')).changes.length, 0)

		await assert.rejects(a.call('doc.open', { path: 'missing.txt' }))

		// A file deleted while open is announced, and an edit afterwards does
		// not bring it back.
		writeFileSync(join(root, 'gone.txt'), 'soon gone\n')
		const dg = await a.open('gone.txt')
		await until('the file to load', () => dg.getText('text').toString() === 'soon gone\n')
		rmSync(join(root, 'gone.txt'))
		await until('the deletion to be announced', () =>
			a.events.some((e) => e.event === 'doc.deleted' && e.params.path === join(root, 'gone.txt')),
		)
		dg.getText('text').insert(0, 'typed after: ')
		await new Promise((r) => setTimeout(r, 1000))
		assert.equal(existsSync(join(root, 'gone.txt')), false)
	} finally {
		a.close()
		b.close()
	}
})

test('partial staging, amending, branches, push and pull', async () => {
	const a = new Client()
	try {
		// Stage part of a file: the index takes the content given, the
		// working tree keeps the rest.
		writeFileSync(join(root, 'p.txt'), 'one\ntwo\n')
		git('add', 'p.txt')
		git('commit', '-q', '-m', 'p')
		writeFileSync(join(root, 'p.txt'), 'ONE\ntwo\nthree\n')
		assert.deepEqual(await a.call('git.show', { path: 'p.txt', from: 'index' }), { text: 'one\ntwo\n' })
		await a.call('git.setIndex', { path: 'p.txt', content: 'ONE\ntwo\n' })
		assert.equal(git('show', ':p.txt').toString(), 'ONE\ntwo\n')
		assert.equal(readFileSync(join(root, 'p.txt'), 'utf8'), 'ONE\ntwo\nthree\n')
		assert.deepEqual(await a.call('git.show', { path: 'nothing.txt', from: 'HEAD' }), { text: null })

		// Amend with no message keeps the last one.
		await a.call('git.commit', { message: 'partial' })
		await a.call('git.stage', { path: 'p.txt' })
		await a.call('git.commit', { message: '', amend: true })
		assert.equal(await a.call('git.lastMessage'), 'partial')
		assert.equal(git('show', 'HEAD:p.txt').toString(), 'ONE\ntwo\nthree\n')
		await assert.rejects(a.call('git.commit', { message: ' ' }))

		// A branch, pushed to a remote it then tracks.
		const remote = join(dir, 'remote.git')
		execFileSync('git', ['init', '-q', '--bare', remote])
		git('remote', 'add', 'origin', remote)
		await a.call('git.createBranch', { name: 'feature' })
		await assert.rejects(a.call('git.createBranch', { name: 'bad..name' }))
		await a.call('git.push')
		let br = await a.call('git.branches')
		assert.equal(br.current, 'feature')
		assert.deepEqual(br.local.find((b: any) => b.name === 'feature'), { name: 'feature', upstream: 'origin/feature', ahead: 0, behind: 0 })
		assert.ok(br.remote.includes('origin/feature'))

		// Someone else pushes; fetch sees it, pull fast-forwards to it.
		const other = join(dir, 'other')
		execFileSync('git', ['clone', '-q', '-b', 'feature', remote, other])
		writeFileSync(join(other, 'theirs.txt'), 'theirs\n')
		execFileSync('git', ['-C', other, 'add', '.'])
		execFileSync('git', ['-C', other, 'commit', '-q', '-m', 'theirs'])
		execFileSync('git', ['-C', other, 'push', '-q'])
		await a.call('git.fetch')
		br = await a.call('git.branches')
		assert.equal(br.local.find((b: any) => b.name === 'feature').behind, 1)
		await a.call('git.pull')
		assert.equal(readFileSync(join(root, 'theirs.txt'), 'utf8'), 'theirs\n')

		// Back to main, and to a remote branch with no local one.
		await a.call('git.checkout', { name: 'main' })
		assert.equal((await a.call('git.status')).branch, 'main')
		execFileSync('git', ['-C', other, 'checkout', '-q', '-b', 'only-remote'])
		execFileSync('git', ['-C', other, 'push', '-q', 'origin', 'only-remote'])
		await a.call('git.fetch')
		await a.call('git.checkout', { name: 'origin/only-remote' })
		br = await a.call('git.branches')
		assert.equal(br.current, 'only-remote')
		assert.equal(br.local.find((b: any) => b.name === 'only-remote').upstream, 'origin/only-remote')
	} finally {
		a.close()
	}
})

test('reading git state is not announced as a change to it', async () => {
	const a = new Client()
	try {
		writeFileSync(join(root, 'touched.txt'), 'x\n')
		git('add', 'touched.txt')
		await new Promise((r) => setTimeout(r, 600))
		a.events.length = 0
		for (let i = 0; i < 3; i++) await a.call('git.status')
		await a.call('git.show', { path: 'touched.txt', from: 'index' })
		await new Promise((r) => setTimeout(r, 600))
		assert.deepEqual(a.events.filter((e) => e.event === 'git.changed'), [])
	} finally {
		a.close()
	}
})

test('a tab working in another folder, searching it', async () => {
	mkdirSync(join(root, 'sub', 'deep'), { recursive: true })
	writeFileSync(join(root, 'sub', 'one.go'), 'package sub\n\nfunc FindMe() {}\n')
	writeFileSync(join(root, 'sub', 'deep', 'two.go'), 'package deep\n// findme too\n')
	writeFileSync(join(root, 'top.txt'), 'FindMe outside\n')
	const a = new Client()
	try {
		await assert.rejects(a.call('hello', { root: 'sub' }), /absolute/)
		await assert.rejects(a.call('hello', { root: join(root, 'missing') }), /does not exist/)
		assert.deepEqual(await a.call('hello', { root: join(root, 'sub') }), { root: join(root, 'sub'), home: root })
		const list = await a.call('fs.list', { path: '.' })
		assert.deepEqual(list.map((e: any) => e.name), ['deep', 'one.go'])

		// Git's paths are the folder's, and only what is within it.
		const st = await a.call('git.status')
		const paths = st.changes.map((c: any) => c.path).sort()
		assert.deepEqual(paths, ['deep/two.go', 'one.go'])

		const { files } = await a.call('search.files')
		assert.deepEqual(files, ['deep/two.go', 'one.go'])

		const found = await a.call('search.text', { query: 'findme' })
		assert.deepEqual(
			found.matches.map((m: any) => [m.path, m.line, m.text.slice(m.ranges[0][0], m.ranges[0][1])]).sort(),
			[
				['deep/two.go', 2, 'findme'],
				['one.go', 3, 'FindMe'],
			],
		)
		const exact = await a.call('search.text', { query: 'FindMe', caseSensitive: true, mask: '*.go, !deep/*' })
		assert.deepEqual(exact.matches.map((m: any) => m.path), ['one.go'])
		const re = await a.call('search.text', { query: 'func \\w+\\(', regex: true })
		assert.deepEqual(re.matches.map((m: any) => m.text), ['func FindMe() {}'])

		assert.equal((await a.call('fs.read', { path: 'one.go' })).text, 'package sub\n\nfunc FindMe() {}\n')
	} finally {
		a.close()
		rmSync(join(root, 'sub'), { recursive: true })
		rmSync(join(root, 'top.txt'))
	}
})

test('settings are shared by every tab, in the home directory', async () => {
	const a = new Client()
	const b = new Client()
	try {
		await a.call('hello')
		await b.call('hello')
		assert.deepEqual(await a.call('settings.get'), {})
		await a.call('settings.set', { settings: { toolWindows: { changes: 'left' } } })
		assert.deepEqual(await b.call('settings.get'), { toolWindows: { changes: 'left' } })
		await until('the change announced', () => b.events.some((e) => e.event === 'settings.changed'))
		assert.deepEqual(JSON.parse(readFileSync(join(dir, '.config', 'hangar', 'code.json'), 'utf8')), {
			toolWindows: { changes: 'left' },
		})
		await assert.rejects(a.call('settings.set', { settings: [1] }), /object/)
	} finally {
		a.close()
		b.close()
	}
})

test('a file moved without git is a rename', async () => {
	writeFileSync(join(root, 'r.txt'), 'some\nlines\nhere\n')
	git('add', 'r.txt')
	git('commit', '-q', '-m', 'r')
	const a = new Client()
	try {
		await a.call('hello')
		renameSync(join(root, 'r.txt'), join(root, 'moved.txt'))
		const st = await a.call('git.status')
		assert.deepEqual(
			st.changes.filter((c: any) => c.path === 'moved.txt' || c.path === 'r.txt'),
			[{ index: '.', worktree: 'R', path: 'moved.txt', from: 'r.txt' }],
		)
		// The index is the repository's own, untouched.
		assert.equal(git('status', '--porcelain', '--', 'moved.txt', 'r.txt').toString(), ' D r.txt\n?? moved.txt\n')

		const d = await a.call('git.diff', { path: 'moved.txt' })
		assert.equal(d.head, 'some\nlines\nhere\n')
		assert.equal(d.from, 'r.txt')

		await a.call('git.stage', { path: 'moved.txt' })
		const staged = (await a.call('git.status')).changes.find((c: any) => c.path === 'moved.txt')
		assert.deepEqual(staged, { index: 'R', worktree: '.', path: 'moved.txt', from: 'r.txt' })

		await a.call('git.rollback', { path: 'moved.txt' })
		assert.ok(existsSync(join(root, 'r.txt')))
		assert.ok(!existsSync(join(root, 'moved.txt')))
		assert.equal(git('status', '--porcelain', '--', 'moved.txt', 'r.txt').toString(), '')
	} finally {
		a.close()
	}
})

test('a language server, shared by tabs and kept across a reload', async () => {
	const a = new Client()
	const b = new Client()
	let reloaded: Client | undefined
	writeFileSync(join(root, 'lsp.fake'), 'one\n')
	const uri = 'file://' + join(root, 'lsp.fake')
	const init = (c: Client, n: number, id: number) =>
		c.sendLsp(n, { jsonrpc: '2.0', id, method: 'initialize', params: { capabilities: {} } })
	const answer = async (c: Client, n: number, id: number) => {
		await until(`answer ${id}`, () => c.lsp.some((m) => m.server === n && m.msg.id === id))
		return c.lsp.find((m) => m.server === n && m.msg.id === id)!.msg
	}
	try {
		await a.call('hello')
		await b.call('hello')
		assert.match((await a.call('lsp.start', { language: 'cobol' })).missing, /no language server for cobol/)

		await a.call('settings.set', {
			settings: { languageServers: { fake: { command: process.execPath, args: [join(import.meta.dirname, 'fake-lsp.mjs')] } } },
		})
		const na = (await a.call('lsp.start', { language: 'fake' })).server
		const started = await b.call('lsp.start', { language: 'fake' })
		assert.equal(started.root, root)
		const nb = started.server

		// One server: both tabs are given its one answer to initialize.
		init(a, na, 1)
		init(b, nb, 7)
		const pid = (await answer(a, na, 1)).result.serverInfo.version
		assert.equal((await answer(b, nb, 7)).result.serverInfo.version, pid)
		// The server's registration is answered by the service, not passed on.
		await until('the registration answered', () => a.lsp.some((m) => m.msg.method === 'test/registered'))
		assert.ok(!a.lsp.some((m) => m.msg.method === 'client/registerCapability'))

		// Each tab's requests are answered to it, under its own ids.
		a.sendLsp(na, { jsonrpc: '2.0', id: 2, method: 'test/echo', params: { from: 'a' } })
		b.sendLsp(nb, { jsonrpc: '2.0', id: 2, method: 'test/echo', params: { from: 'b' } })
		assert.deepEqual((await answer(a, na, 2)).result, { from: 'a' })
		assert.deepEqual((await answer(b, nb, 2)).result, { from: 'b' })

		// Both open the shared document; the server is told once, and of an
		// edit once, from the document, whatever the tabs say.
		const doc = await a.open('lsp.fake')
		await until('synced', () => doc.getText('text').toString() === 'one\n')
		for (const [c, n] of [[a, na], [b, nb]] as const) {
			c.sendLsp(n, { jsonrpc: '2.0', method: 'textDocument/didOpen', params: { textDocument: { uri, languageId: 'fake', version: 0, text: 'stale' } } })
		}
		await until('opened', () => a.lsp.some((m) => m.msg.method === 'test/opened'))
		a.sendLsp(na, { jsonrpc: '2.0', method: 'textDocument/didChange', params: { textDocument: { uri, version: 1 }, contentChanges: [{ text: 'from a' }] } })
		doc.getText('text').insert(0, 'zero ')
		await until('the edit told', () => a.lsp.some((m) => m.msg.method === 'test/edited'))
		await new Promise((r) => setTimeout(r, 200))
		const opened = a.lsp.filter((m) => m.msg.method === 'test/opened')
		const edits = a.lsp.filter((m) => m.msg.method === 'test/edited')
		assert.equal(opened.length, 1)
		assert.equal(opened[0].msg.params.text, 'one\n')
		assert.deepEqual(edits.map((m) => m.msg.params.text), ['zero one\n'])

		// Files changed on disk are told to the server.
		writeFileSync(join(root, 'lsp-touched.txt'), 'x')
		await until('the change told', () => b.lsp.some((m) => m.msg.method === 'test/changed'))

		// A reload is a new connection; the server it finds is the same one.
		a.close()
		reloaded = new Client()
		await reloaded.call('hello')
		const nr = (await reloaded.call('lsp.start', { language: 'fake' })).server
		init(reloaded, nr, 1)
		assert.equal((await answer(reloaded, nr, 1)).result.serverInfo.version, pid)
	} finally {
		a.close()
		b.close()
		reloaded?.close()
		rmSync(join(root, 'lsp-touched.txt'), { force: true })
		rmSync(join(root, 'lsp.fake'), { force: true })
		await new Promise((r) => setTimeout(r, 100))
	}
})

test('docker: containers, actions, logs and changes', async () => {
	// A fake Docker Engine: one container, one image; starting the
	// container is reported as an event; its logs are framed as Docker
	// frames a container without a TTY's.
	const asked: string[] = []
	let events: import('node:http').ServerResponse | undefined
	const frame = (stream: number, text: string) => {
		const body = Buffer.from(text)
		const hdr = Buffer.alloc(8)
		hdr[0] = stream
		hdr.writeUInt32BE(body.length, 4)
		return Buffer.concat([hdr, body])
	}
	const id = 'a'.repeat(64)
	const engine = createServer((req, res) => {
		const url = req.url ?? ''
		asked.push(`${req.method} ${url.split('?')[0]}`)
		const send = (v: unknown) => res.end(JSON.stringify(v))
		if (url.startsWith('/v1.41/containers/json')) {
			send([{ Id: id, Names: ['/web-1'], Image: 'nginx', State: 'running', Status: 'Up 2 minutes', Created: 1, Command: 'nginx -g daemon off;',
				Ports: [{ IP: '0.0.0.0', PrivatePort: 80, PublicPort: 8080, Type: 'tcp' }, { IP: '::', PrivatePort: 80, PublicPort: 8080, Type: 'tcp' }],
				Labels: { 'com.docker.compose.project': 'shop', 'com.docker.compose.service': 'web',
					'com.docker.compose.project.working_dir': '/home/dev/shop',
					'com.docker.compose.project.config_files': '/home/dev/shop/compose.yaml,/home/dev/shop/compose.dev.yaml' } }])
		} else if (url.startsWith('/v1.41/images/json')) {
			send([{ Id: 'sha256:' + 'b'.repeat(64), RepoTags: ['nginx:latest'], Size: 1000, Created: 2, Containers: 1 }])
		} else if (url === `/v1.41/containers/${id}/restart`) {
			res.statusCode = 204
			res.end()
			events?.write(JSON.stringify({ Type: 'container', Action: 'restart' }) + '\n')
		} else if (url === `/v1.41/containers/${id}/json`) {
			send({ Config: { Tty: false } })
		} else if (url.startsWith(`/v1.41/containers/${id}/logs`)) {
			res.write(Buffer.concat([frame(1, 'hello '), frame(2, 'from stderr\n')]))
			setTimeout(() => res.write(frame(1, 'later\n')), 50)
		} else if (url.startsWith('/v1.41/events')) {
			events = res
			res.write('')
		} else {
			res.statusCode = 404
			send({ message: 'no such thing' })
		}
	})
	await new Promise<void>((r) => engine.listen(dockerSock, r))
	const a = new Client()
	try {
		await a.call('hello')
		const o = await a.call('docker.overview')
		assert.equal(o.available, true)
		assert.deepEqual(o.containers, [
			{ id, name: 'web-1', image: 'nginx', state: 'running', status: 'Up 2 minutes', ports: ['8080→80/tcp'],
				created: 1, command: 'nginx -g daemon off;', project: 'shop', service: 'web', workingDir: '/home/dev/shop',
				configFiles: ['/home/dev/shop/compose.yaml', '/home/dev/shop/compose.dev.yaml'] },
		])
		assert.deepEqual(o.images.map((i: any) => i.tags), [['nginx:latest']])

		await until('events watched', () => events !== undefined)
		await a.call('docker.act', { id, action: 'restart' })
		await until('the change announced', () => a.events.some((e) => e.event === 'docker.changed'))
		await assert.rejects(a.call('docker.act', { id, action: 'explode' }), /no such action/)
		// A project's action is each of its containers'.
		const before = asked.length
		await a.call('docker.project', { project: 'shop', action: 'restart' })
		assert.ok(asked.slice(before).includes(`POST /v1.41/containers/${id}/restart`))
		await assert.rejects(a.call('docker.project', { project: '../x', action: 'stop' }), /not a Compose project/)
		await assert.rejects(a.call('docker.act', { id: '../x', action: 'stop' }), /not a container/)

		const { stream } = await a.call('docker.logs', { id, tail: 10 })
		const text = () => a.events.filter((e) => e.event === 'docker.log' && e.params.stream === stream).map((e) => e.params.text).join('')
		await until('the logs', () => text() === 'hello from stderr\nlater\n')
		await a.call('docker.logsStop', { stream })
	} finally {
		a.close()
		engine.closeAllConnections()
		await new Promise((r) => engine.close(r))
	}
	// With no engine, Docker is said to be missing, not an error.
	const b = new Client()
	try {
		await b.call('hello')
		const o = await b.call('docker.overview')
		assert.equal(o.available, false)
		assert.match(o.why, /not running/)
	} finally {
		b.close()
	}
})

test("a branch's changes since it left main", async () => {
	const repo = join(dir, 'branchy')
	const g = (...args: string[]) => execFileSync('git', ['-C', repo, ...args]).toString()
	execFileSync('git', ['init', '-q', '-b', 'main', repo])
	writeFileSync(join(repo, 'kept.txt'), 'kept\n')
	writeFileSync(join(repo, 'old.txt'), 'one\ntwo\nthree\nfour\n')
	g('add', '.')
	g('commit', '-q', '-m', 'base')
	g('checkout', '-q', '-b', 'feature')
	writeFileSync(join(repo, 'kept.txt'), 'kept, changed\n')
	g('commit', '-q', '-am', 'change kept')
	g('mv', 'old.txt', 'new.txt')
	g('commit', '-q', '-m', 'rename')
	// main moves on after the branch left it; that is not the branch's.
	g('checkout', '-q', 'main')
	writeFileSync(join(repo, 'mainonly.txt'), 'x\n')
	g('add', '.')
	g('commit', '-q', '-m', 'main moves on')
	g('checkout', '-q', 'feature')
	// Not committed: an edit, and a file git does not know.
	writeFileSync(join(repo, 'new.txt'), 'one\ntwo\nthree\nfour\nfive\n')
	writeFileSync(join(repo, 'fresh.txt'), 'fresh\n')
	// And moved without git.
	renameSync(join(repo, 'kept.txt'), join(repo, 'moved.txt'))

	const a = new Client()
	try {
		await a.call('hello', { root: repo })
		const bc = await a.call('git.branchChanges', {})
		assert.equal(bc.base, 'main')
		assert.deepEqual(bc.commits.map((c: any) => c.subject), ['rename', 'change kept'])
		assert.deepEqual(bc.changes, [
			{ path: 'fresh.txt', status: 'A' },
			{ path: 'moved.txt', status: 'R', from: 'kept.txt' },
			{ path: 'new.txt', status: 'R', from: 'old.txt' },
		])
		// Blame: the renamed file's lines were committed in the base, its last
		// line is not committed yet.
		const blame = await a.call('git.blame', { path: 'new.txt' })
		assert.equal(blame.lines.length, 5)
		assert.equal(blame.commits[blame.lines[0]].summary, 'base')
		assert.equal(blame.lines[4], '0'.repeat(40))
		const d = await a.call('git.diff', { path: 'new.txt', against: bc.mergeBase, from: 'old.txt' })
		assert.equal(d.head, 'one\ntwo\nthree\nfour\n')
		assert.equal(d.working, 'one\ntwo\nthree\nfour\nfive\n')
		assert.equal(d.from, 'old.txt')
		assert.equal((await a.call('git.diff', { path: 'new.txt' })).from, undefined)
		await assert.rejects(a.call('git.diff', { path: 'new.txt', against: '--output=x' }), /not a revision/)
	} finally {
		a.close()
	}
})
