package master

import (
	"encoding/json"
	"net"
	"strings"
	"testing"

	"github.com/UberMorgott/wartales-mp/internal/link"
	"github.com/UberMorgott/wartales-mp/internal/uid"
)

// A guest names its own ids in its hello, so naming the owner's must not make
// it the owner: not by joining, not by owner-only commands, not by chat.
func TestRemotePeerCannotImpersonateLocalOwner(t *testing.T) {
	s := quietServer(nil)
	owner := &session{uid: uid.Mint("owner"), game: "Sowner", name: "Owner"}
	l := &lobby{id: "L1", owner: owner.game, data: map[string]json.RawMessage{},
		users: []*member{{ID: owner.game, Name: "Owner", peer: owner}}}
	s.lobbies.lobbies[l.id] = l
	fake := &remoteInvitePeer{&session{uid: owner.uid, game: owner.game, name: "Mallory"}}

	if _, err := s.lobbyJoin(lobbyArgs{ID: l.id}, fake); err == nil {
		t.Fatal("a remote peer took the local owner's slot")
	}
	if l.users[0].peer != owner || len(l.users) != 1 {
		t.Fatal("the owner's member was replaced")
	}
	if err := s.lobbySetData(lobbyArgs{ID: l.id, Data: json.RawMessage(`{"k":"t"}`)}, fake); err == nil {
		t.Fatal("a remote peer set data as the owner")
	}
	if err := s.lobbyTransfer(lobbyArgs{ID: l.id, UID: owner.game}, fake); err == nil {
		t.Fatal("a remote peer transferred the lobby as the owner")
	}
	if err := s.lobbySetUserData(lobbyArgs{ID: l.id, Data: json.RawMessage(`"x"`)}, fake); err == nil {
		t.Fatal("a remote peer set the owner's user data")
	}
	if err := s.lobbyChat(lobbyArgs{ID: l.id, Msg: json.RawMessage(`"m"`)}, fake); err == nil {
		t.Fatal("a remote peer chatted as the owner")
	}
	// The local game itself, on another of its sockets, still is the owner.
	again := &session{uid: owner.uid, game: owner.game, name: "Owner"}
	if err := s.lobbySetData(lobbyArgs{ID: l.id, Data: json.RawMessage(`{"k":"t"}`)}, again); err != nil {
		t.Fatalf("the local game lost its ownership: %v", err)
	}
}

// Steam authenticates an SDR stream's peer; a hello claiming another Steam id
// (the owner's, say) is refused, the peer's own one is served.
func TestSDRHelloMustCarryThePeersSteamID(t *testing.T) {
	const peer, other = uint64(76561197960265799), uint64(76561197960265800)
	s := quietServer(nil)
	s.opt.LinkKey = 0x1234
	call := func(steam string, refused bool) error {
		hostEnd, guestEnd := net.Pipe()
		done, closed := make(chan struct{}), make(chan struct{})
		go func() { defer close(done); s.ServeSDRLink(hostEnd, peer) }()
		cl, err := link.DialConn(guestEnd, link.User{ID: uid.Mint("g"), Name: "G", Steam: steam, Key: 0x1234},
			nil, func() { close(closed) })
		if err != nil {
			t.Fatal(err)
		}
		if refused {
			<-closed // the refusal is the hello's reply; the call then reports it
		}
		_, err = cl.Call("lobby/list", nil)
		_ = cl.Close()
		<-done
		return err
	}
	if err := call(uid.FromSteamID64(other), true); err == nil || !strings.Contains(err.Error(), "Steam id does not match") {
		t.Fatalf("foreign Steam id: got %v, want a refusal", err)
	}
	if err := call(uid.FromSteamID64(peer), false); err != nil {
		t.Fatalf("own Steam id refused: %v", err)
	}
	if err := call("", false); err != nil {
		t.Fatalf("no Steam id refused: %v", err)
	}
}
