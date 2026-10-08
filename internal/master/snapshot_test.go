package master

import (
	"encoding/json"
	"fmt"
	"sync"
	"testing"
)

// A LobbyInfo is marshalled after the store lock is released (the reply is
// written later), so it must not share the lobby's live data map with an owner
// that keeps setting data at the same time.
func TestLobbyInfoIsASnapshot(t *testing.T) {
	s := quietServer(nil)
	owner := &session{uid: "Xowner", game: "Sowner", name: "Owner"}
	l := &lobby{id: "L1", owner: owner.game, data: map[string]json.RawMessage{},
		users: []*member{{ID: owner.game, peer: owner}}}
	s.lobbies.lobbies[l.id] = l

	var wg sync.WaitGroup
	wg.Go(func() {
		for i := range 300 {
			data := json.RawMessage(fmt.Sprintf(`{"k%d":"t"}`, i))
			if err := s.lobbySetData(lobbyArgs{ID: l.id, Data: data}, owner); err != nil {
				t.Error(err)
				return
			}
		}
	})
	for range 300 {
		info, err := s.lobbyInfo(l.id)
		if err != nil {
			t.Fatal(err)
		}
		if _, err := json.Marshal(info); err != nil {
			t.Fatal(err)
		}
		if _, err := json.Marshal(s.lobbyList()); err != nil {
			t.Fatal(err)
		}
	}
	wg.Wait()
}
