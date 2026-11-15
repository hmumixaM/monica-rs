package main

import (
	"bufio"
	"bytes"
	"encoding/json"
	"image"
	"image/png"
	"os"
	"path/filepath"
	"strings"
	"testing"
)

func TestBatchRetainsSuccessFailuresAndReplayWithoutPrivateOutput(t *testing.T) {
	root, _ := filepath.EvalSymlinks(t.TempDir())
	source, target := filepath.Join(root, "raw"), filepath.Join(root, "media")
	os.Mkdir(source, 0o700)
	os.Mkdir(target, 0o700)
	os.WriteFile(filepath.Join(target, ".monica-rust-migration-media"), []byte(marker), 0o600)
	var photo bytes.Buffer
	png.Encode(&photo, image.NewRGBA(image.Rect(0, 0, 2, 3)))
	os.WriteFile(filepath.Join(source, "good.png"), photo.Bytes(), 0o600)
	os.WriteFile(filepath.Join(source, "private.dat"), []byte("synthetic private payload"), 0o600)
	var input bytes.Buffer
	json.NewEncoder(&input).Encode(map[string]string{"image_key": "0123456789abcdef", "image_xor": "255"})
	for i, name := range []string{"good.png", "private.dat", "good.png"} {
		json.NewEncoder(&input).Encode(batchRequest{strings.Repeat(string(rune('a'+i)), 64), name, "image"})
	}
	reader := bufio.NewReaderSize(&input, 8193)
	t.Setenv("MONICA_MIGRATION_IMAGE_KEY", "")
	t.Setenv("MONICA_MIGRATION_IMAGE_XOR", "")
	if err := credentials(reader, true); err != nil {
		t.Fatal(err)
	}
	if os.Getenv("MONICA_MIGRATION_IMAGE_KEY") != "" || os.Getenv("MONICA_MIGRATION_IMAGE_XOR") != "" {
		t.Fatal("credentials left in child environment")
	}
	var output bytes.Buffer
	if err := runBatch(reader, &output, source, target); err != nil {
		t.Fatal(err)
	}
	if bytes.Contains(output.Bytes(), []byte("private")) {
		t.Fatal("private input escaped in diagnostics")
	}
	var values []batchResult
	decoder := json.NewDecoder(&output)
	for i := 0; i < 3; i++ {
		var value batchResult
		if err := decoder.Decode(&value); err != nil {
			t.Fatal(err)
		}
		values = append(values, value)
	}
	if values[0].Asset == nil || values[0].Asset.Width != 2 || values[0].Asset.Height != 3 || values[1].Code != "image_conversion_failed" || values[1].Asset != nil || values[2].Asset == nil || *values[0].Asset != *values[2].Asset {
		t.Fatal("batch lost a result or changed replay")
	}
	for i, value := range values {
		if value.ID != strings.Repeat(string(rune('a'+i)), 64) || value.Commit != buildCommit {
			t.Fatal("response lost request/build identity")
		}
	}
	if raw, _ := os.ReadFile(filepath.Join(source, "good.png")); !bytes.Equal(raw, photo.Bytes()) {
		t.Fatal("raw copy changed")
	}
}

func TestBatchRejectsAmbiguousAndOversizedFrames(t *testing.T) {
	request := `{"id":"` + strings.Repeat("a", 64) + `","path":"absent","kind":"image"}` + "\n"
	for _, input := range []string{"", strings.Repeat("x", 8194), request + request, request[:len(request)-1] + "{}\n", `{"id":"no","path":"x","kind":"image"}` + "\n"} {
		if err := runBatch(bufio.NewReaderSize(strings.NewReader(input), 8193), &bytes.Buffer{}, t.TempDir(), t.TempDir()); err == nil {
			t.Fatal("invalid batch accepted")
		}
	}
	t.Setenv("MONICA_MIGRATION_IMAGE_KEY", "")
	t.Setenv("MONICA_MIGRATION_IMAGE_XOR", "")
	if err := credentials(bufio.NewReaderSize(strings.NewReader(`{"unknown":"private"}`+"\n"), 8193), true); err == nil {
		t.Fatal("unknown credential field accepted")
	}
	t.Setenv("MONICA_MIGRATION_IMAGE_KEY", "private")
	if err := credentials(bufio.NewReaderSize(strings.NewReader("{}\n"), 8193), true); err == nil || os.Getenv("MONICA_MIGRATION_IMAGE_KEY") != "" {
		t.Fatal("conflicting credential inputs accepted or retained")
	}
}
