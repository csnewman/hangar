package certs

import (
	"context"
	"log/slog"

	"go.uber.org/zap/zapcore"
)

// slogCore sends certmagic's zap logging to slog. Debug is dropped:
// certmagic logs every handshake at that level.
type slogCore struct {
	log    *slog.Logger
	fields []zapcore.Field
}

func (c *slogCore) Enabled(l zapcore.Level) bool { return l >= zapcore.InfoLevel }

func (c *slogCore) With(fields []zapcore.Field) zapcore.Core {
	return &slogCore{log: c.log, fields: append(append([]zapcore.Field(nil), c.fields...), fields...)}
}

func (c *slogCore) Check(e zapcore.Entry, ce *zapcore.CheckedEntry) *zapcore.CheckedEntry {
	if c.Enabled(e.Level) {
		return ce.AddCore(e, c)
	}
	return ce
}

func (c *slogCore) Write(e zapcore.Entry, fields []zapcore.Field) error {
	enc := zapcore.NewMapObjectEncoder()
	for _, f := range c.fields {
		f.AddTo(enc)
	}
	for _, f := range fields {
		f.AddTo(enc)
	}
	attrs := make([]slog.Attr, 0, len(enc.Fields))
	for k, v := range enc.Fields {
		attrs = append(attrs, slog.Any(k, v))
	}
	level := slog.LevelInfo
	switch {
	case e.Level >= zapcore.ErrorLevel:
		level = slog.LevelError
	case e.Level == zapcore.WarnLevel:
		level = slog.LevelWarn
	}
	c.log.LogAttrs(context.Background(), level, e.Message, attrs...)
	return nil
}

func (c *slogCore) Sync() error { return nil }
