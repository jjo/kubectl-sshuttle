package cmd

import "github.com/spf13/cobra"

var statusCmd = &cobra.Command{
	Use:   "status",
	Short: "Show proxy pod status",
	RunE: func(cmd *cobra.Command, args []string) error {
		return runKubectl("get", "pods", "-l", "app="+effectiveName(), "-o", "wide")
	},
}

func init() {
	rootCmd.AddCommand(statusCmd)
}
