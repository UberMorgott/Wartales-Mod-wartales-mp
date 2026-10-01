package master

import (
	"encoding/json"
	"fmt"
	"strconv"
	"strings"
	"time"

	"github.com/UberMorgott/wartales-mp/internal/link"
	"github.com/UberMorgott/wartales-mp/internal/modver"
)

// The mod-file check on join (see internal/modver).
//
// The host's master adds its fingerprint to every LobbyInfo it answers a
// remote guest's lobby/resolveShortCode or lobby/infoInvite with (modField).
// The guest's master takes it out again before the game sees the answer and
// compares it with its own. On a mismatch it drops the link and rewrites the
// lobby's data so the game refuses to join by itself: Lobby.checkJoinLobby
// (Lobby.hx:868, the gate of both join by code and Steam invite) shows our
// text from modErrorField (the patcher's mod_version pass), and the vanilla
// test of data.version, which the marker no longer matches, blocks the join
// with the game's own version error even where that pass did not apply.
// Missing information (an older helper on either side, a file that could not
// be hashed) is logged as a warning and never blocks.

const (
	// modField is the LobbyInfo field carrying the host's fingerprint over
	// the proxy-link; the game never sees it.
	modField = "mpMod"
	// modErrorField is the lobby data key Lobby.checkJoinLobby shows when
	// set; patcher/src/mod_version.rs reads it under this exact name.
	modErrorField = "mpModError"
	// modMismatchVersion replaces data.version on a mismatch.
	modMismatchVersion = "wartales-mp: mod files differ from the host"
	// modWait bounds the wait for this helper's own fingerprint (hashed in
	// the background at startup; normally long done by the first join).
	modWait = 2 * time.Second
)

// localMod is this helper's fingerprint, nil when it is not known (yet).
func (s *Server) localMod() *modver.Info {
	return s.opt.Mod.Get(modWait)
}

// withHostMod adds this host's fingerprint to a LobbyInfo answered to a
// remote guest.
func (s *Server) withHostMod(info any, err error) (any, error) {
	m, ok := info.(map[string]any)
	if err != nil || !ok {
		return info, err
	}
	if mod := s.localMod(); mod != nil {
		m[modField] = mod
	}
	return m, nil
}

// noteGuestMod is the host's note on a guest's hello: the guest blocks
// itself on a mismatch, the host only learns about it here.
func (s *Server) noteGuestMod(name string, guest *modver.Info) {
	local := s.localMod()
	r := modver.Compare(local, guest)
	switch {
	case guest == nil:
		s.opt.Log.Printf("WARNING: link: guest %q runs an older wartales-mp without version info; its mod files are not checked", name)
	case r.Mismatch():
		s.opt.Log.Printf("WARNING: link: guest %q has different mod files (%s): its game will refuse to join and tell the player "+
			"to download the archive again; guest: %s; host: %s", name, strings.Join(r.Differ, ", "), guest, local)
	case len(r.Unchecked) > 0:
		s.opt.Log.Printf("WARNING: link: guest %q: mod files not compared (unknown on one side): %s", name, strings.Join(r.Unchecked, ", "))
	default:
		s.opt.Log.Printf("link: guest %q has the same mod files (%s)", name, strings.Join(r.Compared, ", "))
	}
}

// checkHostMod is the guest's check of a LobbyInfo that came from the host's
// master. It always strips the transport field; on a mismatch it rewrites the
// lobby data so the game refuses the join, and reports it so the caller drops
// the link. A null or unparsable answer is passed through untouched.
func (s *Server) checkHostMod(raw json.RawMessage) (json.RawMessage, bool) {
	var info map[string]json.RawMessage
	if len(raw) == 0 || json.Unmarshal(raw, &info) != nil || info == nil {
		return raw, false
	}
	var host *modver.Info
	m, stripped := info[modField]
	if stripped {
		delete(info, modField)
		var h modver.Info
		if json.Unmarshal(m, &h) == nil {
			host = &h
		}
	}
	// MPLobby.init@54937 reads the lobby data from info.props.data.
	var props, data map[string]json.RawMessage
	if p, ok := info["props"]; ok {
		_ = json.Unmarshal(p, &props) // not an object: treated as empty
	}
	if d, ok := props["data"]; ok {
		_ = json.Unmarshal(d, &data)
	}
	// Only this master may ask the game to show a mod error.
	_, foreign := data[modErrorField]
	delete(data, modErrorField)

	local := s.localMod()
	r := modver.Compare(local, host)
	switch {
	case host == nil:
		s.opt.Log.Printf("WARNING: the host runs an older wartales-mp without version info; mod files are not checked")
	case r.Mismatch():
		s.opt.Log.Printf("MISMATCH: mod files differ from the host's: %s; host: %s; ours: %s; the join is refused",
			strings.Join(r.Differ, ", "), host, local)
	case len(r.Unchecked) > 0:
		s.opt.Log.Printf("WARNING: mod files not compared with the host (unknown on one side): %s", strings.Join(r.Unchecked, ", "))
	default:
		s.opt.Log.Printf("master: mod files match the host's (%s)", strings.Join(r.Compared, ", "))
	}
	if !stripped && !foreign && !r.Mismatch() {
		return raw, false // nothing to change: pass the answer through as is
	}
	if r.Mismatch() {
		if data == nil {
			data = map[string]json.RawMessage{}
		}
		data["version"] = jsonString(haxeSerialize(modMismatchVersion))
		data[modErrorField] = jsonString(haxeSerialize(modver.Message(r.Differ)))
	}
	if data != nil {
		d, err := json.Marshal(data)
		if err != nil {
			return raw, false
		}
		if props == nil {
			props = map[string]json.RawMessage{}
		}
		props["data"] = d
		p, err := json.Marshal(props)
		if err != nil {
			return raw, false
		}
		info["props"] = p
	}
	out, err := json.Marshal(info)
	if err != nil {
		return raw, false
	}
	return out, r.Mismatch()
}

// fromHost runs checkHostMod on a LobbyInfo the host's master answered over
// cl and drops that link on a mismatch: the game will not join, and the next
// code goes through the cascade (and this check) again.
func (s *Server) fromHost(cl *link.Client, raw json.RawMessage) json.RawMessage {
	out, mismatch := s.checkHostMod(raw)
	if mismatch && cl != nil {
		s.dropLink(cl)
	}
	return out
}

// jsonString is s as a JSON string literal.
func jsonString(s string) json.RawMessage {
	b, _ := json.Marshal(s) // a string always marshals
	return b
}

// haxeSerialize serializes s like haxe.Serializer: "y<len>:<urlEncode(s)>".
// Every byte outside [A-Za-z0-9] is percent-encoded, which HashLink's
// url_decode (UTF-8 aware, '+' is a space) reads back exactly.
func haxeSerialize(s string) string {
	var b strings.Builder
	for _, c := range []byte(s) {
		if 'A' <= c && c <= 'Z' || 'a' <= c && c <= 'z' || '0' <= c && c <= '9' {
			b.WriteByte(c)
		} else {
			fmt.Fprintf(&b, "%%%02X", c)
		}
	}
	enc := b.String()
	return "y" + strconv.Itoa(len(enc)) + ":" + enc
}
