// The part of noVNC's RFB client Hangar uses. noVNC ships no types.
declare module '@novnc/novnc' {
  export interface RFBOptions {
    shared?: boolean
    credentials?: { username?: string; password?: string; target?: string }
    wsProtocols?: string[]
  }

  export default class RFB extends EventTarget {
    constructor(target: HTMLElement, urlOrChannel: string | WebSocket, options?: RFBOptions)
    viewOnly: boolean
    focusOnClick: boolean
    clipViewport: boolean
    scaleViewport: boolean
    resizeSession: boolean
    showDotCursor: boolean
    background: string
    qualityLevel: number
    compressionLevel: number
    readonly capabilities: { power: boolean }
    disconnect(): void
    focus(): void
    blur(): void
    sendCtrlAltDel(): void
    clipboardPasteFrom(text: string): void
  }
}
