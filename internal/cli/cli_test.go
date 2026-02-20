package cli

import (
	"bytes"
	"context"
	"errors"
	"strings"
	"testing"
	"time"

	"github.com/mish/sauron/internal/sauron"
)

type fakeManager struct {
	startResult *sauron.StartResult
	startErr    error
	startCtx    context.Context

	stopErr error
	stopCtx context.Context
	stopID  string

	health    *sauron.HealthStatus
	healthErr error
	healthCtx context.Context
}

func (f *fakeManager) Start(ctx context.Context) (*sauron.StartResult, error) {
	f.startCtx = ctx
	if f.startErr != nil {
		return nil, f.startErr
	}
	if f.startResult == nil {
		return &sauron.StartResult{}, nil
	}
	res := *f.startResult
	return &res, nil
}

func (f *fakeManager) Stop(ctx context.Context, sandboxID string) error {
	f.stopCtx = ctx
	f.stopID = sandboxID
	return f.stopErr
}

func (f *fakeManager) Health(ctx context.Context) (*sauron.HealthStatus, error) {
	f.healthCtx = ctx
	if f.healthErr != nil {
		return nil, f.healthErr
	}
	if f.health == nil {
		return &sauron.HealthStatus{}, nil
	}
	res := *f.health
	return &res, nil
}

func TestStartCommandJSONUsesCommandContext(t *testing.T) {
	origFactory := managerFactory
	t.Cleanup(func() { managerFactory = origFactory })

	fake := &fakeManager{
		startResult: &sauron.StartResult{
			SandboxID: "sb-ctx",
			BrowseURL: "https://example.test",
			Token:     "token-ctx",
		},
	}
	managerFactory = func(_ *rootOptions) managerAPI { return fake }

	opts := &rootOptions{jsonOutput: true}
	cmd := newStartCommand(opts)

	var out bytes.Buffer
	cmd.SetOut(&out)
	ctx := context.WithValue(context.Background(), "ctx-key", "ctx-value")
	cmd.SetContext(ctx)

	if err := cmd.RunE(cmd, nil); err != nil {
		t.Fatalf("RunE returned error: %v", err)
	}
	if fake.startCtx != ctx {
		t.Fatalf("expected manager to receive command context")
	}

	got := out.String()
	if !strings.Contains(got, `"sandbox_id": "sb-ctx"`) {
		t.Fatalf("missing sandbox_id in output: %s", got)
	}
	if !strings.Contains(got, `"browse_url": "https://example.test"`) {
		t.Fatalf("missing browse_url in output: %s", got)
	}
	if !strings.Contains(got, `"token": "token-ctx"`) {
		t.Fatalf("missing token in output: %s", got)
	}
}

func TestStopCommandJSONOutput(t *testing.T) {
	origFactory := managerFactory
	t.Cleanup(func() { managerFactory = origFactory })

	fake := &fakeManager{}
	managerFactory = func(_ *rootOptions) managerAPI { return fake }

	opts := &rootOptions{jsonOutput: true}
	cmd := newStopCommand(opts)

	var out bytes.Buffer
	cmd.SetOut(&out)
	if err := cmd.RunE(cmd, []string{"sb-stop"}); err != nil {
		t.Fatalf("RunE returned error: %v", err)
	}
	if fake.stopID != "sb-stop" {
		t.Fatalf("expected stop to target sandbox id, got %q", fake.stopID)
	}

	if !strings.Contains(out.String(), `"status": "stopped"`) {
		t.Fatalf("unexpected stop output: %s", out.String())
	}
	if !strings.Contains(out.String(), `"sandbox_id": "sb-stop"`) {
		t.Fatalf("missing sandbox_id in output: %s", out.String())
	}
}

func TestHealthCommandJSONOutput(t *testing.T) {
	origFactory := managerFactory
	t.Cleanup(func() { managerFactory = origFactory })

	fake := &fakeManager{
		health: &sauron.HealthStatus{
			Running:          true,
			SandboxID:        "sb-health",
			Remaining:        42 * time.Second,
			RemainingSeconds: 42,
			ExpiresAt:        time.Date(2026, 2, 20, 17, 0, 0, 0, time.UTC),
		},
	}
	managerFactory = func(_ *rootOptions) managerAPI { return fake }

	opts := &rootOptions{jsonOutput: true}
	cmd := newHealthCommand(opts)

	var out bytes.Buffer
	cmd.SetOut(&out)
	if err := cmd.RunE(cmd, nil); err != nil {
		t.Fatalf("RunE returned error: %v", err)
	}

	got := out.String()
	if !strings.Contains(got, `"running": true`) {
		t.Fatalf("missing running flag in output: %s", got)
	}
	if !strings.Contains(got, `"remaining_seconds": 42`) {
		t.Fatalf("missing remaining counter in output: %s", got)
	}
}

type fakeRenderer struct {
	renderOutput string
	renderErr    error
}

func (r *fakeRenderer) Render(_ string) (string, error) {
	if r.renderErr != nil {
		return "", r.renderErr
	}
	return r.renderOutput, nil
}

func TestWriteMarkdownFallbackWhenRendererCreationFails(t *testing.T) {
	origRendererFactory := rendererFactory
	t.Cleanup(func() { rendererFactory = origRendererFactory })

	rendererFactory = func() (markdownRenderer, error) {
		return nil, errors.New("renderer failed")
	}

	var out bytes.Buffer
	if err := writeMarkdown(&out, "## Fallback"); err != nil {
		t.Fatalf("writeMarkdown returned error: %v", err)
	}
	if !strings.Contains(out.String(), "## Fallback") {
		t.Fatalf("expected raw markdown fallback, got: %q", out.String())
	}
}

func TestWriteMarkdownFallbackWhenRenderFails(t *testing.T) {
	origRendererFactory := rendererFactory
	t.Cleanup(func() { rendererFactory = origRendererFactory })

	rendererFactory = func() (markdownRenderer, error) {
		return &fakeRenderer{renderErr: errors.New("render failed")}, nil
	}

	var out bytes.Buffer
	if err := writeMarkdown(&out, "## Fallback Render"); err != nil {
		t.Fatalf("writeMarkdown returned error: %v", err)
	}
	if !strings.Contains(out.String(), "## Fallback Render") {
		t.Fatalf("expected raw markdown fallback, got: %q", out.String())
	}
}

func TestResolvedImageIDFallsBackToEnv(t *testing.T) {
	t.Setenv("SAURON_IMAGE_ID", "img-from-env")
	opts := &rootOptions{imageID: ""}

	got := resolvedImageID(opts)
	if got != "img-from-env" {
		t.Fatalf("expected env image id, got %q", got)
	}
}

func TestResolvedImageIDPrefersCLIArg(t *testing.T) {
	t.Setenv("SAURON_IMAGE_ID", "img-from-env")
	opts := &rootOptions{imageID: "img-from-flag"}

	got := resolvedImageID(opts)
	if got != "img-from-flag" {
		t.Fatalf("expected CLI image id, got %q", got)
	}
}

func TestImageIDFlagScopedToStartCommandOnly(t *testing.T) {
	rootCmd := newRootCommand()
	startCmd, _, err := rootCmd.Find([]string{"start"})
	if err != nil {
		t.Fatalf("failed to find start command: %v", err)
	}
	stopCmd, _, err := rootCmd.Find([]string{"stop"})
	if err != nil {
		t.Fatalf("failed to find stop command: %v", err)
	}

	if startCmd.Flags().Lookup("image-id") == nil {
		t.Fatal("start command should expose --image-id")
	}
	if rootCmd.PersistentFlags().Lookup("image-id") != nil {
		t.Fatal("root command should not expose --image-id as persistent flag")
	}
	if stopCmd.Flags().Lookup("image-id") != nil {
		t.Fatal("stop command should not expose --image-id")
	}
}

func TestStopCommandRequiresSandboxID(t *testing.T) {
	cmd := newStopCommand(&rootOptions{})
	if err := cmd.Args(cmd, []string{}); err == nil {
		t.Fatal("expected argument validation error when sandbox ID is missing")
	}
}
