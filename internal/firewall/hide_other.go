//go:build !windows

package firewall

import "os/exec"

func quiet(cmd *exec.Cmd) *exec.Cmd { return cmd }
