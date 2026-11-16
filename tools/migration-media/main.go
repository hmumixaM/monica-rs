// Offline codecs from the same pinned MIT-licensed Chatlog source as desktop.
// Global image credentials are accepted only from the process environment.
package main

import (
	"bufio"
	"bytes"
	"crypto/sha256"
	"encoding/hex"
	"encoding/json"
	"errors"
	"flag"
	"image"
	_ "image/gif"
	_ "image/jpeg"
	_ "image/png"
	"io"
	"net/http"
	"os"
	"path/filepath"
	"strings"
	"syscall"

	"github.com/rs/zerolog"
	"github.com/sjzar/chatlog/pkg/util/dat2img"
	"github.com/sjzar/chatlog/pkg/util/silk"
)

const marker = "monica-rust-isolated-media-v1\n"

// Release builds stamp this from the reviewed source archive. Never infer it
// from the current runtime checkout, which may differ from the running codec.
var buildCommit = "development"

type codecFailure struct {
	code  string
	cause error
}

func (e *codecFailure) Error() string { return e.code }
func (e *codecFailure) Unwrap() error { return e.cause }

func conversionFailure(code string, err error) error {
	if err == nil {
		return nil
	}
	return &codecFailure{code: code, cause: err}
}

func writeFailure(output io.Writer, err error) error {
	// Causes can contain credentials, paths or decrypted content. The wire
	// protocol deliberately contains only fixed codes and the build identifier.
	return json.NewEncoder(output).Encode(struct {
		Code   string `json:"error_code"`
		Commit string `json:"codec_commit"`
	}{failureCode(err), buildCommit})
}

type request struct {
	Path string `json:"path"`
	Kind string `json:"kind"`
}
type result struct {
	SHA256     string `json:"sha256"`
	Bytes      int64  `json:"byte_length"`
	Type       string `json:"media_type"`
	ObjectPath string `json:"object_path"`
	Width      int    `json:"width,omitempty"`
	Height     int    `json:"height,omitempty"`
}

func beneath(root, path string) bool {
	rel, err := filepath.Rel(root, path)
	return err == nil && rel != "." && rel != ".." && !strings.HasPrefix(rel, ".."+string(filepath.Separator))
}

func directory(root, relative string) (string, error) {
	current := root
	for _, part := range strings.Split(relative, string(filepath.Separator)) {
		if part == "" || part == "." || part == ".." {
			return "", errors.New("invalid output component")
		}
		current = filepath.Join(current, part)
		info, err := os.Lstat(current)
		if os.IsNotExist(err) {
			if err = os.Mkdir(current, 0o755); err != nil {
				return "", err
			}
			continue
		}
		if err != nil || !info.IsDir() || info.Mode()&os.ModeSymlink != 0 {
			return "", errors.New("output directory symlink")
		}
	}
	return current, nil
}

func roots(source, target string) (string, string, error) {
	a, err := filepath.EvalSymlinks(source)
	if err != nil {
		return "", "", err
	}
	b, err := filepath.EvalSymlinks(target)
	if err != nil {
		return "", "", err
	}
	if !filepath.IsAbs(source) || !filepath.IsAbs(target) || a != filepath.Clean(source) || b != filepath.Clean(target) || a == b || beneath(a, b) || beneath(b, a) {
		return "", "", errors.New("isolated canonical roots required")
	}
	data, err := os.ReadFile(filepath.Join(b, ".monica-rust-migration-media"))
	if err != nil || string(data) != marker {
		return "", "", errors.New("dedicated output identity required")
	}
	return a, b, nil
}

func transform(data []byte, kind string) ([]byte, string, error) {
	switch kind {
	case "voice":
		out, err := silk.Silk2MP3(data)
		return out, "audio/mpeg", conversionFailure("voice_conversion_failed", err)
	case "image":
		// The inherited no-FFmpeg fallback scans compressed WXGF bytes for
		// signatures and can publish a random BM suffix as a BMP. This codec
		// requires real WXGF conversion; unavailable FFmpeg must remain a failure.
		dat2img.FFmpegMode = true
		typeName := http.DetectContentType(data)
		if strings.HasPrefix(typeName, "image/") {
			return data, typeName, nil
		}
		// V3 WXGF and BMP share the same XOR relationship between their
		// first two bytes. Prefer the four-byte WXGF signature over BMP.
		out, matchedWXGF := decodeXORWXGF(data)
		ext := "wxgf"
		var err error
		if !matchedWXGF {
			out, ext, err = dat2img.Dat2Image(data)
		}
		if err != nil || !validDecodedImage(out, ext) {
			if len(data) >= 6 && (bytes.Equal(data[:6], dat2img.V4Format1.Header) || bytes.Equal(data[:6], dat2img.V4Format2.Header)) {
				out, ext, err = decodeV4Fallback(data)
			}
		}
		if err != nil {
			var failure *codecFailure
			if errors.As(err, &failure) {
				return nil, "", err
			}
			return nil, "", conversionFailure("image_conversion_failed", err)
		}
		if ext == "wxgf" {
			out, ext, err = convertWXGF(out)
			if err != nil {
				return nil, "", conversionFailure("wxgf_conversion_failed", err)
			}
		}
		if ext == "jpg" {
			ext = "jpeg"
		}
		return out, "image/" + ext, nil
	default:
		return nil, "", errors.New("unsupported codec")
	}
}

func decode(source, target string, r request) (result, error) {
	var empty result
	if filepath.IsAbs(r.Path) || filepath.Clean(r.Path) != r.Path || !beneath(source, filepath.Join(source, r.Path)) {
		return empty, errors.New("invalid source path")
	}
	path := filepath.Join(source, r.Path)
	real, err := filepath.EvalSymlinks(path)
	if err != nil || real != path {
		return empty, errors.New("source symlink or missing file")
	}
	stat, err := os.Stat(path)
	if err != nil || !stat.Mode().IsRegular() {
		return empty, errors.New("regular source required")
	}
	if stat.Size() > 512*1024*1024 {
		return empty, errors.New("file size limit")
	}
	input, err := os.OpenFile(path, os.O_RDONLY|syscall.O_NOFOLLOW, 0)
	if err != nil {
		return empty, err
	}
	defer input.Close()
	var body io.Reader = input
	typeName := "application/octet-stream"
	width, height := 0, 0
	if r.Kind == "image" || r.Kind == "voice" {
		if stat.Size() > 64*1024*1024 {
			return empty, errors.New("codec size limit")
		}
		data, err := io.ReadAll(io.LimitReader(input, 64*1024*1024+1))
		if err != nil {
			return empty, err
		}
		out, mime, err := transform(data, r.Kind)
		if err != nil {
			return empty, err
		}
		if len(out) > 128*1024*1024 {
			return empty, errors.New("decoded size limit")
		}
		typeName = mime
		body = bytes.NewReader(out)
		if strings.HasPrefix(mime, "image/") {
			config, _, imageErr := image.DecodeConfig(bytes.NewReader(out))
			if imageErr == nil {
				width, height = config.Width, config.Height
			} else if mime == "image/jpeg" || mime == "image/png" || mime == "image/gif" {
				return empty, conversionFailure("decoded_image_invalid", imageErr)
			}
		}
	} else if r.Kind == "file" || r.Kind == "video" {
		prefix := make([]byte, 512)
		n, err := input.Read(prefix)
		if err != nil && err != io.EOF {
			return empty, err
		}
		typeName = http.DetectContentType(prefix[:n])
		if _, err = input.Seek(0, io.SeekStart); err != nil {
			return empty, err
		}
	} else {
		return empty, errors.New("unsupported media kind")
	}
	temp, err := os.CreateTemp(target, ".migration-media-")
	if err != nil {
		return empty, err
	}
	defer os.Remove(temp.Name())
	defer temp.Close()
	if err = temp.Chmod(0o644); err != nil {
		return empty, err
	}
	hash := sha256.New()
	size, err := io.Copy(io.MultiWriter(temp, hash), body)
	if err != nil {
		return empty, err
	}
	if err = temp.Sync(); err != nil {
		return empty, err
	}
	if err = temp.Close(); err != nil {
		return empty, err
	}
	stamp, err := os.Stat(path)
	if err != nil || stamp.Size() != stat.Size() || !stamp.ModTime().Equal(stat.ModTime()) {
		return empty, errors.New("source changed")
	}
	digest := hex.EncodeToString(hash.Sum(nil))
	relative := filepath.Join("sha256", digest[:2], digest[2:4], digest)
	parent, err := directory(target, filepath.Dir(relative))
	if err != nil {
		return empty, err
	}
	canonical, err := filepath.EvalSymlinks(parent)
	if err != nil || canonical != parent {
		return empty, errors.New("output directory symlink")
	}
	destination := filepath.Join(target, relative)
	if err = os.Link(temp.Name(), destination); err != nil {
		if !os.IsExist(err) {
			return empty, err
		}
		info, statErr := os.Lstat(destination)
		if statErr != nil || !info.Mode().IsRegular() {
			return empty, errors.New("unsafe existing output")
		}
		if stamp, ok := info.Sys().(*syscall.Stat_t); !ok || stamp.Nlink != 1 {
			return empty, errors.New("existing output is not independent")
		}
		check, openErr := os.OpenFile(destination, os.O_RDONLY|syscall.O_NOFOLLOW, 0)
		if openErr != nil {
			return empty, openErr
		}
		h := sha256.New()
		n, readErr := io.Copy(h, check)
		check.Close()
		if readErr != nil || n != size || hex.EncodeToString(h.Sum(nil)) != digest {
			return empty, errors.New("output checksum conflict")
		}
	}
	return result{digest, size, typeName, filepath.ToSlash(relative), width, height}, nil
}

func run() error {
	zerolog.SetGlobalLevel(zerolog.Disabled)
	source := flag.String("source-root", "", "independent frozen source directory")
	target := flag.String("target-root", "", "marked isolated media destination")
	batch := flag.Bool("batch", false, "decode at most 128 identified requests")
	stdinCredentials := flag.Bool("credentials-stdin", false, "read a private credential frame before requests")
	flag.Parse()
	a, b, err := roots(*source, *target)
	if err != nil {
		return err
	}
	reader := bufio.NewReaderSize(os.Stdin, 8193)
	if err := credentials(reader, *stdinCredentials); err != nil {
		return err
	}
	if *batch {
		return runBatch(reader, os.Stdout, a, b)
	}
	var r request
	decoder := json.NewDecoder(io.LimitReader(reader, 8193))
	decoder.DisallowUnknownFields()
	if err = decoder.Decode(&r); err != nil {
		return errors.New("invalid codec request")
	}
	value, err := decode(a, b, r)
	if err != nil {
		return err
	}
	return json.NewEncoder(os.Stdout).Encode(value)
}

func main() {
	if err := run(); err != nil {
		_ = writeFailure(os.Stderr, err)
		os.Exit(1)
	}
}
