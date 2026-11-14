package main

import (
	"bytes"
	"crypto/aes"
	"encoding/binary"
	"image"
	"image/png"
	"os"
	"path/filepath"
	"testing"

	"github.com/sjzar/chatlog/pkg/util/dat2img"
)

func TestDecodedCopiesAreIndependentAndPathsCannotEscape(t *testing.T) {
	root, err := filepath.EvalSymlinks(t.TempDir())
	if err != nil {
		t.Fatal(err)
	}
	source := filepath.Join(root, "source")
	target := filepath.Join(root, "target")
	os.Mkdir(source, 0o700)
	os.Mkdir(target, 0o700)
	if _, _, err := roots(source, target); err == nil {
		t.Fatal("unmarked destination accepted")
	}
	os.WriteFile(filepath.Join(target, ".monica-rust-migration-media"), []byte(marker), 0o600)
	if _, _, err := roots(source, target); err != nil {
		t.Fatal(err)
	}
	var photo bytes.Buffer
	png.Encode(&photo, image.NewRGBA(image.Rect(0, 0, 2, 3)))
	original := append([]byte(nil), photo.Bytes()...)
	os.WriteFile(filepath.Join(source, "photo.png"), original, 0o600)
	r, err := decode(source, target, request{"photo.png", "image"})
	if err != nil {
		t.Fatal(err)
	}
	if r.Width != 2 || r.Height != 3 || r.Type != "image/png" {
		t.Fatalf("invalid image metadata: %+v", r)
	}
	if _, err = decode(source, target, request{"photo.png", "image"}); err != nil {
		t.Fatal("replay failed", err)
	}
	os.WriteFile(filepath.Join(target, r.ObjectPath), []byte("changed new copy"), 0o600)
	if data, _ := os.ReadFile(filepath.Join(source, "photo.png")); !bytes.Equal(data, original) {
		t.Fatal("source changed")
	}
	if _, err = decode(source, target, request{"photo.png", "image"}); err == nil {
		t.Fatal("tampered destination accepted")
	}
	os.Symlink(filepath.Join(source, "photo.png"), filepath.Join(source, "alias.png"))
	if _, err = decode(source, target, request{"alias.png", "image"}); err == nil {
		t.Fatal("source symlink accepted")
	}
	if _, err = decode(source, target, request{"../outside", "file"}); err == nil {
		t.Fatal("path escape accepted")
	}
	outside := filepath.Join(root, "existing-data")
	os.Mkdir(outside, 0o700)
	second := filepath.Join(root, "another-target")
	os.Mkdir(second, 0o700)
	os.WriteFile(filepath.Join(second, ".monica-rust-migration-media"), []byte(marker), 0o600)
	os.Symlink(outside, filepath.Join(second, "sha256"))
	if _, err = decode(source, second, request{"photo.png", "image"}); err == nil {
		t.Fatal("output symlink accepted")
	}
	entries, _ := os.ReadDir(outside)
	if len(entries) != 0 {
		t.Fatal("created files outside the new destination")
	}
}

func TestV4ImageUsesPinnedCodecWithoutChangingOriginal(t *testing.T) {
	var photo bytes.Buffer
	png.Encode(&photo, image.NewRGBA(image.Rect(0, 0, 3, 2)))
	plain := photo.Bytes()
	padding := aes.BlockSize - len(plain)%aes.BlockSize
	padded := append(append([]byte(nil), plain...), bytes.Repeat([]byte{byte(padding)}, padding)...)
	key := []byte("0123456789abcdef")
	cipher, _ := aes.NewCipher(key)
	encrypted := make([]byte, len(padded))
	for i := 0; i < len(padded); i += aes.BlockSize {
		cipher.Encrypt(encrypted[i:i+aes.BlockSize], padded[i:i+aes.BlockSize])
	}
	header := make([]byte, 15)
	copy(header, dat2img.V4Format2.Header)
	binary.LittleEndian.PutUint32(header[6:10], uint32(len(plain)))
	input := append(header, encrypted...)
	before := append([]byte(nil), input...)
	dat2img.SetAesKey(string(key))
	decoded, mime, err := transform(input, "image")
	if err != nil || mime != "image/png" || !bytes.Equal(decoded, plain) {
		t.Fatal("v4 image failed", err)
	}
	if !bytes.Equal(input, before) {
		t.Fatal("codec changed input")
	}
	if _, _, err = transform([]byte("unsupported image"), "image"); err == nil {
		t.Fatal("unsupported image accepted")
	}
}
