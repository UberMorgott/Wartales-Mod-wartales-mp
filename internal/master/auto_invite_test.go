package master

import (
	"context"
	"encoding/json"
	"io"
	"log"
	"testing"
	"time"

	"github.com/UberMorgott/wartales-mp/internal/code"
	"github.com/UberMorgott/wartales-mp/internal/sdrbridge"
	"github.com/UberMorgott/wartales-mp/internal/sdrbridge/sdrbridgetest"
)

func lobbyInvite(t *testing.T, sw *sdrbridgetest.Switch) string {
	t.Helper()
	select {
	case event := <-sw.LobbyInvites:
		return event.Invite
	case <-time.After(2 * time.Second):
		t.Fatal("native lobby state was not published")
		return ""
	}
}

func TestHostAutomaticallyPublishesSteamLobby(t *testing.T) {
	for _, ending := range []string{"leave", "disconnect", "close"} {
		t.Run(ending, func(t *testing.T) {
			sw := sdrbridgetest.New(t)
			s, _ := cascadeHost(t, sw, cascadeHostID, false)
			host, id := createLobby(t, s, cascadeHostID)
			invite := lobbyInvite(t, sw)
			c, err := code.DecodeAny(invite)
			if err != nil || c.Steam == nil || c.Endpoint != nil || c.Steam.SteamID64() != cascadeHostID {
				t.Fatalf("auto invite route = %+v, %v", c, err)
			}
			var info struct {
				ID string `json:"id"`
			}
			if err := json.Unmarshal(host.call(t, "lobby/infoInvite", map[string]any{"invite": invite}), &info); err != nil || info.ID != id {
				t.Fatalf("auto invite resolved = %+v, %v", info, err)
			}
			switch ending {
			case "leave":
				host.call(t, "lobby/leave", map[string]any{"id": id})
			case "disconnect":
				_ = host.c.Close()
			case "close":
				s.Close()
			}
			if got := lobbyInvite(t, sw); got != "" {
				t.Fatal("owner departure did not clear native lobby")
			}
		})
	}
}

func TestAutoInviteBeforeBridgeReadyWithoutEndpoint(t *testing.T) {
	sw := sdrbridgetest.New(t)
	b := sdrbridge.New(sw.Add(t, cascadeHostID), log.New(io.Discard, "", 0))
	s, _ := startMasterOpts(t, func(o *Options) {
		o.Endpoint = nil
		o.Bridge, o.SDRStatus, o.LinkKey = b, b.Status, 0xdeadbeef
	})
	host, id := createLobby(t, s, cascadeHostID)
	defer host.c.Close()
	ctx, cancel := context.WithCancel(context.Background())
	defer cancel()
	go b.Run(ctx)
	invite := lobbyInvite(t, sw)
	s.lobbies.mu.Lock()
	mapped := s.lobbies.codes[invite]
	s.lobbies.mu.Unlock()
	if mapped != id {
		t.Fatal("delayed invite did not resolve to the created lobby")
	}
}

type remoteInvitePeer struct{ *session }

func (p *remoteInvitePeer) Remote() bool     { return true }
func (p *remoteInvitePeer) Push(string, any) {}

func TestAutoInviteTransferAndStalePeer(t *testing.T) {
	sw := sdrbridgetest.New(t)
	s, _ := cascadeHost(t, sw, cascadeHostID, false)
	host, id := createLobby(t, s, cascadeHostID)
	invite := lobbyInvite(t, sw)
	old := s.localSession()
	guest := &remoteInvitePeer{&session{uid: "Sguest", name: "Guest", steam: "S76561197960265799"}}
	// A remote guest creating its own lobby cannot replace our publication.
	if _, err := s.lobbyCreate(lobbyArgs{}, guest); err != nil {
		t.Fatal(err)
	}
	s.lobbies.mu.Lock()
	if s.lobbies.inviteLobby != id {
		t.Fatal("remote lobby replaced local publication")
	}
	l := s.lobbies.lobbies[id]
	l.users = append(l.users, &member{ID: l.idOf(guest), peer: guest})
	s.lobbies.mu.Unlock()
	if err := s.lobbyTransfer(lobbyArgs{ID: id, UID: l.idOf(guest)}, old); err != nil {
		t.Fatal(err)
	}
	if got := lobbyInvite(t, sw); got != "" {
		t.Fatal("transfer to remote owner kept publishing")
	}
	if err := s.lobbyTransfer(lobbyArgs{ID: id, UID: l.idOf(old)}, guest); err != nil {
		t.Fatal(err)
	}
	if got := lobbyInvite(t, sw); got != invite {
		t.Fatal("transfer back did not restore local publication")
	}
	fresh := &session{uid: old.uid, steam: old.steam, name: old.name}
	s.lobbies.mu.Lock()
	for _, u := range l.users {
		if u.peer == old {
			u.peer = fresh
		}
	}
	s.lobbies.mu.Unlock()
	s.lobbies.peerGone(old)
	s.lobbies.mu.Lock()
	if len(l.users) != 2 || l.owner != l.idOf(fresh) || s.lobbies.inviteLobby != id {
		t.Fatal("stale disconnect changed publication owner")
	}
	s.lobbies.mu.Unlock()
	// Creating a newer lobby updates the same stable invite's resolution.
	result, err := s.lobbyCreate(lobbyArgs{}, fresh)
	if err != nil {
		t.Fatal(err)
	}
	s.lobbies.mu.Lock()
	latest := s.lobbies.codes[invite]
	s.lobbies.mu.Unlock()
	if latest != result {
		t.Fatal("invite did not follow latest local lobby")
	}
	_ = host.c.Close()
}

func TestBecomingGuestClearsAutoInvite(t *testing.T) {
	sw := sdrbridgetest.New(t)
	remote, _ := cascadeHost(t, sw, cascadeHostID, false)
	_, remoteID := createLobby(t, remote, cascadeHostID)
	remoteInvite := lobbyInvite(t, sw)
	const localID = uint64(76561197960265728 + 477)
	local, _ := cascadeHost(t, sw, localID, false)
	game, _ := createLobby(t, local, localID)
	_ = lobbyInvite(t, sw)
	resolveOK(t, game, remoteInvite, remoteID)
	if got := lobbyInvite(t, sw); got != "" {
		t.Fatal("guest kept advertising its previous host lobby")
	}
	local.lobbies.mu.Lock()
	defer local.lobbies.mu.Unlock()
	if local.lobbies.inviteLobby != "" {
		t.Fatal("guest kept a publication owner")
	}
}
