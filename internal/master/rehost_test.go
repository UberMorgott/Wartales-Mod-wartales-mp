package master

import (
	"encoding/json"
	"strings"
	"testing"

	"github.com/UberMorgott/wartales-mp/internal/sdrbridge/sdrbridgetest"
)

// A host that answers a code with "no such lobby" is not a route, and the
// link to it must not capture the guest's next code: that one is another
// host's, and it must reach that host.
func TestNextCodeReachesItsOwnHost(t *testing.T) {
	const otherHostID = uint64(0x0110000100BC6150)
	sw := sdrbridgetest.New(t)
	hostA, addrA := cascadeHost(t, sw, cascadeHostID, true)
	_, idA := createLobby(t, hostA, cascadeHostID)
	hostB, addrB := cascadeHost(t, sw, otherHostID, true)
	_, idB := createLobby(t, hostB, otherHostID)
	codeA := combinedCode(t, addrA, cascadeHostID, 0xdeadbeef)
	codeB := combinedCode(t, addrB, otherHostID, 0xdeadbeef)
	hostB.lobbies.mu.Lock()
	hostB.lobbies.codes[codeB] = idB
	hostB.lobbies.mu.Unlock()

	guest := guestFor(t, sw, 0x0110000100000020, false, nil)
	// A does not know codeA (its lobby closed, say): an error, not a route.
	msg := guest.callErr(t, "lobby/resolveShortCode", map[string]any{"shortCode": codeA, "filters": map[string]any{}})
	if !strings.Contains(msg, "does not know this join code") {
		t.Fatalf("unknown code = %q", msg)
	}
	resolveOK(t, guest, codeB, idB)

	// Linked to B by a code that works, a code of A's still reaches A.
	hostA.lobbies.mu.Lock()
	hostA.lobbies.codes[codeA] = idA
	hostA.lobbies.mu.Unlock()
	resolveOK(t, guest, codeA, idA)
	// And an invitation of B's, while linked to A, reaches B.
	var info struct {
		ID string `json:"id"`
	}
	raw := guest.call(t, "lobby/infoInvite", map[string]any{"invite": codeB})
	if err := json.Unmarshal(raw, &info); err != nil || info.ID != idB {
		t.Fatalf("infoInvite = %s, %v; want lobby %s", raw, err, idB)
	}
}
