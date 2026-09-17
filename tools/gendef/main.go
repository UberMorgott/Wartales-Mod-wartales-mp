// Command gendef reads the export table of a Windows PE file and writes a
// module-definition file whose every entry is a forwarder to a renamed copy of
// that same binary. It is how the drop-in shims in shim/ stay complete: the
// export list is generated from the real game files, never hand-written.
//
//	gendef -in libhl.dll -target libhl_o -out libhl.def [-skip a,b] [-list]
//
// The -skip names are left out of the forwarder list: the shim implements them
// itself.
package main

import (
	"bytes"
	"debug/pe"
	"encoding/binary"
	"flag"
	"fmt"
	"os"
	"sort"
	"strings"
)

func main() {
	if err := run(); err != nil {
		fmt.Fprintln(os.Stderr, "gendef:", err)
		os.Exit(1)
	}
}

func run() error {
	in := flag.String("in", "", "path to the original PE file to read exports from")
	target := flag.String("target", "", "module name the exports are forwarded to, without extension (e.g. libhl_o)")
	out := flag.String("out", "", "path of the .def file to write (default: stdout)")
	library := flag.String("library", "", "LIBRARY name to emit (default: base name of -in)")
	skip := flag.String("skip", "", "comma separated export names the shim implements itself")
	list := flag.Bool("list", false, "print the export names only, one per line")
	flag.Parse()

	if *in == "" {
		return fmt.Errorf("-in is required")
	}
	exports, err := readExports(*in)
	if err != nil {
		return err
	}
	if len(exports) == 0 {
		return fmt.Errorf("%s: no named exports", *in)
	}

	if *list {
		return write(*out, listing(exports))
	}
	if *target == "" {
		return fmt.Errorf("-target is required")
	}
	name := *library
	if name == "" {
		name = baseName(*in)
	}
	return write(*out, def(name, *target, exports, splitNames(*skip)))
}

// export is one named entry of a PE export directory.
type export struct {
	Name    string
	Ordinal uint32
}

// readExports returns the named exports of a PE file, sorted by ordinal.
//
// debug/pe exposes no export-directory reader, so the IMAGE_EXPORT_DIRECTORY at
// data directory 0 is walked by hand.
func readExports(path string) ([]export, error) {
	f, err := pe.Open(path)
	if err != nil {
		return nil, err
	}
	defer f.Close() //nolint:errcheck // opened read-only

	var dir pe.DataDirectory
	switch oh := f.OptionalHeader.(type) {
	case *pe.OptionalHeader64:
		dir = oh.DataDirectory[pe.IMAGE_DIRECTORY_ENTRY_EXPORT]
	case *pe.OptionalHeader32:
		dir = oh.DataDirectory[pe.IMAGE_DIRECTORY_ENTRY_EXPORT]
	default:
		return nil, fmt.Errorf("%s: no optional header", path)
	}
	if dir.VirtualAddress == 0 || dir.Size == 0 {
		return nil, fmt.Errorf("%s: no export directory", path)
	}

	m, err := newImage(f)
	if err != nil {
		return nil, err
	}
	base, err := m.u32(dir.VirtualAddress + 16)
	if err != nil {
		return nil, err
	}
	nameCount, err := m.u32(dir.VirtualAddress + 24)
	if err != nil {
		return nil, err
	}
	namesRVA, err := m.u32(dir.VirtualAddress + 32)
	if err != nil {
		return nil, err
	}
	ordsRVA, err := m.u32(dir.VirtualAddress + 36)
	if err != nil {
		return nil, err
	}

	out := make([]export, 0, nameCount)
	for i := uint32(0); i < nameCount; i++ {
		nameRVA, err := m.u32(namesRVA + i*4)
		if err != nil {
			return nil, err
		}
		name, err := m.cstring(nameRVA)
		if err != nil {
			return nil, err
		}
		ord, err := m.u16(ordsRVA + i*2)
		if err != nil {
			return nil, err
		}
		out = append(out, export{Name: name, Ordinal: base + uint32(ord)})
	}
	sort.Slice(out, func(i, j int) bool { return out[i].Ordinal < out[j].Ordinal })
	return out, nil
}

// image resolves relative virtual addresses against the section raw data.
type image struct{ sections []*pe.Section }

func newImage(f *pe.File) (*image, error) {
	if len(f.Sections) == 0 {
		return nil, fmt.Errorf("no sections")
	}
	return &image{sections: f.Sections}, nil
}

// at returns the section bytes starting at rva.
func (m *image) at(rva uint32) ([]byte, error) {
	for _, s := range m.sections {
		if rva < s.VirtualAddress || rva >= s.VirtualAddress+s.VirtualSize {
			continue
		}
		off := rva - s.VirtualAddress
		data, err := s.Data()
		if err != nil {
			return nil, err
		}
		if off >= uint32(len(data)) {
			return nil, fmt.Errorf("rva %#x past raw data of %s", rva, s.Name)
		}
		return data[off:], nil
	}
	return nil, fmt.Errorf("rva %#x is in no section", rva)
}

func (m *image) u32(rva uint32) (uint32, error) {
	b, err := m.at(rva)
	if err != nil || len(b) < 4 {
		return 0, orShort(err, rva)
	}
	return binary.LittleEndian.Uint32(b), nil
}

func (m *image) u16(rva uint32) (uint16, error) {
	b, err := m.at(rva)
	if err != nil || len(b) < 2 {
		return 0, orShort(err, rva)
	}
	return binary.LittleEndian.Uint16(b), nil
}

func (m *image) cstring(rva uint32) (string, error) {
	b, err := m.at(rva)
	if err != nil {
		return "", err
	}
	i := bytes.IndexByte(b, 0)
	if i < 0 {
		return "", fmt.Errorf("unterminated string at rva %#x", rva)
	}
	return string(b[:i]), nil
}

func orShort(err error, rva uint32) error {
	if err != nil {
		return err
	}
	return fmt.Errorf("truncated read at rva %#x", rva)
}

// def renders the module-definition file. Ordinals are pinned so that the shim
// is export-compatible with the original even for ordinal imports.
func def(library, target string, exports []export, skip map[string]bool) string {
	var b strings.Builder
	fmt.Fprintf(&b, "; generated by tools/gendef - do not edit\n")
	fmt.Fprintf(&b, "LIBRARY %s\n", library)
	fmt.Fprintf(&b, "EXPORTS\n")
	for _, e := range exports {
		if skip[e.Name] {
			fmt.Fprintf(&b, "    %s @%d\n", e.Name, e.Ordinal)
			continue
		}
		fmt.Fprintf(&b, "    %s = %s.%s @%d\n", e.Name, target, e.Name, e.Ordinal)
	}
	return b.String()
}

func listing(exports []export) string {
	var b strings.Builder
	for _, e := range exports {
		fmt.Fprintf(&b, "%d\t%s\n", e.Ordinal, e.Name)
	}
	return b.String()
}

func write(path, text string) error {
	if path == "" {
		_, err := os.Stdout.WriteString(text)
		return err
	}
	return os.WriteFile(path, []byte(text), 0o644)
}

func splitNames(s string) map[string]bool {
	m := map[string]bool{}
	for _, p := range strings.Split(s, ",") {
		if p = strings.TrimSpace(p); p != "" {
			m[p] = true
		}
	}
	return m
}

func baseName(path string) string {
	path = strings.ReplaceAll(path, "\\", "/")
	if i := strings.LastIndex(path, "/"); i >= 0 {
		path = path[i+1:]
	}
	return path
}
