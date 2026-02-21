package cli

import (
	"context"
	"fmt"
	"os"
	"path/filepath"
	"time"

	"github.com/spf13/cobra"

	"github.com/mish/sauron/internal/sauron"
)

type managerAPI interface {
	Start(ctx context.Context) (*sauron.StartResult, error)
	Stop(ctx context.Context, sandboxID string) error
	Health(ctx context.Context) (*sauron.HealthStatus, error)
}

var managerFactory = func(opts *rootOptions) managerAPI {
	return newManager(opts)
}

type rootOptions struct {
	stateFile   string
	appName     string
	imageID     string
	runtime     string
	fromDotenv  string
	secretName  string
	repoURL     string
	repoRef     string
	repoDir     string
	devCommand  string
	devPort     int
	timeout     time.Duration
	idleTimeout time.Duration
	verbose     bool
	jsonOutput  bool
}

// Execute runs the sauron CLI.
func Execute() error {
	return newRootCommand().Execute()
}

func newRootCommand() *cobra.Command {
	opts := rootOptions{
		stateFile:   defaultStateFile(),
		appName:     "sauron",
		runtime:     "node20",
		secretName:  "",
		repoDir:     "/workspace/repo",
		timeout:     1 * time.Hour,
		idleTimeout: 5 * time.Minute,
	}

	rootCmd := &cobra.Command{
		Use:   "sauron",
		Short: "Manage a Modal-hosted Chromium CDP sandbox",
	}

	rootCmd.PersistentFlags().StringVar(&opts.stateFile, "state-file", opts.stateFile, "Path to session state file")
	rootCmd.PersistentFlags().StringVar(&opts.appName, "app", opts.appName, "Modal app name")
	rootCmd.PersistentFlags().DurationVar(&opts.timeout, "timeout", opts.timeout, "Hard sandbox timeout")
	rootCmd.PersistentFlags().DurationVar(&opts.idleTimeout, "idle-timeout", opts.idleTimeout, "Idle sandbox timeout")
	rootCmd.PersistentFlags().BoolVar(&opts.verbose, "verbose", false, "Enable verbose Modal lifecycle logging")
	rootCmd.PersistentFlags().BoolVar(&opts.jsonOutput, "json", false, "Output command response as JSON")

	rootCmd.AddCommand(
		newStartCommand(&opts),
		newStopCommand(&opts),
		newHealthCommand(&opts),
	)

	return rootCmd
}

func newManager(opts *rootOptions) *sauron.Manager {
	store := sauron.NewFileStateStore(opts.stateFile)
	service := sauron.NewModalService(sauron.ModalServiceOptions{
		Verbose: opts.verbose,
		Stdout:  os.Stdout,
	})
	return sauron.NewManager(service, store, sauron.Options{
		AppName:     opts.appName,
		ImageID:     resolvedImageID(opts),
		Runtime:     opts.runtime,
		FromDotenv:  opts.fromDotenv,
		Timeout:     opts.timeout,
		IdleTimeout: opts.idleTimeout,
		SecretName:  opts.secretName,
		RepoURL:     opts.repoURL,
		RepoRef:     opts.repoRef,
		RepoDir:     opts.repoDir,
		DevCommand:  opts.devCommand,
		DevPort:     opts.devPort,
	})
}

func resolvedImageID(opts *rootOptions) string {
	if opts.imageID != "" {
		return opts.imageID
	}
	return os.Getenv("SAURON_IMAGE_ID")
}

func defaultStateFile() string {
	home, err := os.UserHomeDir()
	if err != nil {
		return ".sauron/session.json"
	}
	return filepath.Join(home, ".sauron", "session.json")
}

func runWithManager(ctx context.Context, opts *rootOptions, fn func(ctx context.Context, manager managerAPI) error) error {
	manager := managerFactory(opts)
	if err := fn(ctx, manager); err != nil {
		return err
	}
	return nil
}

func failf(format string, args ...any) error {
	return fmt.Errorf(format, args...)
}
