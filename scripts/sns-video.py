"""Stream one source-backed native video; fully decode it before publication."""
import hashlib,json,os,struct,subprocess,tempfile
from pathlib import Path


def executable(runtime,name):
    root=Path(runtime)/'ffmpeg-runtime'
    return [str(root/'lib/ld-linux-x86-64.so.2'),'--library-path',str(root/'lib'),str(root/(name+'-real'))]


def validate(path,runtime,source_md5=None):
    path=Path(path)
    p=subprocess.run([*executable(runtime,'ffprobe'),'-v','error','-select_streams','v:0','-show_entries','stream=width,height:format=duration','-of','json',str(path)],capture_output=True,timeout=30)
    if p.returncode:raise ValueError('sns_video_probe_failed')
    info=json.loads(p.stdout);stream=info['streams'][0]
    if source_md5 is None:
        p=subprocess.run([*executable(runtime,'ffmpeg'),'-v','error','-xerror','-threads','1','-i',str(path),'-map','0:v:0','-f','null','-'],stdout=subprocess.DEVNULL,stderr=subprocess.PIPE,timeout=180)
        if p.returncode:raise ValueError('sns_video_frames_failed')
    digest = hashlib.sha256()
    original = hashlib.md5()
    with path.open('rb') as f:
        for block in iter(lambda: f.read(1024 * 1024), b''):
            digest.update(block)
            if source_md5 is not None:
                original.update(block)
    if source_md5 is not None and original.hexdigest() != source_md5.lower():
        raise ValueError('sns_video_source_digest_mismatch')
    return dict(sha256=digest.hexdigest(),byte_length=path.stat().st_size,media_type='video/mp4',width=stream['width'],height=stream['height'],duration_ms=round(float(info['format']['duration'])*1000))


def obtain(fetch,source,external,slot,cache,source_tid,stage,runtime,source_media_id=None,live_motion=False,raw_xml=None):
    scripts=Path(__file__).resolve().parent
    with tempfile.TemporaryDirectory(prefix='.video-',dir=stage)as temporary:
        root=Path(temporary);cipher=root/'source';decoded=root/'decoded'
        header,_=fetch.frame(source,external,slot,cache_root=cache,source_tid=source_tid,destination=cipher,source_media_id=source_media_id,live_motion=live_motion,raw_xml=raw_xml)
        source_md5 = header['source_md5'] if header.get('verified_cached_video') else None
        if source_md5:
            with cipher.open('rb') as original:
                os.fsync(original.fileno())
            os.replace(cipher, decoded)
        else:
            encoded=json.dumps(header).encode()
            with cipher.open('rb')as inp,decoded.open('wb')as out:
                child=subprocess.Popen([str(runtime/'node-run'),str(scripts.parent/'tools/sns-decrypt/decrypt-video.cjs'),str(runtime/'wasm')],stdin=subprocess.PIPE,stdout=out,stderr=subprocess.PIPE)
                try:
                    child.stdin.write(struct.pack('>I',len(encoded))+encoded)
                    try:
                        for block in iter(lambda:inp.read(1024*1024),b''):child.stdin.write(block)
                        child.stdin.close()
                    except BrokenPipeError:pass
                    code=child.wait(timeout=120)
                    if code:
                        raise ValueError('sns_video_decode_failed')
                    out.flush();os.fsync(out.fileno())
                finally:
                    if child.poll()is None:child.kill();child.wait()
        asset=validate(decoded,runtime,source_md5=source_md5)
        target=stage/'objects'/asset['sha256']
        if not target.exists():
            os.replace(decoded,target)
        directory = os.open(target.parent, os.O_RDONLY | os.O_DIRECTORY)
        try:
            os.fsync(directory)
        finally:
            os.close(directory)
        header.pop('seed',None);header.update(media=asset,full_video=True)
        return header
