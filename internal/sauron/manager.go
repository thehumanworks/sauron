package sauron

import (
	"context"
	"errors"
	"time"
)

const (
	defaultAppName      = "sauron"
	defaultRuntime      = "node20"
	defaultRepoDir      = "/workspace/repo"
	defaultTimeout      = 1 * time.Hour
	defaultIdleTimeout  = 5 * time.Minute
	defaultUserMetadata = `{"purpose":"sauron"}`
)

// Manager coordinates sandbox lifecycle operations and local session state.
type Manager struct {
	service SandboxService
	store   StateStore
	options Options
}

// NewManager builds a lifecycle manager with safe defaults.
func NewManager(service SandboxService, store StateStore, options Options) *Manager {
	if options.Timeout <= 0 {
		options.Timeout = defaultTimeout
	}
	if options.IdleTimeout <= 0 {
		options.IdleTimeout = defaultIdleTimeout
	}
	if options.AppName == "" {
		options.AppName = defaultAppName
	}
	if options.Runtime == "" {
		options.Runtime = defaultRuntime
	}
	if options.RepoDir == "" {
		options.RepoDir = defaultRepoDir
	}
	if options.Now == nil {
		options.Now = time.Now
	}

	return &Manager{
		service: service,
		store:   store,
		options: options,
	}
}

// Start launches a new sandbox and stores local session metadata.
func (m *Manager) Start(ctx context.Context) (*StartResult, error) {
	result, err := m.service.StartSandbox(ctx, StartSandboxRequest{
		AppName:      m.options.AppName,
		ImageID:      m.options.ImageID,
		Runtime:      m.options.Runtime,
		FromDotenv:   m.options.FromDotenv,
		Timeout:      m.options.Timeout,
		IdleTimeout:  m.options.IdleTimeout,
		UserMetadata: defaultUserMetadata,
		SecretName:   m.options.SecretName,
		RepoURL:      m.options.RepoURL,
		RepoRef:      m.options.RepoRef,
		RepoDir:      m.options.RepoDir,
		DevCommand:   m.options.DevCommand,
		DevPort:      m.options.DevPort,
	})
	if err != nil {
		return nil, err
	}

	if err := m.store.Save(&SessionState{
		SandboxID:      result.SandboxID,
		StartedAt:      m.options.Now().UTC(),
		TimeoutSeconds: int(m.options.Timeout.Seconds()),
	}); err != nil {
		return nil, err
	}

	return result, nil
}

// Stop terminates the provided sandbox ID.
func (m *Manager) Stop(ctx context.Context, sandboxID string) error {
	trackedState, err := m.loadStateIfPresent()
	if err != nil {
		return err
	}
	tracked := trackedState != nil

	if err := m.service.StopSandbox(ctx, sandboxID); err != nil {
		if tracked && trackedState.SandboxID == sandboxID && errors.Is(err, ErrSandboxNotFound) {
			_ = m.store.Clear()
		}
		return err
	}

	if tracked && trackedState.SandboxID == sandboxID {
		return m.store.Clear()
	}
	return nil
}

// Health checks whether the tracked sandbox is still alive and reports hard-timeout remaining.
func (m *Manager) Health(ctx context.Context) (*HealthStatus, error) {
	state, err := m.store.Load()
	if errors.Is(err, ErrStateNotFound) {
		return &HealthStatus{}, nil
	}
	if err != nil {
		return nil, err
	}

	running, err := m.service.SandboxRunning(ctx, state.SandboxID)
	if errors.Is(err, ErrSandboxNotFound) {
		_ = m.store.Clear()
		return &HealthStatus{}, nil
	}
	if err != nil {
		return nil, err
	}
	if !running {
		_ = m.store.Clear()
		return &HealthStatus{}, nil
	}

	expiresAt := state.StartedAt.Add(time.Duration(state.TimeoutSeconds) * time.Second).UTC()
	remaining := expiresAt.Sub(m.options.Now().UTC())
	if remaining < 0 {
		remaining = 0
	}

	return &HealthStatus{
		Running:          true,
		SandboxID:        state.SandboxID,
		Remaining:        remaining,
		RemainingSeconds: int64(remaining / time.Second),
		ExpiresAt:        expiresAt,
	}, nil
}

func (m *Manager) loadStateIfPresent() (*SessionState, error) {
	state, err := m.store.Load()
	if errors.Is(err, ErrStateNotFound) {
		return nil, nil
	}
	if err != nil {
		return nil, err
	}
	return state, nil
}
