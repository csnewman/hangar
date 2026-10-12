// What the Project tree does to files besides creating, renaming and
// deleting them one at a time: copying and moving several, as IntelliJ's
// paste and drag do; putting them in the trash and back, which is how a
// deletion is undone; taking uploads; packing a download; and changing a
// file's permissions, line separators, encoding and byte order mark.

import { randomBytes } from 'node:crypto'
import { createReadStream, createWriteStream, type WriteStream } from 'node:fs'
import {
	appendFile,
	chmod,
	cp,
	lstat,
	mkdir,
	mkdtemp,
	open,
	readdir,
	readFile,
	rename,
	rm,
	stat,
	writeFile,
} from 'node:fs/promises'
import { homedir, tmpdir } from 'node:os'
import { basename, dirname, extname, join, sep } from 'node:path'
import { createDeflateRaw, crc32 } from 'node:zlib'

// --- Copying and moving ---------------------------------------------------

// What a copy or move does with a name the folder already has: report it
// and do nothing ('ask', so the user can choose), put the new one in its
// place, leave that one out, or give it a free name beside it.
export type Clash = 'ask' | 'replace' | 'skip' | 'rename'

export type Transferred = { from: string; to: string }

export type TransferResult = {
	// What went where; nothing for 'ask' with clashes.
	done: Transferred[]
	// The names the folder already has, for 'ask'.
	clashes: string[]
}

// freeName is a name for name in dir that nothing there has: the name
// itself, else "name copy", "name copy 2", ..., the extension kept.
export async function freeName(dir: string, name: string): Promise<string> {
	if (!(await exists(join(dir, name)))) return name
	const ext = extname(name) && extname(name) !== name ? extname(name) : ''
	const stem = name.slice(0, name.length - ext.length)
	for (let i = 1; ; i++) {
		const n = `${stem} copy${i > 1 ? ` ${i}` : ''}${ext}`
		if (!(await exists(join(dir, n)))) return n
	}
}

async function exists(p: string): Promise<boolean> {
	try {
		await lstat(p)
		return true
	} catch {
		return false
	}
}

// inside reports whether p is dir or within it.
function inside(p: string, dir: string): boolean {
	return p === dir || p.startsWith(dir.endsWith(sep) ? dir : dir + sep)
}

// transfer copies or moves each of from (absolute) into the folder to,
// under its own name or, for one, the name given. A folder is not put
// inside itself, and a move to where a thing already is does nothing.
export async function transfer(
	op: 'copy' | 'move',
	from: string[],
	to: string,
	clash: Clash,
	name?: string,
): Promise<TransferResult> {
	if (!(await stat(to)).isDirectory()) throw new Error(`${basename(to)} is not a folder`)
	if (name !== undefined && (from.length !== 1 || !validName(name))) throw new Error(`not a valid name: ${name}`)
	const plan: { from: string; to: string; clash: boolean }[] = []
	for (const f of from) {
		if (inside(to, f)) throw new Error(`cannot put ${basename(f)} inside itself`)
		const target = join(to, name ?? basename(f))
		if (target === f) {
			// Onto itself: a move stays, a copy is a duplicate beside it.
			if (op === 'move') continue
			plan.push({ from: f, to: join(to, await freeName(to, basename(target))), clash: false })
			continue
		}
		plan.push({ from: f, to: target, clash: await exists(target) })
	}
	const clashes = plan.filter((p) => p.clash).map((p) => basename(p.to))
	if (clash === 'ask' && clashes.length > 0) return { done: [], clashes }
	const done: Transferred[] = []
	for (const p of plan) {
		let target = p.to
		if (p.clash) {
			if (clash === 'skip') continue
			if (clash === 'rename') target = join(to, await freeName(to, basename(p.to)))
			else await rm(target, { recursive: true, force: true })
		}
		if (op === 'copy') await cp(p.from, target, { recursive: true, preserveTimestamps: true, verbatimSymlinks: true, errorOnExist: true, force: false })
		else await move(p.from, target)
		done.push({ from: p.from, to: target })
	}
	return { done, clashes: [] }
}

// move renames, or copies and deletes across filesystems.
async function move(from: string, to: string) {
	try {
		await rename(from, to)
	} catch (e) {
		if ((e as NodeJS.ErrnoException).code !== 'EXDEV') throw e
		await cp(from, to, { recursive: true, preserveTimestamps: true, verbatimSymlinks: true, errorOnExist: true, force: false })
		await rm(from, { recursive: true, force: true })
	}
}

function validName(n: string): boolean {
	return n !== '' && n !== '.' && n !== '..' && !n.includes('/') && !n.includes('\0')
}

// --- The trash ------------------------------------------------------------

// The trash is the desktop's, as the freedesktop.org specification has it
// (~/.local/share/Trash): what is put there is in files/, and in info/ where
// it came from, so a file manager can restore it too.
function trashDir(): string {
	return join(process.env.XDG_DATA_HOME || join(homedir(), '.local', 'share'), 'Trash')
}

export type Trashed = { path: string; id: string }

// trash puts each path in the trash, returning where each went: its id
// there, to restore it by.
export async function trash(paths: string[]): Promise<Trashed[]> {
	const dir = trashDir()
	await mkdir(join(dir, 'files'), { recursive: true })
	await mkdir(join(dir, 'info'), { recursive: true })
	const out: Trashed[] = []
	for (const p of paths) {
		const id = await claimInfo(dir, basename(p), p)
		try {
			await move(p, join(dir, 'files', id))
		} catch (e) {
			await rm(join(dir, 'info', `${id}.trashinfo`), { force: true })
			throw e
		}
		out.push({ path: p, id })
	}
	return out
}

// claimInfo writes a trashinfo file for a name not yet in the trash,
// claiming the name, which is the id.
async function claimInfo(dir: string, name: string, path: string): Promise<string> {
	const ext = extname(name) && extname(name) !== name ? extname(name) : ''
	const stem = name.slice(0, name.length - ext.length)
	const when = new Date()
	const pad = (n: number) => String(n).padStart(2, '0')
	const date = `${when.getFullYear()}-${pad(when.getMonth() + 1)}-${pad(when.getDate())}T${pad(when.getHours())}:${pad(when.getMinutes())}:${pad(when.getSeconds())}`
	const info = `[Trash Info]\nPath=${path.split('/').map(encodeURIComponent).join('/')}\nDeletionDate=${date}\n`
	for (let i = 1; ; i++) {
		const id = i === 1 ? name : `${stem}.${i}${ext}`
		try {
			await writeFile(join(dir, 'info', `${id}.trashinfo`), info, { flag: 'wx' })
		} catch (e) {
			if ((e as NodeJS.ErrnoException).code === 'EEXIST') continue
			throw e
		}
		if (await exists(join(dir, 'files', id))) continue
		return id
	}
}

// restore puts what trash put there back where it was. Something there
// since is not replaced.
export async function restore(items: Trashed[]) {
	const dir = trashDir()
	for (const t of items) {
		if (!validName(t.id)) throw new Error(`not in the trash: ${t.id}`)
		if (await exists(t.path)) throw new Error(`${basename(t.path)} is there again; it was not replaced`)
		await mkdir(dirname(t.path), { recursive: true })
		await move(join(dir, 'files', t.id), t.path)
		await rm(join(dir, 'info', `${t.id}.trashinfo`), { force: true })
	}
}

// --- Uploads --------------------------------------------------------------

// Uploads arrive in parts, written to a hidden file beside where they go
// and renamed into place once whole, so nothing sees half a file.
export class Uploads {
	private open = new Map<string, { tmp: string; path: string }>()

	async start(path: string): Promise<string> {
		await mkdir(dirname(path), { recursive: true })
		const token = randomBytes(12).toString('hex')
		const tmp = join(dirname(path), `.${basename(path)}.hangar-upload-${token}`)
		await writeFile(tmp, '', { flag: 'wx' })
		this.open.set(token, { tmp, path })
		return token
	}

	async write(token: string, data: string) {
		const u = this.open.get(token)
		if (!u) throw new Error('no such upload')
		await appendFile(u.tmp, Buffer.from(data, 'base64'))
	}

	// end puts the upload in place, replacing what is there only if asked.
	async end(token: string, replace: boolean): Promise<string> {
		const u = this.open.get(token)
		if (!u) throw new Error('no such upload')
		this.open.delete(token)
		if (!replace && (await exists(u.path))) {
			await rm(u.tmp, { force: true })
			throw new Error(`${basename(u.path)} already exists`)
		}
		if (replace) await rm(u.path, { recursive: true, force: true })
		await rename(u.tmp, u.path)
		return u.path
	}

	async abort(token: string) {
		const u = this.open.get(token)
		this.open.delete(token)
		if (u) await rm(u.tmp, { force: true })
	}

	async abortAll() {
		for (const t of [...this.open.keys()]) await this.abort(t)
	}
}

// --- Downloads ------------------------------------------------------------

let zipDir: Promise<string> | undefined

// isPacked reports whether a path is a download zip packs.
export async function isPacked(p: string): Promise<boolean> {
	return !!zipDir && dirname(p) === (await zipDir)
}

// zip packs files and folders into a zip file of their contents, each under
// its own name, for the browser to read and then remove. Links are kept as
// what they lead to when that is a file, and left out otherwise, so a link
// to a folder above cannot make it endless.
export async function zip(paths: string[]): Promise<{ path: string; size: number }> {
	zipDir ??= mkdtemp(join(tmpdir(), 'hangar-code-zip-'))
	const out = join(await zipDir, `${randomBytes(8).toString('hex')}.zip`)
	const z = new ZipWriter(createWriteStream(out))
	try {
		for (const p of paths) await addTo(z, p, basename(p))
		await z.finish()
	} catch (e) {
		z.abort()
		await rm(out, { force: true })
		throw e
	}
	return { path: out, size: (await stat(out)).size }
}

async function addTo(z: ZipWriter, abs: string, name: string) {
	const st = await lstat(abs)
	if (st.isDirectory()) {
		await z.dir(name + '/', st.mtime, st.mode)
		for (const e of (await readdir(abs)).sort()) await addTo(z, join(abs, e), `${name}/${e}`)
		return
	}
	let file = st
	if (st.isSymbolicLink()) {
		try {
			file = await stat(abs)
		} catch {
			return
		}
	}
	if (file.isFile()) await z.file(name, abs, file.mtime, file.mode)
}

// The most a zip without Zip64 holds: sizes and offsets are four bytes.
const ZIP_MAX = 0xffffffff

// ZipWriter writes a zip file as it goes: each file deflated as it is read,
// its sizes and checksum in a data descriptor after it, and the central
// directory at the end.
class ZipWriter {
	private offset = 0
	private entries: { name: Buffer; crc: number; size: number; packed: number; offset: number; time: number; date: number; mode: number; dir: boolean }[] = []

	constructor(private out: WriteStream) {}

	private write(b: Buffer): Promise<void> {
		this.offset += b.length
		if (this.offset > ZIP_MAX) return Promise.reject(new Error('too large to download as a zip'))
		return new Promise((res, rej) => this.out.write(b, (e) => (e ? rej(e) : res())))
	}

	private header(name: Buffer, time: number, date: number, flags: number, method: number): Buffer {
		const h = Buffer.alloc(30)
		h.writeUInt32LE(0x04034b50, 0)
		h.writeUInt16LE(20, 4)
		h.writeUInt16LE(flags, 6)
		h.writeUInt16LE(method, 8)
		h.writeUInt16LE(time, 10)
		h.writeUInt16LE(date, 12)
		h.writeUInt16LE(name.length, 26)
		return Buffer.concat([h, name])
	}

	async dir(name: string, mtime: Date, mode: number) {
		const n = Buffer.from(name, 'utf8')
		const [time, date] = dosTime(mtime)
		const offset = this.offset
		await this.write(this.header(n, time, date, 0x0800, 0))
		this.entries.push({ name: n, crc: 0, size: 0, packed: 0, offset, time, date, mode, dir: true })
	}

	async file(name: string, abs: string, mtime: Date, mode: number) {
		const n = Buffer.from(name, 'utf8')
		const [time, date] = dosTime(mtime)
		const offset = this.offset
		// Bit 3: the sizes follow the data; bit 11: the name is UTF-8.
		await this.write(this.header(n, time, date, 0x0808, 8))
		let crc = 0
		let size = 0
		let packed = 0
		const deflate = createDeflateRaw()
		const done = new Promise<void>((res, rej) => {
			deflate.on('data', (chunk: Buffer) => {
				packed += chunk.length
				deflate.pause()
				this.write(chunk).then(() => deflate.resume(), rej)
			})
			deflate.on('end', res)
			deflate.on('error', rej)
		})
		const src = createReadStream(abs)
		src.on('data', (chunk) => {
			const b = chunk as Buffer
			crc = crc32(b, crc)
			size += b.length
		})
		src.on('error', (e) => deflate.destroy(e))
		src.pipe(deflate)
		await done
		if (size > ZIP_MAX) throw new Error('too large to download as a zip')
		const d = Buffer.alloc(16)
		d.writeUInt32LE(0x08074b50, 0)
		d.writeUInt32LE(crc >>> 0, 4)
		d.writeUInt32LE(packed, 8)
		d.writeUInt32LE(size, 12)
		await this.write(d)
		this.entries.push({ name: n, crc, size, packed, offset, time, date, mode, dir: false })
	}

	async finish() {
		const start = this.offset
		for (const e of this.entries) {
			const c = Buffer.alloc(46)
			c.writeUInt32LE(0x02014b50, 0)
			// Made by Unix, so the modes below are read.
			c.writeUInt16LE((3 << 8) | 20, 4)
			c.writeUInt16LE(20, 6)
			c.writeUInt16LE(e.dir ? 0x0800 : 0x0808, 8)
			c.writeUInt16LE(e.dir ? 0 : 8, 10)
			c.writeUInt16LE(e.time, 12)
			c.writeUInt16LE(e.date, 14)
			c.writeUInt32LE(e.crc >>> 0, 16)
			c.writeUInt32LE(e.packed, 20)
			c.writeUInt32LE(e.size, 24)
			c.writeUInt16LE(e.name.length, 28)
			c.writeUInt32LE((((e.mode & 0xffff) << 16) | (e.dir ? 0x10 : 0)) >>> 0, 38)
			c.writeUInt32LE(e.offset, 42)
			await this.write(Buffer.concat([c, e.name]))
		}
		const end = Buffer.alloc(22)
		end.writeUInt32LE(0x06054b50, 0)
		end.writeUInt16LE(Math.min(this.entries.length, 0xffff), 8)
		end.writeUInt16LE(Math.min(this.entries.length, 0xffff), 10)
		end.writeUInt32LE(this.offset - start, 12)
		end.writeUInt32LE(start, 16)
		await this.write(end)
		await new Promise<void>((res, rej) => this.out.end((e?: Error | null) => (e ? rej(e) : res())))
	}

	abort() {
		this.out.destroy()
	}
}

// dosTime is a time as a zip has it: two-second time, and date from 1980.
function dosTime(d: Date): [number, number] {
	const y = Math.max(1980, d.getFullYear())
	return [
		(d.getHours() << 11) | (d.getMinutes() << 5) | (d.getSeconds() >> 1),
		((y - 1980) << 9) | ((d.getMonth() + 1) << 5) | d.getDate(),
	]
}

// --- Permissions ----------------------------------------------------------

// setMode makes a file executable or not -- by whoever may read it, as
// chmod +x does -- and writable by its owner or by nobody.
export async function setMode(path: string, change: { exec?: boolean; writable?: boolean }): Promise<number> {
	let mode = (await stat(path)).mode & 0o7777
	if (change.exec !== undefined) mode = change.exec ? mode | ((mode & 0o444) >> 2) : mode & ~0o111
	if (change.writable !== undefined) mode = change.writable ? mode | 0o200 : mode & ~0o222
	await chmod(path, mode)
	return mode
}

// --- Line separators, encoding and byte order mark ------------------------

export type Encoding = 'UTF-8' | 'UTF-16LE' | 'UTF-16BE' | 'ISO-8859-1'
export const ENCODINGS: Encoding[] = ['UTF-8', 'UTF-16LE', 'UTF-16BE', 'ISO-8859-1']
export type Separator = 'LF' | 'CRLF' | 'CR'

// TextProps are what a file's bytes say of its text: how it is encoded,
// whether it starts with a byte order mark, and how its lines end -- one
// kind, several ('mixed'), or none (a file of one line). Its mode too.
export type TextProps = {
	encoding: Encoding | null
	bom: boolean
	separator: Separator | 'mixed' | null
	mode: number
	size: number
	modified: number
}

// The most of a file read to judge it, and the largest file converted.
const MAX_CONVERT = 64 << 20

const BOMS: [Encoding, Buffer][] = [
	['UTF-8', Buffer.from([0xef, 0xbb, 0xbf])],
	['UTF-16LE', Buffer.from([0xff, 0xfe])],
	['UTF-16BE', Buffer.from([0xfe, 0xff])],
]

// sniff is how bytes are encoded: by their byte order mark, else UTF-8 if
// they are valid UTF-8, else ISO-8859-1, which any bytes are; null for
// bytes that are not text (a NUL, and no mark saying UTF-16).
function sniff(b: Buffer): { encoding: Encoding | null; bom: boolean } {
	for (const [encoding, mark] of BOMS) if (b.subarray(0, mark.length).equals(mark)) return { encoding, bom: true }
	if (b.subarray(0, 8000).includes(0)) return { encoding: null, bom: false }
	try {
		new TextDecoder('utf-8', { fatal: true }).decode(b)
		return { encoding: 'UTF-8', bom: false }
	} catch {
		return { encoding: 'ISO-8859-1', bom: false }
	}
}

function decode(b: Buffer, encoding: Encoding, bom: boolean): string {
	const body = bom ? b.subarray(BOMS.find(([e]) => e === encoding)?.[1].length ?? 0) : b
	switch (encoding) {
		case 'UTF-8':
			return body.toString('utf8')
		case 'UTF-16LE':
			return body.toString('utf16le')
		case 'UTF-16BE':
			return Buffer.from(body).swap16().toString('utf16le')
		case 'ISO-8859-1':
			return body.toString('latin1')
	}
}

function encode(s: string, encoding: Encoding, bom: boolean): Buffer {
	let body: Buffer
	switch (encoding) {
		case 'UTF-8':
			body = Buffer.from(s, 'utf8')
			break
		case 'UTF-16LE':
			body = Buffer.from(s, 'utf16le')
			break
		case 'UTF-16BE':
			body = Buffer.from(s, 'utf16le').swap16()
			break
		case 'ISO-8859-1':
			if (/[^\u0000-ÿ]/.test(s)) throw new Error('the file has characters ISO-8859-1 cannot hold')
			body = Buffer.from(s, 'latin1')
			break
	}
	const mark = bom ? BOMS.find(([e]) => e === encoding)?.[1] : undefined
	return mark ? Buffer.concat([mark, body]) : body
}

function separatorOf(s: string): Separator | 'mixed' | null {
	const crlf = (s.match(/\r\n/g) ?? []).length
	const lf = (s.match(/\n/g) ?? []).length - crlf
	const cr = (s.match(/\r/g) ?? []).length - crlf
	const kinds = [lf && 'LF', crlf && 'CRLF', cr && 'CR'].filter(Boolean) as Separator[]
	return kinds.length === 0 ? null : kinds.length === 1 ? kinds[0] : 'mixed'
}

export async function textProps(path: string): Promise<TextProps> {
	const st = await stat(path)
	if (!st.isFile()) throw new Error(`${basename(path)} is not a file`)
	const f = await open(path, 'r')
	let b: Buffer
	try {
		b = Buffer.alloc(Math.min(st.size, MAX_CONVERT))
		await f.read(b, 0, b.length, 0)
	} finally {
		await f.close()
	}
	const { encoding, bom } = sniff(b)
	return {
		encoding,
		bom,
		separator: encoding ? separatorOf(decode(b, encoding, bom)) : null,
		mode: st.mode & 0o7777,
		size: st.size,
		modified: st.mtimeMs,
	}
}

// convert rewrites a text file with other line separators, in another
// encoding, or with its byte order mark added or taken away. The text is
// read as it is encoded, so only its bytes change.
export async function convert(
	path: string,
	to: { separator?: Separator; encoding?: Encoding; bom?: boolean },
): Promise<TextProps> {
	const st = await stat(path)
	if (st.size > MAX_CONVERT) throw new Error(`${basename(path)} is too large to convert`)
	const b = await readFile(path)
	const { encoding, bom } = sniff(b)
	if (!encoding) throw new Error(`${basename(path)} is not text`)
	let s = decode(b, encoding, bom)
	if (to.separator) s = s.replace(/\r\n|\r|\n/g, { LF: '\n', CRLF: '\r\n', CR: '\r' }[to.separator])
	const enc = to.encoding ?? encoding
	// UTF-16 is marked: without a mark, nothing tells its byte order.
	const mark = to.bom ?? (to.encoding && to.encoding !== encoding ? enc.startsWith('UTF-16') : bom)
	if (mark && enc === 'ISO-8859-1') throw new Error('ISO-8859-1 has no byte order mark')
	await writeFile(path, encode(s, enc, mark))
	return textProps(path)
}
