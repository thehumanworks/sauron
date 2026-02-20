package cli

import (
	"encoding/json"
	"fmt"
	"io"

	"github.com/charmbracelet/glamour"
)

type markdownRenderer interface {
	Render(input string) (string, error)
}

var rendererFactory = func() (markdownRenderer, error) {
	return glamour.NewTermRenderer(
		glamour.WithAutoStyle(),
		glamour.WithWordWrap(100),
	)
}

func writeJSON(w io.Writer, value any) error {
	encoder := json.NewEncoder(w)
	encoder.SetIndent("", "  ")
	return encoder.Encode(value)
}

func writeMarkdown(w io.Writer, markdown string) error {
	renderer, err := rendererFactory()
	if err != nil {
		_, writeErr := fmt.Fprintln(w, markdown)
		return writeErr
	}

	out, err := renderer.Render(markdown)
	if err != nil {
		_, writeErr := fmt.Fprintln(w, markdown)
		return writeErr
	}

	_, err = fmt.Fprint(w, out)
	return err
}
