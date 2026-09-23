// Package db is hangar-server's connection to Postgres. It owns the schema's
// migrations and the one way to run SQL: a transaction that is retried when
// Postgres says retrying is the fix.
//
// It knows nothing about what is stored. The SQL for environments, workers
// and placement lives with the managers that do that work, each of which is
// handed a *DB and runs its queries inside Transact.
//
// Transactions run at READ COMMITTED. Several server replicas share one
// database, and what keeps them from racing is explicit row locks -- FOR
// UPDATE on the rows a decision depends on, SKIP LOCKED on queues -- taken by
// the managers where they know a race is possible. SERIALIZABLE would find
// those races without being told, at the price of predicate-lock bookkeeping
// on every read and of aborting transactions that merely touched the same
// pages, which on the hot paths here (a long poll per worker, a status
// report every few seconds) buys retries rather than safety.
package db

import (
	"context"
	"errors"
	"fmt"
	"log/slog"
	"math/rand/v2"
	"time"

	"github.com/jackc/pgx/v5"
	"github.com/jackc/pgx/v5/pgconn"
	"github.com/jackc/pgx/v5/pgxpool"
)

type DB struct {
	pool *pgxpool.Pool
}

// Open connects and brings the schema up to date.
func Open(ctx context.Context, url string) (*DB, error) {
	pool, err := pgxpool.New(ctx, url)
	if err != nil {
		return nil, err
	}
	if err := pool.Ping(ctx); err != nil {
		pool.Close()
		return nil, fmt.Errorf("connecting to the database: %w", err)
	}
	if err := migrate(ctx, pool); err != nil {
		pool.Close()
		return nil, err
	}
	return &DB{pool: pool}, nil
}

func (d *DB) Close() { d.pool.Close() }

// Tx is the transaction a Transact function runs in.
type Tx = pgx.Tx

const (
	maxAttempts = 8
	baseBackoff = 5 * time.Millisecond
	maxBackoff  = 500 * time.Millisecond
)

// Transact runs fn in a READ COMMITTED transaction and commits it, running it
// again from the start when Postgres reports a deadlock, a serialization
// failure, or a connection lost before anything was sent.
//
// fn may run more than once, so it must have no effect outside tx: anything
// it computes for the caller should be assigned, not appended. An error
// returned by fn rolls back and is returned as it is, without a retry.
func (d *DB) Transact(ctx context.Context, fn func(tx Tx) error) error {
	txOpts := pgx.TxOptions{IsoLevel: pgx.ReadCommitted}
	backoff := baseBackoff
	for attempt := 1; ; attempt++ {
		err := pgx.BeginTxFunc(ctx, d.pool, txOpts, fn)
		if err == nil || !retryable(err) || attempt == maxAttempts || ctx.Err() != nil {
			return err
		}
		// Full jitter: replicas that collided once must not collide again
		// on the same schedule.
		sleep := rand.N(backoff) + time.Millisecond
		slog.Debug("retrying transaction", "attempt", attempt, "err", err, "after", sleep)
		select {
		case <-ctx.Done():
			return ctx.Err()
		case <-time.After(sleep):
		}
		backoff = min(backoff*2, maxBackoff)
	}
}

func retryable(err error) bool {
	var pgErr *pgconn.PgError
	if errors.As(err, &pgErr) {
		switch pgErr.Code {
		case "40001", // serialization_failure
			"40P01": // deadlock_detected
			return true
		}
		return false
	}
	// A connection that failed before anything was sent cannot have
	// committed, so running again cannot apply the change twice.
	return pgconn.SafeToRetry(err)
}

// IsUniqueViolation reports whether err is a unique constraint refusing a
// duplicate.
func IsUniqueViolation(err error) bool {
	var pgErr *pgconn.PgError
	return errors.As(err, &pgErr) && pgErr.Code == "23505"
}

// Notify sends a notification on channel when tx commits, and not at all if
// it rolls back.
func Notify(ctx context.Context, tx Tx, channel, payload string) error {
	_, err := tx.Exec(ctx, `SELECT pg_notify($1, $2)`, channel, payload)
	return err
}

// Listen delivers notifications on the given channels to fn until ctx ends,
// reconnecting if the connection drops. After every connect it calls fn with
// an empty channel name: anything sent while disconnected is lost, so a
// listener must then re-read whatever it cares about.
func (d *DB) Listen(ctx context.Context, fn func(channel, payload string), channels ...string) {
	for ctx.Err() == nil {
		err := d.listenOnce(ctx, fn, channels)
		if ctx.Err() != nil {
			return
		}
		slog.Warn("database notifications interrupted, reconnecting", "err", err)
		select {
		case <-ctx.Done():
			return
		case <-time.After(time.Second):
		}
	}
}

func (d *DB) listenOnce(ctx context.Context, fn func(channel, payload string), channels []string) error {
	pooled, err := d.pool.Acquire(ctx)
	if err != nil {
		return err
	}
	// A connection that has run LISTEN must not go back to the pool, where
	// another caller would receive this listener's notifications.
	conn := pooled.Hijack()
	defer conn.Close(context.WithoutCancel(ctx))

	for _, ch := range channels {
		if _, err := conn.Exec(ctx, `LISTEN `+pgx.Identifier{ch}.Sanitize()); err != nil {
			return err
		}
	}
	fn("", "")
	for {
		n, err := conn.WaitForNotification(ctx)
		if err != nil {
			return err
		}
		fn(n.Channel, n.Payload)
	}
}

// ValidUUID reports whether s is a UUID in its text form. IDs from a request
// are checked with it before a query, because a malformed one is a type
// error to Postgres rather than a missing row.
func ValidUUID(s string) bool {
	if len(s) != 36 {
		return false
	}
	for i, c := range s {
		switch i {
		case 8, 13, 18, 23:
			if c != '-' {
				return false
			}
		default:
			if !(c >= '0' && c <= '9' || c >= 'a' && c <= 'f' || c >= 'A' && c <= 'F') {
				return false
			}
		}
	}
	return true
}
