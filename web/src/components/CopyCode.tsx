import { Copy } from 'lucide-react'
import { useState } from 'react'

// CopyCode shows a command or a name, with a button that copies it. With
// href, the text is also a link to it, opened in a new tab.
export function CopyCode({ text, href }: { text: string; href?: string }) {
  const [copied, setCopied] = useState(false)
  const copy = () => {
    navigator.clipboard.writeText(text).then(() => {
      setCopied(true)
      setTimeout(() => setCopied(false), 1500)
    })
  }
  return (
    <span className="ssh-command">
      {href ? (
        <a className="mono" href={href} target="_blank" rel="noreferrer">
          {text}
        </a>
      ) : (
        <code className="mono">{text}</code>
      )}
      <button type="button" className="btn btn-ghost" onClick={copy} title="Copy">
        <Copy size={13} />
        {copied ? 'Copied' : 'Copy'}
      </button>
    </span>
  )
}
