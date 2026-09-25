# Hangar for VS Code

Open [Hangar](https://github.com/csnewman/hangar) environments in VS Code on
your own machine.

The Hangar view lists your environments. Opening one starts it if it is not
running and connects Remote-SSH to it, through the Hangar server's SSH
gateway: environments are never reachable from the network themselves.

## Getting started

1. **Hangar: Sign In**, and give your server's address. Your browser opens
   Hangar, which asks you to allow VS Code. (**Hangar: Sign In with an
   Access Token** takes a token from Account → Access tokens instead.)
2. Open an environment. The first time, the extension:
   - offers to install Remote-SSH (Microsoft's, or `jeanp413.open-remote-ssh`
     in VSCodium) if neither is installed;
   - offers to add one of the public keys in `~/.ssh` to your Hangar
     profile, if none of them is there;
   - asks to add `Include ~/.ssh/hangar/config` to the top of your SSH
     config.

## What it writes

- `~/.ssh/hangar/config`: a host per environment, `hangar-<name>` for your
  own and `hangar-<name>.<owner>` for others you can see. `ssh hangar-dev`
  works from any terminal too.
- `~/.ssh/hangar/known_hosts`: the gateway's host key, as the server gives it
  over HTTPS, so there is no fingerprint to confirm.
- `remote.SSH.remotePlatform`: `linux` for each environment you open.

The access token is kept in VS Code's secret storage.

## Building

    npx @vscode/vsce package --skip-license

makes `hangar-remote-<version>.vsix`; install it with **Extensions: Install
from VSIX…** or `code --install-extension`.
