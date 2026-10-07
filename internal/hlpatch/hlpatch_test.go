package hlpatch

import (
	"bytes"
	"encoding/binary"
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
		buf := append(image(goodInts()), make([]byte, 4096)...)
		if err := Apply(buf); err == nil {
			t.Fatal("want error for an image without the signature")
		}
	})
	t.Run("duplicate", func(t *testing.T) {
		buf := append(append(image(goodInts()), p.Needle...), p.Needle...)
		if err := Apply(buf); err == nil {
			t.Fatal("want error for an ambiguous signature")
		}
	})
	t.Run("brokenSignature", func(t *testing.T) {
		buf := image(goodInts())
		hdr := len(buf)
		for _, q := range Patches {
			buf = append(buf, q.Needle...)
		}
		buf[hdr+p.Index] = 0xff // the byte the patch would rewrite, changed by a game update
		if err := Apply(buf); err == nil {
			t.Fatal("want error when the signature no longer matches")
		}
	})
	t.Run("nothingWrittenOnFailure", func(t *testing.T) {
		buf := append(image(goodInts()), p.Needle...) // only the first patch matches
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
	got, err := os.ReadFile(path) //nolint:gosec // fixed in-repo test fixture path
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

// The captured loop head is from allPlayersAssigned@24643, ops 10..17.
// Its +48 branch lands at op 62 with r3 still null, returning true. Entering
// the loop instead rejects a fourth player in a three-human, one-animal party.
func TestAllPlayersAssignedAllowsPlayerWithoutHuman(t *testing.T) {
	head := []byte{0x42, 0x47, 0x04, 0x26, 0x09, 0x04, 0x00, 0x31, 0x07, 0x09, 0x30, 0x00, 0x08, 0x07, 0x16, 0x07, 0x26, 0x09, 0x04, 0x00, 0x34, 0x08, 0x09, 0x02}
	hdr := image(goodInts())
	buf := append(append([]byte(nil), hdr...), head...)
	for _, p := range Patches {
		if !bytes.Contains(p.Needle, head) {
			buf = append(buf, p.Needle...)
		}
	}
	if err := Apply(buf); err != nil {
		t.Fatal(err)
	}
	buf = buf[len(hdr):]
	// Evaluate the actual JSGte operands for the loop index and player count.
	for players := 1; players <= 4; players++ {
		var regs [10]int
		regs[7], regs[9] = 0, players
		if buf[7] != 0x31 || buf[10] != 48 {
			t.Fatal("player loop no longer branches to the success epilogue")
		}
		if regs[buf[8]] < regs[buf[9]] {
			t.Fatalf("%d players: still enters the mandatory-human assignment loop", players)
		}
	}
}

// goodInts is an int pool holding every constant in Ints.
func goodInts() []int32 {
	pool := make([]int32, 40)
	for _, c := range Ints {
		pool[c.Index] = c.Value
	}
	return pool
}

// image is the header of a version 4 HashLink image with the given int pool:
// "HLB", the version, flags, nints, eight more table sizes and the
// entrypoint (all one-byte hl indexes here), then the ints.
func image(ints []int32) []byte {
	b := []byte(Magic + "\x04")
	b = append(b, 0, byte(len(ints)), 0, 0, 0, 0, 0, 0, 0, 0) //nolint:gosec // test pools are under 128 entries
	for _, v := range ints {
		b = binary.LittleEndian.AppendUint32(b, uint32(v)) //nolint:gosec // two's complement
	}
	return b
}

// The needles pin an int pool index, not its value: an image whose pool was
// reordered is refused even though every needle still matches.
func TestApplyChecksTheIntPool(t *testing.T) {
	code := []byte{}
	for _, p := range Patches {
		code = append(code, p.Needle...)
	}
	if err := Apply(append(image(goodInts()), code...)); err != nil {
		t.Fatalf("a matching pool: %v", err)
	}
	for _, c := range Ints {
		pool := goodInts()
		pool[c.Index] = 1
		buf := append(image(pool), code...)
		before := append([]byte(nil), buf...)
		if err := Apply(buf); err == nil {
			t.Fatalf("int #%d changed to 1: want an error", c.Index)
		}
		if !bytes.Equal(buf, before) {
			t.Fatal("a refused Apply wrote to the image")
		}
	}
	if err := Apply(append(image(goodInts()[:20]), code...)); err == nil {
		t.Fatal("a pool too short for #30: want an error")
	}
}

func TestHLIndex(t *testing.T) {
	for _, c := range []struct {
		in   []byte
		v, n int
	}{
		{[]byte{0x1e}, 30, 1},
		{[]byte{0x81, 0x02}, 0x102, 2},
		{[]byte{0xc1, 0x02, 0x03, 0x04}, 0x01020304, 4},
	} {
		v, n, err := hlIndex(c.in)
		if err != nil || v != c.v || n != c.n {
			t.Fatalf("hlIndex(% x) = %d, %d, %v; want %d, %d", c.in, v, n, err, c.v, c.n)
		}
	}
	for _, bad := range [][]byte{{}, {0x81}, {0xa1, 0}, {0xc1, 0, 0}} {
		if _, _, err := hlIndex(bad); err == nil {
			t.Fatalf("hlIndex(% x): want an error", bad)
		}
	}
}
