package main

import (
	"bytes"
	"crypto/aes"
	"encoding/binary"
	"image"
	"image/jpeg"
	"testing"

	"github.com/sjzar/chatlog/pkg/util/dat2img"
)

func TestV4AlignedPaddingCollisionRecoversExactJPEG(t *testing.T) {
	var encoded bytes.Buffer
	if err := jpeg.Encode(&encoded, image.NewRGBA(image.Rect(0, 0, 3, 2)), nil); err != nil {
		t.Fatal(err)
	}
	// Position the first SOF component ID (1) at the AES boundary. The old
	// decoder mistakes it for padding and inserts a ciphertext block in SOF.
	component := bytes.Index(encoded.Bytes(), []byte{0xff, 0xc0}) + 10
	if component < 10 || encoded.Bytes()[component] != 1 {
		t.Fatal("unexpected synthetic JPEG layout")
	}
	comment := bytes.Repeat([]byte{0x44}, 1023-component-4)
	plain := []byte{0xff, 0xd8, 0xff, 0xfe, 0, 0}
	binary.BigEndian.PutUint16(plain[4:6], uint16(len(comment)+2))
	plain = append(plain, comment...)
	plain = append(plain, encoded.Bytes()[2:]...)
	key := []byte("0123456789abcdef")
	block, _ := aes.NewCipher(key)
	padded := append(append([]byte(nil), plain[:1024]...), bytes.Repeat([]byte{16}, 16)...)
	ciphertext := make([]byte, len(padded))
	for offset := 0; offset < len(padded); offset += 16 {
		block.Encrypt(ciphertext[offset:offset+16], padded[offset:offset+16])
	}
	header := make([]byte, 15)
	copy(header, dat2img.V4Format2.Header)
	binary.LittleEndian.PutUint32(header[6:10], 1024)
	binary.LittleEndian.PutUint32(header[10:14], 17)
	input := append(header, ciphertext...)
	input = append(input, plain[1024:len(plain)-17]...)
	for _, value := range plain[len(plain)-17:] {
		input = append(input, value^dat2img.V4XorKey)
	}
	before := append([]byte(nil), input...)
	dat2img.SetAesKey(string(key))
	old, _, err := dat2img.Dat2Image(input)
	if err != nil || validDecodedImage(old, "jpg") {
		t.Fatal("fixture did not reproduce the padding-valid invalid image")
	}
	actual, mime, err := transform(input, "image")
	if err != nil || mime != "image/jpeg" || !bytes.Equal(actual, plain) {
		t.Fatal("did not recover exact original JPEG", err)
	}
	if _, err := jpeg.Decode(bytes.NewReader(actual)); err != nil {
		t.Fatal("recovered JPEG pixels invalid", err)
	}
	if !bytes.Equal(input, before) {
		t.Fatal("source bytes changed")
	}
	for _, invalid := range [][]byte{input[:14], input[:30], append([]byte(nil), input...)} {
		if len(invalid) > 14 {
			binary.LittleEndian.PutUint32(invalid[6:10], 0xffffffff)
		}
		if _, _, err := decodeV4Fallback(invalid); err == nil {
			t.Fatal("invalid declared size accepted")
		}
	}
}

func TestV4AlignedUnpaddedJPEGRecoversPixels(t *testing.T) {
	var encoded bytes.Buffer
	if err := jpeg.Encode(&encoded, image.NewRGBA(image.Rect(0, 0, 3, 2)), nil); err != nil {
		t.Fatal(err)
	}
	jpegBytes := encoded.Bytes()
	// The first component selector follows the scan marker, length and count.
	// Dropping it leaves the earlier dimensions intact but invalidates the scan.
	index := bytes.Index(jpegBytes, []byte{0xff, 0xda}) + 5
	if index < 5 || jpegBytes[index] != 1 {
		t.Fatal("unexpected JPEG scan layout")
	}
	comment := bytes.Repeat([]byte{0x44}, 1023-index-4)
	expected := []byte{0xff, 0xd8, 0xff, 0xfe, 0, 0}
	binary.BigEndian.PutUint16(expected[4:6], uint16(len(comment)+2))
	expected = append(expected, comment...)
	expected = append(expected, jpegBytes[2:]...)
	header := make([]byte, 15)
	copy(header, dat2img.V4Format2.Header)
	binary.LittleEndian.PutUint32(header[6:10], 1024)
	binary.LittleEndian.PutUint32(header[10:14], 2)
	key := []byte("0123456789abcdef")
	dat2img.SetAesKey(string(key))
	block, _ := aes.NewCipher(key)

	input := append(header, make([]byte, 1024)...)
	for offset := 0; offset < 1024; offset += aes.BlockSize {
		block.Encrypt(input[15+offset:15+offset+aes.BlockSize], expected[offset:offset+aes.BlockSize])
	}
	input = append(input, expected[1024:len(expected)-2]...)
	for _, value := range expected[len(expected)-2:] {
		input = append(input, value^dat2img.V4XorKey)
	}
	old, _, err := dat2img.Dat2Image(input)
	if err != nil {
		t.Fatal(err)
	}
	if _, err := jpeg.DecodeConfig(bytes.NewReader(old)); err != nil {
		t.Fatal("legacy decoder output must retain valid dimensions", err)
	}
	if validDecodedImage(old, "jpg") {
		t.Fatal("legacy decoder output must fail full decoding")
	}
	actual, mime, err := transform(input, "image")
	if err != nil || mime != "image/jpeg" || !bytes.Equal(actual, expected) {
		t.Fatal("did not recover exact pixel-valid JPEG", err)
	}
}
