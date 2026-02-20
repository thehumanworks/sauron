package main

import (
	"fmt"
	"os"

	"github.com/mish/sauron/internal/cli"
)

func main() {
	if err := cli.Execute(); err != nil {
		_, _ = fmt.Fprintf(os.Stderr, "error: %v\n", err)
		os.Exit(1)
	}
}
