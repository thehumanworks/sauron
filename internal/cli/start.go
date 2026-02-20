package cli

import (
	"context"
	"fmt"

	"github.com/spf13/cobra"
)

func newStartCommand(opts *rootOptions) *cobra.Command {
	cmd := &cobra.Command{
		Use:   "start",
		Short: "Start a Chromium sandbox and print CDP credentials",
		RunE: func(cmd *cobra.Command, _ []string) error {
			return runWithManager(cmd.Context(), opts, func(ctx context.Context, manager managerAPI) error {
				result, err := manager.Start(ctx)
				if err != nil {
					return failf("start failed: %w", err)
				}

				if opts.jsonOutput {
					return writeJSON(cmd.OutOrStdout(), result)
				}

				return writeMarkdown(cmd.OutOrStdout(), fmt.Sprintf(
					"## Sauron Started\n\n- Sandbox ID: `%s`\n- CDP URL: `%s`\n- CDP Token: `%s`\n",
					result.SandboxID,
					result.BrowseURL,
					result.Token,
				))
			})
		},
	}

	cmd.Flags().StringVar(&opts.imageID, "image-id", "", "Existing Modal image ID to use instead of building one")
	return cmd
}
