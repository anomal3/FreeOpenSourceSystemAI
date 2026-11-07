//! /bin/jsprobe — SpiderMonkey на FreeOS (шаг N4 плана «Servo нативно»).
//!
//! Поднимает движок (`JS_Init`), рантайм и глобальный объект, вычисляет
//! несколько выражений и печатает строки `jsprobe: ...`; последняя —
//! `jsprobe: ok`, если всё сошлось.

use std::ptr;

use mozjs::jsapi::OnNewGlobalHookOption;
use mozjs::jsval::UndefinedValue;
use mozjs::rooted;
use mozjs::rust::wrappers2::*;
use mozjs::rust::{evaluate_script, CompileOptionsWrapper, JSEngine, RealmOptions, Runtime, SIMPLE_GLOBAL_CLASS};

fn main() {
    println!("jsprobe: init");
    let engine = JSEngine::init().expect("JS_Init");
    println!("jsprobe: engine");
    let mut rt = Runtime::new(engine.handle());
    println!("jsprobe: runtime");

    let options = RealmOptions::default();
    rooted!(&in(rt.cx()) let global = unsafe {
        JS_NewGlobalObject(rt.cx(), &SIMPLE_GLOBAL_CLASS, ptr::null_mut(),
                           OnNewGlobalHookOption::FireOnNewGlobalHook, &*options)
    });
    println!("jsprobe: global");

    let cases: [(&str, i32); 4] = [
        ("1 + 1", 2),
        ("let s = 0; for (let i = 1; i <= 100; i++) s += i; s", 5050),
        ("[3, 1, 2].sort().map(x => x * 10).reduce((a, b) => a + b) + JSON.parse('{\"a\": 4}').a", 64),
        // Intl (ICU4X): разделитель тысяч по локали и сравнение по-немецки.
        (
            "(new Intl.NumberFormat('en-US').format(1234.5) === '1,234.5' ? 1 : 0) + \
             (['z', 'ä', 'a'].sort(new Intl.Collator('de').compare).join('') === 'aäz' ? 2 : 0)",
            3,
        ),
    ];
    let mut ok = true;
    for (n, (src, want)) in cases.iter().enumerate() {
        rooted!(&in(rt.cx()) let mut rval = UndefinedValue());
        let opts = CompileOptionsWrapper::new(rt.cx_no_gc(), c"jsprobe.js".to_owned(), 1);
        let res = evaluate_script(rt.cx(), global.handle(), src, rval.handle_mut(), opts);
        let got = if res.is_ok() && rval.get().is_int32() { Some(rval.get().to_int32()) } else { None };
        println!("jsprobe: case {n} = {got:?} (want {want})");
        ok &= got == Some(*want);
    }
    println!("jsprobe: {}", if ok { "ok" } else { "FAIL" });
}
