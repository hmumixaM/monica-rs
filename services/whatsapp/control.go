package main

import (
	"context"
	"encoding/json"
	"os"
	"path/filepath"
	"time"
)

type bridgeControl struct {
	Enabled *bool `json:"enabled"`
	Paused  bool  `json:"paused"`
}

func readControl(root string) (bridgeControl, error) {
	data, err := os.ReadFile(filepath.Join(root, "control.json"))
	if err != nil {
		return bridgeControl{}, err
	}
	var control bridgeControl
	err = json.Unmarshal(data, &control)
	return control, err
}

// A disconnect erases local session keys; pausing only closes the socket. Facts,
// pending events and immutable attachments are never removed by this control.
func watchControl(ctx context.Context, root string, apply func(bool, bool) bool) {
	ticker := time.NewTicker(time.Second)
	defer ticker.Stop()
	var previous *bridgeControl
	for {
		control, err := readControl(root)
		if err == nil && control.Enabled != nil && (previous == nil || *previous.Enabled != *control.Enabled || previous.Paused != control.Paused) {
			previous = &control
			if !apply(*control.Enabled, control.Paused) {
				return
			}
		}
		select {
		case <-ctx.Done():
			return
		case <-ticker.C:
		}
	}
}
