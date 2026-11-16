package main

import (
	"bytes"
	"context"
	"encoding/binary"
	"errors"
	"image"
	"os/exec"
	"time"

	"github.com/sjzar/chatlog/pkg/util/dat2img"
)

func convertWXGF(data []byte) ([]byte, string, error) {
	out, ext, err := dat2img.Wxam2pic(data)
	if err == nil {
		return out, ext, nil
	}
	// WXGF metadata can contain apparent Annex B markers with plausible length
	// fields. The pinned converter mistakes those for animation partitions.
	// Recover only a single framed, independently decodable static HEVC image.
	out, err = wxgfStaticFallback(data, convertStaticHEVC)
	return out, "jpg", err
}

func convertStaticHEVC(stream []byte) ([]byte, error) {
	ctx, cancel := context.WithTimeout(context.Background(), 30*time.Second)
	defer cancel()
	cmd := exec.CommandContext(ctx, dat2img.FFMpegPath,
		"-v", "error", "-f", "hevc", "-i", "pipe:0",
		"-frames:v", "1", "-c:v", "mjpeg", "-q:v", "4", "-f", "image2pipe", "pipe:1")
	var out bytes.Buffer
	cmd.Stdin, cmd.Stdout = bytes.NewReader(stream), &out
	if err := cmd.Run(); err != nil {
		return nil, errors.New("static hevc conversion failed")
	}
	return out.Bytes(), nil
}

func wxgfStaticFallback(data []byte, convert func([]byte) ([]byte, error)) ([]byte, error) {
	invalid := errors.New("no unambiguous static wxgf image")
	if len(data) < 19 || len(data) > 8*1024*1024 || !bytes.Equal(data[:4], []byte("wxgf")) {
		return nil, invalid
	}
	header := int(data[4])
	w, h := int(binary.BigEndian.Uint16(data[7:9])), int(binary.BigEndian.Uint16(data[9:11]))
	if header < 11 || header >= len(data) || w == 0 || h == 0 || int64(w)*int64(h) > 16*1024*1024 {
		return nil, invalid
	}
	var candidate []byte
	partitions := 0
	marker := []byte{0, 0, 1}
	for cursor := header; cursor < len(data); {
		index := bytes.Index(data[cursor:], marker)
		if index < 0 {
			break
		}
		threeByteStart := cursor + index
		cursor = threeByteStart + len(marker)
		// A leading zero can belong to the length field, the four-byte
		// prefix, or Annex B leading_zero_8bits. Validate the whole zero run.
		for start := threeByteStart; start >= header+4; start-- {
			if start != threeByteStart && data[start] != 0 {
				break
			}
			size := uint64(binary.BigEndian.Uint32(data[start-4 : start]))
			nal := cursor - start
			if size < uint64(nal+2) || size > uint64(len(data)-start) {
				continue
			}
			stream := data[start : start+int(size)]
			if stream[nal]&0x81 != 0 || stream[nal+1]&0xf8 != 0 || stream[nal+1]&7 == 0 || stream[nal]>>1 > 40 {
				continue
			}
			// Later frames can inherit parameter sets and use temporal IDs
			// above zero. Count those partitions before deciding this is static.
			partitions++
			if partitions > 1 {
				return nil, invalid
			}
			if staticHEVCParameters(stream) {
				candidate = stream
			}
		}
	}
	if candidate == nil {
		return nil, invalid
	}
	out, err := convert(candidate)
	if err != nil {
		return nil, err
	}
	config, format, err := image.DecodeConfig(bytes.NewReader(out))
	if err != nil || format != "jpeg" || config.Width != w || config.Height != h {
		return nil, invalid
	}
	if _, _, err := image.Decode(bytes.NewReader(out)); err != nil {
		return nil, invalid
	}
	return out, nil
}

func decodeXORWXGF(data []byte) ([]byte, bool) {
	if len(data) < 19 {
		return nil, false
	}
	key := data[0] ^ 'w'
	for i, value := range []byte("wxgf") {
		if data[i]^key != value {
			return nil, false
		}
	}
	// BMRS is a valid BMP prefix when its file-size low bytes are 0x5352.
	// It also XOR-matches wxgf. Preserve that existing BMP path when its
	// declared file length fits the input; genuine WXGF headers do not.
	var bmpSizeBytes [4]byte
	for i := range bmpSizeBytes {
		bmpSizeBytes[i] = data[2+i] ^ key ^ 0x35
	}
	bmpSize := uint64(binary.LittleEndian.Uint32(bmpSizeBytes[:]))
	if bmpSize >= 26 && bmpSize <= uint64(len(data)) {
		return nil, false
	}
	out := make([]byte, len(data))
	for i, value := range data {
		out[i] = value ^ key
	}
	return out, true
}

func staticHEVCParameters(stream []byte) bool {
	// Admit VPS, SPS, PPS followed by exactly one intra picture NAL. Reject
	// multiple pictures rather than silently flattening an animation.
	stage, pictures := 0, 0
	for cursor := 0; cursor < len(stream); {
		index := bytes.Index(stream[cursor:], []byte{0, 0, 1})
		if index < 0 {
			break
		}
		start := cursor + index + 3
		if start+2 > len(stream) {
			return false
		}
		a, b := stream[start], stream[start+1]
		if a&0x81 != 0 || b != 1 {
			return false
		}
		typeID := (a >> 1) & 63
		switch typeID {
		case 32, 33, 34:
			if int(typeID)-32 != stage || pictures != 0 {
				return false
			}
			stage++
		case 19, 20:
			if stage != 3 {
				return false
			}
			pictures++
		default:
			return false
		}
		cursor = start + 2
	}
	return stage == 3 && pictures == 1
}
