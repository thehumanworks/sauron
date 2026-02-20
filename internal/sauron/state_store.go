package sauron

import (
	"encoding/json"
	"errors"
	"os"
	"path/filepath"
)

// FileStateStore persists session state in a local JSON file.
type FileStateStore struct {
	path string
}

// NewFileStateStore creates a file-backed store at the given path.
func NewFileStateStore(path string) *FileStateStore {
	return &FileStateStore{path: path}
}

// Load retrieves current session state.
func (s *FileStateStore) Load() (*SessionState, error) {
	data, err := os.ReadFile(s.path)
	if errors.Is(err, os.ErrNotExist) {
		return nil, ErrStateNotFound
	}
	if err != nil {
		return nil, err
	}

	var state SessionState
	if err := json.Unmarshal(data, &state); err != nil {
		return nil, err
	}
	return &state, nil
}

// Save writes session state atomically.
func (s *FileStateStore) Save(state *SessionState) error {
	if err := os.MkdirAll(filepath.Dir(s.path), 0o755); err != nil {
		return err
	}

	data, err := json.MarshalIndent(state, "", "  ")
	if err != nil {
		return err
	}
	data = append(data, '\n')

	tmpPath := s.path + ".tmp"
	if err := os.WriteFile(tmpPath, data, 0o600); err != nil {
		return err
	}
	if err := os.Rename(tmpPath, s.path); err != nil {
		_ = os.Remove(tmpPath)
		return err
	}
	return nil
}

// Clear removes stored session state.
func (s *FileStateStore) Clear() error {
	err := os.Remove(s.path)
	if errors.Is(err, os.ErrNotExist) {
		return nil
	}
	return err
}
