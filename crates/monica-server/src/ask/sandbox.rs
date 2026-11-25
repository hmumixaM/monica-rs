//! Fresh WASM instance per script. No WASI, and the only data import is the
//! bounded JSON bridge. Every address received from the guest is bounds-checked.
use anyhow::{Context, Result, anyhow, ensure};
use std::sync::OnceLock;
use wasmi::{
    AsContext, AsContextMut, Caller, Config, Engine, Error, ExternType, Linker, Memory, MemoryType,
    Module, Store, StoreLimits, StoreLimitsBuilder, Val,
};

const MAX_JSON: usize = 65_536;
const MAX_OUTPUT: usize = 48_000;
const MAX_MEMORY: usize = 64 * 1024 * 1024;
// An instruction budget bounds guest CPU without charging for host I/O
// or time the operating system spends running other Monica work.
const FUEL: u64 = 160_000_000;
const ASSET: &[u8] = include_bytes!("../../assets/quickjs/quickjs.wasm");
type Bridge<'a> = dyn FnMut(&str) -> Result<String> + 'a;

struct Guest<'a> {
    bridge: &'a mut Bridge<'a>,
    memory: Option<Memory>,
    instance: Option<wasmi::Instance>,
    limits: StoreLimits,
}

fn module() -> Result<&'static Module> {
    static MODULE: OnceLock<Result<Module, String>> = OnceLock::new();
    MODULE
        .get_or_init(|| {
            let engine = Engine::new(Config::default().consume_fuel(true));
            Module::new(&engine, ASSET).map_err(|err| err.to_string())
        })
        .as_ref()
        .map_err(|_| anyhow!("无法加载 JavaScript 沙箱。"))
}

pub(super) fn execute(source: &str, bridge: &mut Bridge<'_>) -> Result<String> {
    ensure!(source.len() <= 32_000, "JavaScript 查询脚本过长。");
    let module = module()?;
    let mut store = Store::new(
        module.engine(),
        Guest {
            bridge,
            memory: None,
            instance: None,
            limits: StoreLimitsBuilder::new().memory_size(MAX_MEMORY).build(),
        },
    );
    store.limiter(|host| &mut host.limits);
    store.set_fuel(FUEL)?;
    let memory = Memory::new(&mut store, MemoryType::new(256, Some(32_768)))?;
    store.data_mut().memory = Some(memory);
    let mut linker = Linker::new(module.engine());
    linker.define("a", "a", memory)?;
    for import in module.imports() {
        let ExternType::Func(ty) = import.ty() else {
            continue;
        };
        let action = match import.name() {
            "s" => callback,
            "k" => grow_memory,
            "p" => clock,
            "n" => timezone,
            "m" => localtime,
            "f" => empty_environment,
            "t" => free_host_reference,
            "e" => read_empty_environment,
            "d" => fd_not_supported,
            "o" => fd_stat_not_supported,
            // Abort/assertion, output, module loading, timers and system calls
            // have no host capabilities. Reject unsupported calls at the boundary.
            "b" | "c" | "g" | "h" | "i" | "j" | "l" | "q" | "r" => denied,
            _ => return Err(anyhow!("JavaScript 沙箱声明了未知导入。")),
        };
        ensure!(import.module() == "a", "JavaScript 沙箱声明了未知导入。");
        let name = import.name().to_owned();
        linker.func_new(
            "a",
            import.name(),
            ty.clone(),
            move |caller, args, result| {
                action(caller, args, result)
                    .map_err(|error| Error::new(format!("guest import {name}: {error}")))
            },
        )?;
    }
    let instance = linker.instantiate_and_start(&mut store, module)?;
    store.data_mut().instance = Some(instance);

    // Alias mapping comes from the pinned Emscripten release, see assets/quickjs.
    instance
        .get_typed_func::<(), ()>(&store, "u")?
        .call(&mut store, ())?;
    let runtime = instance
        .get_typed_func::<(), i32>(&store, "K")?
        .call(&mut store, ())?;
    instance
        .get_typed_func::<(i32, i32), ()>(&store, "y")?
        .call(&mut store, (runtime, 32 * 1024 * 1024))?;
    let ctx = instance
        .get_typed_func::<(i32, i32), i32>(&store, "N")?
        .call(&mut store, (runtime, 0))?;
    let name = put_string(&mut store, "__lookup")?;
    let function = instance
        .get_typed_func::<(i32, i32, i32, i32, i32), i32>(&store, "Da")?
        .call(&mut store, (ctx, name, 1, 0, 1))?;
    let key = js_string(&mut store, ctx, "__lookup")?;
    let global = instance
        .get_typed_func::<i32, i32>(&store, "ua")?
        .call(&mut store, ctx)?;
    instance
        .get_typed_func::<(i32, i32, i32, i32), ()>(&store, "ja")?
        .call(&mut store, (ctx, global, key, function))?;

    // Compile the model's script separately from the bridge. Its source cannot
    // change the wrapper's control flow or bypass the host allowlist.
    let program = format!("{WRAPPER}\nrun({});", serde_json::to_string(source)?);
    let input = put_string(&mut store, &program)?;
    let filename = put_string(&mut store, "monica-query.js")?;
    let result = instance
        .get_typed_func::<(i32, i32, i32, i32, i32, i32), i32>(&store, "pa")?
        .call(
            &mut store,
            (ctx, input, program.len() as i32, filename, 0, 0),
        )
        .map_err(|_| anyhow!("JavaScript 达到运行限制，或调用了不支持的功能。"))?;
    let exception = instance
        .get_typed_func::<(i32, i32), i32>(&store, "na")?
        .call(&mut store, (ctx, result))?;
    if exception != 0 {
        let dump = instance
            .get_typed_func::<(i32, i32), i32>(&store, "oa")?
            .call(&mut store, (ctx, exception))?;
        let detail = c_string(&store, dump, 8192)
            .ok()
            .and_then(|text| serde_json::from_str::<serde_json::Value>(text).ok())
            .and_then(|value| {
                value["message"]
                    .as_str()
                    .map(|message| message.chars().take(300).collect::<String>())
            })
            .unwrap_or_else(|| "JavaScript 查询执行失败或超时。".into());
        return Err(anyhow!(detail));
    }
    let ptr = instance
        .get_typed_func::<(i32, i32), i32>(&store, "$")?
        .call(&mut store, (ctx, result))?;
    c_string(&store, ptr, MAX_OUTPUT + 1)
        .map(str::to_owned)
        .context("脚本输出过长。请先用 JavaScript 汇总，再返回需要的资料。")
}

const WRAPPER: &str = r#"
function run(source) {
    const lookup = globalThis.__lookup;
    delete globalThis.__lookup;
    const parse = JSON.parse;
    const stringify = JSON.stringify;
    const request = (command, args) => parse(lookup(stringify([command, args])));
    const cli = (command, args = {}, project) => {
        if (command !== 'batch') return request(command, args);
        if (!Array.isArray(args) || args.length > 240) throw new Error('batch 最多接受 240 项查询。');
        if (project !== undefined && typeof project !== 'function') throw new Error('batch 的第三个参数必须是提取函数。');
        const rows = [];
        for (let i = 0; i < args.length; i += 6) {
            const chunk = request('batch', args.slice(i, i + 6));
            if (!Array.isArray(chunk)) throw new Error(chunk.error);
            for (let j = 0; j < chunk.length; j++) {
                rows.push(project ? project(chunk[j], i + j) : chunk[j]);
            }
        }
        return rows;
    };
    const program = new Function('cli', '"use strict";\n' + source);
    const result = program(cli);
    if (result && typeof result.then === 'function') throw new Error('cli 是同步的，请直接 return 查询结果。');
    return stringify(result ?? null) ?? 'null';
}
"#;

fn c_string<'a, 'guest: 'a>(
    store: &'a impl AsContext<Data = Guest<'guest>>,
    ptr: i32,
    max: usize,
) -> Result<&'a str> {
    let store = store.as_context();
    ensure!(ptr > 0, "Invalid guest pointer");
    let data = store.data().memory.unwrap().data(store);
    let bytes = data.get(ptr as usize..).context("Invalid guest pointer")?;
    let end = bytes
        .iter()
        .take(max)
        .position(|byte| *byte == 0)
        .context("Guest string is too long")?;
    Ok(std::str::from_utf8(&bytes[..end])?)
}
fn put_string<'a>(store: &mut impl AsContextMut<Data = Guest<'a>>, text: &str) -> Result<i32> {
    let mut store = store.as_context_mut();
    let host = store.data();
    let (instance, memory) = (host.instance.unwrap(), host.memory.unwrap());
    let ptr = instance
        .get_typed_func::<i32, i32>(&store, "v")?
        .call(&mut store, (text.len() + 1) as i32)?;
    ensure!(ptr > 0, "Guest allocation failed");
    memory.write(&mut store, ptr as usize, text.as_bytes())?;
    memory.write(&mut store, ptr as usize + text.len(), &[0])?;
    Ok(ptr)
}
fn js_string<'a>(
    store: &mut impl AsContextMut<Data = Guest<'a>>,
    ctx: i32,
    text: &str,
) -> Result<i32> {
    let ptr = put_string(store, text)?;
    let mut store = store.as_context_mut();
    let instance = store.data().instance.unwrap();
    let result = instance
        .get_typed_func::<(i32, i32), i32>(&store, "_")?
        .call(&mut store, (ctx, ptr))?;
    instance
        .get_typed_func::<i32, ()>(&store, "M")?
        .call(&mut store, ptr)?;
    Ok(result)
}

fn lookup(mut caller: Caller<'_, Guest<'_>>, args: &[Val]) -> Result<i32> {
    let ctx = args[0].i32().unwrap();
    ensure!(
        args[2].i32() == Some(1) && args[4].i32() == Some(1),
        "Invalid lookup"
    );
    let argv = args[3].i32().unwrap();
    let instance = caller.data().instance.unwrap();
    let value = instance
        .get_typed_func::<(i32, i32), i32>(&caller, "Ea")?
        .call(&mut caller, (argv, 0))?;
    let ptr = instance
        .get_typed_func::<(i32, i32), i32>(&caller, "$")?
        .call(&mut caller, (ctx, value))?;
    let text = c_string(&caller, ptr, MAX_JSON).map(str::to_owned);
    instance
        .get_typed_func::<(i32, i32), ()>(&caller, "S")?
        .call(&mut caller, (ctx, ptr))?;
    let response = match text {
        Ok(text) => (caller.data_mut().bridge)(&text)?,
        Err(_) => r#"{"error":"查询参数过长或格式无效。"}"#.into(),
    };
    ensure!(response.len() <= 2 * 1024 * 1024, "查询结果过长。");
    js_string(&mut caller, ctx, &response)
}

fn callback(caller: Caller<'_, Guest<'_>>, args: &[Val], out: &mut [Val]) -> Result<(), Error> {
    out[0] = Val::I32(lookup(caller, args).map_err(|_| Error::new("query interrupted"))?);
    Ok(())
}
fn denied(_: Caller<'_, Guest<'_>>, _: &[Val], _: &mut [Val]) -> Result<(), Error> {
    Err(Error::new("unsupported guest host call"))
}
fn free_host_reference(_: Caller<'_, Guest<'_>>, _: &[Val], _: &mut [Val]) -> Result<(), Error> {
    Ok(())
}
// environ_sizes_get above advertised zero entries, so there is nothing to copy.
fn read_empty_environment(
    _: Caller<'_, Guest<'_>>,
    _: &[Val],
    out: &mut [Val],
) -> Result<(), Error> {
    out[0] = Val::I32(0);
    Ok(())
}
fn fd_not_supported(_: Caller<'_, Guest<'_>>, _: &[Val], out: &mut [Val]) -> Result<(), Error> {
    out[0] = Val::I32(52);
    Ok(())
}
fn fd_stat_not_supported(
    _: Caller<'_, Guest<'_>>,
    _: &[Val],
    out: &mut [Val],
) -> Result<(), Error> {
    out[0] = Val::I32(70);
    Ok(())
}
fn grow_memory(
    mut caller: Caller<'_, Guest<'_>>,
    args: &[Val],
    out: &mut [Val],
) -> Result<(), Error> {
    let memory = caller.data().memory.unwrap();
    let pages = u64::from(args[0].i32().unwrap() as u32).div_ceil(65_536);
    let extra = pages.saturating_sub(memory.size(&caller));
    out[0] = Val::I32(i32::from(memory.grow(&mut caller, extra).is_ok()));
    Ok(())
}
fn clock(_: Caller<'_, Guest<'_>>, _: &[Val], out: &mut [Val]) -> Result<(), Error> {
    out[0] = Val::F64((chrono::Utc::now().timestamp_millis() as f64).into());
    Ok(())
}
fn write<'a>(
    mut caller: impl AsContextMut<Data = Guest<'a>>,
    address: &Val,
    bytes: &[u8],
) -> Result<(), Error> {
    let memory = caller.as_context().data().memory.unwrap();
    memory
        .write(&mut caller, address.i32().unwrap() as u32 as usize, bytes)
        .map_err(|_| Error::new("invalid guest address"))
}
fn timezone(mut caller: Caller<'_, Guest<'_>>, args: &[Val], _: &mut [Val]) -> Result<(), Error> {
    write(&mut caller, &args[0], &[0; 4])?;
    write(&mut caller, &args[1], &[0; 4])?;
    write(&mut caller, &args[2], b"UTC\0")?;
    write(&mut caller, &args[3], b"UTC\0")
}
fn localtime(mut caller: Caller<'_, Guest<'_>>, args: &[Val], _: &mut [Val]) -> Result<(), Error> {
    use chrono::{Datelike, Timelike};
    let time = chrono::DateTime::from_timestamp(args[0].i64().unwrap(), 0)
        .ok_or_else(|| Error::new("invalid date"))?;
    let fields = [
        time.second() as i32,
        time.minute() as i32,
        time.hour() as i32,
        time.day() as i32,
        time.month0() as i32,
        time.year() - 1900,
        time.weekday().num_days_from_sunday() as i32,
        time.ordinal0() as i32,
        0,
        0,
    ];
    let bytes: Vec<_> = fields.into_iter().flat_map(i32::to_le_bytes).collect();
    write(&mut caller, &args[1], &bytes)
}
fn empty_environment(
    mut caller: Caller<'_, Guest<'_>>,
    args: &[Val],
    out: &mut [Val],
) -> Result<(), Error> {
    write(&mut caller, &args[0], &[0; 4])?;
    write(&mut caller, &args[1], &[0; 4])?;
    out[0] = Val::I32(0);
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sandbox_uses_only_its_json_lookup_and_stops_runaway_code() {
        let mut lookups = 0;
        let mut bridge = |input: &str| {
            lookups += 1;
            assert_eq!(input, r#"["directory",{"tags":["QD"]}]"#);
            Ok(r#"{"total":12}"#.to_owned())
        };
        let result = execute(
            r#"return [cli("directory", {tags:["QD"]}).total,
            typeof process, typeof require, typeof fetch, typeof WebAssembly, typeof __lookup];"#,
            &mut bridge,
        )
        .unwrap();
        assert_eq!(
            result,
            r#"[12,"undefined","undefined","undefined","undefined","undefined"]"#
        );
        assert_eq!(lookups, 1);
        assert!(execute("while (true) {}", &mut |_| unreachable!()).is_err());
        assert!(
            execute("throw new Error('测试失败')", &mut |_| unreachable!())
                .unwrap_err()
                .to_string()
                .contains("测试失败")
        );
    }
    #[test]
    fn script_projects_each_slice_of_a_large_batch() {
        let mut issued = 0;
        let output = execute(
            "return cli('batch', Array.from({length:240}, (_, index) => ({command:'read_messages', args:{index}})), (row, index) => row.index === index);",
            &mut |input| {
                let (command, rows): (String, Vec<serde_json::Value>) = serde_json::from_str(input)?;
                assert_eq!(command, "batch");
                assert_eq!(rows.len(), 6);
                let data = rows.iter().map(|row| row["args"].clone()).collect::<Vec<_>>();
                issued += data.len();
                Ok(serde_json::to_string(&data)?)
            },
        ).unwrap();
        assert_eq!(issued, 240);
        assert_eq!(
            serde_json::from_str::<Vec<bool>>(&output).unwrap(),
            vec![true; 240]
        );
    }
}
