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
     profile, if none of them is there.

Your SSH config is left as it is. Remote-SSH is given the gateway's address
and the environment's username directly, and ssh signs in with the keys it
offers anyway: those your SSH config or the defaults name (`~/.ssh/id_ed25519`
and so on), and those in your SSH agent. When the key Hangar knows you by is
none of them, the extension says so, and offers its own SSH config instead.

## What it writes

- `~/.ssh/known_hosts`: a line for the gateway, `[host]:port` and its host
  key as the server gives it over HTTPS, so there is no fingerprint to
  confirm. If the file already holds a different key for the gateway, the
  extension stops and says how to remove it.
- `remote.SSH.remotePlatform`: `linux` for the gateway.

With **Hangar: Add Environments to SSH Config**, or the `hangar.sshConfig`
setting, it also keeps

- `~/.ssh/hangar/config`: a host per environment, `hangar-<name>` for your
  own and `hangar-<name>.<owner>` for others you can see, naming your key.
  `ssh hangar-dev` works from any terminal, and the extension connects
  through these hosts;
- `~/.ssh/hangar/known_hosts`: the gateway's host key, for those hosts;
- `Include ~/.ssh/hangar/config` at the top of your SSH config, added once
  you agree.

The access token is kept in VS Code's secret storage.

## Building

    npx @vscode/vsce package --skip-license

makes `hangar-remote-<version>.vsix`; install it with **Extensions: Install
from VSIX…** or `code --install-extension`.
