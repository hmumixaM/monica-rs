// Run against a disposable Rust fixture. SDK is installed only in validation
// artifacts: npm install --prefix .local-validation/mcp-client --no-save @modelcontextprotocol/sdk@1.32.2
import {createRequire} from 'node:module';
import {pathToFileURL} from 'node:url';
import assert from 'node:assert/strict';
const require=createRequire(new URL('../.local-validation/mcp-client/package.json',import.meta.url));
const {Client}=await import(pathToFileURL(require.resolve('@modelcontextprotocol/sdk/client/index.js')));
const {StreamableHTTPClientTransport}=await import(pathToFileURL(require.resolve('@modelcontextprotocol/sdk/client/streamableHttp.js')));
let input='';for await(const chunk of process.stdin)input+=chunk;const {url,key}=JSON.parse(input);
const client=new Client({name:'monica-acceptance',version:'1.0.0'});
try {
 await client.connect(new StreamableHTTPClientTransport(new URL(url),{requestInit:{headers:{Authorization:`Bearer ${key}`}}}));
 const {tools}=await client.listTools();assert(tools.some(t=>t.name==='create_profile'));assert(!tools.some(t=>/password|key|pause/.test(t.name)));
 const created=await client.callTool({name:'create_profile',arguments:{body:{display_name:'Official MCP client fixture'}}});assert(!created.isError,JSON.stringify(created));
 const id=JSON.parse(created.content[0].text).data.id;
 const edit=await client.callTool({name:'edit_field',arguments:{id,body:{field_key:'notes',value:'Saved through official MCP SDK'}}});assert(!edit.isError,JSON.stringify(edit));
 const read=await client.callTool({name:'read_profile',arguments:{id}});assert(!read.isError,JSON.stringify(read));assert(read.content[0].text.includes('Official MCP client fixture'));
 const status=await client.callTool({name:'get_import_status',arguments:{}});assert(!status.isError,JSON.stringify(status));
 console.log(JSON.stringify({sdk:'@modelcontextprotocol/sdk 1.32.2',tools:tools.length,created_profile:id,verified:['connect','list','create','edit','read','status']}));
}finally{await client.close();}
