import type { MenuItem } from './ContextMenu'

// The ways a file can be shown: as text to edit, as an image, a PDF, as
// sound or video to play, or as its bytes.
export type FileView = 'text' | 'image' | 'pdf' | 'media' | 'hex'

export const viewNames: Record<FileView, string> = {
  text: 'Text Editor',
  image: 'Image Viewer',
  pdf: 'PDF Viewer',
  media: 'Media Player',
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

const mediaTypes: Record<string, string> = {
  pdf: 'application/pdf',
  mp3: 'audio/mpeg',
  m4a: 'audio/mp4',
  aac: 'audio/aac',
  wav: 'audio/wav',
  flac: 'audio/flac',
  ogg: 'audio/ogg',
  oga: 'audio/ogg',
  opus: 'audio/ogg',
  mp4: 'video/mp4',
  m4v: 'video/mp4',
  mov: 'video/quicktime',
  webm: 'video/webm',
  ogv: 'video/ogg',
}

const ext = (path: string) => (path.split('/').pop() ?? '').split('.').pop()?.toLowerCase() ?? ''

// imageType is the media type of a file a browser can show as an image,
// by its name.
export function imageType(path: string): string | undefined {
  return imageTypes[ext(path)]
}

// mediaType is the media type of a PDF, a sound or a video, by its name.
export function mediaType(path: string): string | undefined {
  return mediaTypes[ext(path)]
}

// opensAs is the view a file opens in without asking, by its name: null
// for one whose content says.
export function opensAs(path: string): FileView | null {
  if (opensAsImage(path)) return 'image'
  const type = mediaType(path)
  if (!type) return null
  return type === 'application/pdf' ? 'pdf' : 'media'
}

// opensAsImage says whether a file opens as an image without asking: an
// image that is not text, as an SVG is.
export function opensAsImage(path: string): boolean {
  return !!imageType(path) && ext(path) !== 'svg'
}

// openWith is the Open With submenu: each view that could show the file,
// the one it opens in first marked.
export function openWith(path: string, open: (view: FileView) => void): MenuItem[] {
  const first = opensAs(path)
  const views: FileView[] = ['text', 'image', ...(first === 'pdf' || first === 'media' ? [first] : []), 'hex']
  return [
    ...views.map((v) => ({
      label: viewNames[v],
      checked: v === first,
      onSelect: () => open(v),
    })),
  ]
}
