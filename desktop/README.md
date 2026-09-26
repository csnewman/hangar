# Hangar for the desktop

Hangar's control panel as an app. A window is one server: its control
panel in the first tab and each environment in a tab of its own, keeping
its state. Another server is another window.

## What it is

The app is two things:

- **Its own parts**, built into the app: the tab bar, the server picker
  (⌘N), the environment switcher (⌘T, ⌘L), the menu bar icon, notifications
  when an environment is ready or fails, and opening an environment in
  desktop VS Code or a terminal. These call each server's **client API**,
  `/api/v1`, which is versioned: the app checks `/api/v1/version` first and
  says whether the server or the app needs updating when they do not agree.
- **Each server's own control panel**, its web UI, shown in the tabs. It
  is always the panel that server ships, so it never falls out of step
  with it.

The first tab of a window is the server's control panel and stays. Every
other tab is one environment: open one from the control panel, the
switcher or the menu bar, and it gets a tab of its own, or the one it
already has. Pages go where they belong -- an environment's pages to its
tab, templates, profile and administration to the control panel -- so a
link in either lands in the right tab.

Right-click a tab for its menu: duplicate it, move it to a new window,
open its environment in VS Code, a terminal or the browser, copy its link,
reload it, or close it, the others, or those to its left or right. Drag a
tab to reorder it, onto another window of the same server to move it
there, or out of the window to give it a window of its own. A server can
have as many windows as you like; a window only ever holds one server.

Every tab is a view of its own and stays alive while another is shown: VS
Code, terminals and desktops keep everything when you move between tabs,
when a tab is dragged to another window, and when it is moved to another
of its environment's pages. A
server's tabs share its session, so signing in once, on its own login
page, signs in the whole window; there is no token to paste. Windows and
their tabs are restored when the app starts.

| Keys | |
| --- | --- |
| ⌘N | Open another server's window, or add a server |
| ⌘T, ⌘L | Go to an environment (↵ open, ⌘↵ VS Code, ⌥↵ terminal) |
| ⌘W | Close the tab; on the control panel, the window |
| ⌘⇧T | Reopen the last closed tab |
| ⌃Tab / ⌃⇧Tab, ⌘1…9 | Move between tabs |

Links of the form `hangar://open?server=https://hangar.example.com&path=/environments/<id>`
open in the app.

## Opening in VS Code and a terminal

- **VS Code** opens through the Hangar VS Code extension (`vscode/`), which
  must be installed: the app hands it a
  `vscode://hangar.hangar-remote/open?server=…&env=…` link.
- **Terminal** runs `ssh` to the server's SSH gateway, with the gateway's
  host key taken from the server, so there is nothing to confirm. It needs
  one of your public keys under Profile → Sign-in keys.

## Building

    npm ci
    npm start               # build and run
    npm run typecheck
    npm run dist            # installers for this platform, in release/

Installers are unsigned. macOS builds are ad-hoc signed, which Apple
Silicon needs to run them at all; opened from a download, macOS still
refuses them until the quarantine flag is cleared
(`xattr -dr com.apple.quarantine /Applications/Hangar.app`), which the
Homebrew cask does itself.

## Releases and Homebrew

Pushing a tag `desktop-v<version>` builds installers for macOS (arm64 and
x64), Linux (AppImage, deb) and Windows, and attaches them to a GitHub
release with `hangar.rb`, the Homebrew cask for that release. To publish
it, copy `hangar.rb` into `Casks/` of a tap repository named
`homebrew-hangar`; then

    brew tap csnewman/hangar
    brew install --cask hangar
