"""Official SDK client against an isolated socket-only PostgreSQL/Rust fixture."""
import importlib.util,json,subprocess,threading,urllib.request,os
from http.server import BaseHTTPRequestHandler,ThreadingHTTPServer
from pathlib import Path
spec=importlib.util.spec_from_file_location('benchmark',Path(__file__).with_name('benchmark-backends.py'))
b=importlib.util.module_from_spec(spec);spec.loader.exec_module(b)
with b.RustBackend(binary=os.environ.get('MONICA_TEST_SERVER')) as backend:
    c=b.Client(backend.url)
    credentials={'email':'mcp-sdk@example.invalid','password':'synthetic-sdk-password'}
    c.request('/api/v1/auth/bootstrap',credentials)
    c.csrf=c.request('/api/v1/auth/csrf')['csrf_token'];c.request('/api/v1/auth/login',credentials)
    c.request('/api/v1/mcp/settings',{'enabled':True},method='PATCH')
    key=c.request('/api/v1/mcp/keys',{'name':'SDK client'})
    subprocess.run(['node',str(Path(__file__).with_name('test-mcp-client.mjs'))],input=json.dumps({'url':backend.url+'/mcp','key':key['key']}),text=True,check=True)
    c.request('/api/v1/mcp/settings',{'enabled':True,'public_url':'https://monica-proxy.example/mcp'},method='PATCH')
    class Proxy(BaseHTTPRequestHandler):
        def log_message(self,*args):pass
        def do_POST(self):
            body=self.rfile.read(int(self.headers.get('Content-Length',0)))
            headers={k:v for k,v in self.headers.items() if k.lower() not in ('host','connection','content-length')}
            headers['Host']='monica-proxy.example'
            req=urllib.request.Request(backend.url+'/mcp',data=body,headers=headers,method='POST')
            with urllib.request.urlopen(req,timeout=30) as response:
                data=response.read();self.send_response(response.status);self.send_header('Content-Type',response.headers['Content-Type']);self.send_header('Content-Length',str(len(data)));self.end_headers();self.wfile.write(data)
    proxy=ThreadingHTTPServer(('127.0.0.1',0),Proxy);thread=threading.Thread(target=proxy.serve_forever,daemon=True);thread.start()
    try:
        subprocess.run(['node',str(Path(__file__).with_name('test-mcp-client.mjs'))],input=json.dumps({'url':f'http://127.0.0.1:{proxy.server_port}/mcp','key':key['key']}),text=True,check=True)
    finally:proxy.shutdown();proxy.server_close();thread.join()
    c.request(f'/api/v1/mcp/keys/{key["id"]}/revoke',{})
    assert 'Official MCP client fixture' in json.dumps(c.request('/api/v1/profiles'))
    print('PASS official SDK with real HTTP/Rust/PostgreSQL and Web-readable writes')
