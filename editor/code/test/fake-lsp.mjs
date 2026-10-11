// A language server for the tests: answers initialize (with its pid) and
// shutdown, echoes test/echo, asks its client to register a capability,
// and reports as notifications what it is told of files, documents and
// that registration.
let buf = Buffer.alloc(0)
const send = (m) => {
	const body = Buffer.from(JSON.stringify({ jsonrpc: '2.0', ...m }))
	process.stdout.write(`Content-Length: ${body.length}\r\n\r\n`)
	process.stdout.write(body)
}
process.stdin.on('data', (c) => {
	buf = Buffer.concat([buf, c])
	for (;;) {
		const end = buf.indexOf('\r\n\r\n')
		if (end < 0) return
		const len = Number(/Content-Length: (\d+)/.exec(buf.subarray(0, end).toString())[1])
		if (buf.length < end + 4 + len) return
		const m = JSON.parse(buf.subarray(end + 4, end + 4 + len).toString())
		buf = buf.subarray(end + 4 + len)
		if (m.method === 'initialize') {
			send({ id: m.id, result: { capabilities: { textDocumentSync: 1 }, serverInfo: { name: 'fake', version: String(process.pid) } } })
			send({ id: 'reg', method: 'client/registerCapability', params: { registrations: [] } })
		} else if (m.id === 'reg') send({ method: 'test/registered', params: m.result ?? null })
		else if (m.method === 'workspace/didChangeWatchedFiles') send({ method: 'test/changed', params: m.params })
		else if (m.method === 'textDocument/didOpen') send({ method: 'test/opened', params: m.params.textDocument })
		else if (m.method === 'textDocument/didChange') send({ method: 'test/edited', params: { version: m.params.textDocument.version, text: m.params.contentChanges[0].text } })
		else if (m.method === 'test/echo') send({ id: m.id, result: m.params })
		else if (m.method === 'shutdown') send({ id: m.id, result: null })
		else if (m.method === 'exit') process.exit(0)
	}
})
