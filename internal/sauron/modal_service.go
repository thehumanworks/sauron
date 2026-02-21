package sauron

import (
	"context"
	"crypto/tls"
	"encoding/json"
	"errors"
	"fmt"
	"io"
	"log/slog"
	"net/http"
	"net/url"
	"os"
	"sort"
	"strings"
	"time"

	"github.com/joho/godotenv"
	modal "github.com/modal-labs/libmodal/modal-go"
)

const (
	defaultRuntimeImage         = "node20"
	browserWSDiscoveryAttempts  = 8
	browserWSDiscoveryRetryWait = 250 * time.Millisecond
	browserWSDiscoveryTimeout   = 8 * time.Second
	browserWSHTTPTimeout        = 1 * time.Second
	devTunnelLookupTimeout      = 10 * time.Second
	cdpTunnelPort               = 9222
	modalClientCloseTimeout     = 2 * time.Second
)

const chromiumBootstrapCommand = `
set -euo pipefail
mkdir -p /tmp/chrome-data /workspace

configure_github_auth() {
  local token=""
  if [ -n "${GITHUB_TOKEN:-}" ]; then
    token="${GITHUB_TOKEN}"
  elif [ -n "${GH_TOKEN:-}" ]; then
    token="${GH_TOKEN}"
  fi

  if [ -n "$token" ]; then
    git config --global url."https://x-access-token:${token}@github.com/".insteadOf "https://github.com/"
  fi

  if command -v gh >/dev/null 2>&1; then
    gh auth setup-git >/tmp/sauron-gh-auth.log 2>&1 || true
  fi
}

clone_repo() {
  if [ -z "${SAURON_REPO_URL:-}" ]; then
    return
  fi

  rm -rf "${SAURON_REPO_DIR}"
  git clone "${SAURON_REPO_URL}" "${SAURON_REPO_DIR}"

  if [ -n "${SAURON_REPO_REF:-}" ]; then
    git -C "${SAURON_REPO_DIR}" fetch --depth=1 origin "${SAURON_REPO_REF}" || true
    git -C "${SAURON_REPO_DIR}" checkout "${SAURON_REPO_REF}"
  fi
}

start_dev_server() {
  if [ -z "${SAURON_DEV_CMD:-}" ]; then
    return
  fi

  local workdir="/workspace"
  if [ -n "${SAURON_REPO_URL:-}" ]; then
    workdir="${SAURON_REPO_DIR}"
  fi

  bash -lc "cd \"${workdir}\" && ${SAURON_DEV_CMD}" >/tmp/sauron-dev.log 2>&1 &
}

configure_github_auth
clone_repo
start_dev_server

# Chromium keeps CDP bound to loopback in current builds. Expose container port
# 9222 for Modal tunneling via a local TCP proxy.
socat TCP-LISTEN:9222,fork,reuseaddr,bind=0.0.0.0 TCP:127.0.0.1:9223 >/tmp/sauron-cdp-proxy.log 2>&1 &

exec chromium \
  --headless=new \
  --lang=en-US \
  --no-sandbox \
  --disable-gpu \
  --user-data-dir=/tmp/chrome-data \
  --remote-debugging-address=127.0.0.1 \
  --remote-debugging-port=9223 \
  --remote-allow-origins=* \
  about:blank
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
	if req.DevPort == cdpTunnelPort {
		return nil, fmt.Errorf("dev port %d is reserved for CDP tunnel", cdpTunnelPort)
	}

	s.logf("creating Modal client")
	client, err := s.newClient()
	if err != nil {
		return nil, err
	}
	defer s.closeClient(client)

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
	image, err := s.resolveImage(ctx, client, req.ImageID, req.Runtime)
	if err != nil {
		return nil, err
	}

	secrets, err := resolveStartSecrets(ctx, client.Secrets, req.SecretName, req.FromDotenv)
	if err != nil {
		return nil, err
	}
	if req.SecretName != "" {
		s.logf("resolved named secret %q", req.SecretName)
	}
	if req.FromDotenv != "" {
		s.logf("processed dotenv file %q for secret injection", req.FromDotenv)
	}

	env := map[string]string{
		"SAURON_REPO_URL": req.RepoURL,
		"SAURON_REPO_REF": req.RepoRef,
		"SAURON_REPO_DIR": req.RepoDir,
		"SAURON_DEV_CMD":  req.DevCommand,
	}

	var h2Ports []int
	h2Ports = append(h2Ports, cdpTunnelPort)
	if req.DevPort > 0 {
		h2Ports = append(h2Ports, req.DevPort)
	}

	s.logf("creating sandbox")
	sb, err := client.Sandboxes.Create(ctx, app, image, &modal.SandboxCreateParams{
		Command:     []string{"bash", "-lc", chromiumBootstrapCommand},
		Env:         env,
		Secrets:     secrets,
		H2Ports:     h2Ports,
		Timeout:     req.Timeout,
		IdleTimeout: req.IdleTimeout,
	})
	if err != nil {
		return nil, err
	}

	cdpURL, err := resolveTunnelURL(ctx, sb, cdpTunnelPort)
	if err != nil {
		return nil, fmt.Errorf("resolve cdp tunnel for port %d: %w", cdpTunnelPort, err)
	}

	wsCtx, cancelWS := context.WithTimeout(ctx, browserWSDiscoveryTimeout)
	defer cancelWS()
	wsURL, err := resolveBrowserWebSocketURL(
		wsCtx,
		cdpURL,
		"",
		browserWSDiscoveryAttempts,
		browserWSDiscoveryRetryWait,
		browserWSHTTPTimeout,
	)
	if err != nil {
		s.logf("browser websocket discovery failed: %v", err)
	}

	var devURL string
	if req.DevPort > 0 {
		devURL, err = resolveTunnelURL(ctx, sb, req.DevPort)
		if err != nil {
			s.logf("dev tunnel lookup failed for port %d: %v", req.DevPort, err)
		}
	}

	s.logf("sandbox ready: %s", sb.SandboxID)
	result := &StartResult{
		SandboxID:    sb.SandboxID,
		BrowseURL:    cdpURL,
		Token:        "",
		BrowserWSURL: wsURL,
		ConnectHeaders: map[string]string{
			"Host": "localhost",
		},
		DevServerURL:  devURL,
		DevServerPort: req.DevPort,
	}
	if req.RepoURL != "" {
		result.RepoPath = req.RepoDir
	}
	return result, nil
}

// StopSandbox terminates a sandbox by ID.
func (s *ModalService) StopSandbox(ctx context.Context, sandboxID string) error {
	s.logf("terminating sandbox: %s", sandboxID)
	client, err := s.newClient()
	if err != nil {
		return err
	}
	defer s.closeClient(client)

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

// ListSandboxes returns running sandboxes for a named Modal app.
func (s *ModalService) ListSandboxes(ctx context.Context, appName string) ([]SandboxSummary, error) {
	client, err := s.newClient()
	if err != nil {
		return nil, err
	}
	defer s.closeClient(client)

	app, err := client.Apps.FromName(ctx, appName, &modal.AppFromNameParams{
		CreateIfMissing: false,
	})
	if err != nil {
		if isModalNotFound(err) {
			return []SandboxSummary{}, nil
		}
		return nil, err
	}

	sandboxIter, err := client.Sandboxes.List(ctx, &modal.SandboxListParams{AppID: app.AppID})
	if err != nil {
		return nil, err
	}

	sandboxes := make([]SandboxSummary, 0)
	for sb, err := range sandboxIter {
		if err != nil {
			return nil, err
		}
		if sb == nil {
			continue
		}
		sandboxes = append(sandboxes, SandboxSummary{SandboxID: sb.SandboxID})
	}

	sort.Slice(sandboxes, func(i, j int) bool {
		return sandboxes[i].SandboxID < sandboxes[j].SandboxID
	})
	return sandboxes, nil
}

// SandboxRunning reports whether a sandbox is still running.
func (s *ModalService) SandboxRunning(ctx context.Context, sandboxID string) (bool, error) {
	client, err := s.newClient()
	if err != nil {
		return false, err
	}
	defer s.closeClient(client)

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

func (s *ModalService) resolveImage(ctx context.Context, client *modal.Client, imageID, runtime string) (*modal.Image, error) {
	if imageID != "" {
		return client.Images.FromID(ctx, imageID)
	}

	baseImage, err := runtimeBaseImage(runtime)
	if err != nil {
		return nil, err
	}

	return client.Images.FromRegistry(baseImage, nil).DockerfileCommands([]string{
		"RUN apt-get update && apt-get install -y --no-install-recommends chromium git gh ca-certificates socat && rm -rf /var/lib/apt/lists/*",
	}, nil), nil
}

func runtimeBaseImage(runtime string) (string, error) {
	switch strings.ToLower(strings.TrimSpace(runtime)) {
	case "", defaultRuntimeImage, "node":
		return "node:20-bookworm-slim", nil
	case "node22":
		return "node:22-bookworm-slim", nil
	case "python312", "python3.12":
		return "python:3.12-slim", nil
	default:
		return "", fmt.Errorf("unsupported runtime %q (supported: node20, node22, python312)", runtime)
	}
}

func loadDotenvValues(path string) (map[string]string, error) {
	dotenvPath := strings.TrimSpace(path)
	if dotenvPath == "" {
		return nil, nil
	}
	values, err := godotenv.Read(dotenvPath)
	if err != nil {
		return nil, fmt.Errorf("load dotenv file %q: %w", dotenvPath, err)
	}
	return values, nil
}

func resolveStartSecrets(
	ctx context.Context,
	secretService modal.SecretService,
	secretName string,
	fromDotenv string,
) ([]*modal.Secret, error) {
	var secrets []*modal.Secret

	if secretName != "" {
		secret, err := secretService.FromName(ctx, secretName, nil)
		if err != nil {
			return nil, fmt.Errorf("resolve secret %q: %w", secretName, err)
		}
		secrets = append(secrets, secret)
	}

	if fromDotenv != "" {
		dotenvValues, err := loadDotenvValues(fromDotenv)
		if err != nil {
			return nil, err
		}
		if len(dotenvValues) > 0 {
			// FromMap uses OBJECT_CREATION_TYPE_EPHEMERAL in Modal, so this
			// dotenv-derived secret is not a named deployed secret.
			dotenvSecret, err := secretService.FromMap(ctx, dotenvValues, nil)
			if err != nil {
				return nil, fmt.Errorf("create dotenv secret: %w", err)
			}
			secrets = append(secrets, dotenvSecret)
		}
	}

	return secrets, nil
}

func resolveBrowserWebSocketURL(
	ctx context.Context,
	browseURL string,
	token string,
	attempts int,
	delay time.Duration,
	requestTimeout time.Duration,
) (string, error) {
	baseURL, err := url.Parse(strings.TrimRight(browseURL, "/"))
	if err != nil {
		return "", fmt.Errorf("parse browse url: %w", err)
	}
	if requestTimeout <= 0 {
		requestTimeout = 10 * time.Second
	}

	headers := map[string]string{}
	if token != "" {
		headers["Authorization"] = fmt.Sprintf("Bearer %s", token)
	}
	httpClient := &http.Client{
		Timeout: requestTimeout,
		Transport: &http.Transport{
			ForceAttemptHTTP2: false,
			TLSClientConfig:   &tls.Config{MinVersion: tls.VersionTLS12},
		},
	}

	var lastErr error
	for i := 0; i < attempts; i++ {
		endpoint := strings.TrimRight(browseURL, "/") + "/json/version"
		req, err := http.NewRequestWithContext(ctx, http.MethodGet, endpoint, nil)
		if err != nil {
			return "", err
		}
		for key, value := range headers {
			req.Header.Set(key, value)
		}
		req.Host = "localhost"

		resp, err := httpClient.Do(req)
		if err == nil {
			var payload struct {
				WebSocketDebuggerURL string `json:"webSocketDebuggerUrl"`
			}
			decodeErr := json.NewDecoder(resp.Body).Decode(&payload)
			_ = resp.Body.Close()

			if resp.StatusCode == http.StatusOK && decodeErr == nil && payload.WebSocketDebuggerURL != "" {
				wsURL, err := url.Parse(payload.WebSocketDebuggerURL)
				if err != nil {
					lastErr = err
				} else {
					wsScheme := "ws"
					if baseURL.Scheme == "https" {
						wsScheme = "wss"
					}
					resolved := &url.URL{
						Scheme:   wsScheme,
						Host:     baseURL.Host,
						Path:     wsURL.Path,
						RawQuery: wsURL.RawQuery,
					}
					return resolved.String(), nil
				}
			} else if decodeErr != nil {
				lastErr = decodeErr
			} else {
				lastErr = fmt.Errorf("unexpected status %d", resp.StatusCode)
			}
		} else {
			lastErr = err
		}

		select {
		case <-ctx.Done():
			return "", ctx.Err()
		case <-time.After(delay):
		}
	}

	if lastErr == nil {
		lastErr = errors.New("unknown websocket discovery error")
	}
	return "", fmt.Errorf("resolve browser websocket url: %w", lastErr)
}

type sandboxTunnelSource interface {
	Tunnels(ctx context.Context, timeout time.Duration) (map[int]*modal.Tunnel, error)
}

func resolveTunnelURL(ctx context.Context, source sandboxTunnelSource, port int) (string, error) {
	return resolveTunnelURLWithTimeout(ctx, source, port, devTunnelLookupTimeout)
}

func resolveTunnelURLWithTimeout(
	ctx context.Context,
	source sandboxTunnelSource,
	port int,
	lookupTimeout time.Duration,
) (string, error) {
	if lookupTimeout <= 0 {
		lookupTimeout = devTunnelLookupTimeout
	}
	lookupCtx, cancel := context.WithTimeout(ctx, lookupTimeout)
	defer cancel()

	type tunnelLookupResult struct {
		tunnels map[int]*modal.Tunnel
		err     error
	}
	resultCh := make(chan tunnelLookupResult, 1)
	go func() {
		tunnels, err := source.Tunnels(lookupCtx, lookupTimeout)
		resultCh <- tunnelLookupResult{tunnels: tunnels, err: err}
	}()

	var lookupResult tunnelLookupResult
	select {
	case <-ctx.Done():
		return "", ctx.Err()
	case <-lookupCtx.Done():
		return "", fmt.Errorf(
			"timed out waiting for tunnel on port %d after %s: %w",
			port,
			lookupTimeout,
			lookupCtx.Err(),
		)
	case lookupResult = <-resultCh:
	}

	if lookupResult.err != nil {
		var timeoutErr modal.SandboxTimeoutError
		if errors.As(lookupResult.err, &timeoutErr) || errors.Is(lookupResult.err, context.DeadlineExceeded) {
			return "", fmt.Errorf(
				"timed out waiting for tunnel on port %d after %s: %w",
				port,
				lookupTimeout,
				lookupResult.err,
			)
		}
		return "", lookupResult.err
	}

	tunnels := lookupResult.tunnels
	tunnel, ok := tunnels[port]
	if !ok {
		return "", fmt.Errorf("no tunnel for port %d", port)
	}
	return tunnel.URL(), nil
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

func (s *ModalService) closeClient(client *modal.Client) {
	done := make(chan struct{}, 1)
	go func() {
		client.Close()
		done <- struct{}{}
	}()

	select {
	case <-done:
	case <-time.After(modalClientCloseTimeout):
		s.logf("modal client close timed out after %s; continuing", modalClientCloseTimeout)
	}
}
