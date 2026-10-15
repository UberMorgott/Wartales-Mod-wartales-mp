package master

import (
	"net"
	"strconv"
	"strings"

	"github.com/UberMorgott/wartales-mp/internal/link"
	"github.com/UberMorgott/wartales-mp/internal/uid"
)

// startAnswer is instance/get for the host's own game, asked when it starts a
// direct-marked lobby (Lobby.setupPlatform). The game's transport is chosen
// here, from how the guests actually joined (issue #7), not at lobby creation:
//
//   - no guest, or every guest over a direct TCP link: our relay
//     ("R127.0.0.1:<port>:<pw>"; each guest's master rewrites the address in
//     the start message to the route it joined over, guestRelayAddr);
//   - every guest over the SDR bridge (a Steam join): the host's Steam id,
//     which the game's patched callback (patcher start_transport.rs) turns
//     into its vanilla Steam path, carried over SDR by the shim;
//   - both kinds: a message naming the Steam players. It is not a platform id,
//     so the same callback shows it and the start fails cleanly.
//
// -transport direct keeps the relay whatever the links.
func (s *Server) startAnswer() map[string]any {
	relay := s.instanceAnswer(net.JoinHostPort("127.0.0.1", strconv.Itoa(s.opt.RelayPort)))
	if s.opt.Transport == ModeDirect {
		return relay
	}
	var host Peer
	var steam, direct []string
	s.lobbies.mu.Lock()
	if l := s.lobbies.lobbies[s.lobbies.inviteLobby]; l != nil {
		for _, u := range l.users {
			switch {
			case u.peer == nil:
			case !u.peer.Remote():
				if u.ID == l.owner {
					host = u.peer
				}
			default:
				if lp, ok := u.peer.(*link.Peer); ok && lp.OverSDR() {
					steam = append(steam, u.Name)
				} else {
					direct = append(direct, u.Name)
				}
			}
		}
	}
	s.lobbies.mu.Unlock()

	switch {
	case len(steam) == 0:
		s.opt.Log.Printf("master: game start: %d guest(s), all over a direct link: our relay", len(direct))
		return relay
	case len(direct) == 0 && host != nil && uid.IsSteam(host.GameID()):
		s.opt.Log.Printf("master: game start: %d guest(s), all over SDR: the game's Steam path (%s)", len(steam), host.GameID())
		return map[string]any{"serverID": host.GameID()}
	case len(direct) == 0:
		s.opt.Log.Printf("master: game start REFUSED: the guests joined over SDR, but the host has no Steam id")
		return map[string]any{"serverID": "Игра не может начаться: у хоста нет Steam id. / The game cannot start: the host has no Steam id."}
	}
	names := strings.Join(steam, ", ")
	s.opt.Log.Printf("master: game start REFUSED: mixed lobby, over SDR: %s; over a direct link: %s", names, strings.Join(direct, ", "))
	return map[string]any{"serverID": "Игроки " + names + " вошли через Steam и не могут подключиться напрямую: войдите все по коду или все через приглашение Steam." +
		"\n\nPlayers " + names + " joined via Steam and cannot connect directly: everyone join by code, or everyone via a Steam invitation."}
}
