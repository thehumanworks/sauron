package sauron

import (
	"context"
	"errors"
	"testing"
	"time"
)

type fakeSandboxService struct {
	startResult StartResult
	startErr    error

	stopErr error

	running    bool
	runningErr error

	startCalls int
	stopCalls  int

	lastStopSandboxID string
}

func (f *fakeSandboxService) StartSandbox(_ context.Context, _ StartSandboxRequest) (*StartResult, error) {
	f.startCalls++
	if f.startErr != nil {
		return nil, f.startErr
	}
	res := f.startResult
	return &res, nil
}

func (f *fakeSandboxService) StopSandbox(_ context.Context, sandboxID string) error {
	f.stopCalls++
	f.lastStopSandboxID = sandboxID
	return f.stopErr
}

func (f *fakeSandboxService) SandboxRunning(_ context.Context, _ string) (bool, error) {
	if f.runningErr != nil {
		return false, f.runningErr
	}
	return f.running, nil
}

type memoryStateStore struct {
	state      *SessionState
	clearCalls int
	saveCalls  int
}

func (m *memoryStateStore) Load() (*SessionState, error) {
	if m.state == nil {
		return nil, ErrStateNotFound
	}
	copy := *m.state
	return &copy, nil
}

func (m *memoryStateStore) Save(state *SessionState) error {
	copy := *state
	m.state = &copy
	m.saveCalls++
	return nil
}

func (m *memoryStateStore) Clear() error {
	m.clearCalls++
	m.state = nil
	return nil
}

func TestManagerStartSavesState(t *testing.T) {
	t.Parallel()

	now := time.Date(2026, 2, 20, 15, 0, 0, 0, time.UTC)
	service := &fakeSandboxService{
		startResult: StartResult{
			SandboxID: "sb-123",
			BrowseURL: "https://sandbox.example",
			Token:     "token-123",
		},
	}
	store := &memoryStateStore{}
	manager := NewManager(service, store, Options{
		Timeout: 1 * time.Hour,
		Now: func() time.Time {
			return now
		},
	})

	result, err := manager.Start(context.Background())
	if err != nil {
		t.Fatalf("Start returned unexpected error: %v", err)
	}

	if result.SandboxID != "sb-123" {
		t.Fatalf("unexpected sandbox id: %q", result.SandboxID)
	}
	if result.BrowseURL != "https://sandbox.example" {
		t.Fatalf("unexpected url: %q", result.BrowseURL)
	}
	if result.Token != "token-123" {
		t.Fatalf("unexpected token: %q", result.Token)
	}
	if service.startCalls != 1 {
		t.Fatalf("expected one start call, got %d", service.startCalls)
	}
	if store.state == nil {
		t.Fatal("state was not saved")
	}
	if store.state.SandboxID != "sb-123" {
		t.Fatalf("unexpected stored sandbox id: %q", store.state.SandboxID)
	}
	if !store.state.StartedAt.Equal(now) {
		t.Fatalf("unexpected started at: %v", store.state.StartedAt)
	}
	if store.state.TimeoutSeconds != 3600 {
		t.Fatalf("unexpected timeout seconds: %d", store.state.TimeoutSeconds)
	}
}

func TestManagerStopClearsTrackedStateForMatchingSandboxID(t *testing.T) {
	t.Parallel()

	service := &fakeSandboxService{}
	store := &memoryStateStore{
		state: &SessionState{SandboxID: "sb-stop"},
	}
	manager := NewManager(service, store, Options{Timeout: 1 * time.Hour})

	if err := manager.Stop(context.Background(), "sb-stop"); err != nil {
		t.Fatalf("Stop returned unexpected error: %v", err)
	}

	if service.stopCalls != 1 {
		t.Fatalf("expected one stop call, got %d", service.stopCalls)
	}
	if service.lastStopSandboxID != "sb-stop" {
		t.Fatalf("unexpected stop sandbox id: %q", service.lastStopSandboxID)
	}
	if store.state != nil {
		t.Fatal("state should be cleared after stop")
	}
}

func TestManagerStopWithDifferentSandboxIDDoesNotClearTrackedState(t *testing.T) {
	t.Parallel()

	service := &fakeSandboxService{}
	store := &memoryStateStore{
		state: &SessionState{SandboxID: "sb-tracked"},
	}
	manager := NewManager(service, store, Options{Timeout: 1 * time.Hour})

	if err := manager.Stop(context.Background(), "sb-other"); err != nil {
		t.Fatalf("Stop returned unexpected error: %v", err)
	}

	if service.stopCalls != 1 {
		t.Fatalf("expected one stop call, got %d", service.stopCalls)
	}
	if service.lastStopSandboxID != "sb-other" {
		t.Fatalf("unexpected stop sandbox id: %q", service.lastStopSandboxID)
	}
	if store.state == nil || store.state.SandboxID != "sb-tracked" {
		t.Fatalf("tracked state should remain untouched for non-tracked stop: %#v", store.state)
	}
}

func TestManagerStopNotFoundClearsMatchingTrackedStateAndReturnsError(t *testing.T) {
	t.Parallel()

	service := &fakeSandboxService{
		stopErr: ErrSandboxNotFound,
	}
	store := &memoryStateStore{
		state: &SessionState{SandboxID: "sb-stale"},
	}
	manager := NewManager(service, store, Options{Timeout: 1 * time.Hour})

	err := manager.Stop(context.Background(), "sb-stale")
	if !errors.Is(err, ErrSandboxNotFound) {
		t.Fatalf("expected ErrSandboxNotFound, got %v", err)
	}
	if store.state != nil {
		t.Fatal("tracked state should be cleared when matching sandbox is gone")
	}
}

func TestManagerHealthRunningIncludesRemainingTime(t *testing.T) {
	t.Parallel()

	started := time.Date(2026, 2, 20, 14, 0, 0, 0, time.UTC)
	now := started.Add(10 * time.Minute)
	service := &fakeSandboxService{running: true}
	store := &memoryStateStore{
		state: &SessionState{
			SandboxID:      "sb-health",
			StartedAt:      started,
			TimeoutSeconds: int((1 * time.Hour).Seconds()),
		},
	}
	manager := NewManager(service, store, Options{
		Timeout: 1 * time.Hour,
		Now: func() time.Time {
			return now
		},
	})

	health, err := manager.Health(context.Background())
	if err != nil {
		t.Fatalf("Health returned unexpected error: %v", err)
	}

	if !health.Running {
		t.Fatal("expected running health")
	}
	if health.SandboxID != "sb-health" {
		t.Fatalf("unexpected sandbox id: %q", health.SandboxID)
	}
	if health.Remaining != 50*time.Minute {
		t.Fatalf("unexpected remaining: %v", health.Remaining)
	}
	if store.clearCalls != 0 {
		t.Fatalf("state should not be cleared, clear calls: %d", store.clearCalls)
	}
}

func TestManagerHealthNotRunningClearsState(t *testing.T) {
	t.Parallel()

	service := &fakeSandboxService{running: false}
	store := &memoryStateStore{
		state: &SessionState{
			SandboxID:      "sb-down",
			StartedAt:      time.Date(2026, 2, 20, 13, 0, 0, 0, time.UTC),
			TimeoutSeconds: int((1 * time.Hour).Seconds()),
		},
	}
	manager := NewManager(service, store, Options{Timeout: 1 * time.Hour})

	health, err := manager.Health(context.Background())
	if err != nil {
		t.Fatalf("Health returned unexpected error: %v", err)
	}

	if health.Running {
		t.Fatal("expected non-running health")
	}
	if health.Remaining != 0 {
		t.Fatalf("expected zero remaining, got %v", health.Remaining)
	}
	if store.clearCalls != 1 {
		t.Fatalf("expected one clear call, got %d", store.clearCalls)
	}
}

func TestManagerHealthNoState(t *testing.T) {
	t.Parallel()

	service := &fakeSandboxService{}
	store := &memoryStateStore{}
	manager := NewManager(service, store, Options{Timeout: 1 * time.Hour})

	health, err := manager.Health(context.Background())
	if err != nil {
		t.Fatalf("Health returned unexpected error: %v", err)
	}
	if health.Running {
		t.Fatal("expected no active sandbox")
	}
	if health.Remaining != 0 {
		t.Fatalf("expected zero remaining, got %v", health.Remaining)
	}
}
