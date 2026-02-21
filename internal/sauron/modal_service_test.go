package sauron

import (
	"bytes"
	"context"
	"errors"
	"net/http"
	"net/http/httptest"
	"os"
	"path/filepath"
	"strings"
	"testing"
	"time"

	modal "github.com/modal-labs/libmodal/modal-go"
)

func TestModalServiceLogfWritesWhenVerbose(t *testing.T) {
	t.Parallel()

	var out bytes.Buffer
	service := NewModalService(ModalServiceOptions{
		Verbose: true,
		Stdout:  &out,
	})

	service.logf("building image %s", "img-123")
	got := out.String()
	if !strings.Contains(got, "[sauron] building image img-123") {
		t.Fatalf("expected verbose log line, got %q", got)
	}
}

func TestModalServiceLogfSkipsWhenNotVerbose(t *testing.T) {
	t.Parallel()

	var out bytes.Buffer
	service := NewModalService(ModalServiceOptions{
		Verbose: false,
		Stdout:  &out,
	})

	service.logf("should not render")
	if out.Len() != 0 {
		t.Fatalf("expected no log output, got %q", out.String())
	}
}

func TestRuntimeBaseImage(t *testing.T) {
	t.Parallel()

	testCases := []struct {
		name        string
		runtime     string
		expected    string
		expectError bool
	}{
		{name: "default", runtime: "", expected: "node:20-bookworm-slim"},
		{name: "node20", runtime: "node20", expected: "node:20-bookworm-slim"},
		{name: "node22", runtime: "node22", expected: "node:22-bookworm-slim"},
		{name: "python312", runtime: "python312", expected: "python:3.12-slim"},
		{name: "unsupported", runtime: "ruby", expectError: true},
	}

	for _, tc := range testCases {
		tc := tc
		t.Run(tc.name, func(t *testing.T) {
			t.Parallel()

			got, err := runtimeBaseImage(tc.runtime)
			if tc.expectError {
				if err == nil {
					t.Fatal("expected error")
				}
				return
			}
			if err != nil {
				t.Fatalf("unexpected error: %v", err)
			}
			if got != tc.expected {
				t.Fatalf("expected %q, got %q", tc.expected, got)
			}
		})
	}
}

func TestResolveBrowserWebSocketURL(t *testing.T) {
	t.Parallel()

	var gotAuthHeader string
	var gotHostHeader string
	server := httptest.NewServer(http.HandlerFunc(func(w http.ResponseWriter, r *http.Request) {
		if r.URL.Path != "/json/version" {
			http.NotFound(w, r)
			return
		}
		gotAuthHeader = r.Header.Get("Authorization")
		gotHostHeader = r.Host
		_, _ = w.Write([]byte(`{"webSocketDebuggerUrl":"ws://127.0.0.1:9223/devtools/browser/abc123"}`))
	}))
	defer server.Close()

	wsURL, err := resolveBrowserWebSocketURL(
		context.Background(),
		server.URL,
		"token-123",
		1,
		10*time.Millisecond,
		100*time.Millisecond,
	)
	if err != nil {
		t.Fatalf("unexpected error: %v", err)
	}
	expectedPrefix := "ws://" + strings.TrimPrefix(server.URL, "http://")
	expected := expectedPrefix + "/devtools/browser/abc123"
	if wsURL != expected {
		t.Fatalf("expected ws url %q, got %q", expected, wsURL)
	}
	if gotAuthHeader != "Bearer token-123" {
		t.Fatalf("unexpected auth header: %q", gotAuthHeader)
	}
	if gotHostHeader != "localhost" {
		t.Fatalf("unexpected host header: %q", gotHostHeader)
	}
}

func TestResolveBrowserWebSocketURLWithoutToken(t *testing.T) {
	t.Parallel()

	var gotAuthHeader string
	var gotHostHeader string
	server := httptest.NewServer(http.HandlerFunc(func(w http.ResponseWriter, r *http.Request) {
		if r.URL.Path != "/json/version" {
			http.NotFound(w, r)
			return
		}
		gotAuthHeader = r.Header.Get("Authorization")
		gotHostHeader = r.Host
		_, _ = w.Write([]byte(`{"webSocketDebuggerUrl":"ws://127.0.0.1:9222/devtools/browser/abc123"}`))
	}))
	defer server.Close()

	wsURL, err := resolveBrowserWebSocketURL(
		context.Background(),
		server.URL,
		"",
		1,
		10*time.Millisecond,
		100*time.Millisecond,
	)
	if err != nil {
		t.Fatalf("unexpected error: %v", err)
	}
	expectedPrefix := "ws://" + strings.TrimPrefix(server.URL, "http://")
	expected := expectedPrefix + "/devtools/browser/abc123"
	if wsURL != expected {
		t.Fatalf("expected ws url %q, got %q", expected, wsURL)
	}
	if gotAuthHeader != "" {
		t.Fatalf("expected empty auth header, got %q", gotAuthHeader)
	}
	if gotHostHeader != "localhost" {
		t.Fatalf("host should be forced to localhost, got %q", gotHostHeader)
	}
}

func TestResolveBrowserWebSocketURLFailure(t *testing.T) {
	t.Parallel()

	server := httptest.NewServer(http.HandlerFunc(func(w http.ResponseWriter, _ *http.Request) {
		w.WriteHeader(http.StatusServiceUnavailable)
		_, _ = w.Write([]byte(`{}`))
	}))
	defer server.Close()

	_, err := resolveBrowserWebSocketURL(
		context.Background(),
		server.URL,
		"token-123",
		1,
		10*time.Millisecond,
		100*time.Millisecond,
	)
	if err == nil {
		t.Fatal("expected error")
	}
	if !strings.Contains(err.Error(), "unexpected status 503") {
		t.Fatalf("unexpected error: %v", err)
	}
}

type fakeTunnelSource struct {
	tunnels             map[int]*modal.Tunnel
	err                 error
	waitForContextDone  bool
	artificialDelayWait time.Duration
	blockUntilRelease   <-chan struct{}
}

func (f *fakeTunnelSource) Tunnels(ctx context.Context, _ time.Duration) (map[int]*modal.Tunnel, error) {
	if f.waitForContextDone {
		<-ctx.Done()
		return nil, ctx.Err()
	}
	if f.blockUntilRelease != nil {
		select {
		case <-ctx.Done():
			return nil, ctx.Err()
		case <-f.blockUntilRelease:
		}
	}
	if f.artificialDelayWait > 0 {
		select {
		case <-ctx.Done():
			return nil, ctx.Err()
		case <-time.After(f.artificialDelayWait):
		}
	}
	if f.err != nil {
		return nil, f.err
	}
	return f.tunnels, nil
}

func TestResolveTunnelURL(t *testing.T) {
	t.Parallel()

	source := &fakeTunnelSource{
		tunnels: map[int]*modal.Tunnel{
			5173: {
				Host: "example.modal.run",
				Port: 443,
			},
		},
	}

	got, err := resolveTunnelURL(context.Background(), source, 5173)
	if err != nil {
		t.Fatalf("unexpected error: %v", err)
	}
	if got != "https://example.modal.run" {
		t.Fatalf("unexpected tunnel URL: %q", got)
	}
}

func TestResolveTunnelURLMissingPort(t *testing.T) {
	t.Parallel()

	source := &fakeTunnelSource{
		tunnels: map[int]*modal.Tunnel{
			3000: {Host: "example.modal.run", Port: 443},
		},
	}

	_, err := resolveTunnelURL(context.Background(), source, 5173)
	if err == nil {
		t.Fatal("expected error")
	}
	if !strings.Contains(err.Error(), "no tunnel for port 5173") {
		t.Fatalf("unexpected error: %v", err)
	}
}

func TestResolveTunnelURLError(t *testing.T) {
	t.Parallel()

	source := &fakeTunnelSource{
		err: errors.New("boom"),
	}

	_, err := resolveTunnelURL(context.Background(), source, 5173)
	if err == nil {
		t.Fatal("expected error")
	}
	if err.Error() != "boom" {
		t.Fatalf("unexpected error: %v", err)
	}
}

func TestResolveTunnelURLWithTimeout(t *testing.T) {
	t.Parallel()

	release := make(chan struct{})
	defer close(release)

	source := &fakeTunnelSource{
		blockUntilRelease: release,
	}

	started := time.Now()
	_, err := resolveTunnelURLWithTimeout(context.Background(), source, 9222, 40*time.Millisecond)
	if err == nil {
		t.Fatal("expected error")
	}
	if !strings.Contains(err.Error(), "timed out waiting for tunnel on port 9222") {
		t.Fatalf("unexpected error: %v", err)
	}
	if elapsed := time.Since(started); elapsed > 2*time.Second {
		t.Fatalf("timeout path took too long: %v", elapsed)
	}
}

func TestLoadDotenvValues(t *testing.T) {
	t.Parallel()

	dir := t.TempDir()
	path := filepath.Join(dir, ".env")
	if err := os.WriteFile(path, []byte("FOO=bar\nEMPTY=\n"), 0o600); err != nil {
		t.Fatalf("failed to write dotenv file: %v", err)
	}

	values, err := loadDotenvValues(path)
	if err != nil {
		t.Fatalf("unexpected error: %v", err)
	}
	if values["FOO"] != "bar" {
		t.Fatalf("unexpected FOO value: %q", values["FOO"])
	}
	if values["EMPTY"] != "" {
		t.Fatalf("unexpected EMPTY value: %q", values["EMPTY"])
	}
}

func TestLoadDotenvValuesMissingFile(t *testing.T) {
	t.Parallel()

	_, err := loadDotenvValues("/no/such/path/.env")
	if err == nil {
		t.Fatal("expected error")
	}
	if !strings.Contains(err.Error(), "load dotenv file") {
		t.Fatalf("unexpected error: %v", err)
	}
}

func TestLoadDotenvValuesEmptyPath(t *testing.T) {
	t.Parallel()

	values, err := loadDotenvValues("")
	if err != nil {
		t.Fatalf("unexpected error: %v", err)
	}
	if values != nil {
		t.Fatalf("expected nil values for empty path, got %#v", values)
	}
}

type fakeSecretService struct {
	nameSecret *modal.Secret
	nameErr    error
	mapSecret  *modal.Secret
	mapErr     error

	fromNameCalls int
	fromMapCalls  int
	lastName      string
	lastMap       map[string]string
}

func (f *fakeSecretService) FromName(_ context.Context, name string, _ *modal.SecretFromNameParams) (*modal.Secret, error) {
	f.fromNameCalls++
	f.lastName = name
	if f.nameErr != nil {
		return nil, f.nameErr
	}
	if f.nameSecret != nil {
		return f.nameSecret, nil
	}
	return &modal.Secret{SecretID: "st-name"}, nil
}

func (f *fakeSecretService) FromMap(_ context.Context, values map[string]string, _ *modal.SecretFromMapParams) (*modal.Secret, error) {
	f.fromMapCalls++
	f.lastMap = values
	if f.mapErr != nil {
		return nil, f.mapErr
	}
	if f.mapSecret != nil {
		return f.mapSecret, nil
	}
	return &modal.Secret{SecretID: "st-dotenv"}, nil
}

func (f *fakeSecretService) Delete(_ context.Context, _ string, _ *modal.SecretDeleteParams) error {
	return nil
}

func TestResolveStartSecretsNamedAndDotenv(t *testing.T) {
	t.Parallel()

	dir := t.TempDir()
	path := filepath.Join(dir, ".env")
	if err := os.WriteFile(path, []byte("TOKEN=abc\n"), 0o600); err != nil {
		t.Fatalf("failed to write dotenv file: %v", err)
	}

	service := &fakeSecretService{
		nameSecret: &modal.Secret{SecretID: "st-named"},
		mapSecret:  &modal.Secret{SecretID: "st-dotenv"},
	}

	secrets, err := resolveStartSecrets(context.Background(), service, "github", path)
	if err != nil {
		t.Fatalf("unexpected error: %v", err)
	}
	if service.fromNameCalls != 1 {
		t.Fatalf("expected one FromName call, got %d", service.fromNameCalls)
	}
	if service.fromMapCalls != 1 {
		t.Fatalf("expected one FromMap call, got %d", service.fromMapCalls)
	}
	if service.lastName != "github" {
		t.Fatalf("unexpected secret name: %q", service.lastName)
	}
	if service.lastMap["TOKEN"] != "abc" {
		t.Fatalf("unexpected dotenv token value: %q", service.lastMap["TOKEN"])
	}
	if len(secrets) != 2 {
		t.Fatalf("expected two secrets, got %d", len(secrets))
	}
}

func TestResolveStartSecretsSkipsEmptyDotenv(t *testing.T) {
	t.Parallel()

	dir := t.TempDir()
	path := filepath.Join(dir, ".env")
	if err := os.WriteFile(path, []byte(""), 0o600); err != nil {
		t.Fatalf("failed to write dotenv file: %v", err)
	}

	service := &fakeSecretService{}
	secrets, err := resolveStartSecrets(context.Background(), service, "", path)
	if err != nil {
		t.Fatalf("unexpected error: %v", err)
	}
	if service.fromMapCalls != 0 {
		t.Fatalf("expected zero FromMap calls, got %d", service.fromMapCalls)
	}
	if len(secrets) != 0 {
		t.Fatalf("expected no secrets, got %d", len(secrets))
	}
}

func TestResolveStartSecretsFromNameError(t *testing.T) {
	t.Parallel()

	service := &fakeSecretService{nameErr: errors.New("not found")}
	_, err := resolveStartSecrets(context.Background(), service, "github", "")
	if err == nil {
		t.Fatal("expected error")
	}
	if !strings.Contains(err.Error(), "resolve secret") {
		t.Fatalf("unexpected error: %v", err)
	}
}

func TestResolveStartSecretsFromMapError(t *testing.T) {
	t.Parallel()

	dir := t.TempDir()
	path := filepath.Join(dir, ".env")
	if err := os.WriteFile(path, []byte("TOKEN=abc\n"), 0o600); err != nil {
		t.Fatalf("failed to write dotenv file: %v", err)
	}

	service := &fakeSecretService{mapErr: errors.New("boom")}
	_, err := resolveStartSecrets(context.Background(), service, "", path)
	if err == nil {
		t.Fatal("expected error")
	}
	if !strings.Contains(err.Error(), "create dotenv secret") {
		t.Fatalf("unexpected error: %v", err)
	}
}
