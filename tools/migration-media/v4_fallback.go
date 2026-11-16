package main

import (
	"bytes"
	"crypto/aes"
	"encoding/binary"
	"errors"
	"image"

	"github.com/sjzar/chatlog/pkg/util/dat2img"
)

func validDecodedImage(data []byte, ext string) bool {
	switch ext {
	case "jpg", "jpeg", "png", "gif":
		_, _, err := image.DecodeConfig(bytes.NewReader(data))
		return err == nil
	default:
		return true
	}
}

// The pinned decoder accepts the first padding-valid AES boundary, before
// validating the decoded image. An aligned plaintext length can accidentally
// look padded as well. Retry the known boundary/key combinations only when
// the original decoder failed or returned an invalid image header.
func decodeV4Fallback(data []byte) ([]byte, string, error) {
	if len(data) < 15 {
		return nil, "", errors.New("invalid v4 header")
	}
	var keys [][]byte
	switch {
	case bytes.Equal(data[:6], dat2img.V4Format1.Header):
		keys = [][]byte{dat2img.V4Format1.AesKey}
	case bytes.Equal(data[:6], dat2img.V4Format2.Header):
		keys = [][]byte{dat2img.V4Format2.AesKey, dat2img.V4Format2AltAesKey}
	default:
		return nil, "", errors.New("not a v4 image")
	}
	length := uint64(binary.LittleEndian.Uint32(data[6:10]))
	xorLength := uint64(binary.LittleEndian.Uint32(data[10:14]))
	body := data[15:]
	last := errors.New("no valid v4 image candidate")
	for _, key := range keys {
		block, err := aes.NewCipher(key)
		if err != nil {
			continue
		}
		for _, boundary := range []uint64{(length + 15) / 16 * 16, (length/16 + 1) * 16} {
			if boundary > uint64(len(body)) || xorLength > uint64(len(body))-boundary {
				continue
			}
			part := make([]byte, int(boundary))
			for offset := 0; offset < len(part); offset += aes.BlockSize {
				block.Decrypt(part[offset:offset+aes.BlockSize], body[offset:offset+aes.BlockSize])
			}
			if len(part) > 0 {
				padding := int(part[len(part)-1])
				if padding == 0 || padding > aes.BlockSize || padding > len(part) ||
					!bytes.Equal(part[len(part)-padding:], bytes.Repeat([]byte{byte(padding)}, padding)) {
					continue
				}
				part = part[:len(part)-padding]
			}
			end := len(body) - int(xorLength)
			decoded := make([]byte, 0, len(body))
			decoded = append(decoded, part...)
			decoded = append(decoded, body[int(boundary):end]...)
			for _, value := range body[end:] {
				decoded = append(decoded, value^dat2img.V4XorKey)
			}
			for _, format := range dat2img.Formats {
				if !bytes.HasPrefix(decoded, format.Header) {
					continue
				}
				ext := format.Ext
				if ext == "wxgf" {
					decoded, ext, err = dat2img.Wxam2pic(decoded)
					if err != nil {
						last = conversionFailure("wxgf_conversion_failed", err)
						break
					}
				}
				if validDecodedImage(decoded, ext) {
					return decoded, ext, nil
				}
				last = conversionFailure("decoded_image_invalid", errors.New("invalid image candidate"))
				break
			}
		}
	}
	return nil, "", last
}
