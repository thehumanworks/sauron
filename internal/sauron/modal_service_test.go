package sauron

import (
	"bytes"
	"strings"
	"testing"
)

func TestModalServiceLogfWritesWhenVerbose(t *testing.T) {
	t.Parallel()

	var out bytes.Buffer
	service := NewModalService(ModalServiceOptions{
		Verbose: true,
		Stdout:  &out,
	})

	service.logf("building image %s", "img-123")
	got := out.String()
	if !strings.Contains(got, "[sauron] building image img-123") {
		t.Fatalf("expected verbose log line, got %q", got)
	}
}

func TestModalServiceLogfSkipsWhenNotVerbose(t *testing.T) {
	t.Parallel()

	var out bytes.Buffer
	service := NewModalService(ModalServiceOptions{
		Verbose: false,
		Stdout:  &out,
	})

	service.logf("should not render")
	if out.Len() != 0 {
		t.Fatalf("expected no log output, got %q", out.String())
	}
}
