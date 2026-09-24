// Package zone makes hangar-server the authoritative DNS server for its own
// zone: the host in HANGAR_PUBLIC_URL and every name under it.
//
// The zone is delegated to it: the parent zone's NS records name
// hangar-server's nameservers, with glue giving their addresses. Every name
// in the zone resolves to Hangar -- Hangar itself, and each environment's
// editor on e-<id>.<zone> -- so the zone needs no records made per
// environment. What it does need is TXT records made on demand: ACME's
// DNS-01 challenge, which is the only way to be issued the wildcard
// certificate the editors' names need. Those are kept in Postgres, so every
// replica serves the same answers.
package zone

import (
	"context"
	"strings"
	"time"

	"github.com/csnewman/hangar/internal/db"
)

// Record is one record the zone serves beyond those its configuration
// implies.
type Record struct {
	// Name is fully qualified, lower case, without a trailing dot.
	Name  string
	Type  string
	Value string
	TTL   time.Duration
}

// Manager keeps the zone's records.
type Manager struct {
	db *db.DB
}

func NewManager(d *db.DB) *Manager { return &Manager{db: d} }

// Canonical is a name as the zone keeps it: lower case, without a trailing
// dot.
func Canonical(name string) string {
	return strings.TrimSuffix(strings.ToLower(name), ".")
}

// Add adds records, leaving any the zone already has.
func (m *Manager) Add(ctx context.Context, recs ...Record) error {
	return m.db.Transact(ctx, func(tx db.Tx) error {
		for _, r := range recs {
			if _, err := tx.Exec(ctx, `INSERT INTO dns_records (name, type, value, ttl) VALUES ($1, $2, $3, $4)
				ON CONFLICT (name, type, value) DO UPDATE SET ttl = EXCLUDED.ttl`,
				Canonical(r.Name), strings.ToUpper(r.Type), r.Value, int(r.TTL.Seconds())); err != nil {
				return err
			}
		}
		return nil
	})
}

// Remove removes records that match on name, type and value.
func (m *Manager) Remove(ctx context.Context, recs ...Record) error {
	return m.db.Transact(ctx, func(tx db.Tx) error {
		for _, r := range recs {
			if _, err := tx.Exec(ctx, `DELETE FROM dns_records WHERE name = $1 AND type = $2 AND value = $3`,
				Canonical(r.Name), strings.ToUpper(r.Type), r.Value); err != nil {
				return err
			}
		}
		return nil
	})
}

// Lookup returns the records of one name and type.
func (m *Manager) Lookup(ctx context.Context, name, typ string) ([]Record, error) {
	var out []Record
	err := m.db.Transact(ctx, func(tx db.Tx) error {
		out = nil
		rows, err := tx.Query(ctx, `SELECT value, ttl FROM dns_records WHERE name = $1 AND type = $2 ORDER BY created_at`,
			Canonical(name), strings.ToUpper(typ))
		if err != nil {
			return err
		}
		defer rows.Close()
		for rows.Next() {
			r := Record{Name: Canonical(name), Type: strings.ToUpper(typ)}
			var ttl int
			if err := rows.Scan(&r.Value, &ttl); err != nil {
				return err
			}
			r.TTL = time.Duration(ttl) * time.Second
			out = append(out, r)
		}
		return rows.Err()
	})
	return out, err
}

// Prune removes records older than age: a challenge's record is removed
// when the challenge is done, and one left by a replica that died midway is
// removed here.
func (m *Manager) Prune(ctx context.Context, age time.Duration) (int64, error) {
	var n int64
	err := m.db.Transact(ctx, func(tx db.Tx) error {
		tag, err := tx.Exec(ctx, `DELETE FROM dns_records WHERE created_at < $1`, time.Now().Add(-age))
		n = tag.RowsAffected()
		return err
	})
	return n, err
}
