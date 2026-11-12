package main

import (
	"context"
	"crypto/sha256"
	"encoding/json"
	"errors"
	"fmt"
	"go.mau.fi/whatsmeow"
	"go.mau.fi/whatsmeow/proto/waE2E"
	"io"
	"os"
	"path/filepath"
	"sync/atomic"
	"time"
)

func (j *journal) queueMedia(records []map[string]any) error {
	directory := filepath.Join(j.root, "media-queue")
	if err := os.MkdirAll(directory, 0700); err != nil {
		return err
	}
	for _, record := range records {
		attachments, ok := record["attachments"].([]map[string]any)
		if !ok || len(attachments) == 0 || record["media_update"] == true || record["deleted"] == true {
			continue
		}
		raw, err := json.Marshal(record)
		if err != nil {
			return err
		}
		path := filepath.Join(directory, fmt.Sprintf("%x.json", sha256.Sum256(raw)))
		if _, err := os.Stat(path + ".done"); err == nil {
			continue
		} else if !os.IsNotExist(err) {
			return err
		}
		if _, err := os.Stat(path); os.IsNotExist(err) {
			if err := writeJSON(path, record); err != nil {
				return err
			}
		} else if err != nil {
			return err
		}
	}
	return nil
}
func downloadable(msg *waE2E.Message) whatsmeow.DownloadableMessage {
	switch {
	case msg.ImageMessage != nil:
		return msg.ImageMessage
	case msg.VideoMessage != nil:
		return msg.VideoMessage
	case msg.AudioMessage != nil:
		return msg.AudioMessage
	case msg.DocumentMessage != nil:
		return msg.DocumentMessage
	case msg.StickerMessage != nil:
		return msg.StickerMessage
	default:
		return nil
	}
}

var errMediaSuspended = errors.New("media download suspended")

func downloadMedia(ctx context.Context, client *whatsmeow.Client, root string, record map[string]any) error {
	raw, err := json.Marshal(record["raw_message"])
	if err != nil {
		return err
	}
	var message waE2E.Message
	if err = json.Unmarshal(raw, &message); err != nil {
		return err
	}
	media := downloadable(&message)
	if media == nil {
		return fmt.Errorf("missing downloadable message")
	}
	directory := filepath.Join(root, "attachments")
	if err = os.MkdirAll(directory, 0700); err != nil {
		return err
	}
	file, err := os.CreateTemp(directory, ".download-")
	if err != nil {
		return err
	}
	defer os.Remove(file.Name())
	defer file.Close()
	deadline, cancel := context.WithTimeout(ctx, 2*time.Minute)
	defer cancel()
	var suspended atomic.Bool
	go func() {
		ticker := time.NewTicker(250 * time.Millisecond)
		defer ticker.Stop()
		for {
			select {
			case <-deadline.Done():
				return
			case <-ticker.C:
				control, err := readControl(root)
				if err == nil && control.Enabled != nil && (!*control.Enabled || control.Paused) {
					suspended.Store(true)
					cancel()
					return
				}
			}
		}
	}()
	if err = client.DownloadToFile(deadline, media, file); err != nil {
		if suspended.Load() {
			return errMediaSuspended
		}
		return err
	}
	if _, err = file.Seek(0, io.SeekStart); err != nil {
		return err
	}
	hash := sha256.New()
	size, err := io.Copy(hash, file)
	if err != nil {
		return err
	}
	if err = file.Sync(); err != nil {
		return err
	}
	digest := fmt.Sprintf("%x", hash.Sum(nil))
	if err = os.Rename(file.Name(), filepath.Join(directory, digest)); err != nil {
		return err
	}
	if err = syncDirectory(directory); err != nil {
		return err
	}
	for _, item := range record["attachments"].([]any) {
		attachment := item.(map[string]any)
		attachment["local_blob"] = digest
		attachment["size"] = size
		attachment["availability"] = "downloaded"
	}
	return nil
}
func (j *journal) mediaLoop(ctx context.Context, client *whatsmeow.Client) {
	for ctx.Err() == nil {
		files, err := filepath.Glob(filepath.Join(j.root, "media-queue", "*.json"))
		if err != nil {
			panic(err)
		}
		if len(files) == 0 || !client.IsConnected() {
			select {
			case <-ctx.Done():
				return
			case <-time.After(time.Second):
			}
			continue
		}
		for _, path := range files {
			if ctx.Err() != nil {
				return
			}
			active := path + ".working"
			if err := os.Rename(path, active); os.IsNotExist(err) {
				continue
			} else if err != nil {
				panic(err)
			}
			raw, err := os.ReadFile(active)
			if err != nil {
				panic(err)
			}
			var record map[string]any
			if err = json.Unmarshal(raw, &record); err != nil {
				panic(err)
			}
			err = downloadMedia(ctx, client, j.root, record)
			if ctx.Err() != nil {
				if err := os.Rename(active, path); err != nil {
					panic(err)
				}
				return
			}
			if errors.Is(err, errMediaSuspended) {
				if err := os.Rename(active, path); err != nil {
					panic(err)
				}
				break
			}
			if err != nil {
				for _, item := range record["attachments"].([]any) {
					item.(map[string]any)["availability"] = "unavailable_from_device_history"
				}
			}
			record["media_update"] = true
			j.append([]map[string]any{record})
			if err := os.Rename(active, path+".done"); err != nil {
				panic(err)
			}
		}
	}
}
