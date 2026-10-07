// Package firewall reports and adds the Windows Firewall inbound rule the
// direct transport needs. The helper is started hidden from inside the game,
// so it never asks for elevation itself: a UAC prompt popping up behind a
// full-screen game, with no window to explain it, would be dismissed and the
// mod would fail silently. The helper never checks the rule at start: SDR is
// outbound-only and needs no rule, and a direct attempt that the firewall
// drops simply times out and falls back to SDR. These are power-user
// commands: `wartales-mp firewall`, run once from an elevated prompt, adds
// the rule; `wartales-mp firewall check` reports it.
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
	out, err := quiet(exec.CommandContext(ctx, "netsh", "advfirewall", "firewall", "show", "rule",
		"name="+RuleName, "dir=in")).CombinedOutput()
	text := strings.TrimSpace(string(bytes.ToValidUTF8(out, []byte("?"))))
	if err != nil {
		// netsh exits 1 when no rule matches. Its text is localised ("No rules
		// match ..." only on English Windows), so the exit code decides; the
		// text goes into the log as is.
		var exit *exec.ExitError
		if errors.As(err, &exit) && exit.ExitCode() == 1 {
			return State{Present: false, Detail: "no inbound rule " + strconv.Quote(RuleName) + " (netsh: " + firstLine(text) + ")"}, nil
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
	_ = quiet(exec.CommandContext(ctx, "netsh", "advfirewall", "firewall", "delete", "rule", "name="+RuleName)).Run()
	//nolint:gosec // exe is os.Executable() of this very process and port a parsed int: nothing user-typed reaches netsh
	out, err := quiet(exec.CommandContext(ctx, "netsh", "advfirewall", "firewall", "add", "rule",
		"name="+RuleName, "dir=in", "action=allow", "protocol=TCP",
		"localport="+strconv.Itoa(port), "program="+exe, "profile=any",
		"description=wartales-mp direct transport (relay and proxy-link)")).CombinedOutput()
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
