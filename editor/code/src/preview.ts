import { randomBytes, timingSafeEqual } from 'node:crypto'
import { createReadStream } from 'node:fs'
import { stat } from 'node:fs/promises'
import { createServer, type Server } from 'node:http'
import { extname, join, normalize } from 'node:path'

// What a browser is told each file is, by its extension. Anything else is
// sent as bytes to download, never as something to run.
const types: Record<string, string> = {
	'.html': 'text/html; charset=utf-8',
	'.htm': 'text/html; charset=utf-8',
	'.css': 'text/css; charset=utf-8',
	'.js': 'text/javascript; charset=utf-8',
	'.mjs': 'text/javascript; charset=utf-8',
	'.json': 'application/json; charset=utf-8',
	'.map': 'application/json; charset=utf-8',
	'.txt': 'text/plain; charset=utf-8',
	'.md': 'text/plain; charset=utf-8',
	'.xml': 'application/xml; charset=utf-8',
	'.svg': 'image/svg+xml',
	'.png': 'image/png',
	'.jpg': 'image/jpeg',
	'.jpeg': 'image/jpeg',
	'.gif': 'image/gif',
	'.webp': 'image/webp',
	'.avif': 'image/avif',
	'.ico': 'image/x-icon',
	'.woff': 'font/woff',
	'.woff2': 'font/woff2',
	'.ttf': 'font/ttf',
	'.otf': 'font/otf',
	'.wasm': 'application/wasm',
	'.pdf': 'application/pdf',
	'.mp4': 'video/mp4',
	'.webm': 'video/webm',
	'.mp3': 'audio/mpeg',
	'.wav': 'audio/wav',
}

// Preview serves the user's files over HTTP, for the Code tab to show an
// HTML file as a browser would, its styles, scripts and images beside it
// loaded as they would be from the disk. It listens on the guest's loopback,
// which Hangar reaches through the environment's numbered host
// (<port>-env<short>), and answers only under a secret first segment of the
// path: the port is open to anyone who can reach the environment's ports,
// and the files are the user's. /<secret>/home/dev/site/index.html is
// /home/dev/site/index.html; a folder is its index.html.
export class Preview {
	private readonly secret = randomBytes(24).toString('base64url')
	private listening: Promise<number> | null = null
	private server: Server | null = null

	// start listens if the service is not already, and says where.
	start(): Promise<{ port: number; secret: string }> {
		this.listening ??= new Promise<number>((resolve, reject) => {
			const server = createServer((req, res) => {
				this.serve(req.url ?? '/').then(
					(r) => {
						res.writeHead(r.status, {
							'Content-Type': r.type,
							'Cache-Control': 'no-store',
							'X-Content-Type-Options': 'nosniff',
						})
						if (r.file && req.method !== 'HEAD') createReadStream(r.file).pipe(res)
						else res.end(r.body ?? '')
					},
					() => {
						res.writeHead(500, { 'Content-Type': 'text/plain; charset=utf-8' })
						res.end('The file could not be read.')
					},
				)
			})
			server.once('error', (e) => {
				this.listening = null
				reject(e)
			})
			server.listen(0, '127.0.0.1', () => {
				const a = server.address()
				resolve(typeof a === 'object' && a ? a.port : 0)
			})
			this.server = server
		})
		return this.listening.then((port) => ({ port, secret: this.secret }))
	}

	close() {
		this.server?.close()
	}

	private async serve(url: string): Promise<{ status: number; type: string; file?: string; body?: string }> {
		const text = (status: number, body: string) => ({ status, type: 'text/plain; charset=utf-8', body })
		const path = url.split('?')[0].split('#')[0]
		const cut = path.indexOf('/', 1)
		const given = Buffer.from(cut < 0 ? path.slice(1) : path.slice(1, cut))
		const want = Buffer.from(this.secret)
		if (given.length !== want.length || !timingSafeEqual(given, want)) return text(404, 'Not found.')
		let file: string
		try {
			file = normalize(decodeURIComponent(cut < 0 ? '/' : path.slice(cut)))
		} catch {
			return text(400, 'Bad path.')
		}
		let st
		try {
			st = await stat(file)
			if (st.isDirectory()) {
				file = join(file, 'index.html')
				st = await stat(file)
			}
		} catch {
			return text(404, `${file} does not exist.`)
		}
		if (!st.isFile()) return text(404, `${file} is not a file.`)
		return { status: 200, type: types[extname(file).toLowerCase()] ?? 'application/octet-stream', file }
	}
}
