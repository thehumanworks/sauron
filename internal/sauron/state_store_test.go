package sauron

import (
	"errors"
	"path/filepath"
	"testing"
	"time"
)

func TestFileStateStoreLoadMissingReturnsNotFound(t *testing.T) {
	t.Parallel()

	store := NewFileStateStore(filepath.Join(t.TempDir(), "session.json"))
	_, err := store.Load()
	if !errors.Is(err, ErrStateNotFound) {
		t.Fatalf("expected ErrStateNotFound, got %v", err)
	}
}

func TestFileStateStoreSaveLoadAndClear(t *testing.T) {
	t.Parallel()

	path := filepath.Join(t.TempDir(), "session.json")
	store := NewFileStateStore(path)

	expected := &SessionState{
		SandboxID:      "sb-file",
		StartedAt:      time.Date(2026, 2, 20, 15, 0, 0, 0, time.UTC),
		TimeoutSeconds: 3600,
	}
	if err := store.Save(expected); err != nil {
		t.Fatalf("Save returned unexpected error: %v", err)
	}

	got, err := store.Load()
	if err != nil {
		t.Fatalf("Load returned unexpected error: %v", err)
	}
	if got.SandboxID != expected.SandboxID {
		t.Fatalf("unexpected sandbox id: %q", got.SandboxID)
	}
	if !got.StartedAt.Equal(expected.StartedAt) {
		t.Fatalf("unexpected started at: %v", got.StartedAt)
	}
	if got.TimeoutSeconds != expected.TimeoutSeconds {
		t.Fatalf("unexpected timeout seconds: %d", got.TimeoutSeconds)
	}

	if err := store.Clear(); err != nil {
		t.Fatalf("Clear returned unexpected error: %v", err)
	}
	if _, err := store.Load(); !errors.Is(err, ErrStateNotFound) {
		t.Fatalf("expected ErrStateNotFound after clear, got %v", err)
	}
}
