package sauron

import (
	"context"
	"errors"
	"time"
)

var (
	ErrStateNotFound   = errors.New("sauron session state not found")
	ErrSandboxNotFound = errors.New("sauron sandbox not found")
)

// StartSandboxRequest contains the inputs needed to create a new browser sandbox.
type StartSandboxRequest struct {
	AppName      string
	ImageID      string
	Timeout      time.Duration
	IdleTimeout  time.Duration
	UserMetadata string
}

// StartResult contains the credentials needed to connect to Chromium over CDP.
type StartResult struct {
	SandboxID string `json:"sandbox_id"`
	BrowseURL string `json:"browse_url"`
	Token     string `json:"token"`
}

// SessionState tracks the active sandbox and timeout window for health checks.
type SessionState struct {
	SandboxID      string    `json:"sandbox_id"`
	StartedAt      time.Time `json:"started_at"`
	TimeoutSeconds int       `json:"timeout_seconds"`
}

// HealthStatus reports whether a sandbox is running and its non-idle lifetime remaining.
type HealthStatus struct {
	Running          bool          `json:"running"`
	SandboxID        string        `json:"sandbox_id,omitempty"`
	Remaining        time.Duration `json:"-"`
	RemainingSeconds int64         `json:"remaining_seconds"`
	ExpiresAt        time.Time     `json:"expires_at,omitempty"`
}

// Options configures Sauron lifecycle behavior.
type Options struct {
	AppName     string
	ImageID     string
	Timeout     time.Duration
	IdleTimeout time.Duration
	Now         func() time.Time
}

// SandboxService is the Modal-facing contract for sandbox lifecycle operations.
type SandboxService interface {
	StartSandbox(ctx context.Context, req StartSandboxRequest) (*StartResult, error)
	StopSandbox(ctx context.Context, sandboxID string) error
	SandboxRunning(ctx context.Context, sandboxID string) (bool, error)
}

// StateStore stores and retrieves local session state.
type StateStore interface {
	Load() (*SessionState, error)
	Save(state *SessionState) error
	Clear() error
}
