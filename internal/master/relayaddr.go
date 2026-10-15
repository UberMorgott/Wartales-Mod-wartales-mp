package master

import (
	"encoding/json"
	"net"
	"regexp"
	"strconv"
)

// The host's game asks its own master for the relay (instanceGet) and gets
// "R127.0.0.1:<port>:<pw>": the host's game reaches its relay on loopback.
// The game builds the guests' WServer id from the same host and port
// (Lobby.setupPlatform callback @26421) and sends it to them in a lobby
// message (StartClients; a drop-in joiner gets game.options.multi.serverID
// the same way), which our masters carry as lobby/chat. On another PC that
// address is the guest's own machine. So a guest's master that joined over a
// direct route puts the address it dialed in its place before the message
// reaches its game; a link over SDR is left alone.
//
// In a haxe-serialized string "y<len>:<urlencoded>" a ':' only appears
// encoded (%3A) inside the content, so the pattern below cannot start inside
// another string; nor inside a Bytes value, whose haxe base64 alphabet
// (A-Z a-z 0-9 % :) has no '.'.
var loopbackWServer = regexp.MustCompile(`y(\d+):W127\.0\.0\.1%3A(\d+)%3A`)

// guestRelayAddr rewrites every loopback WServer id in a host's lobby/chat
// push to route ("ip:port", the host endpoint this guest joined over). It
// returns args unchanged when there is nothing to rewrite.
func (s *Server) guestRelayAddr(args json.RawMessage, route string) json.RawMessage {
	host, port, err := net.SplitHostPort(route)
	if err != nil {
		return args
	}
	var m map[string]json.RawMessage
	var msg string
	if json.Unmarshal(args, &m) != nil || json.Unmarshal(m["msg"], &msg) != nil {
		return args
	}
	out, n := rewriteWServer(msg, host, port)
	if n == 0 {
		return args
	}
	m["msg"] = jsonString(out)
	raw, err := json.Marshal(m)
	if err != nil {
		return args
	}
	s.opt.Log.Printf("master: host's relay address 127.0.0.1 -> %s in a lobby message (%d id(s)); the game connects over the route it joined by", route, n)
	return raw
}

// rewriteWServer replaces host and port of each "y<len>:W127.0.0.1%3A<port>%3A..."
// string in msg, fixing its length prefix. The rest of the id (the password)
// is kept as encoded.
func rewriteWServer(msg, host, port string) (string, int) {
	var out []byte
	n, last := 0, 0
	for _, loc := range loopbackWServer.FindAllStringSubmatchIndex(msg, -1) {
		size, err := strconv.Atoi(msg[loc[2]:loc[3]])
		body := loc[3] + 1 // after "y<len>:"
		if err != nil || loc[0] < last || body+size > len(msg) || body+size < loc[1] {
			continue
		}
		token := "W" + host + "%3A" + port + "%3A" + msg[loc[1]:body+size]
		out = append(out, msg[last:loc[0]]...)
		out = append(out, "y"+strconv.Itoa(len(token))+":"+token...)
		last = body + size
		n++
	}
	if n == 0 {
		return msg, 0
	}
	return string(append(out, msg[last:]...)), n
}
