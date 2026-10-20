// Native messaging helper for the flash-runner extension: gives Flash content the raw TCP sockets a browser cannot open.
//
// Chrome starts one copy per connection from the extension (it passes the extension origin as the first argument) and talks to it
// over stdin/stdout: a 4-byte little-endian length followed by a JSON message. Requests: hello, connect {host, port}, write {id, data},
// close {id}, pause {id}, resume {id}, tls {id, name}; each answered with { rid, ok, ... }. Received bytes arrive as { t: "data", id, data }
// (base64), a socket going away as { t: "close", id, error? }.
//
// Started any other way (double-clicked, "install", "uninstall") the same binary installs or removes itself for the current user:
// see install.go. One file, nothing else to install.
//
// Safety: only public addresses are reachable unless ~/.flash-runner/sockets.json says {"allowPrivate": true}; the destination is
// resolved here and the connection goes to the resolved address (no DNS rebinding); at most 64 sockets per helper process;
// an optional {"allowHosts": ["*.example.com"], "denyHosts": [...]} list is applied to the requested host name.
package main

import (
	"context"
	"crypto/tls"
	"encoding/base64"
	"encoding/binary"
	"encoding/json"
	"errors"
	"fmt"
	"io"
	"net"
	"os"
	"path/filepath"
	"regexp"
	"strings"
	"sync"
	"sync/atomic"
	"syscall"
	"time"
)

const (
	hostName   = "com.flashrunner.sockets"
	extOrigin  = "chrome-extension://njekchgflloenifdhaehploldjanaonk/"
	firefoxID  = "flash-runner@khoaakt.github.io" // must equal browser_specific_settings.gecko.id in tools/firefox/manifest-overlay.json
	version    = 2                                // the Node.js helper this replaces was 1
	chunk      = 384 * 1024
	maxMessage = 80 << 20
	dialWait   = 15 * time.Second
)

type config struct {
	AllowPrivate bool     `json:"allowPrivate"`
	AllowHosts   []string `json:"allowHosts"`
	DenyHosts    []string `json:"denyHosts"`
	MaxSockets   int      `json:"maxSockets"`
	AllowUDP     bool     `json:"allowUDP"` // RTMFP: see udp.go
}

type request struct {
	T    string `json:"t"`
	Rid  int64  `json:"rid"`
	Host string `json:"host"`
	Port int    `json:"port"`
	ID   int64  `json:"id"`
	Data string `json:"data"`
	Name string `json:"name"`
}

type codedError struct{ code, msg string }

func (e *codedError) Error() string { return e.msg }
func coded(code, msg string) error  { return &codedError{code, msg} }

// errCode maps a Go network error onto the errno-style names the extension understands (web/ppb-sockets.js).
func errCode(err error, def string) string {
	var ce *codedError
	if errors.As(err, &ce) {
		return ce.code
	}
	var dnsErr *net.DNSError
	if errors.As(err, &dnsErr) {
		return "ENOTFOUND"
	}
	var ne net.Error
	if errors.As(err, &ne) && ne.Timeout() {
		return "ETIMEDOUT"
	}
	switch {
	case errors.Is(err, syscall.ECONNREFUSED):
		return "ECONNREFUSED"
	case errors.Is(err, syscall.ECONNRESET):
		return "ECONNRESET"
	case errors.Is(err, syscall.EPIPE):
		return "EPIPE"
	case errors.Is(err, syscall.EHOSTUNREACH), errors.Is(err, syscall.ENETUNREACH):
		return "EHOSTUNREACH"
	}
	return def
}

func isPrivate(ip net.IP) bool {
	if ip.IsLoopback() || ip.IsPrivate() || ip.IsLinkLocalUnicast() || ip.IsLinkLocalMulticast() || ip.IsMulticast() || ip.IsUnspecified() {
		return true
	}
	if v4 := ip.To4(); v4 != nil {
		return v4[0] == 0 || (v4[0] == 100 && v4[1] >= 64 && v4[1] < 128) || v4[0] >= 224
	}
	return false
}

func globToRe(g string) *regexp.Regexp {
	return regexp.MustCompile("(?i)^" + strings.ReplaceAll(regexp.QuoteMeta(g), `\*`, ".*") + "$")
}

type writeReq struct {
	rid  int64
	data []byte
}

type sock struct {
	id        int64
	connMu    sync.RWMutex
	conn      net.Conn
	writes    chan writeReq
	done      chan struct{}
	closeOnce sync.Once
	pauseMu   sync.Mutex
	paused    bool
	pauseCond *sync.Cond
	switching atomic.Bool // the reader is being stopped for a TLS upgrade: exit without reporting a close
	readerEnd chan struct{}
}

func (s *sock) getConn() net.Conn { s.connMu.RLock(); defer s.connMu.RUnlock(); return s.conn }
func (s *sock) waitUnpaused() {
	s.pauseMu.Lock()
	for s.paused {
		s.pauseCond.Wait()
	}
	s.pauseMu.Unlock()
}
func (s *sock) setPaused(p bool) {
	s.pauseMu.Lock()
	s.paused = p
	s.pauseCond.Broadcast()
	s.pauseMu.Unlock()
}

type helper struct {
	cfg    config
	out    sync.Mutex
	w      io.Writer
	mu     sync.Mutex
	socks  map[int64]*sock
	udps   map[int64]*udpSock
	nextID int64
	allow  []*regexp.Regexp
	deny   []*regexp.Regexp
}

func (h *helper) send(v any) {
	b, err := json.Marshal(v)
	if err != nil {
		return
	}
	frame := make([]byte, 4+len(b))
	binary.LittleEndian.PutUint32(frame, uint32(len(b)))
	copy(frame[4:], b)
	h.out.Lock()
	_, _ = h.w.Write(frame)
	h.out.Unlock()
}
func (h *helper) reply(rid int64, extra map[string]any) {
	m := map[string]any{"rid": rid, "ok": true}
	for k, v := range extra {
		m[k] = v
	}
	h.send(m)
}
func (h *helper) fail(rid int64, err error, def string) {
	h.send(map[string]any{"rid": rid, "ok": false, "code": errCode(err, def), "message": err.Error()})
}

func (h *helper) hostAllowed(host string) bool {
	for _, r := range h.deny {
		if r.MatchString(host) {
			return false
		}
	}
	if h.allow == nil {
		return true
	}
	for _, r := range h.allow {
		if r.MatchString(host) {
			return true
		}
	}
	return false
}

func (h *helper) resolveTarget(ctx context.Context, host string) (string, error) {
	if !h.hostAllowed(host) {
		return "", coded("EDENIED", "host not allowed")
	}
	var ips []net.IP
	if ip := net.ParseIP(host); ip != nil {
		ips = []net.IP{ip}
	} else {
		addrs, err := net.DefaultResolver.LookupIPAddr(ctx, host)
		if err != nil {
			return "", coded("ENOTFOUND", "name not found")
		}
		for _, a := range addrs {
			ips = append(ips, a.IP)
		}
	}
	for _, ip := range ips {
		if h.cfg.AllowPrivate || !isPrivate(ip) {
			return ip.String(), nil
		}
	}
	return "", coded("EDENIED", "only public addresses are reachable")
}

func addrMap(a net.Addr) map[string]any {
	if t, ok := a.(*net.TCPAddr); ok {
		return map[string]any{"ip": t.IP.String(), "port": t.Port}
	}
	if u, ok := a.(*net.UDPAddr); ok {
		return map[string]any{"ip": u.IP.String(), "port": u.Port}
	}
	return map[string]any{"ip": "", "port": 0}
}

// readLoop ships everything the socket receives to the extension, honouring pause/resume so TCP flow control reaches the sender.
func (h *helper) readLoop(s *sock, c net.Conn) {
	defer close(s.readerEnd)
	buf := make([]byte, chunk)
	for {
		n, err := c.Read(buf)
		if n > 0 {
			s.waitUnpaused() // a paused socket keeps what it already read until resume; the next Read waits too, so TCP flow control reaches the sender
			h.send(map[string]any{"t": "data", "id": s.id, "data": base64.StdEncoding.EncodeToString(buf[:n])})
		}
		if err != nil {
			if s.switching.Load() {
				return
			}
			if h.remove(s.id) {
				m := map[string]any{"t": "close", "id": s.id}
				if !errors.Is(err, io.EOF) {
					m["error"] = errCode(err, "ECONNRESET")
				}
				h.send(m)
			}
			h.shutdown(s)
			return
		}
	}
}

// writeLoop performs the extension's writes in order and acknowledges each one, so the worker's backpressure follows the real socket.
func (h *helper) writeLoop(s *sock) {
	for {
		select {
		case w := <-s.writes:
			c := s.getConn()
			if _, err := c.Write(w.data); err != nil {
				h.fail(w.rid, err, "EPIPE")
			} else {
				h.reply(w.rid, nil)
			}
		case <-s.done:
			for {
				select {
				case w := <-s.writes:
					h.fail(w.rid, coded("EPIPE", "socket closed"), "EPIPE")
				default:
					return
				}
			}
		}
	}
}

func (h *helper) remove(id int64) bool {
	h.mu.Lock()
	defer h.mu.Unlock()
	if _, ok := h.socks[id]; !ok {
		return false
	}
	delete(h.socks, id)
	return true
}
func (h *helper) shutdown(s *sock) {
	s.closeOnce.Do(func() {
		close(s.done)
		s.setPaused(false)
		_ = s.getConn().Close()
	})
}
func (h *helper) get(id int64) *sock { h.mu.Lock(); defer h.mu.Unlock(); return h.socks[id] }

func (h *helper) handle(m request) {
	switch m.T {
	case "hello":
		h.reply(m.Rid, map[string]any{"version": version, "allowPrivate": h.cfg.AllowPrivate, "udp": h.cfg.AllowUDP})
	case "connect":
		h.mu.Lock()
		n := len(h.socks)
		h.mu.Unlock()
		if n >= h.cfg.MaxSockets {
			h.fail(m.Rid, coded("EDENIED", "too many sockets"), "EDENIED")
			return
		}
		if m.Port <= 0 || m.Port > 65535 {
			h.fail(m.Rid, coded("EDENIED", "bad port"), "EDENIED")
			return
		}
		go func() {
			ctx, cancel := context.WithTimeout(context.Background(), dialWait)
			defer cancel()
			addr, err := h.resolveTarget(ctx, m.Host)
			if err != nil {
				h.fail(m.Rid, err, "ENOTFOUND")
				return
			}
			d := net.Dialer{Timeout: dialWait}
			c, err := d.DialContext(ctx, "tcp", net.JoinHostPort(addr, fmt.Sprint(m.Port)))
			if err != nil {
				h.fail(m.Rid, err, "ECONNREFUSED")
				return
			}
			if t, ok := c.(*net.TCPConn); ok {
				_ = t.SetNoDelay(true)
			}
			s := &sock{conn: c, writes: make(chan writeReq, 1024), done: make(chan struct{}), readerEnd: make(chan struct{})}
			s.pauseCond = sync.NewCond(&s.pauseMu)
			h.mu.Lock()
			h.nextID++
			s.id = h.nextID
			h.socks[s.id] = s
			h.mu.Unlock()
			go h.readLoop(s, c)
			go h.writeLoop(s)
			h.reply(m.Rid, map[string]any{"id": s.id, "local": addrMap(c.LocalAddr()), "remote": addrMap(c.RemoteAddr())})
		}()
	case "write":
		s := h.get(m.ID)
		if s == nil {
			h.fail(m.Rid, coded("EPIPE", "no such socket"), "EPIPE")
			return
		}
		data, err := base64.StdEncoding.DecodeString(m.Data)
		if err != nil {
			h.fail(m.Rid, coded("EINVAL", "bad data"), "EINVAL")
			return
		}
		select {
		case s.writes <- writeReq{m.Rid, data}:
		case <-s.done:
			h.fail(m.Rid, coded("EPIPE", "socket closed"), "EPIPE")
		}
	case "close":
		if s := h.get(m.ID); s != nil && h.remove(m.ID) {
			h.shutdown(s)
		}
	case "pause":
		if s := h.get(m.ID); s != nil {
			s.setPaused(true)
		}
	case "resume":
		if s := h.get(m.ID); s != nil {
			s.setPaused(false)
		}
	case "tls": // STARTTLS on an open socket (Flash's secure sockets)
		s := h.get(m.ID)
		if s == nil {
			h.fail(m.Rid, coded("EPIPE", "no such socket"), "EPIPE")
			return
		}
		go func() {
			s.connMu.Lock()
			defer s.connMu.Unlock()
			raw := s.conn
			s.setPaused(false) // a reader waiting in pause could not be stopped
			s.switching.Store(true)
			_ = raw.SetReadDeadline(time.Now()) // unblock the reader; it exits quietly because of `switching`
			<-s.readerEnd
			_ = raw.SetReadDeadline(time.Time{})
			tc := tls.Client(raw, &tls.Config{ServerName: m.Name, MinVersion: tls.VersionTLS12})
			ctx, cancel := context.WithTimeout(context.Background(), dialWait)
			defer cancel()
			if err := tc.HandshakeContext(ctx); err != nil {
				s.switching.Store(false)
				h.fail(m.Rid, err, "ECONNRESET")
				if h.remove(s.id) {
					h.send(map[string]any{"t": "close", "id": s.id, "error": "ECONNRESET"})
				}
				h.shutdown(s)
				return
			}
			s.conn = tc
			s.readerEnd = make(chan struct{})
			s.switching.Store(false)
			go h.readLoop(s, tc)
			h.reply(m.Rid, nil)
		}()
	case "resolve": // host name -> addresses (HostResolver for RTMFP); the private-address rule is applied when a socket is used, not here
		go h.resolveNames(m)
	case "udp-bind", "udp-send", "udp-close":
		h.handleUDP(m)
	default:
		h.fail(m.Rid, coded("EINVAL", "unknown request "+m.T), "EINVAL")
	}
}

func loadConfig() config {
	cfg := config{MaxSockets: 64}
	path := os.Getenv("FLASHRUNNER_SOCKETS_CONFIG")
	if path == "" {
		if home, err := os.UserHomeDir(); err == nil {
			path = filepath.Join(home, ".flash-runner", "sockets.json")
		}
	}
	if b, err := os.ReadFile(path); err == nil {
		_ = json.Unmarshal(b, &cfg)
	}
	if cfg.MaxSockets <= 0 {
		cfg.MaxSockets = 64
	}
	return cfg
}

// serve runs the native messaging protocol on stdin/stdout until Chrome closes the pipe.
func serve() {
	h := &helper{cfg: loadConfig(), w: os.Stdout, socks: map[int64]*sock{}, udps: map[int64]*udpSock{}}
	if h.cfg.AllowHosts != nil {
		for _, g := range h.cfg.AllowHosts {
			h.allow = append(h.allow, globToRe(g))
		}
	}
	for _, g := range h.cfg.DenyHosts {
		h.deny = append(h.deny, globToRe(g))
	}
	in := os.Stdin
	hdr := make([]byte, 4)
	for {
		if _, err := io.ReadFull(in, hdr); err != nil {
			break
		}
		n := binary.LittleEndian.Uint32(hdr)
		if n > maxMessage {
			os.Exit(2)
		}
		body := make([]byte, n)
		if _, err := io.ReadFull(in, body); err != nil {
			break
		}
		var m request
		if err := json.Unmarshal(body, &m); err != nil {
			os.Exit(2)
		}
		h.handle(m)
	}
	h.mu.Lock()
	all := make([]*sock, 0, len(h.socks))
	for _, s := range h.socks {
		all = append(all, s)
	}
	h.socks = map[int64]*sock{}
	h.mu.Unlock()
	for _, s := range all {
		h.shutdown(s)
	}
	h.mu.Lock()
	us := make([]*udpSock, 0, len(h.udps))
	for _, u := range h.udps {
		us = append(us, u)
	}
	h.mu.Unlock()
	for _, u := range us {
		h.removeUDP(u)
	}
}

func main() {
	args := os.Args[1:]
	for _, a := range args {
		if strings.HasPrefix(a, "chrome-extension://") { // started by Chrome
			serve()
			return
		}
	}
	if len(args) == 2 && args[1] == firefoxID && strings.HasSuffix(strings.ToLower(args[0]), ".json") { // started by Firefox: <manifest path> <add-on id>
		serve()
		return
	}
	cmd := ""
	if len(args) > 0 {
		cmd = args[0]
	}
	switch cmd {
	case "serve":
		serve()
	case "--version", "version":
		fmt.Println(version)
	case "manifest": // manifest <absolute path of the helper> [--firefox]: the JSON the browser reads (used by the package builders)
		if len(args) < 2 {
			fmt.Fprintln(os.Stderr, "usage: manifest <path> [--firefox]")
			os.Exit(2)
		}
		os.Stdout.Write(manifestJSON(args[1], len(args) > 2 && args[2] == "--firefox"))
	case "uninstall":
		os.Exit(runInstall(false, args[1:]))
	case "", "install":
		rest := args // a double-click passes no arguments at all
		if len(rest) > 0 {
			rest = rest[1:]
		}
		os.Exit(runInstall(true, rest))
	default:
		fmt.Fprintln(os.Stderr, "usage: flash-helper [install|uninstall] [--dir <NativeMessagingHosts dir>] [--firefox-dir <dir>] [--quiet]")
		os.Exit(2)
	}
}
