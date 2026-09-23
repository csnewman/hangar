// VS Code's terminal panel as a view of the environment's terminal sessions:
// the shells Hangar's agent keeps, which Hangar's Terminal tab shows too.
//
// A session belongs to the environment, not to this window. It outlives the
// window, a refresh and the extension host; this extension only attaches to
// it. Each terminal here is one attach, speaking the agent's protocol on its
// socket (internal/terminal): a JSON request line, a JSON reply line, then
// frames of a type byte, a four-byte big-endian length and a payload.
'use strict';

const net = require('net');
const vscode = require('vscode');

const SOCKET = '/run/hangar/terminal.sock';
const FRAME_INPUT = 0x69; // 'i'
const FRAME_RESIZE = 0x72; // 'r'
const FRAME_OUTPUT = 0x6f; // 'o'
const FRAME_EXIT = 0x78; // 'x'

// How long after a terminal's tab is closed its session is ended. Reloading
// the window closes every tab, and also ends this extension host within this
// time, so a reload never ends a session; closing a tab by hand does.
const END_AFTER_CLOSE_MS = 3000;

// How often sessions opened elsewhere are looked for.
const SYNC_MS = 4000;

/** @type {Map<string, HangarPty>} sessions shown in this window, by ID */
const shown = new Map();
/** @type {Set<string>} sessions whose tab was closed here, being ended */
const closing = new Set();
/** new sessions this window has asked for and not yet been told the ID of */
let starting = 0;
let deactivating = false;
/** @type {vscode.LogOutputChannel} */
let log;

function request(req) {
	return new Promise((resolve, reject) => {
		const sock = net.createConnection(SOCKET);
		let buf = Buffer.alloc(0);
		sock.on('error', reject);
		sock.on('data', (chunk) => {
			buf = Buffer.concat([buf, chunk]);
			const nl = buf.indexOf(0x0a);
			if (nl < 0) return;
			sock.destroy();
			try {
				resolve(JSON.parse(buf.subarray(0, nl).toString('utf8')));
			} catch (e) {
				reject(e);
			}
		});
		sock.write(JSON.stringify(req) + '\n');
	});
}

function frame(type, payload) {
	const hdr = Buffer.alloc(5);
	hdr[0] = type;
	hdr.writeUInt32BE(payload.length, 1);
	return Buffer.concat([hdr, payload]);
}

/** A terminal attached to one session. */
class HangarPty {
	/** @param {string | undefined} session an existing session, or none for a new one */
	constructor(session) {
		this.session = session;
		this.writeEmitter = new vscode.EventEmitter();
		this.closeEmitter = new vscode.EventEmitter();
		this.nameEmitter = new vscode.EventEmitter();
		this.onDidWrite = this.writeEmitter.event;
		this.onDidClose = this.closeEmitter.event;
		this.onDidChangeName = this.nameEmitter.event;
		this.decoder = new TextDecoder('utf-8');
		this.sock = undefined;
		this.attached = false;
		this.exited = false;
		this.done = false;
		this.counted = false;
	}

	open(dims) {
		const folder = vscode.workspace.workspaceFolders?.[0]?.uri;
		const req = {
			op: 'attach',
			session: this.session,
			cols: dims?.columns,
			rows: dims?.rows,
			dir: !this.session && folder?.scheme === 'file' ? folder.fsPath : undefined,
		};
		log.info(`attaching to ${this.session ?? 'a new session'}`, JSON.stringify(req));
		const sock = net.createConnection(SOCKET);
		this.sock = sock;
		if (!this.session) {
			starting++;
			sock.once('close', () => this.started());
		}
		let buf = Buffer.alloc(0);
		sock.on('data', (chunk) => {
			buf = Buffer.concat([buf, chunk]);
			if (!this.attached) {
				const nl = buf.indexOf(0x0a);
				if (nl < 0) return;
				let reply;
				try {
					reply = JSON.parse(buf.subarray(0, nl).toString('utf8'));
				} catch {
					reply = { err: 'the agent sent a reply that could not be read' };
				}
				buf = buf.subarray(nl + 1);
				log.info('attach reply', JSON.stringify(reply));
				if (reply.err || !reply.session) {
					this.writeEmitter.fire(`\r\n[Hangar: ${reply.err || 'the session could not be opened'}]\r\n`);
					this.finish(1);
					return;
				}
				this.attached = true;
				this.session = reply.session.id;
				shown.set(this.session, this);
				this.started();
				if (reply.session.title) this.nameEmitter.fire(reply.session.title);
			}
			while (buf.length >= 5) {
				const len = buf.readUInt32BE(1);
				if (buf.length < 5 + len) break;
				const type = buf[0];
				const payload = buf.subarray(5, 5 + len);
				buf = buf.subarray(5 + len);
				if (type === FRAME_OUTPUT) {
					this.writeEmitter.fire(this.decoder.decode(payload, { stream: true }));
				} else if (type === FRAME_EXIT) {
					this.exited = true;
					this.finish(payload.length >= 4 ? payload.readInt32BE(0) : 0);
					return;
				}
			}
		});
		sock.on('error', (err) => {
			log.warn(`session ${this.session ?? '(new)'}: ${err.message}`);
			if (!this.attached) {
				this.writeEmitter.fire(`\r\n[Hangar: cannot reach the environment's terminals: ${err.message}]\r\n`);
				this.finish(1);
			}
		});
		sock.on('close', () => {
			if (this.attached && !this.exited && !this.done) {
				this.writeEmitter.fire('\r\n[Hangar: the connection to this session was lost]\r\n');
				this.finish(undefined);
			}
		});
		sock.write(JSON.stringify(req) + '\n');
	}

	// started records that a new session's ID is known, or never will be.
	started() {
		if (this.counted) return;
		this.counted = true;
		starting--;
	}

	handleInput(data) {
		if (this.attached) this.sock.write(frame(FRAME_INPUT, Buffer.from(data, 'utf8')));
	}

	setDimensions(dims) {
		if (!this.attached) return;
		const p = Buffer.alloc(4);
		p.writeUInt16BE(Math.min(dims.columns, 0xffff), 0);
		p.writeUInt16BE(Math.min(dims.rows, 0xffff), 2);
		this.sock.write(frame(FRAME_RESIZE, p));
	}

	// close is called when the tab goes, whether the user closed it or the
	// window is going away. The session is ended only if this extension host
	// is still here a moment later, which it is not after a reload.
	close() {
		const session = this.session;
		this.detach();
		if (!session || this.exited || deactivating) return;
		closing.add(session);
		setTimeout(() => {
			if (!deactivating) request({ op: 'close', session }).catch(() => {}).finally(() => closing.delete(session));
		}, END_AFTER_CLOSE_MS);
	}

	detach() {
		this.done = true;
		if (this.session && shown.get(this.session) === this) shown.delete(this.session);
		this.sock?.destroy();
	}

	finish(code) {
		this.detach();
		this.closeEmitter.fire(code);
	}
}

function show(session, title) {
	const pty = new HangarPty(session);
	if (session) shown.set(session, pty);
	return vscode.window.createTerminal({ name: title || 'Hangar', pty, iconPath: new vscode.ThemeIcon('terminal') });
}

// sync shows every session not yet shown here: those that were open before
// a reload, and those opened since in Hangar or in another window.
async function sync() {
	if (!vscode.workspace.getConfiguration('hangar.terminal').get('showAllSessions')) return;
	let reply;
	try {
		reply = await request({ op: 'list' });
	} catch {
		return;
	}
	// A session this window is starting would otherwise be shown twice.
	if (starting > 0) return;
	for (const s of reply.sessions || []) {
		if (!shown.has(s.id) && !closing.has(s.id)) show(s.id, s.title);
	}
}

function activate(context) {
	log = vscode.window.createOutputChannel('Hangar', { log: true });
	context.subscriptions.push(log);
	context.subscriptions.push(
		vscode.window.registerTerminalProfileProvider('hangar.terminal', {
			provideTerminalProfile() {
				log.info('providing a terminal');
				return new vscode.TerminalProfile({ name: 'Hangar', pty: new HangarPty(undefined) });
			},
		}),
		vscode.commands.registerCommand('hangar.terminal.new', () => show(undefined).show()),
		vscode.commands.registerCommand('hangar.terminal.attachAll', () => sync()),
	);
	sync();
	const timer = setInterval(sync, SYNC_MS);
	context.subscriptions.push({ dispose: () => clearInterval(timer) });
}

function deactivate() {
	deactivating = true;
	for (const pty of shown.values()) pty.detach();
}

module.exports = { activate, deactivate };
