//! Standalone proof that the `cwd_get_suggested` Preview 1 hostcall added to
//! `wasi-common` round-trips a host-provided hint string through the guest and
//! back, and returns `errno::nosys` when no hint is configured.
//!
//! Run with: `cargo run --example cwd_hint_proof`
//!
//! This is the load-bearing local verification for Slice A of the
//! `cwd-hint-demo` reference implementation. CI on the `cwd-hint` branch of
//! the fork executes this example and asserts both branches behave as the
//! proposal describes.

use wasi_common::sync::{WasiCtxBuilder, add_to_linker};
use wasmtime::{Config, Engine, Linker, Module, Result, Store};

/// Tiny guest module that calls `cwd_get_suggested` into a fixed buffer at
/// linear-memory offset 0, then exposes the returned length and a byte-reader
/// so the host can verify the round-trip.
///
/// The guest writes the errno value into linear memory at offset 0x1000 when
/// the hostcall fails, so the host can distinguish "wrote 0 bytes" from
/// "hostcall returned an error" — the former should never happen for the set
/// hint, the latter is the documented backward-compat path when no hint is
/// set.
const GUEST_WAT: &str = r#"
(module
  (import "wasi_snapshot_preview1" "cwd_get_suggested"
    (func $cwd_get_suggested (param i32 i32 i32) (result i32)))
  (memory (export "memory") 1)
  (global $written_size (mut i32) (i32.const -1))
  (global $errno_code   (mut i32) (i32.const 0))

  ;; call_hostcall: invokes cwd_get_suggested(buf=0, buf_len=4096,
  ;; written_size_ptr=0x800). Stashes the errno-code in $errno_code (0 on
  ;; success), and on success copies the written length from 0x800 into
  ;; $written_size.
  (func $call_hostcall (export "call_hostcall")
    (local $rc i32)
    (local.set $rc
      (call $cwd_get_suggested
        (i32.const 0)        ;; buf
        (i32.const 4096)     ;; buf_len
        (i32.const 0x800)))  ;; out: size written
    (global.set $errno_code (local.get $rc))
    (if (i32.eqz (local.get $rc))
      (then
        (global.set $written_size (i32.load (i32.const 0x800)))))
  )

  (func (export "written_size") (result i32)
    global.get $written_size)
  (func (export "errno_code") (result i32)
    global.get $errno_code)
  (func (export "read_byte") (param $i i32) (result i32)
    (i32.load8_u (local.get $i)))
)
"#;

fn main() -> Result<()> {
    let config = Config::new();
    let engine = Engine::new(&config)?;

    let module = Module::new(&engine, GUEST_WAT)?;

    // ---- branch 1: hint configured -----------------------------------------
    {
        let mut linker: Linker<wasi_common::WasiCtx> = Linker::new(&engine);
        add_to_linker(&mut linker, |s| s)?;
        let mut builder = WasiCtxBuilder::new();
        builder.cwd_hint("/some/path");
        let ctx = builder.build();
        let mut store = Store::new(&engine, ctx);

        let instance = linker.instantiate(&mut store, &module)?;
        let call =
            instance.get_typed_func::<(), ()>(&mut store, "call_hostcall")?;
        call.call(&mut store, ())?;

        let errno = instance
            .get_typed_func::<(), i32>(&mut store, "errno_code")?
            .call(&mut store, ())?;
        let size = instance
            .get_typed_func::<(), i32>(&mut store, "written_size")?
            .call(&mut store, ())?;
        let read_byte =
            instance.get_typed_func::<i32, i32>(&mut store, "read_byte")?;

        assert_eq!(
            errno, 0,
            "with hint set, hostcall must succeed (got errno={errno})"
        );
        assert_eq!(
            size, 10,
            "expected 10 bytes of \"/some/path\", got {size}"
        );
        let mut buf = Vec::with_capacity(size as usize);
        for i in 0..size {
            let b = read_byte.call(&mut store, i)? as u8;
            buf.push(b);
        }
        let got = std::str::from_utf8(&buf)?;
        assert_eq!(got, "/some/path", "round-trip string mismatch: {got:?}");
        println!("[hint-set]   errno=0 size={size} got={got:?}  OK");
    }

    // ---- branch 2: no hint configured (must report errno::nosys) -----------
    {
        let mut linker: Linker<wasi_common::WasiCtx> = Linker::new(&engine);
        add_to_linker(&mut linker, |s| s)?;
        let ctx = WasiCtxBuilder::new().build();
        let mut store = Store::new(&engine, ctx);

        let instance = linker.instantiate(&mut store, &module)?;
        let call =
            instance.get_typed_func::<(), ()>(&mut store, "call_hostcall")?;
        call.call(&mut store, ())?;

        let errno = instance
            .get_typed_func::<(), i32>(&mut store, "errno_code")?
            .call(&mut store, ())?;

        // Errno::Nosys is the documented backward-compat path. Its numeric
        // value in the WASI Preview 1 errno enum is 52 (see typenames.witx).
        const ERRNO_NOSYS: i32 = 52;
        assert_eq!(
            errno, ERRNO_NOSYS,
            "without hint, hostcall must return errno::nosys (got {errno})"
        );
        println!("[hint-unset] errno={errno} (nosys)  OK");
    }

    println!("cwd_get_suggested round-trip + backward-compat: PASS");
    Ok(())
}
