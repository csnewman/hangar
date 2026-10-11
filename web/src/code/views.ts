import type { MenuItem } from './ContextMenu'

// The ways a file can be shown: as text to edit, as an image, or as its
// bytes.
export type FileView = 'text' | 'image' | 'hex'

export const viewNames: Record<FileView, string> = {
  text: 'Text Editor',
  image: 'Image Viewer',
  hex: 'Hex Editor',
}

const imageTypes: Record<string, string> = {
  png: 'image/png',
  jpg: 'image/jpeg',
  jpeg: 'image/jpeg',
  gif: 'image/gif',
  webp: 'image/webp',
  bmp: 'image/bmp',
  ico: 'image/x-icon',
  avif: 'image/avif',
  svg: 'image/svg+xml',
}

const ext = (path: string) => (path.split('/').pop() ?? '').split('.').pop()?.toLowerCase() ?? ''

// imageType is the media type of a file a browser can show as an image,
// by its name.
export function imageType(path: string): string | undefined {
  return imageTypes[ext(path)]
}

// opensAsImage says whether a file opens as an image without asking: an
// image that is not text, as an SVG is.
export function opensAsImage(path: string): boolean {
  return !!imageType(path) && ext(path) !== 'svg'
}

// openWith is the Open With menu: each view, the one a file opens in
// first marked.
export function openWith(path: string, open: (view: FileView) => void): MenuItem[] {
  const first: FileView | null = opensAsImage(path) ? 'image' : null
  return [
    { heading: 'Open With' },
    ...(['text', 'image', 'hex'] as FileView[]).map((v) => ({
      label: viewNames[v],
      checked: v === first,
      onSelect: () => open(v),
    })),
  ]
}
