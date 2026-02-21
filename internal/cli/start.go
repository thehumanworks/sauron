package cli

import (
	"context"
	"fmt"

	"github.com/spf13/cobra"
)

const cdpTunnelPort = 9222

func newStartCommand(opts *rootOptions) *cobra.Command {
	cmd := &cobra.Command{
		Use:   "start",
		Short: "Start a Chromium sandbox and print CDP credentials",
		RunE: func(cmd *cobra.Command, _ []string) error {
			if opts.repoURL == "" && opts.repoRef != "" {
				return failf("--ref requires --repo")
			}
			if opts.devPort < 0 {
				return failf("--dev-port must be >= 0")
			}
			if opts.devPort == cdpTunnelPort {
				return failf("--dev-port %d is reserved for CDP tunnel", cdpTunnelPort)
			}
			return runWithManager(cmd.Context(), opts, func(ctx context.Context, manager managerAPI) error {
				result, err := manager.Start(ctx)
				if err != nil {
					return failf("start failed: %w", err)
				}

				if opts.jsonOutput {
					return writeJSON(cmd.OutOrStdout(), result)
				}

				markdown := fmt.Sprintf(
					"## Sauron Started\n\n- Sandbox ID: `%s`\n- CDP URL: `%s`\n",
					result.SandboxID,
					result.BrowseURL,
				)
				if result.Token != "" {
					markdown += fmt.Sprintf("- CDP Token: `%s`\n", result.Token)
				}
				if result.BrowserWSURL != "" {
					markdown += fmt.Sprintf("- Browser WS URL: `%s`\n", result.BrowserWSURL)
				}
				if result.DevServerURL != "" {
					markdown += fmt.Sprintf("- Dev Server URL: `%s`\n", result.DevServerURL)
				}
				if result.RepoPath != "" {
					markdown += fmt.Sprintf("- Repo Path: `%s`\n", result.RepoPath)
				}
				return writeMarkdown(cmd.OutOrStdout(), markdown)
			})
		},
	}

	cmd.Flags().StringVar(&opts.imageID, "image-id", opts.imageID, "Existing Modal image ID to use instead of building one")
	cmd.Flags().StringVar(&opts.runtime, "runtime", opts.runtime, "Base runtime image to use (node20, node22, python312)")
	cmd.Flags().StringVar(&opts.fromDotenv, "from-dotenv", opts.fromDotenv, "Load env vars from a dotenv file and inject them as an ephemeral secret (defaults to .env when flag has no value)")
	cmd.Flags().StringVar(&opts.secretName, "secret", opts.secretName, "Modal secret name to inject as env vars (set empty to disable)")
	cmd.Flags().StringVar(&opts.repoURL, "repo", opts.repoURL, "Repository URL to clone before exposing CDP")
	cmd.Flags().StringVar(&opts.repoRef, "ref", opts.repoRef, "Git ref (branch/tag/SHA) to checkout after clone")
	cmd.Flags().StringVar(&opts.repoDir, "repo-dir", opts.repoDir, "Directory in sandbox where the repo is cloned")
	cmd.Flags().StringVar(&opts.devCommand, "dev-cmd", opts.devCommand, "Command to start a dev server in background")
	cmd.Flags().IntVar(&opts.devPort, "dev-port", opts.devPort, "Dev server port to expose as a public tunnel")
	if flag := cmd.Flags().Lookup("from-dotenv"); flag != nil {
		flag.NoOptDefVal = ".env"
	}
	return cmd
}
