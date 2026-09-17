package install

import (
	"os"
	"path/filepath"
	"strings"
)

// Markers delimiting our block inside the hosts file, so that install and
// uninstall never touch anything else.
const (
	blockBegin = "# BEGIN wartales-mp"
	blockEnd   = "# END wartales-mp"
)

// HostsPath returns %SystemRoot%\System32\drivers\etc\hosts.
func HostsPath() string {
	root := os.Getenv("SystemRoot")
	if root == "" {
		root = `C:\Windows`
	}
	return filepath.Join(root, "System32", "drivers", "etc", "hosts")
}

// AddHostsEntries rewrites our marked block, leaving the rest of the file
// untouched. Running it twice changes nothing.
func AddHostsEntries() error {
	lines := []string{blockBegin}
	for _, h := range MasterHosts {
		lines = append(lines, "127.0.0.1 "+h)
	}
	lines = append(lines, blockEnd)
	return rewriteHosts(lines)
}

// RemoveHostsEntries deletes our block.
func RemoveHostsEntries() error { return rewriteHosts(nil) }

func rewriteHosts(block []string) error {
	path := HostsPath()
	raw, err := os.ReadFile(path)
	if err != nil {
		if !os.IsNotExist(err) {
			return err
		}
		raw = nil
	}

	kept := stripBlock(splitLines(string(raw)))
	if len(block) > 0 {
		kept = append(kept, block...)
	}

	out := strings.Join(kept, "\r\n")
	if out != "" {
		out += "\r\n"
	}
	return os.WriteFile(path, []byte(out), 0o644)
}

// stripBlock removes a previously installed block and any trailing blank lines.
func stripBlock(lines []string) []string {
	var kept []string
	inBlock := false
	for _, l := range lines {
		switch {
		case strings.TrimSpace(l) == blockBegin:
			inBlock = true
		case strings.TrimSpace(l) == blockEnd:
			inBlock = false
		case !inBlock:
			kept = append(kept, l)
		}
	}
	for len(kept) > 0 && strings.TrimSpace(kept[len(kept)-1]) == "" {
		kept = kept[:len(kept)-1]
	}
	return kept
}

func splitLines(s string) []string {
	if s == "" {
		return nil
	}
	s = strings.ReplaceAll(s, "\r\n", "\n")
	return strings.Split(strings.TrimSuffix(s, "\n"), "\n")
}
