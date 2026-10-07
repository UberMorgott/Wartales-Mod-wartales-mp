package main

import (
	"os"
	"path/filepath"
	"syscall"
	"unsafe"

	"github.com/UberMorgott/wartales-mp/internal/applog"
)

var (
	procAttachConsole = syscall.NewLazyDLL("kernel32.dll").NewProc("AttachConsole")
	procMessageBoxW   = syscall.NewLazyDLL("user32.dll").NewProc("MessageBoxW")
)

const (
	attachParentProcess = ^uint32(0) // ATTACH_PARENT_PROCESS, (DWORD)-1
	mbOK                = 0x00000000
	mbIconError         = 0x00000010
	mbSetForeground     = 0x00010000
	mbTopmost           = 0x00040000
)

// consoleAttached is true once useParentConsole found a console to write to.
var consoleAttached bool

// useParentConsole makes the command-line commands readable. The exe is built
// -H windowsgui, so the copy the game starts opens no window at all; run from a
// command prompt, it attaches to that prompt's console instead. Handles that
// are already redirected (a pipe or a file) are kept as they are.
func useParentConsole() {
	if valid(syscall.Stdout) && valid(syscall.Stderr) {
		consoleAttached = true
		return
	}
	if r, _, _ := procAttachConsole.Call(uintptr(attachParentProcess)); r == 0 {
		return // started by the game: no console anywhere, the log file is it
	}
	consoleAttached = true
	if !valid(syscall.Stdout) {
		if f, err := os.OpenFile("CONOUT$", os.O_WRONLY, 0); err == nil {
			os.Stdout = f
		}
	}
	if !valid(syscall.Stderr) {
		if f, err := os.OpenFile("CONOUT$", os.O_WRONLY, 0); err == nil {
			os.Stderr = f
		}
	}
}

func valid(h syscall.Handle) bool { return h != 0 && h != syscall.InvalidHandle }

// fatalNotice tells the player that the helper stopped, when nobody would see
// stderr: the game started it hidden. The error is in the log as well.
func fatalNotice(err error) {
	if consoleAttached {
		return
	}
	text := "The co-op helper (wartales-mp) stopped:\n\n" + err.Error() +
		"\n\nCo-op will not work until the game is restarted. Details are in:\n" +
		filepath.Join(applog.Dir(), applog.FileName)
	title := "Wartales Co-op Fix"
	t, err1 := syscall.UTF16PtrFromString(text)
	c, err2 := syscall.UTF16PtrFromString(title)
	if err1 != nil || err2 != nil {
		return
	}
	//nolint:gosec // UTF-16 pointers into Go memory, live for the duration of this syscall
	_, _, _ = procMessageBoxW.Call(0, uintptr(unsafe.Pointer(t)), uintptr(unsafe.Pointer(c)),
		mbOK|mbIconError|mbSetForeground|mbTopmost)
}
