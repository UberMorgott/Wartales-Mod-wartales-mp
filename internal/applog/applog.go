// Package applog is the helper's debug log.
//
// Everything the helper does with the game ends up in
// %LOCALAPPDATA%\wartales-mp\wartales-mp.log: accepted connections and their
// handshake headers, every master command with its arguments and its reply,
// relay traffic as counters, NAT discovery and every internal error with the
// context it happened in. The previous run is kept as wartales-mp.log.1.
//
// This is a debugging tool for a single player, not telemetry: writes are
// synchronous, nothing is sampled and nothing leaves the machine. Payloads are
// truncated so a replication burst cannot fill the disk.
package applog

import (
	"encoding/json"
	"fmt"
	"io"
	"log"
	"os"
	"path/filepath"
	"strconv"
	"strings"
	"unicode/utf8"
)

// FileName is the log file, Dir is the directory holding it.
const FileName = "wartales-mp.log"

// MaxValue is how much of any logged payload is kept.
const MaxValue = 512

// Dir returns %LOCALAPPDATA%\wartales-mp (the temp dir if LOCALAPPDATA is not
// set, which is the case on non-Windows builds).
func Dir() string {
	base := os.Getenv("LOCALAPPDATA")
	if base == "" {
		base = os.Getenv("XDG_STATE_HOME")
	}
	if base == "" {
		base = os.TempDir()
	}
	return filepath.Join(base, "wartales-mp")
}

// Open rotates the previous log to <name>.1, opens a fresh one and returns a
// logger that writes both to it and to also (normally os.Stdout). The returned
// closer must be called on shutdown. If the file cannot be opened, logging
// falls back to also alone and the error is reported through the logger, so a
// read-only profile never stops the helper from running.
func Open(also io.Writer) (*log.Logger, func() error) {
	dir := Dir()
	path := filepath.Join(dir, FileName)

	var sinks []io.Writer

	var openErr error
	var f *os.File
	if err := os.MkdirAll(dir, 0o755); err != nil {
		openErr = err
	} else {
		// Keep exactly one previous run. Remove first: on Windows a rename
		// onto an existing name fails.
		_ = os.Remove(path + ".1")
		_ = os.Rename(path, path+".1")
		var err error
		//nolint:gosec // path is our own log file under Dir(), not user input
		f, err = os.OpenFile(path, os.O_CREATE|os.O_WRONLY|os.O_TRUNC, 0o644)
		if err != nil {
			openErr = err
		} else {
			sinks = append(sinks, f)
		}
	}
	// The file comes first and every sink is written independently: the helper
	// runs hidden, with no console, so writing to os.Stdout fails -- and
	// io.MultiWriter would give up on the remaining sinks at the first error,
	// which is how the log file ended up empty on the first live run.
	if also != nil {
		sinks = append(sinks, also)
	}

	logger := log.New(tolerantWriter(sinks), "", log.LstdFlags|log.Lmicroseconds)
	if openErr != nil {
		logger.Printf("log: cannot write %s: %v", path, openErr)
	} else {
		logger.Printf("log: %s", path)
	}

	return logger, func() error {
		if f == nil {
			return nil
		}
		return f.Close()
	}
}

// tolerantWriter writes to every sink and reports success as long as the first
// one accepted the bytes, so a dead console cannot silence the log file.
type tolerantWriter []io.Writer

func (w tolerantWriter) Write(p []byte) (int, error) {
	n, err := 0, error(nil)
	for i, s := range w {
		wn, werr := s.Write(p)
		if i == 0 {
			n, err = wn, werr
		}
	}
	if len(w) == 0 {
		return len(p), nil
	}
	return n, err
}

// Trunc renders a payload for the log, bounded to MaxValue bytes. Control
// characters are escaped so one frame stays on one line.
func Trunc(v any) string {
	var s string
	switch t := v.(type) {
	case nil:
		return "null"
	case string:
		s = t
	case []byte:
		s = string(t)
	case error:
		s = t.Error()
	case interface{ String() string }:
		s = t.String()
	default:
		b, err := json.Marshal(v)
		if err != nil {
			s = fmt.Sprint(v)
		} else {
			s = string(b)
		}
	}
	if s == "" {
		return `""`
	}
	full := len(s)
	if full > MaxValue {
		s = s[:MaxValue]
		// do not cut a rune in half
		for len(s) > 0 && !utf8.ValidString(s) {
			s = s[:len(s)-1]
		}
	}
	s = strings.Map(func(r rune) rune {
		if r < 0x20 || r == 0x7f {
			return ' '
		}
		return r
	}, s)
	if full > MaxValue {
		return s + "...(" + strconv.Itoa(full) + "B)"
	}
	return s
}
