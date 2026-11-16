// Offline full-pixel validation of already decrypted JPEG/PNG bytes.
package main

import (
	"bytes"
	"crypto/sha256"
	"encoding/hex"
	"encoding/json"
	"errors"
	"image"
	_ "image/jpeg"
	_ "image/png"
	"io"
	"os"
)

func validate(input io.Reader, output io.Writer) error {
	data, err := io.ReadAll(io.LimitReader(input, 32*1024*1024+1))
	if err != nil || len(data) == 0 || len(data) > 32*1024*1024 {
		return errors.New("image_size")
	}
	cfg, kind, err := image.DecodeConfig(bytes.NewReader(data))
	if err != nil || (kind != "jpeg" && kind != "png") || cfg.Width <= 0 || cfg.Height <= 0 || int64(cfg.Width)*int64(cfg.Height) > 40_000_000 {
		return errors.New("image_header")
	}
	if _, _, err = image.Decode(bytes.NewReader(data)); err != nil {
		return errors.New("image_pixels")
	}
	sum := sha256.Sum256(data)
	return json.NewEncoder(output).Encode(map[string]any{"sha256": hex.EncodeToString(sum[:]), "byte_length": len(data), "media_type": "image/" + kind, "width": cfg.Width, "height": cfg.Height})
}
func main() {
	if validate(os.Stdin, os.Stdout) != nil {
		_, _ = io.WriteString(os.Stderr, "image_validation_failed\n")
		os.Exit(1)
	}
}
