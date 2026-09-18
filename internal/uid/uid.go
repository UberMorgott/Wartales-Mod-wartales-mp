// Package uid mints the player ids our master puts on the wire.
//
// The client refuses to ask the master for a transport when every member of a
// lobby looks like a Steam account: Lobby.isSteamOnly@24596 returns true when
// each member id starts with 'S', and Lobby.setupPlatform@24597 then skips
// instance/get and takes the game's Steam path, bypassing our relay. That
// switch is the master's to throw: a lobby on the direct relay gets Session
// ids ('X') for every member, a lobby on SDR gets the players' real Steam ids
// (which the shim then carries over ISteamNetworkingMessages). An id that
// merely looks like Steam but is not one is never emitted: the game would
// derive a bogus SteamID from it.
package uid

import (
	"crypto/sha256"
	"encoding/binary"
	"encoding/hex"
)

// PFChars are the platform characters mpman.UserID.getPlatform@25195 accepts:
// S = Steam, W = WServer, R = RelayP2P, X = Session, M = Multi, L = Lobby.
// A first character outside this set makes the client reject the id.
const PFChars = "SWRXML"

// Session is the platform char of the ids we mint.
const Session = 'X'

// minLen is the shortest id getPlatform can still split into a token and a
// signature; anything shorter risks the "Invalid platform" branch.
const minLen = 5

// Mint maps whatever id a player reports (normally a Steam "S<steamid>") onto a
// stable Session id. The mapping is deterministic, so the same player keeps the
// same id across reconnects and across both ends of a proxy-link.
func Mint(raw string) string {
	sum := sha256.Sum256([]byte("wartales-mp/uid/" + raw))
	return string(Session) + hex.EncodeToString(sum[:10])
}

// IsSession reports whether id already has the shape Mint produces.
func IsSession(id string) bool {
	return len(id) >= minLen && id[0] == Session
}

// steamLen is the length of a Steam id as mpman.UserID.fromPlatform builds it:
// 'S' followed by 8 bytes in hex (the SteamID64 with its high dword xor'ed
// with 0x1100001, see UserID.hx:29/113).
const steamLen = 1 + 16

// IsSteam reports whether id is a well-formed Steam id the game can turn back
// into a SteamID64. Only such an id may be put on the wire when a lobby runs
// over SDR: the game derives the peer's SteamID from it.
func IsSteam(id string) bool {
	if len(id) != steamLen || id[0] != 'S' {
		return false
	}
	for _, c := range id[1:] {
		if (c < '0' || c > '9') && (c < 'a' || c > 'f') && (c < 'A' || c > 'F') {
			return false
		}
	}
	return true
}

// steamXor is what mpman.UserID xors into the high dword of the 8 id bytes
// (UserID.hx:29 on parse, :114 on print): 0x1100001, the constant high dword
// of every player SteamID64, so a player's id prints with zeros there.
const steamXor = 0x01100001

// SteamID64 turns a Steam id back into the SteamID64 the game derives from
// it: the 16 hex digits are the 8 little-endian bytes of the id with the high
// dword xor'ed. ok is false for anything IsSteam rejects.
func SteamID64(id string) (uint64, bool) {
	if !IsSteam(id) {
		return 0, false
	}
	b, err := hex.DecodeString(id[1:])
	if err != nil {
		return 0, false
	}
	v := binary.LittleEndian.Uint64(b)
	return v ^ (steamXor << 32), true
}

// FromSteamID64 is the inverse: the id the game reports for a SteamID64.
func FromSteamID64(steamID64 uint64) string {
	var b [8]byte
	binary.LittleEndian.PutUint64(b[:], steamID64^(steamXor<<32))
	return "S" + hex.EncodeToString(b[:])
}

// Ensure returns id when it is already a Session id and a minted one otherwise.
// It is the guard for ids that arrive from another process: a guest that claims
// a Steam shaped id must not be able to re-enable the Steam only path for the
// whole lobby.
func Ensure(id string) string {
	if IsSession(id) {
		return id
	}
	return Mint(id)
}
