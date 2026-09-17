package hlpatch

import (
	"bytes"
	"os"
	"path/filepath"
	"testing"
)

// gamePath is where the retail game keeps its bytecode. The file is read-only
// here and is never written; when it is absent (CI, another machine, another
// install path) the live check is skipped, and WARTALES_HLBOOT overrides it.
const gamePath = `D:\Steam\steamapps\common\Wartales\hlboot.dat`

func TestApplyPatchesLiveBytecode(t *testing.T) {
	path := gamePath
	if env := os.Getenv("WARTALES_HLBOOT"); env != "" {
		path = env
	}
	buf, err := os.ReadFile(path)
	if err != nil {
		t.Skipf("no game bytecode at %s: %v", path, err)
	}
	if string(buf[:len(Magic)]) != Magic {
		t.Fatalf("%s does not start with %q", path, Magic)
	}
	orig := append([]byte(nil), buf...)
	if err := Apply(buf); err != nil {
		t.Fatalf("Apply on the real bytecode: %v", err)
	}
	// Exactly one byte per patch, each the one we asked for.
	changed := 0
	for i := range buf {
		if buf[i] != orig[i] {
			changed++
		}
	}
	if changed != len(Patches) {
		t.Fatalf("changed %d bytes, want %d", changed, len(Patches))
	}
	for _, p := range Patches {
		pos := bytes.Index(buf, patched(p))
		if pos < 0 {
			t.Fatalf("patch %q: patched signature not found", p.Name)
		}
		if bytes.Count(buf, p.Needle) != 0 {
			t.Fatalf("patch %q: original signature still present", p.Name)
		}
	}
	// Re-applying must refuse: the needles are gone.
	if err := Apply(buf); err == nil {
		t.Fatal("Apply on an already patched image: want error, got nil")
	}
}

func TestApplyRequiresExactlyOneMatch(t *testing.T) {
	p := Patches[0]
	t.Run("missing", func(t *testing.T) {
		buf := make([]byte, 4096)
		if err := Apply(buf); err == nil {
			t.Fatal("want error for an image without the signature")
		}
	})
	t.Run("duplicate", func(t *testing.T) {
		buf := append(append([]byte{0, 1, 2}, p.Needle...), p.Needle...)
		if err := Apply(buf); err == nil {
			t.Fatal("want error for an ambiguous signature")
		}
	})
	t.Run("brokenSignature", func(t *testing.T) {
		buf := make([]byte, 0, 64)
		for _, q := range Patches {
			buf = append(buf, q.Needle...)
		}
		buf[p.Index] = 0xff // the byte the patch would rewrite, changed by a game update
		if err := Apply(buf); err == nil {
			t.Fatal("want error when the signature no longer matches")
		}
	})
	t.Run("nothingWrittenOnFailure", func(t *testing.T) {
		buf := append([]byte(nil), p.Needle...) // only the first patch matches
		before := append([]byte(nil), buf...)
		if err := Apply(buf); err == nil {
			t.Fatal("want error when a later patch does not match")
		}
		if !bytes.Equal(buf, before) {
			t.Fatal("a failed Apply wrote to the image")
		}
	})
}

// TestPatchesAreWellFormed guards the invariants the C side relies on.
func TestPatchesAreWellFormed(t *testing.T) {
	for _, p := range Patches {
		if p.Index < 0 || p.Index >= len(p.Needle) {
			t.Fatalf("patch %q: index %d outside the needle", p.Name, p.Index)
		}
		if p.Needle[p.Index] != p.From {
			t.Fatalf("patch %q: needle byte 0x%02x != From 0x%02x", p.Name, p.Needle[p.Index], p.From)
		}
		if p.From == p.To {
			t.Fatalf("patch %q: patch is a no-op", p.Name)
		}
	}
}

// TestHeaderMatchesShim keeps the generated C table in sync with the table
// above; regenerate with: go run ./tools/hlpatchgen -out shim/proxy/hlpatch.h
func TestHeaderMatchesShim(t *testing.T) {
	path := filepath.Join("..", "..", "shim", "proxy", "hlpatch.h")
	got, err := os.ReadFile(path)
	if err != nil {
		t.Fatalf("read %s: %v", path, err)
	}
	if !bytes.Equal(bytes.ReplaceAll(got, []byte("\r\n"), []byte("\n")), Header()) {
		t.Fatalf("%s is stale: run go run ./tools/hlpatchgen -out shim/proxy/hlpatch.h", path)
	}
}

// patched returns the needle as it looks once the patch is applied.
func patched(p Patch) []byte {
	out := append([]byte(nil), p.Needle...)
	out[p.Index] = p.To
	return out
}
