// Package firewall reports and adds the Windows Firewall inbound rule the
// direct transport needs. The helper is started hidden from inside the game,
// so it never asks for elevation itself: a UAC prompt popping up behind a
// full-screen game, with no window to explain it, would be dismissed and the
// mod would fail silently. Instead the rule's presence is checked and logged
// at every start, the direct route simply stays unverified without it (SDR
// needs no rule at all), and `wartales-mp firewall`, run once from an
// elevated prompt, adds it.
package firewall

import (
	"bytes"
	"context"
	"errors"
	"fmt"
	"os/exec"
	"runtime"
	"strconv"
	"strings"
	"time"
)

// RuleName is the inbound rule's name.
const RuleName = "wartales-mp"

// ErrUnsupported is returned off Windows.
var ErrUnsupported = errors.New("firewall rules are a Windows feature")

// State is what the check found.
type State struct {
	Present bool
	Detail  string // for the log
}

// Check reports whether the inbound rule exists, via netsh (no elevation
// needed to read).
func Check(ctx context.Context) (State, error) {
	if runtime.GOOS != "windows" {
		return State{}, ErrUnsupported
	}
	ctx, cancel := context.WithTimeout(ctx, 10*time.Second)
	defer cancel()
	out, err := exec.CommandContext(ctx, "netsh", "advfirewall", "firewall", "show", "rule",
		"name="+RuleName, "dir=in").CombinedOutput()
	text := strings.TrimSpace(string(bytes.ToValidUTF8(out, []byte("?"))))
	if err != nil {
		// netsh exits 1 when no rule matches; anything it printed says so.
		if strings.Contains(strings.ToLower(text), "no rules match") {
			return State{Present: false, Detail: "no inbound rule " + strconv.Quote(RuleName)}, nil
		}
		return State{}, fmt.Errorf("netsh: %w (%s)", err, firstLine(text))
	}
	return State{Present: true, Detail: "inbound rule " + strconv.Quote(RuleName) + " present"}, nil
}

// Add creates the inbound rule for exe on port. Needs an elevated process;
// the error says so when it is not.
func Add(ctx context.Context, exe string, port int) error {
	if runtime.GOOS != "windows" {
		return ErrUnsupported
	}
	ctx, cancel := context.WithTimeout(ctx, 20*time.Second)
	defer cancel()
	// Replace an older rule for the same name first, so a moved exe or a new
	// port never leaves two rules behind.
	_ = exec.CommandContext(ctx, "netsh", "advfirewall", "firewall", "delete", "rule", "name="+RuleName).Run()
	//nolint:gosec // exe is os.Executable() of this very process and port a parsed int: nothing user-typed reaches netsh
	out, err := exec.CommandContext(ctx, "netsh", "advfirewall", "firewall", "add", "rule",
		"name="+RuleName, "dir=in", "action=allow", "protocol=TCP",
		"localport="+strconv.Itoa(port), "program="+exe, "profile=any",
		"description=wartales-mp direct transport (relay and proxy-link)").CombinedOutput()
	text := strings.TrimSpace(string(bytes.ToValidUTF8(out, []byte("?"))))
	if err != nil {
		if strings.Contains(strings.ToLower(text), "requested operation requires elevation") ||
			strings.Contains(strings.ToLower(text), "administrator") {
			return fmt.Errorf("adding the rule needs an elevated prompt (run as administrator): %s", firstLine(text))
		}
		return fmt.Errorf("netsh: %w (%s)", err, firstLine(text))
	}
	return nil
}

func firstLine(s string) string {
	if i := strings.IndexAny(s, "\r\n"); i >= 0 {
		return s[:i]
	}
	return s
}
