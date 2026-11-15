package main

import (
	"bufio"
	"bytes"
	"encoding/hex"
	"encoding/json"
	"errors"
	"io"
	"os"
	"strconv"

	"github.com/sjzar/chatlog/pkg/util/dat2img"
)

func failureCode(err error) string {
	var failure *codecFailure
	if errors.As(err, &failure) {
		return failure.code
	}
	return "media_decode_failed"
}

func frame(reader *bufio.Reader) ([]byte, error) {
	line, err := reader.ReadSlice('\n')
	if len(line) > 8192 || (err != nil && err != io.EOF) {
		return nil, errors.New("invalid codec frame")
	}
	if len(line) == 0 && err == io.EOF {
		return nil, io.EOF
	}
	return line, nil
}

func strictJSON(data []byte, value any) error {
	d := json.NewDecoder(bytes.NewReader(data))
	d.DisallowUnknownFields()
	if err := d.Decode(value); err != nil {
		return errors.New("invalid codec frame")
	}
	if d.Decode(new(any)) != io.EOF {
		return errors.New("invalid codec frame")
	}
	return nil
}

func credentials(reader *bufio.Reader, fromStdin bool) error {
	key, xor := os.Getenv("MONICA_MIGRATION_IMAGE_KEY"), os.Getenv("MONICA_MIGRATION_IMAGE_XOR")
	os.Unsetenv("MONICA_MIGRATION_IMAGE_KEY")
	os.Unsetenv("MONICA_MIGRATION_IMAGE_XOR")
	if fromStdin {
		if key != "" || xor != "" {
			return errors.New("conflicting credential inputs")
		}
		var input struct {
			Key string `json:"image_key"`
			Xor string `json:"image_xor"`
		}
		line, err := frame(reader)
		if err != nil || strictJSON(line, &input) != nil || len(input.Key) > 256 || len(input.Xor) > 8 {
			return errors.New("invalid credential frame")
		}
		key, xor = input.Key, input.Xor
	}
	if key != "" {
		dat2img.SetAesKey(key)
	}
	if xor != "" {
		n, err := strconv.ParseUint(xor, 0, 8)
		if err != nil {
			return errors.New("invalid XOR setting")
		}
		dat2img.V4XorKey = byte(n)
	}
	return nil
}

type batchRequest struct {
	ID   string `json:"id"`
	Path string `json:"path"`
	Kind string `json:"kind"`
}

type batchResult struct {
	ID     string  `json:"id"`
	Asset  *result `json:"asset,omitempty"`
	Code   string  `json:"error_code,omitempty"`
	Commit string  `json:"codec_commit"`
}

func batchItem(source, target string, r batchRequest) (value batchResult) {
	value = batchResult{ID: r.ID, Commit: buildCommit}
	// A dependency panic must not print its private cause or terminate the batch.
	defer func() {
		if recover() != nil {
			value.Asset = nil
			value.Code = "media_decode_failed"
		}
	}()
	asset, err := decode(source, target, request{r.Path, r.Kind})
	if err != nil {
		value.Code = failureCode(err)
	} else {
		value.Asset = &asset
	}
	return value
}

func runBatch(reader *bufio.Reader, output io.Writer, source, target string) error {
	seen := make(map[string]bool)
	for {
		line, err := frame(reader)
		if err == io.EOF && len(seen) > 0 {
			return nil
		}
		var r batchRequest
		if err != nil || strictJSON(line, &r) != nil || len(seen) >= 128 || len(r.ID) != 64 || seen[r.ID] {
			return errors.New("invalid batch request")
		}
		if _, err := hex.DecodeString(r.ID); err != nil {
			return errors.New("invalid batch identifier")
		}
		seen[r.ID] = true
		if err := json.NewEncoder(output).Encode(batchItem(source, target, r)); err != nil {
			return errors.New("codec response unavailable")
		}
	}
}
