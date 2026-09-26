import { useMutation } from '@tanstack/react-query'
import { useSearchParams } from 'react-router'

import { api } from '../api'
import { Logo } from '../components/Logo'
import { useMe } from '../session'

// The editors the Hangar extension runs in, by the URI scheme each opens
// links to it with.
const editors: Record<string, string> = {
  vscode: 'VS Code',
  'vscode-insiders': 'VS Code Insiders',
  vscodium: 'VSCodium',
  'vscodium-insiders': 'VSCodium Insiders',
  'code-oss': 'Code - OSS',
  cursor: 'Cursor',
  windsurf: 'Windsurf',
  positron: 'Positron',
}

const extensionID = 'hangar.hangar-remote'

// callback is where the token goes back to: the Hangar extension's own
// URI in an editor on this machine, and nowhere else.
function callback(encoded: string | null): { url: URL; editor: string } | null {
  if (!encoded) return null
  try {
    const url = new URL(atob(encoded.replace(/-/g, '+').replace(/_/g, '/')))
    const editor = editors[url.protocol.replace(/:$/, '')]
    if (!editor || url.host.toLowerCase() !== extensionID || url.pathname !== '/signed-in') return null
    return { url, editor }
  } catch {
    return null
  }
}

// ConnectVSCodePage signs the Hangar extension in: once the user allows it,
// it makes an access token and hands it to the extension.
export function ConnectVSCodePage() {
  const me = useMe()
  const [params] = useSearchParams()
  const target = callback(params.get('redirect'))
  const state = params.get('state') ?? ''
  const allow = useMutation({
    mutationFn: () => {
      const day = new Date().toLocaleDateString(undefined, { day: 'numeric', month: 'short', year: 'numeric' })
      return api.createToken(`${target!.editor}, ${day}`, 365)
    },
    onSuccess: (t) => {
      // The editor's own parameters stay: windowId sends the link to the
      // window that asked to sign in.
      const url = new URL(target!.url)
      url.searchParams.set('token', t.token)
      url.searchParams.set('state', state)
      window.location.assign(url.toString())
    },
  })

  return (
    <div className="login">
      <div className="login-card">
        <div className="login-brand">
          <Logo size={28} />
          <span>Hangar</span>
        </div>
        {!target || !/^[0-9a-f]{32}$/.test(state) ? (
          <>
            <h1>Not a sign-in link</h1>
            <p className="muted">Start signing in from the Hangar extension in VS Code.</p>
          </>
        ) : allow.isSuccess ? (
          <>
            <h1>Signed in</h1>
            <p className="muted">
              {target.editor} is signed in as {me.username}. You can close this tab. The token is under Account, where
              it can be revoked.
            </p>
          </>
        ) : (
          <>
            <h1>Sign in {target.editor}?</h1>
            <p className="muted">
              The Hangar extension in {target.editor} asks to act as <strong>{me.username}</strong>: to list, start and
              stop your environments and open them over SSH. It gets an access token for a year, which you can revoke
              under Account.
            </p>
            <p className="muted small">Allow it only if you started this from {target.editor} yourself.</p>
            {allow.error && <div className="alert">{allow.error.message}</div>}
            <button
              type="button"
              className="btn btn-primary btn-block"
              disabled={allow.isPending}
              onClick={() => allow.mutate()}
            >
              Allow {target.editor}
            </button>
          </>
        )}
      </div>
    </div>
  )
}
