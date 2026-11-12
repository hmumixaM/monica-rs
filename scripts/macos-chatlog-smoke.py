#!/usr/bin/env python3
"""Verify the bundled Chatlog CLI fails closed on an empty synthetic source."""
import argparse,json,os,socket,subprocess,tempfile
from pathlib import Path
p=argparse.ArgumentParser(description=__doc__);p.add_argument('binary',type=Path);a=p.parse_args()
binary=a.binary.resolve()
with tempfile.TemporaryDirectory(prefix='monica-chatlog-fixture-') as directory:
 root=Path(directory);source=root/'empty-source';source.mkdir();config=root/'config';config.mkdir()
 with socket.socket() as listener:
  listener.bind(('127.0.0.1',0));port=listener.getsockname()[1]
 payload={'platform':'darwin','version':4,'data_dir':str(source),'data_key':'1'*64,'work_dir':str(root/'work'),'http_addr':f'127.0.0.1:{port}','desktop_mode':True}
 (config/'chatlog-server.json').write_text(json.dumps(payload))
 result=subprocess.run([str(binary),'serve','--config-dir',str(config)],env={'PATH':'/usr/bin:/bin','HOME':str(root),'CHATLOG_WCDB_CACHE_DIR':str(root/'cache')},capture_output=True,timeout=25)
 assert result.returncode!=0,'Empty source must not report ready'
 with socket.socket() as probe:
  probe.settimeout(0.5);assert probe.connect_ex(('127.0.0.1',port))!=0,'Failed Chatlog must not expose a listener'
 assert not list(source.iterdir()),'Read-only source was modified'
 print(json.dumps({'binary':str(binary),'result':'actual CLI rejected synthetic empty source, no listener and no source writes'}))
