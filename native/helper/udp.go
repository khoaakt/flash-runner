package main

// UDP for Flash's RTMFP (NetConnection "rtmfp://", the only thing in Flash Player that uses UDP). Off unless ~/.flash-runner/sockets.json says
// {"allowUDP": true}. Requests: udp-bind {host?, port}, udp-send {id, host, port, data}, udp-close {id}; received datagrams arrive as
// { t: "udp-data", id, ip, port, data } (base64) and a socket going away as { t: "udp-close", id }.
//
// Safety, on top of the TCP rules (destinations are resolved here and must be public unless allowPrivate): a UDP socket is bound to the wildcard
// address, so anything on the network could aim datagrams at it. Only datagrams from an IP address this socket has already sent to are passed
// on, which is what a reply (or a hole-punched peer) looks like; at most 16 UDP sockets per helper process; 64 KiB per datagram.

import (
	"context"
	"encoding/base64"
	"fmt"
	"net"
	"sync"
)

const (
	maxUDPSockets  = 16
	maxDatagram    = 65507
	udpResolveWait = dialWait
)

type udpSock struct {
	id    int64
	conn  *net.UDPConn
	mu    sync.Mutex
	peers map[string]bool // IPs that have been sent to
}

func (h *helper) handleUDP(m request) {
	if !h.cfg.AllowUDP {
		h.fail(m.Rid, coded("EDENIED", "UDP is disabled (set allowUDP in sockets.json)"), "EDENIED")
		return
	}
	switch m.T {
	case "udp-bind":
		h.mu.Lock()
		n := len(h.udps)
		h.mu.Unlock()
		if n >= maxUDPSockets {
			h.fail(m.Rid, coded("EDENIED", "too many UDP sockets"), "EDENIED")
			return
		}
		if m.Port < 0 || m.Port > 65535 {
			h.fail(m.Rid, coded("EDENIED", "bad port"), "EDENIED")
			return
		}
		network, ip := "udp4", net.IPv4zero
		if p := net.ParseIP(m.Host); p != nil && p.To4() == nil {
			network, ip = "udp6", net.IPv6unspecified
		}
		c, err := net.ListenUDP(network, &net.UDPAddr{IP: ip, Port: m.Port})
		if err != nil {
			h.fail(m.Rid, err, "EADDRINUSE")
			return
		}
		u := &udpSock{conn: c, peers: map[string]bool{}}
		h.mu.Lock()
		h.nextID++
		u.id = h.nextID
		h.udps[u.id] = u
		h.mu.Unlock()
		go h.udpReadLoop(u)
		h.reply(m.Rid, map[string]any{"id": u.id, "local": addrMap(c.LocalAddr())})
	case "udp-send":
		u := h.getUDP(m.ID)
		if u == nil {
			h.fail(m.Rid, coded("EPIPE", "no such socket"), "EPIPE")
			return
		}
		data, err := base64.StdEncoding.DecodeString(m.Data)
		if err != nil || len(data) > maxDatagram {
			h.fail(m.Rid, coded("EINVAL", "bad data"), "EINVAL")
			return
		}
		if m.Port <= 0 || m.Port > 65535 {
			h.fail(m.Rid, coded("EDENIED", "bad port"), "EDENIED")
			return
		}
		go func() {
			ctx, cancel := context.WithTimeout(context.Background(), udpResolveWait)
			defer cancel()
			addr, err := h.resolveTarget(ctx, m.Host)
			if err != nil {
				h.fail(m.Rid, err, "ENOTFOUND")
				return
			}
			dst, err := net.ResolveUDPAddr("udp", net.JoinHostPort(addr, fmt.Sprint(m.Port)))
			if err != nil {
				h.fail(m.Rid, err, "ENOTFOUND")
				return
			}
			u.mu.Lock()
			u.peers[dst.IP.String()] = true
			u.mu.Unlock()
			n, err := u.conn.WriteToUDP(data, dst)
			if err != nil {
				h.fail(m.Rid, err, "EPIPE")
				return
			}
			h.reply(m.Rid, map[string]any{"n": n})
		}()
	case "udp-close":
		if u := h.getUDP(m.ID); u != nil {
			h.removeUDP(u)
		}
	}
}

func (h *helper) getUDP(id int64) *udpSock { h.mu.Lock(); defer h.mu.Unlock(); return h.udps[id] }

func (h *helper) removeUDP(u *udpSock) bool {
	h.mu.Lock()
	_, ok := h.udps[u.id]
	delete(h.udps, u.id)
	h.mu.Unlock()
	_ = u.conn.Close()
	return ok
}

func (h *helper) udpReadLoop(u *udpSock) {
	buf := make([]byte, maxDatagram)
	for {
		n, from, err := u.conn.ReadFromUDP(buf)
		if err != nil {
			if h.removeUDP(u) {
				h.send(map[string]any{"t": "udp-close", "id": u.id})
			}
			return
		}
		u.mu.Lock()
		known := u.peers[from.IP.String()]
		u.mu.Unlock()
		if !known {
			continue // nobody was ever sent anything from here: not a reply
		}
		h.send(map[string]any{"t": "udp-data", "id": u.id, "ip": from.IP.String(), "port": from.Port, "data": base64.StdEncoding.EncodeToString(buf[:n])})
	}
}

// resolveNames answers {t: "resolve", host}: every address of the name (the deny/allow host lists still apply), at most 16.
func (h *helper) resolveNames(m request) {
	if !h.cfg.AllowUDP { // only RTMFP's HostResolver needs it
		h.fail(m.Rid, coded("EDENIED", "name resolution is part of UDP support (set allowUDP in sockets.json)"), "EDENIED")
		return
	}
	if !h.hostAllowed(m.Host) {
		h.fail(m.Rid, coded("EDENIED", "host not allowed"), "EDENIED")
		return
	}
	ctx, cancel := context.WithTimeout(context.Background(), udpResolveWait)
	defer cancel()
	addrs, err := net.DefaultResolver.LookupIPAddr(ctx, m.Host)
	if err != nil {
		h.fail(m.Rid, coded("ENOTFOUND", "name not found"), "ENOTFOUND")
		return
	}
	out := []string{}
	for _, a := range addrs {
		if len(out) < 16 {
			out = append(out, a.IP.String())
		}
	}
	h.reply(m.Rid, map[string]any{"addrs": out})
}
