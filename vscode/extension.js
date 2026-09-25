// Hangar for VS Code on the desktop: sign in to a Hangar server, see its
// environments, and open any of them through Remote-SSH.
//
// Every environment is reached through the server's SSH gateway, which names
// the environment in the username. The extension keeps an SSH config file of
// its own, ~/.ssh/hangar/config, with a host per environment (hangar-<name>),
// and a known_hosts file holding the gateway's key as the server's API gives
// it, so neither ssh nor Remote-SSH has anything to ask.

const vscode = require('vscode')
const crypto = require('crypto')
const fs = require('fs')
const os = require('os')
const path = require('path')

const serverKey = 'hangar.server'
const tokenKey = 'hangar.token'
const identityKey = 'hangar.identityFile'
const hangarDir = path.join(os.homedir(), '.ssh', 'hangar')
const hangarConfig = path.join(hangarDir, 'config')
const hangarKnownHosts = path.join(hangarDir, 'known_hosts')
const includeLine = 'Include ~/.ssh/hangar/config'
const remoteSSH = ['ms-vscode-remote.remote-ssh', 'jeanp413.open-remote-ssh']

// Phases an environment passes through on its own; while any is showing,
// the list is refreshed more often.
const moving = new Set(['pending', 'starting', 'stopping', 'suspending', 'deleting'])

/** @type {vscode.ExtensionContext} */
let ctx
let log
let tree
// The sign-in waiting on the browser, if any.
let pending = null

class HTTPError extends Error {
  constructor(status, message) {
    super(message)
    this.status = status
  }
}

// Client calls the server's API as the signed-in user.
class Client {
  constructor(server, token) {
    this.server = server
    this.token = token
  }

  async call(method, apiPath, body) {
    let res
    try {
      res = await fetch(this.server + '/api/frontend' + apiPath, {
        method,
        headers: {
          Authorization: 'Bearer ' + this.token,
          ...(body === undefined ? {} : { 'Content-Type': 'application/json' }),
        },
        body: body === undefined ? undefined : JSON.stringify(body),
      })
    } catch (err) {
      throw new Error(`Hangar at ${this.server} cannot be reached: ${err.cause?.message ?? err.message}`)
    }
    if (res.status === 204) return undefined
    const text = await res.text()
    let data
    try {
      data = text ? JSON.parse(text) : undefined
    } catch {
      data = undefined
    }
    if (!res.ok) throw new HTTPError(res.status, data?.error ?? `${res.status} ${res.statusText}`)
    return data
  }

  me() {
    return this.call('GET', '/me')
  }
  environments() {
    return this.call('GET', '/environments')
  }
  profile() {
    return this.call('GET', '/me/profile')
  }
  addLoginKey(publicKey) {
    return this.call('POST', '/me/profile/login-keys', { public_key: publicKey })
  }
  act(id, action) {
    return this.call('POST', `/environments/${encodeURIComponent(id)}/${action}`)
  }
}

async function client() {
  const server = ctx.globalState.get(serverKey)
  const token = await ctx.secrets.get(tokenKey)
  return server && token ? new Client(server, token) : null
}

// Tree is the Environments view: the user's own environments, then any
// others they can see, grouped by owner.
class Tree {
  constructor() {
    this.changed = new vscode.EventEmitter()
    this.onDidChangeTreeData = this.changed.event
    this.me = null
    this.envs = []
    this.error = null
    this.timer = null
  }

  async refresh() {
    clearTimeout(this.timer)
    const c = await client()
    if (!c) {
      this.me = null
      this.envs = []
      this.error = null
      await vscode.commands.executeCommand('setContext', 'hangar.signedIn', false)
      this.changed.fire()
      return
    }
    try {
      const [me, envs] = await Promise.all([c.me(), c.environments()])
      this.me = me
      this.envs = envs.filter((e) => e.desired !== 'deleted')
      this.error = null
      writeSSHConfig(c.server, me, this.envs)
    } catch (err) {
      if (err instanceof HTTPError && err.status === 401) {
        await ctx.secrets.delete(tokenKey)
        vscode.window.showWarningMessage('Hangar: the access token was refused. Sign in again.', 'Sign In').then((a) => {
          if (a) vscode.commands.executeCommand('hangar.signIn')
        })
        return this.refresh()
      }
      this.error = err.message
      log.appendLine(`listing environments: ${err.message}`)
    }
    await vscode.commands.executeCommand('setContext', 'hangar.signedIn', true)
    this.changed.fire()
    const soon = this.envs.some((e) => moving.has(e.phase))
    this.timer = setTimeout(() => this.refresh(), soon ? 3000 : 30000)
  }

  getTreeItem(node) {
    return node.item
  }

  getChildren(node) {
    if (node) return node.children ?? []
    if (!this.me) return []
    if (this.error) {
      const item = new vscode.TreeItem(this.error)
      item.iconPath = new vscode.ThemeIcon('warning')
      item.command = { command: 'hangar.refresh', title: 'Refresh' }
      return [{ item }]
    }
    const own = this.envs.filter((e) => e.owner_id === this.me.id).map((e) => this.envNode(e, true))
    const others = new Map()
    for (const e of this.envs) {
      if (e.owner_id === this.me.id) continue
      if (!others.has(e.owner)) others.set(e.owner, [])
      others.get(e.owner).push(this.envNode(e, false))
    }
    if (others.size === 0) return own
    const groups = [...others.entries()]
      .sort(([a], [b]) => a.localeCompare(b))
      .map(([owner, children]) => {
        const item = new vscode.TreeItem(owner, vscode.TreeItemCollapsibleState.Collapsed)
        item.iconPath = new vscode.ThemeIcon('person')
        item.contextValue = 'owner'
        return { item, children }
      })
    return [...own, ...groups]
  }

  envNode(env, own) {
    const item = new vscode.TreeItem(env.name)
    item.id = env.id
    item.description = env.phase === env.desired || env.phase === 'running' ? env.phase : `${env.phase} → ${env.desired}`
    item.iconPath = phaseIcon(env.phase)
    item.contextValue = `env-${env.phase}`
    const tip = new vscode.MarkdownString()
    tip.appendMarkdown(`**${env.name}**${own ? '' : ` — ${env.owner}`}\n\n`)
    tip.appendMarkdown(`${env.phase}${env.reason ? `: ${env.reason}` : ''}\n\n`)
    tip.appendMarkdown(`${env.template} · ${env.cpus} vCPU · ${Math.round(env.memory_mib / 1024)} GiB`)
    for (const r of env.spec?.repos ?? []) tip.appendMarkdown(`\n\n\`${r.url}\` in \`${r.path}\``)
    item.tooltip = tip
    return { item, env }
  }
}

function phaseIcon(phase) {
  switch (phase) {
    case 'running':
      return new vscode.ThemeIcon('vm-running', new vscode.ThemeColor('charts.green'))
    case 'suspended':
      return new vscode.ThemeIcon('debug-pause')
    case 'failed':
      return new vscode.ThemeIcon('error', new vscode.ThemeColor('errorForeground'))
    case 'stopped':
      return new vscode.ThemeIcon('vm-outline')
    default:
      return new vscode.ThemeIcon('loading~spin')
  }
}

// alias is the SSH host an environment is known by: hangar-<name> for the
// user's own, hangar-<name>.<owner> for anyone else's.
function alias(me, env) {
  if (env.owner_id === me.id) return `hangar-${env.name}`
  return `hangar-${env.name}.${env.owner.replace(/[^A-Za-z0-9_-]/g, '_')}`
}

function sshUser(me, env) {
  return env.owner_id === me.id ? env.name : `${env.owner}/${env.name}`
}

// writeSSHConfig writes the extension's SSH config and known_hosts for
// what the server lists, where they differ from what is on disk.
function writeSSHConfig(server, me, envs) {
  const gw = me.ssh
  if (!gw) return
  const keyAlias = `hangar-gateway-${gw.host}-${gw.port}`
  const identity = ctx.globalState.get(identityKey)
  const lines = [
    `# Hangar environments on ${server}, written by the Hangar extension from`,
    '# the server\'s list. Changes here are replaced.',
    '',
  ]
  for (const env of envs) {
    lines.push(
      `Host ${alias(me, env)}`,
      `  HostName ${gw.host}`,
      `  Port ${gw.port}`,
      `  User ${sshUser(me, env)}`,
      `  HostKeyAlias ${keyAlias}`,
      '  UserKnownHostsFile ~/.ssh/hangar/known_hosts',
      '  StrictHostKeyChecking yes',
      '  ServerAliveInterval 30',
    )
    if (identity) lines.push(`  IdentityFile "${identity}"`)
    lines.push('')
  }
  fs.mkdirSync(hangarDir, { recursive: true, mode: 0o700 })
  replaceFile(hangarConfig, lines.join('\n'))
  replaceFile(hangarKnownHosts, `${keyAlias} ${gw.host_key}\n`)
}

function replaceFile(file, content) {
  try {
    if (fs.readFileSync(file, 'utf8') === content) return
  } catch {
    // Not written yet.
  }
  const tmp = `${file}.${process.pid}.tmp`
  fs.writeFileSync(tmp, content, { mode: 0o600 })
  fs.renameSync(tmp, file)
}

// userSSHConfig is the SSH config Remote-SSH reads.
function userSSHConfig() {
  const configured = vscode.workspace.getConfiguration('remote.SSH').get('configFile')
  if (configured) return configured.replace(/^~(?=$|[\\/])/, os.homedir())
  return path.join(os.homedir(), '.ssh', 'config')
}

// ensureInclude has the user's SSH config include the extension's, asking
// first. The line goes at the top: an Include after a Host block would
// belong to that block.
async function ensureInclude() {
  const file = userSSHConfig()
  let current = ''
  try {
    current = fs.readFileSync(file, 'utf8')
  } catch {
    // No config yet.
  }
  if (current.split(/\r?\n/).some((l) => l.trim() === includeLine)) return true
  const answer = await vscode.window.showInformationMessage(
    `Hangar needs one line at the top of ${file} so that ssh and Remote-SSH can find your environments:`,
    { modal: true, detail: includeLine },
    'Add It',
  )
  if (answer !== 'Add It') return false
  fs.mkdirSync(path.dirname(file), { recursive: true, mode: 0o700 })
  fs.writeFileSync(file, `# Hangar environments.\n${includeLine}\n\n${current}`, { mode: 0o600 })
  return true
}

// ensureRemoteSSH has an SSH remote extension installed: Microsoft's, or
// the open one where Microsoft's is not available.
async function ensureRemoteSSH() {
  if (remoteSSH.some((id) => vscode.extensions.getExtension(id))) return true
  const id = /code - oss|codium|vscodium/i.test(vscode.env.appName) ? remoteSSH[1] : remoteSSH[0]
  const answer = await vscode.window.showInformationMessage(
    'Opening a Hangar environment needs an SSH remote extension.',
    { modal: true },
    `Install ${id}`,
  )
  if (!answer) return false
  await vscode.window.withProgress(
    { location: vscode.ProgressLocation.Notification, title: `Installing ${id}…` },
    () => vscode.commands.executeCommand('workbench.extensions.installExtension', id),
  )
  return true
}

// localKeys are the public keys in ~/.ssh.
function localKeys() {
  const dir = path.join(os.homedir(), '.ssh')
  let names = []
  try {
    names = fs.readdirSync(dir).filter((n) => n.endsWith('.pub'))
  } catch {
    return []
  }
  const keys = []
  for (const name of names) {
    try {
      const line = fs.readFileSync(path.join(dir, name), 'utf8').trim()
      const [type, blob] = line.split(/\s+/)
      if (!type || !blob) continue
      const fingerprint =
        'SHA256:' + crypto.createHash('sha256').update(Buffer.from(blob, 'base64')).digest('base64').replace(/=+$/, '')
      keys.push({ file: path.join(dir, name), line, type, fingerprint })
    } catch {
      // Unreadable; not offered.
    }
  }
  return keys
}

// ensureLoginKey has a key of this machine's among the user's sign-in keys,
// offering to add one when none is.
async function ensureLoginKey(c) {
  const profile = await c.profile()
  const registered = new Set(profile.login_keys.map((k) => k.fingerprint))
  const local = localKeys()
  const match = local.find((k) => registered.has(k.fingerprint))
  if (match) {
    rememberIdentity(match.file)
    return true
  }
  const answer = await vscode.window.showWarningMessage(
    profile.login_keys.length === 0
      ? 'You have no SSH sign-in keys in Hangar, so SSH cannot sign you in.'
      : 'None of the SSH keys in ~/.ssh is one of your Hangar sign-in keys.',
    { modal: true, detail: 'Add one of this machine\'s public keys to your Hangar profile?' },
    'Add a Key',
    'Continue Anyway',
  )
  if (answer === 'Continue Anyway') return true
  if (answer === 'Add a Key') return addKey()
  return false
}

function rememberIdentity(pubFile) {
  const priv = pubFile.replace(/\.pub$/, '')
  const home = os.homedir()
  const value = (priv.startsWith(home) ? '~' + priv.slice(home.length) : priv).replace(/\\/g, '/')
  if (ctx.globalState.get(identityKey) !== value) {
    ctx.globalState.update(identityKey, value)
    tree.refresh()
  }
}

async function addKey() {
  const c = await client()
  if (!c) return false
  const keys = localKeys()
  const picks = keys.map((k) => ({ label: path.basename(k.file), description: k.type, detail: k.fingerprint, key: k }))
  picks.push({ label: 'Paste a public key…', key: null })
  const pick = await vscode.window.showQuickPick(picks, {
    title: 'Add an SSH key to your Hangar profile',
    placeHolder: keys.length ? 'A public key from ~/.ssh' : 'No public keys in ~/.ssh: make one with ssh-keygen -t ed25519',
  })
  if (!pick) return false
  let line = pick.key?.line
  if (!line) {
    line = await vscode.window.showInputBox({
      title: 'Add an SSH key to your Hangar profile',
      prompt: 'A public key, as in ~/.ssh/id_ed25519.pub',
      ignoreFocusOut: true,
    })
    if (!line) return false
  }
  try {
    await c.addLoginKey(line.trim())
  } catch (err) {
    vscode.window.showErrorMessage(`Hangar: ${err.message}`)
    return false
  }
  if (pick.key) rememberIdentity(pick.key.file)
  vscode.window.showInformationMessage('Hangar: the key signs you in to your environments.')
  return true
}

// waitRunning starts an environment if it is not running, and waits until
// it is.
async function waitRunning(c, env) {
  if (env.phase === 'running') return env
  return vscode.window.withProgress(
    { location: vscode.ProgressLocation.Notification, title: `Starting ${env.name}…`, cancellable: true },
    async (progress, cancel) => {
      if (env.desired !== 'running') await c.act(env.id, 'start')
      const deadline = Date.now() + 10 * 60 * 1000
      for (;;) {
        if (cancel.isCancellationRequested) return null
        const now = (await c.environments()).find((e) => e.id === env.id)
        if (!now) throw new Error(`${env.name} is gone`)
        if (now.phase === 'running') {
          tree.refresh()
          return now
        }
        if (now.phase === 'failed') throw new Error(`${env.name} failed to start${now.reason ? `: ${now.reason}` : ''}`)
        if (now.desired !== 'running') throw new Error(`${env.name} is asked to be ${now.desired}`)
        progress.report({ message: now.reason ?? now.phase })
        if (Date.now() > deadline) throw new Error(`${env.name} has not started after ten minutes`)
        await new Promise((r) => setTimeout(r, 2000))
      }
    },
  )
}

async function pickEnvironment() {
  const c = await client()
  if (!c) {
    await signIn()
    return null
  }
  const [me, envs] = await Promise.all([c.me(), c.environments()])
  const pick = await vscode.window.showQuickPick(
    envs
      .filter((e) => e.desired !== 'deleted')
      .map((e) => ({
        label: e.name,
        description: e.owner_id === me.id ? e.phase : `${e.owner} · ${e.phase}`,
        detail: e.template,
        env: e,
      })),
    { title: 'Open a Hangar environment' },
  )
  return pick?.env ?? null
}

async function open(node, newWindow) {
  try {
    let env = node?.env ?? (await pickEnvironment())
    if (!env) return
    const c = await client()
    if (!c) return signIn()
    const me = await c.me()
    if (!me.ssh) {
      vscode.window.showErrorMessage('Hangar: this server runs no SSH gateway, so environments cannot be opened here.')
      return
    }
    if (!(await ensureRemoteSSH())) return
    if (!(await ensureLoginKey(c))) return
    if (!(await ensureInclude())) return
    env = await waitRunning(c, env)
    if (!env) return
    writeSSHConfig(c.server, me, tree.envs.some((e) => e.id === env.id) ? tree.envs : [...tree.envs, env])
    const host = alias(me, env)
    // Remote-SSH asks for a host's platform unless it is told.
    const remote = vscode.workspace.getConfiguration('remote.SSH')
    const platforms = remote.get('remotePlatform') ?? {}
    if (platforms[host] !== 'linux') {
      await remote.update('remotePlatform', { ...platforms, [host]: 'linux' }, vscode.ConfigurationTarget.Global)
    }
    const folder = env.spec?.editor_path || '/home/dev'
    const uri = vscode.Uri.from({ scheme: 'vscode-remote', authority: `ssh-remote+${host}`, path: folder })
    log.appendLine(`opening ${uri.toString()}`)
    await vscode.commands.executeCommand('vscode.openFolder', uri, { forceNewWindow: newWindow })
  } catch (err) {
    vscode.window.showErrorMessage(`Hangar: ${err.message}`)
  }
}

async function act(node, action) {
  const c = await client()
  if (!c || !node?.env) return
  try {
    await c.act(node.env.id, action)
  } catch (err) {
    vscode.window.showErrorMessage(`Hangar: ${err.message}`)
  }
  tree.refresh()
}

function normaliseServer(input) {
  let s = input.trim()
  if (!/^https?:\/\//i.test(s)) s = 'https://' + s
  const u = new URL(s)
  return u.origin
}

async function askServer() {
  const value = await vscode.window.showInputBox({
    title: 'Sign in to Hangar',
    prompt: "The Hangar server's address",
    placeHolder: 'https://hangar.example.com',
    value: ctx.globalState.get(serverKey) ?? '',
    ignoreFocusOut: true,
    validateInput: (v) => {
      try {
        normaliseServer(v)
        return null
      } catch {
        return 'Not an address'
      }
    },
  })
  return value ? normaliseServer(value) : null
}

// signIn sends the user to the server, which asks them to allow VS Code,
// makes an access token and sends it back through the extension's URI.
async function signIn() {
  const server = await askServer()
  if (!server) return
  const state = crypto.randomBytes(16).toString('hex')
  const callback = await vscode.env.asExternalUri(
    vscode.Uri.parse(`${vscode.env.uriScheme}://${ctx.extension.id}/signed-in`),
  )
  const redirect = Buffer.from(callback.toString(true)).toString('base64url')
  const url = `${server}/connect/vscode?redirect=${redirect}&state=${state}`
  const done = new Promise((resolve) => {
    pending = { server, state, resolve }
  })
  await vscode.env.openExternal(vscode.Uri.parse(url, true))
  const result = await vscode.window.withProgress(
    {
      location: vscode.ProgressLocation.Notification,
      title: `Signing in to ${server} in your browser…`,
      cancellable: true,
    },
    (_, cancel) =>
      Promise.race([done, new Promise((resolve) => cancel.onCancellationRequested(() => resolve('cancelled')))]),
  )
  pending = null
  if (result === 'cancelled') {
    const answer = await vscode.window.showInformationMessage(
      'Sign in with an access token from Hangar instead?',
      'Paste a Token',
    )
    if (answer) await signInWithToken(server)
    return
  }
  await finishSignIn(server, result)
}

async function signInWithToken(server) {
  server = typeof server === 'string' ? server : await askServer()
  if (!server) return
  vscode.env.openExternal(vscode.Uri.parse(`${server}/account`))
  const token = await vscode.window.showInputBox({
    title: 'Sign in to Hangar',
    prompt: 'An access token, from Account → Access tokens',
    placeHolder: 'hgr_…',
    password: true,
    ignoreFocusOut: true,
  })
  if (token) await finishSignIn(server, token.trim())
}

async function finishSignIn(server, token) {
  let me
  try {
    me = await new Client(server, token).me()
  } catch (err) {
    vscode.window.showErrorMessage(`Hangar: ${err.message}`)
    return
  }
  await ctx.globalState.update(serverKey, server)
  await ctx.secrets.store(tokenKey, token)
  vscode.window.showInformationMessage(`Hangar: signed in to ${new URL(server).host} as ${me.username}.`)
  await tree.refresh()
}

async function signOut() {
  await ctx.secrets.delete(tokenKey)
  await tree.refresh()
}

async function openInBrowser(target) {
  const server = ctx.globalState.get(serverKey)
  if (server) vscode.env.openExternal(vscode.Uri.parse(server + target))
}

function activate(context) {
  ctx = context
  log = vscode.window.createOutputChannel('Hangar')
  tree = new Tree()
  context.subscriptions.push(
    log,
    vscode.window.registerTreeDataProvider('hangar.environments', tree),
    vscode.window.registerUriHandler({
      handleUri(uri) {
        if (uri.path !== '/signed-in' || !pending) return
        const q = new URLSearchParams(uri.query)
        if (q.get('state') !== pending.state || !q.get('token')) {
          log.appendLine('a sign-in came back that this window did not start')
          return
        }
        pending.resolve(q.get('token'))
      },
    }),
    vscode.commands.registerCommand('hangar.signIn', signIn),
    vscode.commands.registerCommand('hangar.signInWithToken', signInWithToken),
    vscode.commands.registerCommand('hangar.signOut', signOut),
    vscode.commands.registerCommand('hangar.refresh', () => tree.refresh()),
    vscode.commands.registerCommand('hangar.addKey', addKey),
    vscode.commands.registerCommand('hangar.createInBrowser', () => openInBrowser('/environments/new')),
    vscode.commands.registerCommand('hangar.open', (node) => open(node, false)),
    vscode.commands.registerCommand('hangar.openInNewWindow', (node) => open(node, true)),
    vscode.commands.registerCommand('hangar.openInBrowser', (node) => openInBrowser(`/environments/${node.env.id}`)),
    vscode.commands.registerCommand('hangar.start', (node) => act(node, 'start')),
    vscode.commands.registerCommand('hangar.stop', (node) => act(node, 'stop')),
    vscode.commands.registerCommand('hangar.suspend', (node) => act(node, 'suspend')),
    vscode.commands.registerCommand('hangar.copySSH', async (node) => {
      const c = await client()
      if (!c) return
      const me = await c.me()
      if (!me.ssh) return
      const port = me.ssh.port === 22 ? '' : ` -p ${me.ssh.port}`
      await vscode.env.clipboard.writeText(`ssh ${sshUser(me, node.env)}@${me.ssh.host}${port}`)
    }),
    vscode.window.onDidChangeWindowState((s) => {
      if (s.focused) tree.refresh()
    }),
    { dispose: () => clearTimeout(tree.timer) },
  )
  tree.refresh()
}

function deactivate() {}

module.exports = { activate, deactivate }
