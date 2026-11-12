// Monica linked-device reader. No message sending commands are exposed.
package main

import (
	"context"
	"encoding/base64"
	"encoding/json"
	"errors"
	"flag"
	"fmt"
	_ "github.com/mattn/go-sqlite3"
	"github.com/skip2/go-qrcode"
	"go.mau.fi/whatsmeow"
	"go.mau.fi/whatsmeow/store/sqlstore"
	"go.mau.fi/whatsmeow/types"
	"go.mau.fi/whatsmeow/types/events"
	waLog "go.mau.fi/whatsmeow/util/log"
	"os"
	"os/signal"
	"path/filepath"
	"sync"
	"sync/atomic"
	"syscall"
	"time"
)

type journal struct {
	root     string
	mu       sync.Mutex
	sequence uint64
	account  func() string
}

func writeJSON(path string, value any) error {
	data, err := json.Marshal(value)
	if err != nil {
		return err
	}
	return writeBytes(path, data)
}
func writeBytes(path string, data []byte) error {
	file, err := os.OpenFile(path+".tmp", os.O_WRONLY|os.O_CREATE|os.O_TRUNC, 0600)
	if err != nil {
		return err
	}
	if _, err = file.Write(data); err == nil {
		err = file.Sync()
	}
	closeErr := file.Close()
	if err != nil {
		return err
	}
	if closeErr != nil {
		return closeErr
	}
	if err := os.Rename(path+".tmp", path); err != nil {
		return err
	}
	return syncDirectory(filepath.Dir(path))
}
func syncDirectory(path string) error {
	directory, err := os.Open(path)
	if err != nil {
		return err
	}
	defer directory.Close()
	return directory.Sync()
}
func (j *journal) status(state string, extra map[string]any) {
	j.mu.Lock()
	defer j.mu.Unlock()
	if extra == nil {
		extra = map[string]any{}
	}
	extra["state"] = state
	extra["updated_at"] = time.Now().UTC().Format(time.RFC3339)
	if err := writeJSON(filepath.Join(j.root, "status.json"), extra); err != nil {
		panic(err)
	}
}
func (j *journal) append(records []map[string]any) {
	if len(records) == 0 {
		return
	}
	j.mu.Lock()
	defer j.mu.Unlock()
	j.sequence++
	if j.account != nil {
		account := j.account()
		path := filepath.Join(j.root, "account.json")
		previous, err := os.ReadFile(path)
		if err == nil && string(previous) != account {
			panic("Linked device account changed: use a new connection directory")
		}
		if os.IsNotExist(err) {
			if err = writeBytes(path, []byte(account)); err != nil {
				panic(err)
			}
		} else if err != nil {
			panic(err)
		}
	}
	if err := j.queueMedia(records); err != nil {
		panic(err)
	}
	path := filepath.Join(j.root, "pending", fmt.Sprintf("%020d-%06d.json", time.Now().UnixNano(), j.sequence))
	if err := writeJSON(path, map[string]any{"records": records}); err != nil {
		panic(err)
	}
}
func participant(jid types.JID, role string, own bool) map[string]any {
	phones := []string{}
	if jid.Server == types.DefaultUserServer {
		phones = append(phones, "+"+jid.User)
	}
	return map[string]any{"id": jid.ToNonAD().String(), "role": role, "is_self": own, "phones": phones}
}
func record(event *events.Message) map[string]any {
	msg := event.Message
	info := event.Info
	body := msg.GetConversation()
	if body == "" {
		body = msg.GetExtendedTextMessage().GetText()
	}
	if body == "" {
		body = msg.GetImageMessage().GetCaption()
	}
	if body == "" {
		body = msg.GetVideoMessage().GetCaption()
	}
	if body == "" {
		body = msg.GetDocumentMessage().GetCaption()
	}
	sender := info.Sender
	if info.SenderAlt.Server == types.DefaultUserServer {
		sender = info.SenderAlt
	}
	people := []map[string]any{participant(sender, "sender", info.IsFromMe)}
	// A group marker prevents mistaking its only known sender for a direct conversation.
	if info.IsGroup {
		people = append(people, map[string]any{"id": info.Chat.String(), "role": "group"})
	} else if info.IsFromMe {
		recipient := info.Chat
		if info.RecipientAlt.Server == types.DefaultUserServer {
			recipient = info.RecipientAlt
		}
		people = append(people, participant(recipient, "recipient", false))
	}
	direction := "received"
	if info.IsFromMe {
		direction = "sent"
	}
	id := info.ID
	deleted := false
	if protocol := msg.GetProtocolMessage(); protocol != nil && protocol.GetType().String() == "REVOKE" {
		id = protocol.GetKey().GetID()
		deleted = true
	}
	attachments := []map[string]any{}
	if media := msg.GetStickerMessage(); media != nil {
		attachments = append(attachments, map[string]any{"kind": "sticker", "content_type": media.GetMimetype(), "size": media.GetFileLength()})
	}
	if media := msg.GetImageMessage(); media != nil {
		attachments = append(attachments, map[string]any{"kind": "image", "content_type": media.GetMimetype(), "size": media.GetFileLength()})
	}
	if media := msg.GetVideoMessage(); media != nil {
		attachments = append(attachments, map[string]any{"kind": "video", "content_type": media.GetMimetype(), "size": media.GetFileLength()})
	}
	if media := msg.GetAudioMessage(); media != nil {
		attachments = append(attachments, map[string]any{"kind": "audio", "content_type": media.GetMimetype(), "size": media.GetFileLength()})
	}
	if media := msg.GetDocumentMessage(); media != nil {
		attachments = append(attachments, map[string]any{"kind": "document", "filename": media.GetFileName(), "content_type": media.GetMimetype(), "size": media.GetFileLength()})
	}
	return map[string]any{"external_id": info.Chat.String() + ":" + id, "thread_id": info.Chat.String(), "kind": "message", "body": body, "participants": people, "direction": direction, "occurred_at": info.Timestamp.UTC().Format(time.RFC3339), "included": true, "deleted": deleted, "attachments": attachments, "is_group": info.IsGroup, "edited": event.IsEdit, "raw_message": msg}
}
func clearDevice(ctx context.Context, client *whatsmeow.Client) error {
	if client.Store.ID == nil {
		return nil
	}
	return client.Store.Delete(ctx)
}

var errReconfigure = errors.New("bridge control changed; reopen local session")

func run(parent context.Context, root string) error {
	if err := os.MkdirAll(filepath.Join(root, "pending"), 0700); err != nil {
		return err
	}
	if err := os.Chmod(root, 0700); err != nil {
		return err
	}
	ctx, cancel := context.WithCancel(parent)
	defer cancel()
	j := &journal{root: root}
	j.status("connecting", nil)
	container, err := sqlstore.New(ctx, "sqlite3", "file:"+filepath.Join(root, "device.sqlite")+"?_foreign_keys=on&_journal_mode=WAL", waLog.Noop)
	if err != nil {
		return err
	}
	defer container.Close()
	device, err := container.GetFirstDevice(ctx)
	if err != nil {
		return err
	}
	client := whatsmeow.NewClient(device, waLog.Noop)
	unfinished, err := filepath.Glob(filepath.Join(root, "media-queue", "*.working"))
	if err != nil {
		return err
	}
	for _, path := range unfinished {
		if err := os.Rename(path, path[:len(path)-len(".working")]); err != nil {
			return err
		}
	}
	var mediaWorkers sync.WaitGroup
	defer mediaWorkers.Wait()
	defer cancel()
	for i := 0; i < 2; i++ {
		mediaWorkers.Add(1)
		go func() { defer mediaWorkers.Done(); j.mediaLoop(ctx, client) }()
	}

	j.account = func() string {
		if client.Store.ID == nil {
			panic("Missing linked account")
		}
		return client.Store.ID.ToNonAD().String()
	}
	client.AddEventHandler(func(raw any) {
		switch event := raw.(type) {
		case *events.Message:
			j.append([]map[string]any{record(event)})
		case *events.HistorySync:
			for _, conversation := range event.Data.GetConversations() {
				chat, err := types.ParseJID(conversation.GetID())
				if err != nil {
					j.status("error", map[string]any{"error": "invalid_history_chat"})
					continue
				}
				batch := []map[string]any{}
				for _, item := range conversation.GetMessages() {
					parsed, err := client.ParseWebMessage(chat, item.GetMessage())
					if err != nil {
						j.status("error", map[string]any{"error": "history_message_decode"})
						continue
					}
					batch = append(batch, record(parsed))
					if len(batch) == 100 {
						j.append(batch)
						batch = nil
					}
				}
				j.append(batch)
			}
		case *events.Connected:
			j.status("connected", map[string]any{"coverage": "关联设备实际收到的消息与历史"})
		case *events.LoggedOut:
			j.status("needs_login", nil)
		case *events.Disconnected:
			j.status("connecting", nil)
		}
	})
	// An external container can be started while disconnected. Never connect
	// until the explicit connection control enables it.
	for {
		control, controlErr := readControl(root)
		if controlErr != nil || control.Enabled == nil {
			select {
			case <-ctx.Done():
				return nil
			case <-time.After(time.Second):
			}
			continue
		}
		if !*control.Enabled {
			if err := clearDevice(ctx, client); err != nil {
				return err
			}
			j.status("disconnected", nil)
			for {
				select {
				case <-ctx.Done():
					return nil
				case <-time.After(time.Second):
				}
				next, err := readControl(root)
				if err == nil && next.Enabled != nil && *next.Enabled {
					return errReconfigure
				}
			}
		}
		if !control.Paused {
			break
		}
		select {
		case <-ctx.Done():
			return nil
		case <-time.After(time.Second):
		}
	}
	if client.Store.ID == nil {
		codes, err := client.GetQRChannel(ctx)
		if err != nil {
			return err
		}
		go func() {
			for item := range codes {
				if item.Event == "code" {
					png, err := qrcode.Encode(item.Code, qrcode.Medium, 320)
					if err != nil {
						j.status("error", map[string]any{"error": "qr_encoding"})
						continue
					}
					j.status("qr", map[string]any{"qr_data": "data:image/png;base64," + base64.StdEncoding.EncodeToString(png)})
				} else if item.Event != "success" {
					j.status(item.Event, nil)
				}
			}
		}()
	}
	if err := client.ConnectContext(ctx); err != nil {
		return err
	}
	defer client.Disconnect()
	var disconnected atomic.Bool
	go watchControl(ctx, root, func(enabled, paused bool) bool {
		if !enabled {
			client.Disconnect()
			if err := clearDevice(ctx, client); err != nil {
				j.status("error", map[string]any{"error": "session_clear"})
			} else {
				j.status("disconnected", nil)
			}
			disconnected.Store(true)
			cancel()
			return false
		}
		if paused {
			client.Disconnect()
			j.status("paused", nil)
		} else if !client.IsConnected() {
			if err := client.ConnectContext(ctx); err != nil {
				j.status("error", map[string]any{"error": "reconnect"})
			}
		}
		return true
	})
	<-ctx.Done()
	if disconnected.Load() && parent.Err() == nil {
		return errReconfigure
	}
	return nil
}
func main() {
	root := flag.String("root", "", "private connection directory")
	flag.Parse()
	if *root == "" {
		fmt.Fprintln(os.Stderr, "--root is required")
		os.Exit(2)
	}
	ctx, cancel := signal.NotifyContext(context.Background(), os.Interrupt, syscall.SIGTERM)
	defer cancel()
	for ctx.Err() == nil {
		err := run(ctx, *root)
		if errors.Is(err, errReconfigure) {
			continue
		}
		if err != nil {
			fmt.Fprintln(os.Stderr, "WhatsApp bridge failed")
			os.Exit(1)
		}
		return
	}
}
