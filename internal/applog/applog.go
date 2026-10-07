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
// truncated so a replication burst cannot fill the disk, and secrets (session
// token, relay passwords, join codes, credential headers) are logged as their
// length only, because players attach this file to public bug reports.
package applog

import (
	"encoding/json"
	"fmt"
	"io"
	"log"
	"os"
	"path/filepath"
	"regexp"
	"sort"
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

// secretKeys are the JSON keys whose string values never reach the log: the
// Steam session token and session id of user/login, the relay passwords of
// instance/get, and the join codes (they carry the lobby's route and key).
// Players attach this log to bug reports, so the value is replaced with its
// length only. Compared case-insensitively.
var secretKeys = []string{
	"token", "sid", "hostpw", "slavepw", "password", "passwd", "pass", "pw",
	"ticket", "authticket", "secret", "auth", "authorization", "cookie",
	"invite", "shortcode", "code",
}

// secretValue matches `"key": "value"` for every key in secretKeys;
// secretEmbedded the same inside a JSON document embedded as a string
// (`\"key\":\"value\"`, an escape-free value).
var (
	secretAlt      = `(?:` + strings.Join(secretKeys, "|") + `)`
	secretValue    = regexp.MustCompile(`(?i)("` + secretAlt + `"\s*:\s*")((?:[^"\\]|\\.)*)(")`)
	secretEmbedded = regexp.MustCompile(`(?i)(\\"` + secretAlt + `\\"\s*:\s*\\")([^"\\]*)(\\")`)
)

// secretHeaders are handshake headers that hold credentials.
var secretHeaders = map[string]bool{"x-pass": true, "authorization": true, "cookie": true, "proxy-authorization": true}

// secretCmds are commands whose payload is a bare JSON string that is a secret
// (lobby/initInvite answers the join code itself).
var secretCmds = map[string]bool{"lobby/initInvite": true}

// Redacted is what a secret of n bytes is logged as.
func Redacted(n int) string { return "<redacted:" + strconv.Itoa(n) + ">" }

// Redact replaces the value of every secret key in s with Redacted(len).
func Redact(s string) string {
	for _, re := range []*regexp.Regexp{secretValue, secretEmbedded} {
		s = re.ReplaceAllStringFunc(s, func(m string) string {
			g := re.FindStringSubmatch(m)
			if strings.HasPrefix(g[2], "<redacted:") {
				return m
			}
			return g[1] + Redacted(len(g[2])) + g[3]
		})
	}
	return s
}

// HeaderLine renders handshake headers in a stable order, credentials redacted.
func HeaderLine(h map[string]string) string {
	keys := make([]string, 0, len(h))
	for k := range h {
		keys = append(keys, k)
	}
	sort.Strings(keys)
	var b strings.Builder
	for _, k := range keys {
		if b.Len() > 0 {
			b.WriteString(" ")
		}
		b.WriteString(k)
		b.WriteString("=")
		if secretHeaders[strings.ToLower(k)] {
			b.WriteString(Redacted(len(h[k])))
			continue
		}
		b.WriteString(h[k])
	}
	return b.String()
}

// Payload is Trunc for a command's arguments or reply: on top of the secret
// keys, a command whose whole payload is a secret string is redacted.
func Payload(cmd string, v any) string {
	s := Trunc(v)
	if secretCmds[cmd] && strings.HasPrefix(s, `"`) {
		var str string
		n := len(s) - 2
		if json.Unmarshal([]byte(s), &str) == nil {
			n = len(str)
		}
		return `"` + Redacted(n) + `"`
	}
	return s
}

// Secret renders a secret value for a log line: its length only.
func Secret(s string) string { return Redacted(len(s)) }

// Trunc renders a payload for the log, bounded to MaxValue bytes, with the
// values of secret keys redacted. Control characters are escaped so one frame
// stays on one line.
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
	s = Redact(s) // before the cut, so a secret is never half kept
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
