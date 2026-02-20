package cli

import (
	"context"

	"github.com/spf13/cobra"
)

type stopResponse struct {
	Status    string `json:"status"`
	SandboxID string `json:"sandbox_id"`
}

func newStopCommand(opts *rootOptions) *cobra.Command {
	return &cobra.Command{
		Use:   "stop <sandbox-id>",
		Short: "Stop a specific sandbox by ID",
		Args:  cobra.ExactArgs(1),
		RunE: func(cmd *cobra.Command, args []string) error {
			sandboxID := args[0]
			return runWithManager(cmd.Context(), opts, func(ctx context.Context, manager managerAPI) error {
				if err := manager.Stop(ctx, sandboxID); err != nil {
					return failf("stop failed: %w", err)
				}

				if opts.jsonOutput {
					return writeJSON(cmd.OutOrStdout(), stopResponse{
						Status:    "stopped",
						SandboxID: sandboxID,
					})
				}

				return writeMarkdown(cmd.OutOrStdout(), "## Sauron Stopped\n\n- Sandbox ID: `"+sandboxID+"`\n")
			})
		},
	}
}
