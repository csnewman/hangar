package zone

import (
	"context"
	"errors"
	"fmt"
	"log/slog"
	"net"
	"net/netip"
	"strings"
	"time"

	"github.com/miekg/dns"
)

// Config is the zone Hangar serves.
type Config struct {
	// Zone is the zone's name, the host of Hangar's public URL.
	Zone string
	// Addresses are where every name in the zone points: the addresses
	// browsers and workers reach Hangar on. IPv4 ones answer A queries and
	// IPv6 ones AAAA.
	Addresses []netip.Addr
	// Nameservers are the zone's nameservers, as the parent's delegation
	// names them. Names inside the zone resolve to Addresses like any other,
	// which is the glue the parent must also carry. Empty is ns1.<zone>.
	Nameservers []string
	// Hostmaster is the SOA's responsible mailbox, as an address. Empty is
	// hostmaster@<zone>.
	Hostmaster string
	// TTL is the lifetime of the records the configuration implies.
	TTL time.Duration
}

// Server answers DNS queries for the zone, and only the zone: it does not
// recurse, and refuses questions about any other name.
type Server struct {
	cfg     Config
	zone    string // canonical
	fqdn    string // with the trailing dot
	records *Manager
	log     *slog.Logger
}

func NewServer(cfg Config, records *Manager, log *slog.Logger) (*Server, error) {
	cfg.Zone = Canonical(cfg.Zone)
	if cfg.Zone == "" || net.ParseIP(cfg.Zone) != nil {
		return nil, fmt.Errorf("the DNS zone must be a name, not %q", cfg.Zone)
	}
	if len(cfg.Addresses) == 0 {
		return nil, errors.New("the DNS zone needs at least one address for its names to point at")
	}
	if len(cfg.Nameservers) == 0 {
		cfg.Nameservers = []string{"ns1." + cfg.Zone}
	}
	for i, ns := range cfg.Nameservers {
		cfg.Nameservers[i] = Canonical(ns)
	}
	if cfg.Hostmaster == "" {
		cfg.Hostmaster = "hostmaster@" + cfg.Zone
	}
	if cfg.TTL == 0 {
		cfg.TTL = 5 * time.Minute
	}
	return &Server{cfg: cfg, zone: cfg.Zone, fqdn: dns.Fqdn(cfg.Zone), records: records, log: log}, nil
}

// Serve answers on addr over UDP and TCP until ctx ends.
func (s *Server) Serve(ctx context.Context, addr string) error {
	udp := &dns.Server{Addr: addr, Net: "udp", Handler: s}
	tcp := &dns.Server{Addr: addr, Net: "tcp", Handler: s}
	errc := make(chan error, 2)
	go func() { errc <- udp.ListenAndServe() }()
	go func() { errc <- tcp.ListenAndServe() }()
	s.log.Info("serving DNS", "zone", s.zone, "addr", addr, "nameservers", s.cfg.Nameservers)
	select {
	case err := <-errc:
		udp.Shutdown()
		tcp.Shutdown()
		return err
	case <-ctx.Done():
		udp.Shutdown()
		tcp.Shutdown()
		return nil
	}
}

// ServeDNS answers one query.
func (s *Server) ServeDNS(w dns.ResponseWriter, req *dns.Msg) {
	m := new(dns.Msg)
	m.SetReply(req)
	m.Authoritative = true
	m.RecursionAvailable = false
	defer func() {
		size := dns.MinMsgSize
		if o := req.IsEdns0(); o != nil {
			size = int(o.UDPSize())
			m.SetEdns0(o.UDPSize(), false)
		}
		if _, tcp := w.RemoteAddr().(*net.TCPAddr); !tcp {
			m.Truncate(size)
		}
		w.WriteMsg(m)
	}()

	if req.Opcode != dns.OpcodeQuery || len(req.Question) != 1 {
		m.Rcode = dns.RcodeNotImplemented
		return
	}
	q := req.Question[0]
	name := Canonical(q.Name)
	if name != s.zone && !strings.HasSuffix(name, "."+s.zone) {
		m.Authoritative = false
		m.Rcode = dns.RcodeRefused
		return
	}

	hdr := func(t uint16, ttl time.Duration) dns.RR_Header {
		return dns.RR_Header{Name: q.Name, Rrtype: t, Class: dns.ClassINET, Ttl: uint32(ttl.Seconds())}
	}
	switch q.Qtype {
	case dns.TypeA, dns.TypeAAAA, dns.TypeANY:
		for _, a := range s.cfg.Addresses {
			if a.Is4() && q.Qtype != dns.TypeAAAA {
				m.Answer = append(m.Answer, &dns.A{Hdr: hdr(dns.TypeA, s.cfg.TTL), A: a.AsSlice()})
			}
			if a.Is6() && !a.Is4In6() && q.Qtype != dns.TypeA {
				m.Answer = append(m.Answer, &dns.AAAA{Hdr: hdr(dns.TypeAAAA, s.cfg.TTL), AAAA: a.AsSlice()})
			}
		}
	case dns.TypeNS:
		if name == s.zone {
			for _, ns := range s.cfg.Nameservers {
				m.Answer = append(m.Answer, &dns.NS{Hdr: hdr(dns.TypeNS, s.cfg.TTL), Ns: dns.Fqdn(ns)})
			}
		}
	case dns.TypeSOA:
		if name == s.zone {
			m.Answer = append(m.Answer, s.soa())
		}
	case dns.TypeTXT:
		ctx, cancel := context.WithTimeout(context.Background(), 2*time.Second)
		recs, err := s.records.Lookup(ctx, name, "TXT")
		cancel()
		if err != nil {
			s.log.Warn("looking up TXT records", "name", name, "err", err)
			m.Rcode = dns.RcodeServerFailure
			return
		}
		for _, r := range recs {
			m.Answer = append(m.Answer, &dns.TXT{Hdr: hdr(dns.TypeTXT, r.TTL), Txt: splitTXT(r.Value)})
		}
	}
	// Every name in the zone exists, so a question with no answer is
	// NODATA, which carries the SOA for negative caching.
	if len(m.Answer) == 0 {
		m.Ns = append(m.Ns, s.soa())
	}
}

func (s *Server) soa() dns.RR {
	mbox := strings.Replace(s.cfg.Hostmaster, "@", ".", 1)
	return &dns.SOA{
		Hdr:     dns.RR_Header{Name: s.fqdn, Rrtype: dns.TypeSOA, Class: dns.ClassINET, Ttl: uint32(s.cfg.TTL.Seconds())},
		Ns:      dns.Fqdn(s.cfg.Nameservers[0]),
		Mbox:    dns.Fqdn(mbox),
		Serial:  1,
		Refresh: 3600,
		Retry:   600,
		Expire:  604800,
		// Negative answers are cached for a minute: a challenge's record is
		// looked for soon after it is made.
		Minttl: 60,
	}
}

// splitTXT cuts a value into the 255-byte strings a TXT record is made of.
func splitTXT(v string) []string {
	var out []string
	for len(v) > 255 {
		out = append(out, v[:255])
		v = v[255:]
	}
	return append(out, v)
}
