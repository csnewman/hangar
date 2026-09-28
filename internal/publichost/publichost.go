// Package publichost names the parts of Hangar that are served on hosts of
// their own -- each environment's editor and web servers, and the container
// registry -- after Hangar's public host.
//
// A name is a label joined to the public host in one of two styles. With
// Subdomain, the default, it is a subdomain: code<id>.hangar.example.com,
// which needs the zone under the public host and a certificate for
// *.<host>. With Prefix it is a sibling that ends in the public host's
// first label: code<id>-hangar.example.com, for networks where a machine is
// given one name, and every <anything>-<name> beside it in the same domain
// already resolves to it and is covered by a wildcard certificate for the
// domain.
//
// Either way the names are in the same site as Hangar's own, which the
// editor needs: the browser then sends its cookie inside Hangar's frame.
package publichost

import (
	"fmt"
	"net"
	"net/url"
	"strings"
)

// Style is how a name is joined to the public host.
type Style string

const (
	Subdomain Style = "subdomain"
	Prefix    Style = "prefix"
)

// ParseStyle reads a style as configured; empty is Subdomain.
func ParseStyle(s string) (Style, error) {
	switch Style(strings.ToLower(strings.TrimSpace(s))) {
	case "", Subdomain:
		return Subdomain, nil
	case Prefix:
		return Prefix, nil
	}
	return "", fmt.Errorf("the host style is %q or %q, not %q", Subdomain, Prefix, s)
}

// Public is Hangar's origin as browsers reach it, and the style its other
// hosts are named in.
type Public struct {
	scheme string
	// host is lower case, with the port if the origin has one.
	host  string
	style Style
}

// Parse reads Hangar's public URL, such as https://hangar.example.com, of
// which only the origin is used.
func Parse(raw string, style Style) (*Public, error) {
	u, err := url.Parse(raw)
	if err != nil || (u.Scheme != "http" && u.Scheme != "https") || u.Hostname() == "" {
		return nil, fmt.Errorf("the public URL must be an http or https origin, such as https://hangar.example.com; got %q", raw)
	}
	if style == "" {
		style = Subdomain
	}
	if style == Prefix {
		h := u.Hostname()
		if net.ParseIP(h) != nil || !strings.Contains(h, ".") {
			return nil, fmt.Errorf("prefix host names are siblings of the public host in its domain, so it must be a name in one, such as hangar.example.com; got %q", h)
		}
	}
	return &Public{scheme: u.Scheme, host: strings.ToLower(u.Host), style: style}, nil
}

// Style is the style other hosts are named in.
func (p *Public) Style() Style { return p.style }

// Scheme is http or https.
func (p *Public) Scheme() string { return p.scheme }

// Host is the public host, with its port if it has one.
func (p *Public) Host() string { return p.host }

// Origin is Hangar's own origin.
func (p *Public) Origin() string { return p.scheme + "://" + p.host }

// Name is the host for label, with the public host's port if it has one.
func (p *Public) Name(label string) string {
	return strings.ToLower(label) + p.join() + p.host
}

// NameOrigin is the origin for label.
func (p *Public) NameOrigin(label string) string { return p.scheme + "://" + p.Name(label) }

// Label is the label host is named for, if it is one of the names: host
// must end in the public host, port included, joined in this style.
func (p *Public) Label(host string) (string, bool) {
	label, ok := strings.CutSuffix(strings.ToLower(host), p.join()+p.host)
	if !ok || label == "" || strings.Contains(label, ".") {
		return "", false
	}
	return label, true
}

func (p *Public) join() string {
	if p.style == Prefix {
		return "-"
	}
	return "."
}
