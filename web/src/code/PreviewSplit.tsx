import { Columns2, Eye, SquarePen } from 'lucide-react'
import { useEffect, useRef, useState, type ReactNode } from 'react'
import type * as Y from 'yjs'

// PreviewLayout is what the tab of a file with a preview shows: its
// editor, its preview, or both side by side, as IntelliJ offers them.
export type PreviewLayout = 'editor' | 'split' | 'preview'

function remembered<T>(key: string, parse: (v: string | null) => T): T {
  try {
    return parse(localStorage.getItem(key))
  } catch {
    return parse(null)
  }
}

function remember(key: string, value: string) {
  try {
    localStorage.setItem(key, value)
  } catch {
    // Chosen again next time.
  }
}

// useYText is a shared document's text, kept up with its edits, at most
// every tenth of a second.
export function useYText(text: Y.Text): string {
  const [value, setValue] = useState(() => text.toString())
  useEffect(() => {
    let timer: ReturnType<typeof setTimeout> | undefined
    const changed = () => {
      if (timer) return
      timer = setTimeout(() => {
        timer = undefined
        setValue(text.toString())
      }, 100)
    }
    setValue(text.toString())
    text.observe(changed)
    return () => {
      clearTimeout(timer)
      text.unobserve(changed)
    }
  }, [text])
  return value
}

// PreviewSplit lays a file's editor and its preview out as the layout
// chosen last for files of its kind, which its buttons at the top right
// change, with the split between them dragged.
export function PreviewSplit({
  kind,
  initial = 'split',
  names,
  previewIcon,
  editor,
  preview,
}: {
  // kind names the files the layout is remembered for: markdown, table.
  kind: string
  initial?: PreviewLayout
  // names are the buttons' titles: the editor alone, both, the preview
  // alone.
  names: [string, string, string]
  previewIcon?: ReactNode
  editor: ReactNode
  preview: ReactNode
}) {
  const layoutKey = `hangar.code.${kind}`
  const ratioKey = `hangar.code.${kind}.ratio`
  const [layout, setLayout] = useState<PreviewLayout>(() =>
    remembered(layoutKey, (v) => (v === 'editor' || v === 'preview' || v === 'split' ? v : initial)),
  )
  const [ratio, setRatio] = useState(() =>
    remembered(ratioKey, (v) => {
      const n = Number(v)
      return v && n >= 0.15 && n <= 0.85 ? n : 0.5
    }),
  )
  const box = useRef<HTMLDivElement>(null)
  const choose = (l: PreviewLayout) => {
    setLayout(l)
    remember(layoutKey, l)
  }
  const drag = (e: React.PointerEvent) => {
    const el = box.current
    if (!el) return
    e.preventDefault()
    const rect = el.getBoundingClientRect()
    const at = (x: number) => Math.min(Math.max((x - rect.left) / rect.width, 0.15), 0.85)
    const move = (m: PointerEvent) => setRatio(at(m.clientX))
    const up = (u: PointerEvent) => {
      move(u)
      remember(ratioKey, String(at(u.clientX)))
      window.removeEventListener('pointermove', move)
      window.removeEventListener('pointerup', up)
    }
    window.addEventListener('pointermove', move)
    window.addEventListener('pointerup', up)
  }
  const button = (l: PreviewLayout, title: string, icon: ReactNode) => (
    <button
      type="button"
      className={layout === l ? 'code-tool-btn code-tool-btn-on' : 'code-tool-btn'}
      title={title}
      aria-label={title}
      aria-pressed={layout === l}
      onClick={() => choose(l)}
    >
      {icon}
    </button>
  )
  return (
    <div ref={box} className={`md-split md-layout-${layout}`}>
      <div className="md-split-editor" style={layout === 'split' ? { flexBasis: `${ratio * 100}%` } : undefined}>
        {editor}
      </div>
      {layout === 'split' && <div className="md-split-bar" onPointerDown={drag} />}
      {layout !== 'editor' && <div className="md-split-preview">{preview}</div>}
      <div className="md-split-tools">
        {button('editor', names[0], <SquarePen size={14} />)}
        {button('split', names[1], <Columns2 size={14} />)}
        {button('preview', names[2], previewIcon ?? <Eye size={14} />)}
      </div>
    </div>
  )
}
