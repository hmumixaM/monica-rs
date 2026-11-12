package main

import (
	"context"
	"os"
	"path/filepath"
	"strings"
	"testing"
	"time"
)

func TestControlSeparatesPauseAndDisconnect(t *testing.T) {
	root := t.TempDir()
	if err := os.WriteFile(filepath.Join(root, "control.json"), []byte(`{"enabled":true,"paused":true}`), 0600); err != nil {
		t.Fatal(err)
	}
	ctx, cancel := context.WithTimeout(context.Background(), 3*time.Second)
	defer cancel()
	var events [][2]bool
	watchControl(ctx, root, func(enabled, paused bool) bool {
		events = append(events, [2]bool{enabled, paused})
		if len(events) == 1 {
			if err := writeJSON(filepath.Join(root, "control.json"), map[string]bool{"enabled": false, "paused": false}); err != nil {
				t.Fatal(err)
			}
			return true
		}
		return false
	})
	if len(events) != 2 || events[0] != [2]bool{true, true} || events[1] != [2]bool{false, false} {
		t.Fatalf("wrong control transitions: %v", events)
	}
}

func TestDisabledReaderWaitsOfflineAndKeepsEvidence(t *testing.T) {
	root := t.TempDir()
	if err := writeJSON(filepath.Join(root, "control.json"), map[string]bool{"enabled": false, "paused": false}); err != nil {
		t.Fatal(err)
	}
	if err := os.WriteFile(filepath.Join(root, "synthetic-evidence.json"), []byte(`{"body":"preserved"}`), 0600); err != nil {
		t.Fatal(err)
	}
	ctx, cancel := context.WithTimeout(context.Background(), time.Second)
	defer cancel()
	if err := run(ctx, root); err != nil {
		t.Fatal(err)
	}
	if _, err := os.Stat(filepath.Join(root, "synthetic-evidence.json")); err != nil {
		t.Fatal(err)
	}
	status, err := os.ReadFile(filepath.Join(root, "status.json"))
	if err != nil {
		t.Fatal(err)
	}
	if !strings.Contains(string(status), `"state":"disconnected"`) {
		t.Fatalf("not disconnected: %s", status)
	}
}

func TestJournalAccountIsDurableAndCannotChange(t *testing.T) {
	root := t.TempDir()
	if err := os.MkdirAll(filepath.Join(root, "pending"), 0700); err != nil {
		t.Fatal(err)
	}
	j := &journal{root: root, account: func() string { return "synthetic@s.whatsapp.net" }}
	j.append([]map[string]any{{"external_id": "one", "body": "synthetic"}})
	account, err := os.ReadFile(filepath.Join(root, "account.json"))
	if err != nil {
		t.Fatal(err)
	}
	if string(account) != "synthetic@s.whatsapp.net" {
		t.Fatalf("account format changed: %s", account)
	}
	j.account = func() string { return "different@s.whatsapp.net" }
	defer func() {
		if recover() == nil {
			t.Fatal("account switch must be rejected")
		}
		files, err := filepath.Glob(filepath.Join(root, "pending", "*.json"))
		if err != nil || len(files) != 1 {
			t.Fatalf("unexpected publication after account switch: %v %v", files, err)
		}
	}()
	j.append([]map[string]any{{"external_id": "two", "body": "wrong account"}})
}
