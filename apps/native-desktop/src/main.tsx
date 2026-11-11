import { useEffect, useState } from "react";
import { createRoot } from "react-dom/client";
import { invoke } from "@tauri-apps/api/core";
import "./styles.css";
type Status={phase:string;url:string|null;paused:boolean;draining:boolean;active_jobs:number;error:string|null;data_directory:string;adapters_enabled:boolean;ai_enabled:boolean};
const labels:Record<string,string>={starting:"正在启动",running:"运行中",stopping:"正在安全退出",stopped:"已停止",failed:"需要处理"};
function App(){
 const [status,setStatus]=useState<Status>();const [error,setError]=useState("");const [busy,setBusy]=useState(false);const [auto,setAuto]=useState(false);const [account,setAccount]=useState(false);const [email,setEmail]=useState("");const [password,setPassword]=useState("");const [created,setCreated]=useState(false);
 useEffect(()=>{let mounted=true;const read=()=>invoke<Status>("runtime_status").then(s=>{if(mounted)setStatus(s)}).catch(e=>{if(mounted)setError(String(e))});void read();void invoke<boolean>("autostart_enabled").then(setAuto).catch(e=>setError(String(e)));const timer=setInterval(read,1500);return()=>{mounted=false;clearInterval(timer)}},[]);
 async function action(name:string){setBusy(true);setError("");try{setStatus(await invoke<Status>("runtime_action",{action:name}))}catch(e){setError(String(e))}finally{setBusy(false)}}
 async function command(name:string,args?:Record<string,unknown>){setError("");try{await invoke(name,args)}catch(e){setError(String(e))}}
 async function bootstrap(event:React.FormEvent){event.preventDefault();setBusy(true);setError("");try{await invoke("create_account",{email,password});setPassword("");setCreated(true);setAccount(false)}catch(e){setError(String(e))}finally{setBusy(false)}}
 const ready=status?.phase==="running";const waiting=busy||status?.phase==="starting"||status?.phase==="stopping";
 return <main><header><div className="mark">M</div><div><h1>Monica Rust</h1><p>关系档案，保留在你的 Mac。</p></div></header>
 <section className="runtime" aria-label="运行状态"><div className="status"><span className={ready?"dot ready":"dot"}/><h2>{status?.draining?"正在暂停后台任务":status?.paused?"后台任务已暂停":labels[status?.phase??"starting"]}</h2></div><p>{ready?"浏览器和数据库已就绪。关闭此窗口后，菜单栏继续运行。":"数据库与服务将使用独立的 Monica Rust 数据目录。"}</p>{status?.url&&<code>{status.url}</code>}
 <div className="actions"><button className="primary" disabled={!ready} onClick={()=>void command("open_monica")}>打开 Monica</button>{ready?<button disabled={waiting} onClick={()=>void action(status?.paused?"resume":"pause")}>{status?.paused?"恢复后台任务":"暂停后台任务"}</button>:<button disabled={waiting} onClick={()=>void action("start")}>重新启动</button>}<button onClick={()=>void command("open_logs")}>查看日志</button></div>
 {(error||status?.error)&&<p className="error" role="alert">{error||status?.error}</p>}
 {ready&&<p className="minor">正在执行的后台任务：{status?.active_jobs??0}</p>}</section>
 <section><h2>本地账号</h2><p>首次使用时创建账号，再到浏览器中登录。已有账号可直接打开 Monica。</p>{created&&<p role="status">账号已创建，可以在浏览器登录。</p>}{account?<form onSubmit={bootstrap}><label>邮箱<input type="email" autoComplete="username" value={email} onChange={e=>setEmail(e.target.value)} required spellCheck={false}/></label><label>密码<input type="password" minLength={12} autoComplete="new-password" value={password} onChange={e=>setPassword(e.target.value)} required/></label><div className="actions"><button type="submit" className="primary" disabled={waiting||!ready}>创建账号</button><button type="button" onClick={()=>{setAccount(false);setPassword("")}}>取消</button></div></form>:<button disabled={!ready} onClick={()=>setAccount(true)}>创建首次使用账号</button>}</section>
 <section><h2>后台与启动</h2><label className="check"><input type="checkbox" checked={auto} onChange={async e=>{const enabled=e.target.checked;try{await invoke("set_autostart",{enabled});setAuto(enabled)}catch(error){setError(String(error))}}}/>登录 Mac 后自动启动</label><p>暂停会停止领取新任务，正在执行的工作完成后保留检查点。浏览已有档案仍可继续。</p><p className="minor">本测试版默认不启动真实采集器或模型调用。采集适配器的连接设置尚未接入此窗口。</p></section>
 <footer><span title={status?.data_directory}>{status?.data_directory||"正在读取本地状态…"}</span><button disabled={waiting} onClick={()=>void command("quit_app")}>退出 Monica Rust</button></footer>
 </main>;
}
createRoot(document.getElementById("root")!).render(<App/>);
