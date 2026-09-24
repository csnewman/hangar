// Package sysuser starts processes in the guest as one of its users.
package sysuser

import (
	"bufio"
	"os"
	"os/exec"
	"os/user"
	"strconv"
	"strings"
	"syscall"
)

// SSHAuthSock is the guest's SSH agent, which signs with the environment
// owner's keys (internal/profile). Every process started for the user is
// given it.
const SSHAuthSock = "/run/hangar/ssh-agent.sock"

// Credential is who a user's processes run as, with every group they are
// in: without the supplementary groups a user in docker's group could not
// reach Docker.
func Credential(u *user.User) (*syscall.Credential, error) {
	uid, err := strconv.ParseUint(u.Uid, 10, 32)
	if err != nil {
		return nil, err
	}
	gid, err := strconv.ParseUint(u.Gid, 10, 32)
	if err != nil {
		return nil, err
	}
	cred := &syscall.Credential{Uid: uint32(uid), Gid: uint32(gid)}
	if ids, err := u.GroupIds(); err == nil {
		for _, id := range ids {
			if g, err := strconv.ParseUint(id, 10, 32); err == nil {
				cred.Groups = append(cred.Groups, uint32(g))
			}
		}
	}
	return cred, nil
}

// RunAs makes cmd run as u, unless this process already is u.
func RunAs(cmd *exec.Cmd, u *user.User) error {
	if strconv.Itoa(os.Geteuid()) == u.Uid {
		return nil
	}
	cred, err := Credential(u)
	if err != nil {
		return err
	}
	if cmd.SysProcAttr == nil {
		cmd.SysProcAttr = &syscall.SysProcAttr{}
	}
	cmd.SysProcAttr.Credential = cred
	return nil
}

// LoginShell reads a user's shell from /etc/passwd, which os/user does not
// report, and falls back to bash or sh.
func LoginShell(name string) string {
	if f, err := os.Open("/etc/passwd"); err == nil {
		defer f.Close()
		sc := bufio.NewScanner(f)
		for sc.Scan() {
			fields := strings.Split(sc.Text(), ":")
			if len(fields) == 7 && fields[0] == name && fields[6] != "" {
				if _, err := os.Stat(fields[6]); err == nil && !strings.HasSuffix(fields[6], "nologin") {
					return fields[6]
				}
			}
		}
	}
	for _, sh := range []string{"/bin/bash", "/bin/sh"} {
		if _, err := os.Stat(sh); err == nil {
			return sh
		}
	}
	return "/bin/sh"
}
