// Package hlpatch holds the HashLink bytecode patches wartales-mp applies to
// the game, and generates the C table the winmm.dll proxy compiles in. Every
// patch rewrites exactly one byte, so no register, op count, jump target or
// debug line can shift.
//
// Patch 1+2 -- the game's own timeout path is fatal:
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
// length.
//
// Patches 3..6 -- the title screen's "join by code" field is built by
// joinWithCode2@23875 (src/ui/win/TitleScreen.hx:638-661) around the length
// of the game's own lobby codes, 5, in four places; ours are 13, 16 or 25
// symbols (see internal/code). All four load the 5 from int constant #30:
//
//   - the submit handler @34236 (TitleScreen.hx:646) and the validate
//     closure @34233 (:641) refuse any other length: their JNotEq (0x39)
//     becomes JSLt (0x30) with the same operands, so only a code shorter than
//     5 is refused, vanilla 5-symbol codes keep working and everything longer
//     reaches joinCode@24714, where the mod's master takes over;
//   - InputText.maxLen (:640) and the substr(0, 5) in the formatText closure
//     @34234 (:642) cap what can be typed and what is kept: their Int operand
//     is redirected from int constant #30 (5) to #19 (32), which leaves room
//     for a 25-symbol code with a few separators and keeps the widget sane.
//     Both indices fit one varint byte, so nothing shifts.
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
// The first two needles cover the same seven ops of hxbit.NetworkHost.flush@3969, once
// for the pendingClients loop (NetworkHost.hx:1387) and once for the clients
// loop (NetworkHost.hx:1390); they differ only in the JNotLt jump offset:
//
//	47 10           NullCheck   r16
//	26 0b 10 07     Field       r11 = r16.lastMessage
//	08 09 06 0b     Sub         r9  = r6 - r11          (now - lastMessage)
//	28 0b 06        GetThis     r11 = this.clientTimeout
//	36 0b 09 kk     JNotLt      if r11 !< r9 jump over  <- r9 becomes r11
//	1e 01 02 01 10  CallMethod  r1  = r16.timeout()     (proto index 2)
//
// The third needle is ops 6..12 of the title screen's join handler @34236;
// the two four-byte function indices (34237 = 0x85bd, 24714 = 0x608a) make it
// unique on their own:
//
//	47 02                    NullCheck     r2
//	26 04 02 01              Field         r4 = r2.length
//	01 06 1e                 Int           r6 = int@30 (= 5)
//	39 04 06 03              JNotEq        if r4 != r6 jump +3  <- becomes JSLt (0x30)
//	21 07 c0 00 85 bd        StaticClosure r7 = fn@34237
//	1a 01 c0 00 60 8a 02 07  Call2         r1 = joinCode@24714(r2, r7)
//	3a 04                    JAlways       jump +4 (over the error branch)
//
// The fourth is the whole validate closure @34233 (8 ops):
//
//	47 00          NullCheck r0
//	26 02 00 01    Field     r2 = r0.length
//	01 03 1e       Int       r3 = int@30 (= 5)
//	39 02 03 02    JNotEq    if r2 != r3 jump +2  <- becomes JSLt (0x30)
//	03 01 01       Bool      r1 = true
//	3a 01          JAlways   jump +1
//	03 01 00       Bool      r1 = false
//	43 01          Ret       r1
//
// The fifth is ops 8..12 of joinWithCode2@23875, where maxLen is set:
//
//	52 06                New           r6 = new {formatText, maxLen, validate}
//	01 07 1e             Int           r7 = int@30 (= 5)  <- int@19 (= 32)
//	3b 08 07             ToDyn         r8 = r7
//	27 06 01 08          SetField      r6.maxLen = r8
//	21 09 c0 00 85 b9    StaticClosure r9 = fn@34233
//
// The sixth is ops 6..11 of the formatText closure @34234, the substr cap:
//
//	47 01                NullCheck r1
//	01 06 01             Int       r6 = int@1 (= 0)
//	01 07 1e             Int       r7 = int@30 (= 5)  <- int@19 (= 32)
//	3b 08 07             ToDyn     r8 = r7
//	1b 01 08 01 06 08    Call3     r1 = substr@8(r1, r6, r8)
//	19 01 c0 00 5b 53 01 Call1     r1 = trim@23379(r1)
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
	{
		Name:   "TitleScreen join code length check (TitleScreen.hx:646)",
		Needle: []byte{0x47, 0x02, 0x26, 0x04, 0x02, 0x01, 0x01, 0x06, 0x1e, 0x39, 0x04, 0x06, 0x03, 0x21, 0x07, 0xc0, 0x00, 0x85, 0xbd, 0x1a, 0x01, 0xc0, 0x00, 0x60, 0x8a, 0x02, 0x07, 0x3a, 0x04},
		Index:  9,
		From:   0x39, // JNotEq: error unless length == 5
		To:     0x30, // JSLt:   error only if length < 5
	},
	{
		Name:   "TitleScreen join code validate closure (TitleScreen.hx:641)",
		Needle: []byte{0x47, 0x00, 0x26, 0x02, 0x00, 0x01, 0x01, 0x03, 0x1e, 0x39, 0x02, 0x03, 0x02, 0x03, 0x01, 0x01, 0x3a, 0x01, 0x03, 0x01, 0x00, 0x43, 0x01},
		Index:  9,
		From:   0x39, // JNotEq: false unless length == 5
		To:     0x30, // JSLt:   false only if length < 5
	},
	{
		Name:   "TitleScreen join code maxLen (TitleScreen.hx:640)",
		Needle: []byte{0x52, 0x06, 0x01, 0x07, 0x1e, 0x3b, 0x08, 0x07, 0x27, 0x06, 0x01, 0x08, 0x21, 0x09, 0xc0, 0x00, 0x85, 0xb9},
		Index:  4,
		From:   0x1e, // int@30 = 5
		To:     0x13, // int@19 = 32
	},
	{
		Name:   "TitleScreen join code formatText substr (TitleScreen.hx:642)",
		Needle: []byte{0x47, 0x01, 0x01, 0x06, 0x01, 0x01, 0x07, 0x1e, 0x3b, 0x08, 0x07, 0x1b, 0x01, 0x08, 0x01, 0x06, 0x08, 0x19, 0x01, 0xc0, 0x00, 0x5b, 0x53, 0x01},
		Index:  7,
		From:   0x1e, // int@30 = 5
		To:     0x13, // int@19 = 32
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
