// Package uid mints the player ids our master puts on the wire.
//
// The client refuses to ask the master for a transport when every member of a
// lobby looks like a Steam account: Lobby.isSteamOnly@24596 returns true when
// each member id starts with 'S', and Lobby.setupPlatform@24597 then skips
// instance/get and falls back to Steam P2P, bypassing our relay. So no id we
// emit may start with 'S'; we hand out Session ids ('X') instead.
package uid

import (
	"crypto/sha256"
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
