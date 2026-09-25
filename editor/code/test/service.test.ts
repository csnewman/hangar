// Runs the built service as the agent does and talks to it as two browser
// windows would: files, git, and one shared document edited from both sides
// and from the disk.
import { execFileSync, spawn } from 'node:child_process'
import { existsSync, mkdtempSync, readFileSync, rmSync, writeFileSync } from 'node:fs'
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
		assert.deepEqual(await a.call('hello'), { root })
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
