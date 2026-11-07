package main

import (
	"os"
	"strconv"
	"strings"

	"github.com/csnewman/hangar/internal/agentstatus"
)

// agentProcesses finds coding agents' processes in /proc.
var agentProcesses = agentstatus.Processes{Agent: agentOf, Alive: alive}

// hookShells are what an agent may run its hooks through.
var hookShells = map[string]bool{"sh": true, "bash": true, "dash": true, "zsh": true, "fish": true, "env": true}

// agentOf is the process a hook's parent belongs to: the parent, or, when
// it is a shell the agent ran the hook in, what started that.
func agentOf(parent int) int {
	pid := parent
	for range 4 {
		comm, ppid, ok := stat(pid)
		if !ok || !hookShells[comm] || ppid <= 1 {
			return pid
		}
		pid = ppid
	}
	return pid
}

// stat reads a process's command name and parent from /proc.
func stat(pid int) (comm string, ppid int, ok bool) {
	b, err := os.ReadFile("/proc/" + strconv.Itoa(pid) + "/stat")
	if err != nil {
		return "", 0, false
	}
	// pid (comm) state ppid ...; comm may hold spaces and parentheses.
	s := string(b)
	open, end := strings.IndexByte(s, '('), strings.LastIndexByte(s, ')')
	if open < 0 || end < open {
		return "", 0, false
	}
	fields := strings.Fields(s[end+1:])
	if len(fields) < 2 {
		return "", 0, false
	}
	ppid, err = strconv.Atoi(fields[1])
	return s[open+1 : end], ppid, err == nil
}

// alive is whether a process is running: there, and not a zombie.
func alive(pid int) bool {
	b, err := os.ReadFile("/proc/" + strconv.Itoa(pid) + "/stat")
	if err != nil {
		return false
	}
	end := strings.LastIndexByte(string(b), ')')
	fields := strings.Fields(string(b)[end+1:])
	return len(fields) > 0 && fields[0] != "Z" && fields[0] != "X"
}
