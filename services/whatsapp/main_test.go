package main

import (
	"encoding/json"
	"go.mau.fi/whatsmeow/proto/waE2E"
	"go.mau.fi/whatsmeow/types"
	"go.mau.fi/whatsmeow/types/events"
	"google.golang.org/protobuf/proto"
	"os"
	"path/filepath"
	"testing"
	"time"
)

func TestGroupAuthorAndOwnDirection(t *testing.T) {
	event := &events.Message{Info: types.MessageInfo{MessageSource: types.MessageSource{Chat: types.NewJID("group", types.GroupServer), Sender: types.NewJID("15550000001", types.DefaultUserServer), IsGroup: true}, ID: "same-id", Timestamp: time.Unix(10, 0)}, Message: &waE2E.Message{Conversation: proto.String("hello")}}
	row := record(event)
	if row["body"] != "hello" || row["is_group"] != true {
		t.Fatal(row)
	}
	people := row["participants"].([]map[string]any)
	if len(people) != 2 || people[0]["role"] != "sender" {
		t.Fatal(people)
	}
	event.Info.IsFromMe = true
	event.Info.IsGroup = false
	event.Info.Chat = types.NewJID("15550000002", types.DefaultUserServer)
	own := record(event)
	if own["direction"] != "sent" || own["external_id"] == row["external_id"] {
		t.Fatal(own)
	}
}
func TestDurableJournalAndStatus(t *testing.T) {
	root := t.TempDir()
	if err := os.Mkdir(filepath.Join(root, "pending"), 0700); err != nil {
		t.Fatal(err)
	}
	j := &journal{root: root}
	j.append([]map[string]any{{"external_id": "one"}})
	j.append([]map[string]any{{"external_id": "two"}})
	files, err := filepath.Glob(filepath.Join(root, "pending", "*.json"))
	if err != nil || len(files) != 2 {
		t.Fatal(files, err)
	}
	for _, path := range files {
		data, err := os.ReadFile(path)
		if err != nil {
			t.Fatal(err)
		}
		var value map[string]any
		if err = json.Unmarshal(data, &value); err != nil {
			t.Fatal(err)
		}
		info, _ := os.Stat(path)
		if info.Mode().Perm() != 0600 {
			t.Fatal(info.Mode())
		}
	}
	j.status("qr", map[string]any{"qr_data": "test"})
	j.status("connected", nil)
	data, _ := os.ReadFile(filepath.Join(root, "status.json"))
	var status map[string]any
	json.Unmarshal(data, &status)
	if _, ok := status["qr_data"]; ok {
		t.Fatal("stale QR")
	}
}

func TestMediaQueueSurvivesReplayWithoutDuplicatingCompletedDownloads(t *testing.T) {
	root := t.TempDir()
	os.Mkdir(filepath.Join(root, "pending"), 0700)
	j := &journal{root: root}
	row := map[string]any{"external_id": "chat:one", "attachments": []map[string]any{{"kind": "image"}}, "raw_message": map[string]any{"imageMessage": map[string]any{}}}
	j.append([]map[string]any{row})
	j.append([]map[string]any{row})
	files, _ := filepath.Glob(filepath.Join(root, "media-queue", "*.json"))
	if len(files) != 1 {
		t.Fatalf("queue: %d", len(files))
	}
	if err := os.Rename(files[0], files[0]+".done"); err != nil {
		t.Fatal(err)
	}
	j.append([]map[string]any{row})
	files, _ = filepath.Glob(filepath.Join(root, "media-queue", "*.json"))
	if len(files) != 0 {
		t.Fatal("completed media was queued again")
	}
}
