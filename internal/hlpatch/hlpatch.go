// Package hlpatch holds the one HashLink bytecode patch wartales-mp applies to
// the game, and generates the C table the winmm.dll proxy compiles in.
//
// Why a bytecode patch at all -- the game's own timeout path is fatal:
//
//	hxbit.NetworkHost.flush@3969 (hxbit/NetworkHost.hx:1387,1390) calls
//	c.timeout() on every client whose lastMessage is older than clientTimeout
//	(60 s). mpman.net.Client.timeout@24013 then runs, in order:
//	  1. super.timeout() -> NetworkClient.stop@3982, which REMOVES the client
//	     from host.clients, and then service.onStop();
//	  2. for a lib.HostWT service that onStop is @55008 (src/lib/HostWT.hx:93),
//	     whose first statement is clients[0].stop() -- on the array the step
//	     above just shortened, so a host with a single client reads index 0 of
//	     an empty array: "Null access", HostWT.hx:93;
//	  3. if it got that far, s.onConnect(null) -> @55007 (HostWT.hx:85) builds
//	     new ClientWT(this, null) whenever isAuth is set, and
//	     mpman.net.Client.__constructor__@24016 null-checks the service at
//	     Client.hx:13: "Null access", the crash users actually hit.
//
// So no client of any role can time out without killing the process: nothing
// working is lost by never letting the check fire. It fires in an idle lobby
// because the lobby owner's own clients[0] (HostWT.hx:70) and the per-user
// service client pushed by @55007 never receive anything -- for the owner
// LobbyService.onMessage@54929 routes every inbound packet to a per-guest
// LobbyUserService (get_isAuth@54925 is true), so their lastMessage is frozen
// from the moment the lobby is created. Real disconnects keep being reported by
// the transport itself (Host.close@12150 -> service.onStop) and by the master's
// lobby/leave pushes (onUserLeft@54928), neither of which goes through timeout.
//
// The patch turns each of the two "did this client time out" comparisons into a
// comparison of clientTimeout with itself, which JNotLt always takes, so the
// call is skipped and the loop moves on. One byte each, same opcode, same
// length: no register, op count, jump target or debug line can shift.
package hlpatch

import (
	"bytes"
	"fmt"
	"strings"
)

// A Patch is a single byte rewritten inside a HashLink bytecode image, found by
// an exact match of Needle that must occur exactly once in the whole image.
type Patch struct {
	Name   string // short description, for errors and the generated C table
	Needle []byte // unique byte signature, taken verbatim from hlboot.dat
	Index  int    // offset of the patched byte inside Needle
	From   byte   // the byte that must be there
	To     byte   // the byte written instead
}

// Patches are all patches applied to the bytecode image, in order.
//
// Both needles cover the same seven ops of hxbit.NetworkHost.flush@3969, once
// for the pendingClients loop (NetworkHost.hx:1387) and once for the clients
// loop (NetworkHost.hx:1390); they differ only in the JNotLt jump offset:
//
//	47 10           NullCheck   r16
//	26 0b 10 07     Field       r11 = r16.lastMessage
//	08 09 06 0b     Sub         r9  = r6 - r11          (now - lastMessage)
//	28 0b 06        GetThis     r11 = this.clientTimeout
//	36 0b 09 kk     JNotLt      if r11 !< r9 jump over  <- r9 becomes r11
//	1e 01 02 01 10  CallMethod  r1  = r16.timeout()     (proto index 2)
var Patches = []Patch{
	{
		Name:   "flush/pendingClients timeout (NetworkHost.hx:1387)",
		Needle: []byte{0x47, 0x10, 0x26, 0x0b, 0x10, 0x07, 0x08, 0x09, 0x06, 0x0b, 0x28, 0x0b, 0x06, 0x36, 0x0b, 0x09, 0x01, 0x1e, 0x01, 0x02, 0x01, 0x10},
		Index:  15,
		From:   0x09, // r9  = now - lastMessage
		To:     0x0b, // r11 = clientTimeout, i.e. !(x < x) -> always jump
	},
	{
		Name:   "flush/clients timeout (NetworkHost.hx:1390)",
		Needle: []byte{0x47, 0x10, 0x26, 0x0b, 0x10, 0x07, 0x08, 0x09, 0x06, 0x0b, 0x28, 0x0b, 0x06, 0x36, 0x0b, 0x09, 0x02, 0x1e, 0x01, 0x02, 0x01, 0x10},
		Index:  15,
		From:   0x09,
		To:     0x0b,
	},
}

// Magic is the first bytes of a HashLink bytecode image; the proxy refuses to
// patch an hlboot.dat that does not start with it.
const Magic = "HLB"

// Apply rewrites buf in place. Every patch must match exactly once and find the
// byte it expects, or nothing is written at all and an error is returned: a
// game update that moves this code must leave the bytecode alone, not corrupt
// it.
func Apply(buf []byte) error {
	at := make([]int, len(Patches))
	for i, p := range Patches {
		n := bytes.Count(buf, p.Needle)
		if n != 1 {
			return fmt.Errorf("hlpatch %q: %d matches, want 1", p.Name, n)
		}
		pos := bytes.Index(buf, p.Needle) + p.Index
		if buf[pos] != p.From {
			return fmt.Errorf("hlpatch %q: byte at %d is 0x%02x, want 0x%02x", p.Name, pos, buf[pos], p.From)
		}
		at[i] = pos
	}
	for i, p := range Patches {
		buf[at[i]] = p.To
	}
	return nil
}

// Header returns the C header tools/hlpatchgen writes for the winmm.dll proxy,
// so the table lives in exactly one place.
func Header() []byte {
	var b strings.Builder
	b.WriteString("/* Generated by tools/hlpatchgen from internal/hlpatch. Do not edit. */\n")
	b.WriteString("#ifndef WARTALES_MP_HLPATCH_H\n#define WARTALES_MP_HLPATCH_H\n\n")
	fmt.Fprintf(&b, "#define HL_MAGIC %q\n\n", Magic)
	b.WriteString("struct hl_patch {\n\tconst unsigned char *needle;\n\tunsigned len;\n\tunsigned index;\n\tunsigned char from;\n\tunsigned char to;\n};\n\n")
	for i, p := range Patches {
		fmt.Fprintf(&b, "/* %s */\nstatic const unsigned char hl_needle_%d[] = {", p.Name, i)
		for j, c := range p.Needle {
			if j%11 == 0 {
				b.WriteString("\n\t")
			}
			fmt.Fprintf(&b, "0x%02x,", c)
		}
		b.WriteString("\n};\n\n")
	}
	b.WriteString("static const struct hl_patch hl_patches[] = {\n")
	for i, p := range Patches {
		fmt.Fprintf(&b, "\t{ hl_needle_%d, %d, %d, 0x%02x, 0x%02x },\n", i, len(p.Needle), p.Index, p.From, p.To)
	}
	b.WriteString("};\n\n")
	fmt.Fprintf(&b, "#define HL_PATCH_COUNT %d\n\n#endif /* WARTALES_MP_HLPATCH_H */\n", len(Patches))
	return []byte(b.String())
}
