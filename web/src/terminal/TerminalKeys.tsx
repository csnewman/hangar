import { ArrowDown, ArrowLeft, ArrowRight, ArrowUp, Keyboard, SendHorizontal } from 'lucide-react'
import { useState, type ReactNode } from 'react'

// KeyTarget is what the keys act on: the terminal, as TerminalPane gives it.
export interface KeyTarget {
  // press sends a key as if pressed on a keyboard, so the terminal encodes
  // it as the program in it asked: an arrow in application cursor mode, a
  // key under the kitty keyboard protocol.
  press(key: string, code: string, keyCode: number, ctrl?: boolean): void
  // type sends text as typed.
  type(text: string): void
  // submit pastes text, if there is any, and presses Enter.
  submit(text: string): void
  // focus gives the terminal the keyboard, to type into it directly.
  focus(): void
}

// The keys a phone's keyboard lacks, or hides a page away.
const keys: { label: ReactNode; name: string; key: string; code: string; keyCode: number }[] = [
  { label: 'esc', name: 'Escape', key: 'Escape', code: 'Escape', keyCode: 27 },
  { label: 'tab', name: 'Tab', key: 'Tab', code: 'Tab', keyCode: 9 },
  { label: <ArrowUp size={16} />, name: 'Up', key: 'ArrowUp', code: 'ArrowUp', keyCode: 38 },
  { label: <ArrowDown size={16} />, name: 'Down', key: 'ArrowDown', code: 'ArrowDown', keyCode: 40 },
  { label: <ArrowLeft size={16} />, name: 'Left', key: 'ArrowLeft', code: 'ArrowLeft', keyCode: 37 },
  { label: <ArrowRight size={16} />, name: 'Right', key: 'ArrowRight', code: 'ArrowRight', keyCode: 39 },
]

// Characters a phone's keyboard keeps a page or two away, typed as they are.
const chars = ['|', '/', '-', '~']

// TerminalKeys is a terminal's keyboard on a phone: a row of the keys a
// phone's keyboard lacks, and a box to write in, sent with Send. The box is
// the phone's own text field, so dictation, autocorrect and the predictive
// bar work in it, and nothing reaches the program until it is sent; the
// keyboard button types into the terminal directly instead.
//
// Ctrl applies to the next key: one in the row, a letter written in the box,
// or one typed into the terminal (TerminalPane asks ctrl).
export function TerminalKeys({
  target,
  ctrl,
  onCtrl,
}: {
  target: () => KeyTarget | null
  ctrl: boolean
  onCtrl: (on: boolean) => void
}) {
  const [text, setText] = useState('')

  const press = (k: { key: string; code: string; keyCode: number }) => {
    target()?.press(k.key, k.code, k.keyCode, ctrl)
    onCtrl(false)
  }
  const type = (c: string) => {
    if (ctrl) {
      const k = keyFor(c)
      target()?.press(k.key, k.code, k.keyCode, true)
      onCtrl(false)
      return
    }
    target()?.type(c)
  }
  const send = () => {
    target()?.submit(text)
    setText('')
  }

  return (
    <div className="term-keys">
      <div className="term-keys-row" role="toolbar" aria-label="Keys">
        {keys.slice(0, 2).map((k) => (
          <Key key={k.name} label={k.label} name={k.name} onPress={() => press(k)} />
        ))}
        <button
          type="button"
          className={ctrl ? 'term-key term-key-on' : 'term-key'}
          aria-pressed={ctrl}
          onPointerDown={(e) => e.preventDefault()}
          onClick={() => onCtrl(!ctrl)}
        >
          ctrl
        </button>
        {keys.slice(2).map((k) => (
          <Key key={k.name} label={k.label} name={k.name} onPress={() => press(k)} />
        ))}
        {chars.map((c) => (
          <Key key={c} label={c} name={c} onPress={() => type(c)} />
        ))}
      </div>
      <div className="term-compose">
        <button
          type="button"
          className="term-compose-btn"
          aria-label="Type straight into the terminal"
          title="Type straight into the terminal"
          onClick={() => target()?.focus()}
        >
          <Keyboard size={20} />
        </button>
        <textarea
          className="term-compose-box"
          rows={1}
          placeholder={ctrl ? 'Ctrl+…' : 'Send to the terminal…'}
          aria-label="Text to send"
          value={text}
          autoCapitalize="off"
          onChange={(e) => {
            const v = e.target.value
            // With Ctrl on, a single character written is a Ctrl key, sent at
            // once rather than written.
            if (ctrl && v.length === text.length + 1 && v.startsWith(text)) {
              const k = keyFor(v[v.length - 1])
              target()?.press(k.key, k.code, k.keyCode, true)
              onCtrl(false)
              return
            }
            setText(v)
          }}
        />
        <button
          type="button"
          className="term-compose-btn term-compose-send"
          aria-label={text ? 'Send' : 'Press Enter'}
          title={text ? 'Send, then press Enter' : 'Press Enter'}
          onPointerDown={(e) => e.preventDefault()}
          onClick={send}
        >
          <SendHorizontal size={20} />
        </button>
      </div>
    </div>
  )
}

function Key({ label, name, onPress }: { label: ReactNode; name: string; onPress: () => void }) {
  return (
    <button
      type="button"
      className="term-key"
      aria-label={name}
      // Pressing a key leaves the keyboard where it was: on the box, or on
      // the terminal.
      onPointerDown={(e) => e.preventDefault()}
      onClick={onPress}
    >
      {label}
    </button>
  )
}

// keyFor is the key that types a character, as a keyboard reports it.
function keyFor(c: string): { key: string; code: string; keyCode: number } {
  const upper = c.toUpperCase()
  if (upper >= 'A' && upper <= 'Z') return { key: c.toLowerCase(), code: `Key${upper}`, keyCode: upper.charCodeAt(0) }
  if (c >= '0' && c <= '9') return { key: c, code: `Digit${c}`, keyCode: c.charCodeAt(0) }
  const named: Record<string, { code: string; keyCode: number }> = {
    ' ': { code: 'Space', keyCode: 32 },
    '[': { code: 'BracketLeft', keyCode: 219 },
    ']': { code: 'BracketRight', keyCode: 221 },
    '\\': { code: 'Backslash', keyCode: 220 },
    '/': { code: 'Slash', keyCode: 191 },
    '-': { code: 'Minus', keyCode: 189 },
    '|': { code: 'Backslash', keyCode: 220 },
    '~': { code: 'Backquote', keyCode: 192 },
  }
  const n = named[c] ?? { code: '', keyCode: 0 }
  return { key: c, ...n }
}
