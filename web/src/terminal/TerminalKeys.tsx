import {
  ArrowDown,
  ArrowLeft,
  ArrowRight,
  ArrowUp,
  CornerDownLeft,
  Keyboard,
  KeyboardOff,
  PenLine,
  SendHorizontal,
  X,
} from 'lucide-react'
import { useRef, useState, type ReactNode } from 'react'
import { flushSync } from 'react-dom'

import { useKeyboard } from '../phone'

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
  // blur takes the keyboard away from the terminal, putting it down.
  blur(): void
}

type KeyDef = { label: ReactNode; name: string; key: string; code: string; keyCode: number; ctrl?: boolean }

const esc: KeyDef = { label: 'esc', name: 'Escape', key: 'Escape', code: 'Escape', keyCode: 27 }
const tab: KeyDef = { label: 'tab', name: 'Tab', key: 'Tab', code: 'Tab', keyCode: 9 }
const up: KeyDef = { label: <ArrowUp size={16} />, name: 'Up', key: 'ArrowUp', code: 'ArrowUp', keyCode: 38 }
const down: KeyDef = { label: <ArrowDown size={16} />, name: 'Down', key: 'ArrowDown', code: 'ArrowDown', keyCode: 40 }
const left: KeyDef = { label: <ArrowLeft size={16} />, name: 'Left', key: 'ArrowLeft', code: 'ArrowLeft', keyCode: 37 }
const right: KeyDef = {
  label: <ArrowRight size={16} />,
  name: 'Right',
  key: 'ArrowRight',
  code: 'ArrowRight',
  keyCode: 39,
}
const enter: KeyDef = { label: <CornerDownLeft size={16} />, name: 'Enter', key: 'Enter', code: 'Enter', keyCode: 13 }
const interrupt: KeyDef = { label: '^C', name: 'Ctrl+C', key: 'c', code: 'KeyC', keyCode: 67, ctrl: true }

// Characters a phone's keyboard keeps a page or two away, typed as they are.
const chars = ['|', '/', '-', '~']

// TerminalKeys is a terminal's keyboard on a phone, one way of writing at a
// time:
//
//   - With the phone's keyboard down, a short row of what answers a
//     program's question (Escape, the arrows, Enter, Ctrl+C), and buttons
//     to type into the terminal or write a message.
//   - Typing into the terminal, the keys a phone's keyboard lacks, on top
//     of it. Ctrl applies to the next key, in the row or typed
//     (TerminalPane asks ctrl).
//   - Writing a message, the phone's own text field, so dictation,
//     autocorrect and the predictive bar work in it and nothing reaches the
//     program until it is sent, with Enter after it. Closing it keeps what
//     was written.
export function TerminalKeys({
  target,
  name,
  ctrl,
  onCtrl,
}: {
  target: () => KeyTarget | null
  // name is what a message goes to, the session's title.
  name?: string
  ctrl: boolean
  onCtrl: (on: boolean) => void
}) {
  const keyboard = useKeyboard()
  const [writing, setWriting] = useState(false)
  const [text, setText] = useState('')
  const box = useRef<HTMLTextAreaElement>(null)

  const press = (k: KeyDef) => {
    target()?.press(k.key, k.code, k.keyCode, k.ctrl || ctrl)
    onCtrl(false)
  }
  // The box is focused in the tap that opens it, or the phone keeps its
  // keyboard down.
  const write = () => {
    onCtrl(false)
    flushSync(() => setWriting(true))
    box.current?.focus()
  }
  const send = () => {
    target()?.submit(text)
    setText('')
    setWriting(false)
  }

  if (writing) {
    return (
      <div className="term-keys term-write">
        <div className="term-write-head">
          <span>{name ? `To ${name}` : 'To the terminal'} · sent with Enter</span>
          <button
            type="button"
            className="term-compose-btn term-write-close"
            aria-label="Close, keeping what is written"
            onClick={() => setWriting(false)}
          >
            <X size={18} />
          </button>
        </div>
        <div className="term-compose">
          <textarea
            ref={box}
            className="term-compose-box"
            rows={3}
            aria-label="Message"
            value={text}
            autoCapitalize="sentences"
            onChange={(e) => setText(e.target.value)}
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

  const writeButton = (
    <ModeButton label={text ? 'Write a message (a draft is kept)' : 'Write a message'} onPress={write} on={!!text}>
      <PenLine size={20} />
    </ModeButton>
  )

  if (keyboard) {
    return (
      <div className="term-keys term-keys-typing">
        <div className="term-keys-row" role="toolbar" aria-label="Keys">
          <Key k={esc} onPress={press} />
          <Key k={tab} onPress={press} />
          <button
            type="button"
            className={ctrl ? 'term-key term-key-on' : 'term-key'}
            aria-pressed={ctrl}
            onPointerDown={(e) => e.preventDefault()}
            onClick={() => onCtrl(!ctrl)}
          >
            ctrl
          </button>
          {[up, down, left, right].map((k) => (
            <Key key={k.name} k={k} onPress={press} />
          ))}
          {chars.map((c) => (
            <Key key={c} k={{ label: c, name: c, ...keyFor(c) }} onPress={() => typeChar(c)} />
          ))}
        </div>
        <span className="term-keys-sep" aria-hidden="true" />
        {writeButton}
        <ModeButton label="Put the keyboard away" onPress={() => target()?.blur()}>
          <KeyboardOff size={20} />
        </ModeButton>
      </div>
    )
  }

  return (
    <div className="term-keys term-keys-rest" role="toolbar" aria-label="Keys">
      <ModeButton label="Type into the terminal" onPress={() => target()?.focus()}>
        <Keyboard size={20} />
      </ModeButton>
      <span className="term-keys-sep" aria-hidden="true" />
      {[esc, up, down, enter, interrupt].map((k) => (
        <Key key={k.name} k={k} onPress={press} />
      ))}
      <span className="term-keys-gap" />
      <span className="term-keys-sep" aria-hidden="true" />
      {writeButton}
    </div>
  )

  function typeChar(c: string) {
    if (ctrl) {
      const k = keyFor(c)
      target()?.press(k.key, k.code, k.keyCode, true)
      onCtrl(false)
      return
    }
    target()?.type(c)
  }
}

function ModeButton({
  label,
  onPress,
  on = false,
  children,
}: {
  label: string
  onPress: () => void
  on?: boolean
  children: ReactNode
}) {
  return (
    <button
      type="button"
      className={on ? 'term-mode term-mode-on' : 'term-mode'}
      aria-label={label}
      title={label}
      onPointerDown={(e) => e.preventDefault()}
      onClick={onPress}
    >
      {children}
    </button>
  )
}

function Key({ k, onPress }: { k: KeyDef; onPress: (k: KeyDef) => void }) {
  return (
    <button
      type="button"
      className="term-key"
      aria-label={k.name}
      // Pressing a key leaves the keyboard where it was: up or down.
      onPointerDown={(e) => e.preventDefault()}
      onClick={() => onPress(k)}
    >
      {k.label}
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
