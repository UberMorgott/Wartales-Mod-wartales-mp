//go:build windows

package main

import (
	"fmt"
	"math"
	"os"
	"strings"
	"syscall"
	"unsafe"
)

// gameExe is the process wartales-mp lives and dies with.
const gameExe = "Wartales.exe"

// watchGame finds the game process and returns its PID plus a channel that is
// closed when it exits.
//
// The winmm.dll proxy starts us from inside the game, so our parent is normally
// the game itself; the name is checked before trusting the parent PID, because
// PIDs are reused. If the parent is something else (running wartales-mp.exe by
// hand), the first Wartales.exe on the machine is used instead.
func watchGame() (uint32, <-chan struct{}, error) {
	procs, err := processList()
	if err != nil {
		return 0, nil, err
	}

	rawSelf := os.Getpid()
	if rawSelf < 0 || rawSelf > math.MaxUint32 {
		return 0, nil, fmt.Errorf("own pid %d does not fit in a uint32", rawSelf)
	}
	self := uint32(rawSelf)
	pid := uint32(0)
	for _, p := range procs {
		if p.pid == self && p.parent != 0 {
			if name, ok := procs.name(p.parent); ok && strings.EqualFold(name, gameExe) {
				pid = p.parent
			}
			break
		}
	}
	if pid == 0 {
		for _, p := range procs {
			if strings.EqualFold(p.name, gameExe) {
				pid = p.pid
				break
			}
		}
	}
	if pid == 0 {
		return 0, nil, fmt.Errorf("no %s process found", gameExe)
	}

	h, err := syscall.OpenProcess(syscall.SYNCHRONIZE, false, pid)
	if err != nil {
		return 0, nil, fmt.Errorf("open process %d: %w", pid, err)
	}

	done := make(chan struct{})
	go func() {
		defer close(done)
		defer syscall.CloseHandle(h) //nolint:errcheck // process is gone anyway
		_, _ = syscall.WaitForSingleObject(h, syscall.INFINITE)
	}()
	return pid, done, nil
}

// proc is one entry of the process snapshot.
type proc struct {
	pid    uint32
	parent uint32
	name   string
}

type procs []proc

func (ps procs) name(pid uint32) (string, bool) {
	for _, p := range ps {
		if p.pid == pid {
			return p.name, true
		}
	}
	return "", false
}

// processList snapshots the running processes via Toolhelp.
func processList() (procs, error) {
	snap, err := syscall.CreateToolhelp32Snapshot(syscall.TH32CS_SNAPPROCESS, 0)
	if err != nil {
		return nil, fmt.Errorf("process snapshot: %w", err)
	}
	defer syscall.CloseHandle(snap) //nolint:errcheck // nothing to do on failure

	var e syscall.ProcessEntry32
	e.Size = uint32(unsafe.Sizeof(e))
	if err := syscall.Process32First(snap, &e); err != nil {
		return nil, fmt.Errorf("process snapshot: %w", err)
	}

	var out procs
	for {
		out = append(out, proc{
			pid:    e.ProcessID,
			parent: e.ParentProcessID,
			name:   syscall.UTF16ToString(e.ExeFile[:]),
		})
		if err := syscall.Process32Next(snap, &e); err != nil {
			if err == syscall.ERROR_NO_MORE_FILES {
				return out, nil
			}
			return nil, fmt.Errorf("process snapshot: %w", err)
		}
	}
}
