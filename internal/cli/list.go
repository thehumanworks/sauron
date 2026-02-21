package cli

import (
	"context"
	"fmt"
	"strings"

	"github.com/spf13/cobra"

	"github.com/mish/sauron/internal/sauron"
)

type listResponse struct {
	AppName   string                  `json:"app_name"`
	Sandboxes []sauron.SandboxSummary `json:"sandboxes"`
}

func newListCommand(opts *rootOptions) *cobra.Command {
	return &cobra.Command{
		Use:     "list",
		Aliases: []string{"ls"},
		Short:   "List running sandboxes in the configured Modal app",
		RunE: func(cmd *cobra.Command, _ []string) error {
			return runWithManager(cmd.Context(), opts, func(ctx context.Context, manager managerAPI) error {
				sandboxes, err := manager.List(ctx)
				if err != nil {
					return failf("list failed: %w", err)
				}

				if opts.jsonOutput {
					return writeJSON(cmd.OutOrStdout(), listResponse{
						AppName:   opts.appName,
						Sandboxes: sandboxes,
					})
				}

				if len(sandboxes) == 0 {
					return writeMarkdown(
						cmd.OutOrStdout(),
						fmt.Sprintf(
							"## Sauron Sandboxes\n\nNo running sandboxes found in Modal app `%s`.\n",
							opts.appName,
						),
					)
				}

				var markdown strings.Builder
				fmt.Fprintf(&markdown, "## Sauron Sandboxes\n\n- App: `%s`\n- Running sandboxes: `%d`\n", opts.appName, len(sandboxes))
				for _, sandbox := range sandboxes {
					fmt.Fprintf(&markdown, "- Sandbox ID: `%s`\n", sandbox.SandboxID)
				}
				return writeMarkdown(cmd.OutOrStdout(), markdown.String())
			})
		},
	}
}
