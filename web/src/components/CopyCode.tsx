import { Copy } from 'lucide-react'
import { useState } from 'react'

// CopyCode shows a command or a name, with a button that copies it.
export function CopyCode({ text }: { text: string }) {
  const [copied, setCopied] = useState(false)
  const copy = () => {
    navigator.clipboard.writeText(text).then(() => {
      setCopied(true)
      setTimeout(() => setCopied(false), 1500)
    })
  }
  return (
    <span className="ssh-command">
      <code className="mono">{text}</code>
      <button type="button" className="btn btn-ghost" onClick={copy} title="Copy">
        <Copy size={13} />
        {copied ? 'Copied' : 'Copy'}
      </button>
    </span>
  )
}
