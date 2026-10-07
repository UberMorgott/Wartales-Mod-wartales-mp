package firewall

import (
	"os/exec"
	"syscall"
)

// createNoWindow is CREATE_NO_WINDOW. netsh is a console program and the
// helper has no console (it is built -H windowsgui and started detached), so
// without it every netsh call opens its own empty terminal window next to the
// game while the output is piped back to us.
const createNoWindow = 0x08000000

func quiet(cmd *exec.Cmd) *exec.Cmd {
	cmd.SysProcAttr = &syscall.SysProcAttr{HideWindow: true, CreationFlags: createNoWindow}
	return cmd
}
