package sauron

import (
	"context"
	"errors"
	"fmt"
	"io"
	"log/slog"
	"os"

	modal "github.com/modal-labs/libmodal/modal-go"
)

const chromiumBootstrapCommand = `
set -euo pipefail
mkdir -p /tmp/chrome-data

# Chromium listens on localhost only on recent versions (security hardening),
# so keep it internal (9223) and forward a reachable port to it.
chromium \
  --headless=new \
  --lang=en-US \
  --no-sandbox \
  --disable-gpu \
  --user-data-dir=/tmp/chrome-data \
  --remote-debugging-port=9223 \
  --remote-allow-origins=* \
  about:blank &

# Sandbox Connect Tokens require the server to listen on port 8080.
exec socat TCP-LISTEN:8080,fork,reuseaddr TCP:127.0.0.1:9223
`

// ModalService adapts modal-go to the SandboxService interface.
type ModalService struct {
	newClient func() (*modal.Client, error)
	verbose   bool
	stdout    io.Writer
}

// ModalServiceOptions controls Modal client behavior.
type ModalServiceOptions struct {
	Verbose bool
	Stdout  io.Writer
}

// NewModalService constructs a service backed by modal-go.
func NewModalService(opts ModalServiceOptions) *ModalService {
	stdout := opts.Stdout
	if stdout == nil {
		stdout = os.Stdout
	}

	newClient := modal.NewClient
	if opts.Verbose {
		logger := slog.New(slog.NewTextHandler(stdout, &slog.HandlerOptions{
			Level: slog.LevelDebug,
		}))
		newClient = func() (*modal.Client, error) {
			return modal.NewClientWithOptions(&modal.ClientParams{
				Logger: logger,
			})
		}
	}

	return &ModalService{
		newClient: newClient,
		verbose:   opts.Verbose,
		stdout:    stdout,
	}
}

// StartSandbox creates a Chromium sandbox and returns CDP connect credentials.
func (s *ModalService) StartSandbox(ctx context.Context, req StartSandboxRequest) (*StartResult, error) {
	s.logf("creating Modal client")
	client, err := s.newClient()
	if err != nil {
		return nil, err
	}
	defer client.Close()

	s.logf("resolving app %q", req.AppName)
	app, err := client.Apps.FromName(ctx, req.AppName, &modal.AppFromNameParams{
		CreateIfMissing: true,
	})
	if err != nil {
		return nil, err
	}

	if req.ImageID == "" {
		s.logf("no image ID provided, building default Chromium image")
	} else {
		s.logf("using existing image ID %q", req.ImageID)
	}
	image, err := s.resolveImage(ctx, client, req.ImageID)
	if err != nil {
		return nil, err
	}

	s.logf("creating sandbox")
	sb, err := client.Sandboxes.Create(ctx, app, image, &modal.SandboxCreateParams{
		Command:     []string{"bash", "-lc", chromiumBootstrapCommand},
		Timeout:     req.Timeout,
		IdleTimeout: req.IdleTimeout,
	})
	if err != nil {
		return nil, err
	}

	s.logf("creating connect token")
	creds, err := sb.CreateConnectToken(ctx, &modal.SandboxCreateConnectTokenParams{
		UserMetadata: req.UserMetadata,
	})
	if err != nil {
		return nil, err
	}

	s.logf("sandbox ready: %s", sb.SandboxID)
	return &StartResult{
		SandboxID: sb.SandboxID,
		BrowseURL: creds.URL,
		Token:     creds.Token,
	}, nil
}

// StopSandbox terminates a sandbox by ID.
func (s *ModalService) StopSandbox(ctx context.Context, sandboxID string) error {
	s.logf("terminating sandbox: %s", sandboxID)
	client, err := s.newClient()
	if err != nil {
		return err
	}
	defer client.Close()

	sb, err := client.Sandboxes.FromID(ctx, sandboxID)
	if err != nil {
		if isModalNotFound(err) {
			return ErrSandboxNotFound
		}
		return err
	}

	if err := sb.Terminate(ctx); err != nil {
		if isModalNotFound(err) {
			return ErrSandboxNotFound
		}
		return err
	}
	return nil
}

// SandboxRunning reports whether a sandbox is still running.
func (s *ModalService) SandboxRunning(ctx context.Context, sandboxID string) (bool, error) {
	client, err := s.newClient()
	if err != nil {
		return false, err
	}
	defer client.Close()

	sb, err := client.Sandboxes.FromID(ctx, sandboxID)
	if err != nil {
		if isModalNotFound(err) {
			return false, ErrSandboxNotFound
		}
		return false, err
	}

	exitCode, err := sb.Poll(ctx)
	if err != nil {
		return false, err
	}
	return exitCode == nil, nil
}

func (s *ModalService) resolveImage(ctx context.Context, client *modal.Client, imageID string) (*modal.Image, error) {
	if imageID != "" {
		return client.Images.FromID(ctx, imageID)
	}

	return client.Images.FromRegistry("python:3.12-slim", nil).DockerfileCommands([]string{
		"RUN apt-get update && apt-get install -y --no-install-recommends chromium socat && rm -rf /var/lib/apt/lists/*",
	}, nil), nil
}

func isModalNotFound(err error) bool {
	var notFound modal.NotFoundError
	if errors.As(err, &notFound) {
		return true
	}
	return false
}

func (s *ModalService) logf(format string, args ...any) {
	if !s.verbose {
		return
	}
	_, _ = fmt.Fprintf(s.stdout, "[sauron] "+format+"\n", args...)
}
