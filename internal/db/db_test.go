package db_test

import (
	"context"
	"errors"
	"sync"
	"sync/atomic"
	"testing"

	"github.com/jackc/pgx/v5/pgconn"

	"github.com/csnewman/hangar/internal/db"
	"github.com/csnewman/hangar/internal/dbtest"
)

func TestTransactRetriesSerializationFailures(t *testing.T) {
	d := dbtest.Open(t)
	calls := 0
	err := d.Transact(context.Background(), func(tx db.Tx) error {
		calls++
		if calls < 3 {
			return &pgconn.PgError{Code: "40001", Message: "could not serialize access"}
		}
		return nil
	})
	if err != nil {
		t.Fatalf("Transact: %v", err)
	}
	if calls != 3 {
		t.Fatalf("fn ran %d times, want 3", calls)
	}
}

func TestTransactReturnsOtherErrorsAtOnce(t *testing.T) {
	d := dbtest.Open(t)
	want := errors.New("not a database error")
	calls := 0
	err := d.Transact(context.Background(), func(tx db.Tx) error {
		calls++
		return want
	})
	if !errors.Is(err, want) {
		t.Fatalf("Transact returned %v, want %v", err, want)
	}
	if calls != 1 {
		t.Fatalf("fn ran %d times, want 1", calls)
	}
}

func TestTransactGivesUp(t *testing.T) {
	d := dbtest.Open(t)
	calls := 0
	err := d.Transact(context.Background(), func(tx db.Tx) error {
		calls++
		return &pgconn.PgError{Code: "40P01"}
	})
	if err == nil {
		t.Fatal("Transact succeeded on a function that always deadlocks")
	}
	if calls < 2 {
		t.Fatalf("fn ran %d times; a deadlock should be retried", calls)
	}
}

// Two transactions that lock the same rows in opposite orders deadlock.
// Postgres aborts one of them, and Transact must run it again so that both
// commit.
func TestTransactRetriesRealDeadlock(t *testing.T) {
	d := dbtest.Open(t)
	ctx := context.Background()
	if err := d.Transact(ctx, func(tx db.Tx) error {
		_, err := tx.Exec(ctx, `CREATE TABLE pair (id integer PRIMARY KEY); INSERT INTO pair VALUES (1), (2)`)
		return err
	}); err != nil {
		t.Fatal(err)
	}

	// Each side, on its first attempt only, waits until the other holds its
	// first lock before asking for its second. That guarantees the deadlock
	// once and lets the retry through.
	held := [2]chan struct{}{make(chan struct{}), make(chan struct{})}
	var once [2]sync.Once
	var attempts atomic.Int32

	lockBoth := func(side, first, second int) error {
		return d.Transact(ctx, func(tx db.Tx) error {
			attempts.Add(1)
			if _, err := tx.Exec(ctx, `SELECT 1 FROM pair WHERE id = $1 FOR UPDATE`, first); err != nil {
				return err
			}
			once[side].Do(func() {
				close(held[side])
				<-held[1-side]
			})
			_, err := tx.Exec(ctx, `SELECT 1 FROM pair WHERE id = $1 FOR UPDATE`, second)
			return err
		})
	}

	var wg sync.WaitGroup
	errs := make([]error, 2)
	wg.Go(func() { errs[0] = lockBoth(0, 1, 2) })
	wg.Go(func() { errs[1] = lockBoth(1, 2, 1) })
	wg.Wait()

	for i, err := range errs {
		if err != nil {
			t.Fatalf("side %d: %v", i, err)
		}
	}
	if n := attempts.Load(); n < 3 {
		t.Fatalf("%d attempts; a deadlock should have forced a retry", n)
	}
}

// Every replica migrates on startup, so migrations must survive being run by
// several at once.
func TestConcurrentMigration(t *testing.T) {
	url := dbtest.URL(t)
	var wg sync.WaitGroup
	errs := make(chan error, 4)
	for range 4 {
		wg.Go(func() {
			d, err := db.Open(context.Background(), url)
			if err == nil {
				d.Close()
			}
			errs <- err
		})
	}
	wg.Wait()
	close(errs)
	for err := range errs {
		if err != nil {
			t.Fatalf("Open: %v", err)
		}
	}
}

func TestValidUUID(t *testing.T) {
	for s, want := range map[string]bool{
		"5c7b9f4c-250f-49d2-b276-c9b127230208": true,
		"5C7B9F4C-250F-49D2-B276-C9B127230208": true,
		"5c7b9f4c250f49d2b276c9b127230208":     false,
		"5c7b9f4c-250f-49d2-b276-c9b12723020g": false,
		"":                                     false,
		"nope":                                 false,
	} {
		if got := db.ValidUUID(s); got != want {
			t.Errorf("ValidUUID(%q) = %v, want %v", s, got, want)
		}
	}
}
