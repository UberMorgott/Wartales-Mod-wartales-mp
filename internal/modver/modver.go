// Package modver fingerprints the mod files a co-op session depends on, so a
// guest whose files differ from the host's is stopped at the join instead of
// crashing later on "Missing <sheet>.<id>".
//
// The hxbit signature the game exchanges covers code schemas only; data.cdb
// (inside the Remastered res1.pak) is neither exchanged nor hashed, and two
// different wartales-mp builds patch the bytecode differently. So each helper
// hashes, once per session, the two files players get from the mod archive:
// the game folder's winmm.dll (this helper is embedded in it, and so is the
// bytecode patcher) and res1.pak.
package modver

import (
	"crypto/sha256"
	"encoding/hex"
	"errors"
	"fmt"
	"io"
	"io/fs"
	"os"
	"path/filepath"
	"runtime/debug"
	"strings"
	"time"
)

// None is the hash of a file that does not exist: a player without the
// Remastered mod has no res1.pak, which differs from one who has it.
const None = "none"

// Files compared, in the order they are reported.
const (
	FileDLL  = "winmm.dll"
	FileRes1 = "res1.pak"
)

// Info is one player's fingerprint. An empty hash is unknown and is never
// compared; Build is for logs only (the DLL hash already covers it).
type Info struct {
	Build string `json:"build,omitempty"`
	DLL   string `json:"dll,omitempty"`
	Res1  string `json:"res1,omitempty"`
}

// String renders the fingerprint for the log.
func (i *Info) String() string {
	if i == nil {
		return "none (older wartales-mp)"
	}
	return fmt.Sprintf("build %s, %s %s, %s %s", orUnknown(i.Build), FileDLL, short(i.DLL), FileRes1, short(i.Res1))
}

func orUnknown(s string) string {
	if s == "" {
		return "unknown"
	}
	return s
}

func short(h string) string {
	switch {
	case h == "":
		return "unknown"
	case len(h) > 12:
		return h[:12]
	}
	return h
}

// Result is the outcome of comparing two fingerprints.
type Result struct {
	// Differ lists the files whose hashes differ.
	Differ []string
	// Compared lists the files known on both sides.
	Compared []string
	// Unchecked lists the files unknown on either side, or every file when
	// the other side sent no fingerprint at all (an older wartales-mp).
	Unchecked []string
}

// Mismatch reports that at least one compared file differs.
func (r Result) Mismatch() bool { return len(r.Differ) > 0 }

// Compare checks remote against local. A nil fingerprint, or a file unknown
// on either side, is unchecked: never a mismatch.
func Compare(local, remote *Info) Result {
	var r Result
	if local == nil {
		local = &Info{}
	}
	if remote == nil {
		remote = &Info{}
	}
	for _, f := range []struct {
		name string
		a, b string
	}{
		{FileDLL, local.DLL, remote.DLL},
		{FileRes1, local.Res1, remote.Res1},
	} {
		switch {
		case f.a == "" || f.b == "":
			r.Unchecked = append(r.Unchecked, f.name)
		case f.a != f.b:
			r.Compared = append(r.Compared, f.name)
			r.Differ = append(r.Differ, f.name)
		default:
			r.Compared = append(r.Compared, f.name)
		}
	}
	return r
}

// Message is the text a guest's game shows when its files differ from the
// host's. One line, no markup: the game's Confirm window renders it as is.
func Message(differ []string) string {
	files := strings.Join(differ, ", ")
	return "Версия мода отличается от хоста: скачай архив заново (отличается: " + files + "). " +
		"Mod version differs from the host: download the archive again (differs: " + files + ")."
}

// Compute hashes the mod files in gameDir. gameDir "" (unknown) leaves both
// hashes unknown; a file that cannot be read is unknown too, and its error is
// returned alongside the partial fingerprint.
func Compute(gameDir string) (*Info, error) {
	info := &Info{Build: Build()}
	if gameDir == "" {
		return info, errors.New("game folder unknown")
	}
	var errs []error
	var err error
	if info.DLL, err = hashFile(filepath.Join(gameDir, FileDLL)); err != nil {
		errs = append(errs, err)
	}
	if info.Res1, err = hashFile(filepath.Join(gameDir, FileRes1)); err != nil {
		errs = append(errs, err)
	}
	return info, errors.Join(errs...)
}

// hashFile is the hex sha256 of path, None when it does not exist, or "" with
// the error when it cannot be read.
func hashFile(path string) (string, error) {
	f, err := os.Open(path) //nolint:gosec // G304: a fixed file name in the game's own folder, read only
	if errors.Is(err, fs.ErrNotExist) {
		return None, nil
	}
	if err != nil {
		return "", err
	}
	defer func() { _ = f.Close() }() // read-only; a close error changes nothing
	h := sha256.New()
	if _, err := io.Copy(h, f); err != nil {
		return "", fmt.Errorf("%s: %w", path, err)
	}
	return hex.EncodeToString(h.Sum(nil)), nil
}

// Build is the commit this helper was built from (12 hex digits, "-dirty"
// when the tree had changes), or "" when the build carries no VCS stamp.
func Build() string {
	bi, ok := debug.ReadBuildInfo()
	if !ok {
		return ""
	}
	var rev string
	var dirty bool
	for _, s := range bi.Settings {
		switch s.Key {
		case "vcs.revision":
			rev = s.Value
		case "vcs.modified":
			dirty = s.Value == "true"
		}
	}
	if len(rev) > 12 {
		rev = rev[:12]
	}
	if rev != "" && dirty {
		rev += "-dirty"
	}
	return rev
}

// Session holds this helper's fingerprint, computed once in the background:
// the files are hashed as the game starts, which is what it loaded.
type Session struct {
	done chan struct{}
	info *Info
}

// NewSession starts hashing gameDir's files in the background; log gets the
// result.
func NewSession(gameDir string, log func(format string, a ...any)) *Session {
	s := &Session{done: make(chan struct{})}
	go func() {
		start := time.Now()
		info, err := Compute(gameDir)
		if err != nil {
			log("modver: fingerprint incomplete (%v); unknown files are not compared with other players", err)
		}
		log("modver: this player's mod files: %s (%s)", info, time.Since(start).Round(time.Millisecond))
		s.info = info
		close(s.done)
	}()
	return s
}

// Fixed is a Session whose fingerprint is already known (tests, tools).
func Fixed(info *Info) *Session {
	s := &Session{done: make(chan struct{}), info: info}
	close(s.done)
	return s
}

// Get returns the fingerprint, waiting at most wait for it; nil when it is
// not ready by then (it is then unchecked).
func (s *Session) Get(wait time.Duration) *Info {
	if s == nil {
		return nil
	}
	select {
	case <-s.done:
		return s.info
	default:
	}
	select {
	case <-s.done:
		return s.info
	case <-time.After(wait):
		return nil
	}
}
