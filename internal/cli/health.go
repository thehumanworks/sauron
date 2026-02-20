package cli

import (
	"context"
	"fmt"
	"time"

	"github.com/spf13/cobra"
)

func newHealthCommand(opts *rootOptions) *cobra.Command {
	return &cobra.Command{
		Use:   "health",
		Short: "Check sandbox health and non-idle remaining time",
		RunE: func(cmd *cobra.Command, _ []string) error {
			return runWithManager(cmd.Context(), opts, func(ctx context.Context, manager managerAPI) error {
				health, err := manager.Health(ctx)
				if err != nil {
					return failf("health failed: %w", err)
				}

				if opts.jsonOutput {
					return writeJSON(cmd.OutOrStdout(), health)
				}

				if !health.Running {
					return writeMarkdown(cmd.OutOrStdout(), "## Sauron Health\n\nNo active sandbox is running.\n")
				}

				remaining := formatDuration(health.Remaining)
				return writeMarkdown(cmd.OutOrStdout(), fmt.Sprintf(
					"## Sauron Health\n\n- Status: `running`\n- Sandbox ID: `%s`\n- Non-idle time remaining: `%s` (`%d` seconds)\n- Expires at: `%s`\n",
					health.SandboxID,
					remaining,
					health.RemainingSeconds,
					health.ExpiresAt.Format(time.RFC3339),
				))
			})
		},
	}
}

func formatDuration(d time.Duration) string {
	if d < 0 {
		d = 0
	}
	return d.Round(time.Second).String()
}
