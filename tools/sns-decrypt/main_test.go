package main

import (
	"bytes"
	"image"
	"image/color"
	"image/jpeg"
	"testing"
)

func TestValidateDecodedImageAndRejectTruncation(t *testing.T) {
	picture := image.NewRGBA(image.Rect(0, 0, 32, 24))
	for y := 0; y < 24; y++ {
		for x := 0; x < 32; x++ {
			picture.Set(x, y, color.RGBA{uint8(x * 8), uint8(y * 10), 80, 255})
		}
	}
	var encoded, result bytes.Buffer
	if err := jpeg.Encode(&encoded, picture, nil); err != nil {
		t.Fatal(err)
	}
	data := encoded.Bytes()
	if err := validate(bytes.NewReader(data), &result); err != nil {
		t.Fatal(err)
	}
	for _, corrupt := range [][]byte{data[:len(data)/2], []byte("<html>unavailable</html>"), {}} {
		result.Reset()
		if err := validate(bytes.NewReader(corrupt), &result); err == nil || result.Len() != 0 {
			t.Fatal("invalid image was accepted")
		}
	}
}
