// Renders the app's icons from Hangar's logo: the app icon, for
// electron-builder to turn into each platform's format, and the menu bar
// icon, a template image macOS tints for light and dark bars.
import { Resvg } from '@resvg/resvg-js'
import { mkdirSync, writeFileSync } from 'node:fs'

const hangar = 'M6 24V14l10-6 10 6v10h-4v-8H10v8z'
const app = `<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 32 32">
  <rect x="1.5" y="1.5" width="29" height="29" rx="7" fill="#2a78d6"/>
  <path d="${hangar}" fill="#fff"/></svg>`
const tray = `<svg xmlns="http://www.w3.org/2000/svg" viewBox="4 4 24 24"><path d="${hangar}" fill="#000"/></svg>`

const png = (svg, size) => new Resvg(svg, { fitTo: { mode: 'width', value: size } }).render().asPng()

mkdirSync('build', { recursive: true })
mkdirSync('assets', { recursive: true })
writeFileSync('build/icon.png', png(app, 1024))
writeFileSync('assets/icon.png', png(app, 256))
writeFileSync('assets/trayTemplate.png', png(tray, 16))
writeFileSync('assets/trayTemplate@2x.png', png(tray, 32))
// Windows and Linux do not tint tray icons, so they get the coloured one.
writeFileSync('assets/tray.png', png(app, 32))
