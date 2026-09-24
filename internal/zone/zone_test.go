package zone_test

import (
	"context"
	"io"
	"log/slog"
	"net"
	"net/netip"
	"strings"
	"testing"
	"time"

	"github.com/miekg/dns"

	"github.com/csnewman/hangar/internal/dbtest"
	"github.com/csnewman/hangar/internal/zone"
)

func freePort(t *testing.T) string {
	t.Helper()
	c, err := net.ListenPacket("udp", "127.0.0.1:0")
	if err != nil {
		t.Fatal(err)
	}
	addr := c.LocalAddr().String()
	c.Close()
	return addr
}

func TestZone(t *testing.T) {
	ctx, cancel := context.WithCancel(context.Background())
	defer cancel()
	records := zone.NewManager(dbtest.Open(t))
	srv, err := zone.NewServer(zone.Config{
		Zone:        "Hangar.Example.com.",
		Addresses:   []netip.Addr{netip.MustParseAddr("203.0.113.10"), netip.MustParseAddr("2001:db8::10")},
		Nameservers: []string{"ns1.hangar.example.com", "ns2.hangar.example.com"},
	}, records, slog.New(slog.NewTextHandler(io.Discard, nil)))
	if err != nil {
		t.Fatal(err)
	}
	addr := freePort(t)
	go srv.Serve(ctx, addr)

	ask := func(network, name string, qtype uint16) *dns.Msg {
		t.Helper()
		c := &dns.Client{Net: network, Timeout: 2 * time.Second}
		m := new(dns.Msg)
		m.SetQuestion(dns.Fqdn(name), qtype)
		var r *dns.Msg
		var err error
		for range 50 {
			if r, _, err = c.Exchange(m, addr); err == nil {
				return r
			}
			time.Sleep(20 * time.Millisecond)
		}
		t.Fatalf("%s %s: %v", name, dns.TypeToString[qtype], err)
		return nil
	}
	answers := func(r *dns.Msg) []string {
		var out []string
		for _, rr := range r.Answer {
			f := strings.Fields(rr.String())
			out = append(out, f[3]+" "+strings.Join(f[4:], " "))
		}
		return out
	}
	expect := func(what string, got []string, want ...string) {
		t.Helper()
		if strings.Join(got, "|") != strings.Join(want, "|") {
			t.Errorf("%s: got %q, want %q", what, got, want)
		}
	}

	// Every name in the zone is Hangar: the zone itself, an editor's host,
	// and the nameservers, whose addresses are the delegation's glue.
	for _, name := range []string{"hangar.example.com", "e-2a04a86a-260e-4fdf-9d94-d1b5a1ce8a7c.hangar.example.com", "ns2.hangar.example.com", "HANGAR.example.COM"} {
		r := ask("udp", name, dns.TypeA)
		if !r.Authoritative || r.Rcode != dns.RcodeSuccess {
			t.Errorf("%s: authoritative %v, rcode %s", name, r.Authoritative, dns.RcodeToString[r.Rcode])
		}
		expect(name+" A", answers(r), "A 203.0.113.10")
		expect(name+" AAAA", answers(ask("udp", name, dns.TypeAAAA)), "AAAA 2001:db8::10")
	}
	expect("NS", answers(ask("udp", "hangar.example.com", dns.TypeNS)), "NS ns1.hangar.example.com.", "NS ns2.hangar.example.com.")
	soa := ask("udp", "hangar.example.com", dns.TypeSOA)
	if len(soa.Answer) != 1 || soa.Answer[0].(*dns.SOA).Ns != "ns1.hangar.example.com." {
		t.Errorf("SOA: %v", soa.Answer)
	}

	// A challenge's TXT record is served while it exists; without one the
	// name has no data, and the SOA says how long to remember that.
	name := "_acme-challenge.hangar.example.com"
	empty := ask("udp", name, dns.TypeTXT)
	if len(empty.Answer) != 0 || empty.Rcode != dns.RcodeSuccess || len(empty.Ns) != 1 {
		t.Errorf("TXT before: %v %s %v", empty.Answer, dns.RcodeToString[empty.Rcode], empty.Ns)
	}
	recs := []zone.Record{
		{Name: name, Type: "TXT", Value: "token-for-apex", TTL: time.Minute},
		{Name: name + ".", Type: "txt", Value: "token-for-wildcard", TTL: time.Minute},
	}
	if err := records.Add(ctx, recs...); err != nil {
		t.Fatal(err)
	}
	expect("TXT", answers(ask("udp", name, dns.TypeTXT)), `TXT "token-for-apex"`, `TXT "token-for-wildcard"`)
	expect("TXT over TCP", answers(ask("tcp", name, dns.TypeTXT)), `TXT "token-for-apex"`, `TXT "token-for-wildcard"`)
	if err := records.Remove(ctx, recs[0]); err != nil {
		t.Fatal(err)
	}
	expect("TXT after removing one", answers(ask("udp", name, dns.TypeTXT)), `TXT "token-for-wildcard"`)

	// Nothing outside the zone is answered.
	for _, other := range []string{"example.com", "nothangar.example.com", "google.com"} {
		r := ask("udp", other, dns.TypeA)
		if r.Rcode != dns.RcodeRefused || len(r.Answer) != 0 || r.Authoritative {
			t.Errorf("%s: rcode %s, %d answers", other, dns.RcodeToString[r.Rcode], len(r.Answer))
		}
	}
}
