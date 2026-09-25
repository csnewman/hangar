// Opening an environment outside the app: in desktop VS Code, through the
// Hangar extension, or in a terminal over the server's SSH gateway.

import { app, shell } from 'electron'
import { spawn } from 'node:child_process'
import { chmodSync, mkdirSync, writeFileSync } from 'node:fs'
import { join } from 'node:path'

import type { Environment, Me } from './api'
import type { Server } from './store'

// openInVSCode hands the environment to the Hangar extension, which signs
// in to the server if it must and opens it over Remote-SSH.
export function openInVSCode(server: Server, env: Environment) {
  const q = new URLSearchParams({ server: server.url, env: env.id })
  return shell.openExternal(`vscode://hangar.hangar-remote/open?${q}`)
}

// sshArgs is the ssh command for an environment. The gateway's host key
// comes from the server over HTTPS and is kept in the app's own
// known_hosts, so ssh has nothing to ask.
function sshArgs(me: Me, env: Environment): string[] {
  const gw = me.ssh!
  const alias = `hangar-gateway-${gw.host}-${gw.port}`
  const dir = join(app.getPath('userData'), 'ssh')
  mkdirSync(dir, { recursive: true })
  const knownHosts = join(dir, 'known_hosts')
  writeFileSync(knownHosts, `${alias} ${gw.host_key}\n`)
  const user = env.owner_id === me.id ? env.name : `${env.owner}/${env.name}`
  return [
    '-p',
    String(gw.port),
    '-o',
    `HostKeyAlias=${alias}`,
    '-o',
    `UserKnownHostsFile=${knownHosts}`,
    `${user}@${gw.host}`,
  ]
}

const quote = (a: string) => `'${a.replace(/'/g, `'\\''`)}'`

// openTerminal opens the platform's terminal with ssh into the environment.
export function openTerminal(me: Me, env: Environment): string | null {
  if (!me.ssh) return 'this server runs no SSH gateway'
  const args = sshArgs(me, env)
  switch (process.platform) {
    case 'darwin': {
      // A .command file opens in Terminal with no automation permission to
      // ask for, and runs in the user's own login shell.
      const dir = join(app.getPath('temp'), 'hangar')
      mkdirSync(dir, { recursive: true })
      const file = join(dir, `${env.name}.command`)
      writeFileSync(file, `#!/bin/sh\nexec ssh ${args.map(quote).join(' ')}\n`)
      chmodSync(file, 0o755)
      spawn('open', ['-a', 'Terminal', file], { detached: true, stdio: 'ignore' }).unref()
      return null
    }
    case 'win32': {
      // start takes its first quoted word as the window's title, so one is
      // given; the rest is passed as written, quoted where it has spaces.
      const words = ['/c', 'start', '"Hangar"', 'ssh', ...args.map((a) => (a.includes(' ') ? `"${a}"` : a))]
      spawn('cmd.exe', words, { detached: true, stdio: 'ignore', windowsVerbatimArguments: true }).unref()
      return null
    }
    default: {
      // The first of the usual terminals that is installed.
      const terms: [string, string[]][] = [
        ['x-terminal-emulator', ['-e', 'ssh', ...args]],
        ['gnome-terminal', ['--', 'ssh', ...args]],
        ['konsole', ['-e', 'ssh', ...args]],
        ['xterm', ['-e', 'ssh', ...args]],
      ]
      const tryNext = (i: number) => {
        if (i >= terms.length) return
        const p = spawn(terms[i][0], terms[i][1], { detached: true, stdio: 'ignore' })
        p.on('error', () => tryNext(i + 1))
        p.unref()
      }
      tryNext(0)
      return null
    }
  }
}
