package main

import (
	"bytes"
	"encoding/binary"
	"errors"
	"image"
	"image/jpeg"
	"testing"
)

func TestWXGFStaticRecoveryIgnoresMetadataMarkers(t *testing.T) {
	stream := []byte{}
	for _, kind := range []byte{32, 33, 34, 19} {
		stream = append(stream, 0, 0, 0, 1, kind<<1, 1, 0x80)
	}
	data := make([]byte, 64)
	copy(data, "wxgf")
	data[4] = 19
	binary.BigEndian.PutUint16(data[7:9], 3)
	binary.BigEndian.PutUint16(data[9:11], 2)
	// Plausible false partition in the metadata, without HEVC parameter sets.
	copy(data[23:], []byte{0, 0, 0, 12, 0, 0, 0, 1, 0x50, 0})
	copy(data[36:], []byte{0, 0, 0, 1, 0, 0, 0, 1})
	appendPartition := func(input, payload []byte) []byte {
		header := make([]byte, 4)
		binary.BigEndian.PutUint32(header, uint32(len(payload)))
		return append(append(input, header...), payload...)
	}
	data = appendPartition(data, stream)
	before := append([]byte(nil), data...)
	var encoded bytes.Buffer
	if err := jpeg.Encode(&encoded, image.NewRGBA(image.Rect(0, 0, 3, 2)), nil); err != nil {
		t.Fatal(err)
	}
	convert := func(actual []byte) ([]byte, error) {
		if !bytes.Equal(actual, stream) {
			return nil, errors.New("wrong partition")
		}
		return encoded.Bytes(), nil
	}
	out, err := wxgfStaticFallback(data, convert)
	if err != nil || !bytes.Equal(out, encoded.Bytes()) || !bytes.Equal(data, before) {
		t.Fatal("static image was not recovered without changing its source", err)
	}
	if _, err := wxgfStaticFallback(appendPartition(append([]byte(nil), data...), stream), convert); err == nil {
		t.Fatal("ambiguous image accepted")
	}
	wrongSize := append([]byte(nil), data...)
	wrongSize[8] = 4
	if _, err := wxgfStaticFallback(wrongSize, convert); err == nil {
		t.Fatal("incorrect dimensions accepted")
	}
	multipleFrames := append(append([]byte(nil), stream...), 0, 0, 0, 1, 38, 1, 0x80)
	if staticHEVCParameters(multipleFrames) {
		t.Fatal("multiple pictures accepted as a static image")
	}
	for _, extra := range [][]byte{
		{0, 0, 0, 1, 38, 1, 0x80},                                 // IDR without repeated parameter sets
		{0, 0, 1, 2, 1, 0x80, 0x81},                               // P-frame with three-byte prefix
		{0, 0, 0, 1, 2, 2, 0x80},                                  // higher temporal layer, inherited parameters
		append([]byte{0, 0, 1, 2, 1, 0x80}, make([]byte, 250)...), // length ends in zero
		append([]byte{0}, []byte{0, 0, 0, 1, 2, 1, 0x80}...),      // Annex B leading_zero_8bits
		append(make([]byte, 8), []byte{0, 0, 0, 1, 2, 1, 0x80}...),
		multipleFrames,
	} {
		animated := appendPartition(append([]byte(nil), data...), extra)
		if _, err := wxgfStaticFallback(animated, convert); err == nil {
			t.Fatal("additional animation partition accepted")
		}
	}
	if _, err := wxgfStaticFallback(data, func([]byte) ([]byte, error) { return []byte{0xff, 0xd8}, nil }); err == nil {
		t.Fatal("invalid decoded pixels accepted")
	}
}

func TestV3WXGFDoesNotBecomeInvalidBMP(t *testing.T) {
	plain := append([]byte("wxgf"), make([]byte, 40)...)
	input := make([]byte, len(plain))
	for i, value := range plain {
		input[i] = value ^ 0x65
	}
	before := append([]byte(nil), input...)
	decoded, ok := decodeXORWXGF(input)
	if !ok || !bytes.Equal(decoded, plain) || !bytes.Equal(input, before) {
		t.Fatal("XOR WXGF identity lost")
	}
	// Invalid WXGF is rejected; it must not be accepted as a fake BMP merely
	// because that format has a shorter matching XOR signature.
	if _, _, err := transform(input, "image"); err == nil || failureCode(err) != "wxgf_conversion_failed" {
		t.Fatal("invalid WXGF was accepted as another image format", err)
	}
}

func TestV3BMPWithCollidingWXGFPrefixIsPreserved(t *testing.T) {
	// A valid 24-bit, 1x5319 BMP has total length 21330 and starts BMRS.
	plain := make([]byte, 21330)
	copy(plain, "BM")
	binary.LittleEndian.PutUint32(plain[2:6], uint32(len(plain)))
	binary.LittleEndian.PutUint32(plain[10:14], 54)
	binary.LittleEndian.PutUint32(plain[14:18], 40)
	binary.LittleEndian.PutUint32(plain[18:22], 1)
	binary.LittleEndian.PutUint32(plain[22:26], 5319)
	binary.LittleEndian.PutUint16(plain[26:28], 1)
	binary.LittleEndian.PutUint16(plain[28:30], 24)
	input := make([]byte, len(plain))
	for i, value := range plain {
		input[i] = value ^ 0x65
	}
	out, mime, err := transform(input, "image")
	if err != nil || mime != "image/bmp" || !bytes.Equal(out, plain) {
		t.Fatal("valid BMP with colliding prefix changed", err)
	}
}
