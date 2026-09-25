# Hangar for the desktop

Hangar's control panel as an app: every server you use in one window, in
tabs that keep their state.

## What it is

The app is two things:

- **Its own parts**, built into the app: the tab bar, the home page across
  servers, the environment switcher (⌘L), the menu bar icon, notifications
  when an environment is ready or fails, and opening an environment in
  desktop VS Code or a terminal. These call each server's **client API**,
  `/api/v1`, which is versioned: the app checks `/api/v1/version` first and
  says whether the server or the app needs updating when they do not agree.
- **Each server's own control panel**, its web UI, shown in a tab. It is
  always the panel that server ships, so it never falls out of step with it.

Every tab is a view of its own and stays alive while another is shown: VS
Code, terminals and desktops keep everything when you move between tabs. A
server's tabs share its session, so signing in once, on its own login page,
signs in the whole app; there is no token to paste.

Middle-click or ⌘-click an environment in a control panel to open it in a
new tab.

| Keys | |
| --- | --- |
| ⌘T / ⌘W | New tab / close tab |
| ⌘⇧T | Reopen the last closed tab |
| ⌃Tab / ⌃⇧Tab, ⌘1…9 | Move between tabs |
| ⌘L | Go to an environment (↵ open, ⌘↵ VS Code, ⌥↵ terminal) |

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
