package cmd

import "github.com/spf13/cobra"

var deleteCmd = &cobra.Command{
	Use:   "delete",
	Short: "Delete the sshuttle proxy deployment",
	RunE: func(cmd *cobra.Command, args []string) error {
		return runKubectl("delete", "deploy", cfg.Name)
	},
}

func init() {
	rootCmd.AddCommand(deleteCmd)
}
