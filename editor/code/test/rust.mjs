// Runs the tests the Rust service, hangar-agent-rs, passes so far: those
// named in rust.txt, against the binary HANGAR_CODE_BIN names.
import { execFileSync } from 'node:child_process'
import { readFileSync } from 'node:fs'
import { join } from 'node:path'

if (!process.env.HANGAR_CODE_BIN) {
	console.error('HANGAR_CODE_BIN names hangar-agent-rs')
	process.exit(2)
}
const names = readFileSync(join(import.meta.dirname, 'rust.txt'), 'utf8')
	.split('\n')
	.filter(Boolean)
	.map((n) => '^' + n.replace(/[.*+?^${}()|[\]\\]/g, '\\$&') + '$')
execFileSync(process.execPath, ['--test', '--test-name-pattern', names.join('|'), join(import.meta.dirname, 'service.test.ts')], {
	stdio: 'inherit',
})
