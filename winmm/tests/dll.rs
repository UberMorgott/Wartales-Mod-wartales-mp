//! Loads the built winmm.dll by path, as the game does, and checks that it
//! exports exactly what System32\winmm.dll exports and forwards calls there.

use std::collections::BTreeSet;
use std::ffi::CStr;
use std::os::windows::ffi::OsStrExt;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use windows_sys::Win32::Foundation::HMODULE;
use windows_sys::Win32::System::LibraryLoader::{GetProcAddress, LoadLibraryW};
use winmm::pe::{exports, Export};

type U32Fn = unsafe extern "system" fn() -> u32;

fn load(p: &Path) -> HMODULE {
    let wide: Vec<u16> = p.as_os_str().encode_wide().chain([0]).collect();
    // SAFETY: wide is NUL-terminated.
    let h = unsafe { LoadLibraryW(wide.as_ptr()) };
    assert!(!h.is_null(), "LoadLibraryW({})", p.display());
    h
}

/// A `u32 f(void)` export of m.
fn u32_fn(m: HMODULE, name: &CStr) -> U32Fn {
    // SAFETY: m is a loaded module, name NUL-terminated.
    let f = unsafe { GetProcAddress(m, name.as_ptr().cast()) }.expect("export missing");
    // SAFETY: both winmm functions tested here take nothing and return a u32.
    unsafe { std::mem::transmute::<unsafe extern "system" fn() -> isize, U32Fn>(f) }
}

#[test]
fn forwards_to_system32() {
    let deps = std::env::current_exe().unwrap();
    let dll = deps.parent().unwrap().parent().unwrap().join("winmm.dll");
    let root = std::env::var_os("SystemRoot").unwrap_or_else(|| r"C:\Windows".into());
    let sys = PathBuf::from(root).join(r"System32\winmm.dll");

    let ours = exports(&std::fs::read(&dll).unwrap()).unwrap();
    let theirs = exports(&std::fs::read(&sys).unwrap()).unwrap();
    let names = |e: &[Export]| {
        e.iter()
            .filter_map(|e| e.name.clone())
            .collect::<BTreeSet<_>>()
    };
    let noname = |e: &[Export]| {
        e.iter()
            .filter(|e| e.name.is_none())
            .map(|e| e.ordinal)
            .collect::<Vec<_>>()
    };
    assert_eq!(names(&ours), names(&theirs), "named exports differ");
    assert_eq!(noname(&ours), noname(&theirs), "NONAME ordinals differ");
    println!(
        "exports: {} named + {:?} NONAME, equal to System32",
        names(&ours).len(),
        noname(&ours)
    );

    // shim.log goes to a fresh LOCALAPPDATA; the only test in this binary.
    let tmp = std::env::temp_dir().join(format!("winmm-dll-test-{}", std::process::id()));
    std::env::set_var("LOCALAPPDATA", &tmp);
    let h = load(&dll);
    let real = load(&sys);
    assert_ne!(h, real, "loaded System32's copy instead of the built one");

    // SAFETY: winmm functions without arguments.
    let (t_ours, t_real) = unsafe { (u32_fn(h, c"timeGetTime")(), u32_fn(real, c"timeGetTime")()) };
    assert!(
        t_real.wrapping_sub(t_ours) < 1000,
        "timeGetTime {t_ours} vs {t_real}"
    );
    // SAFETY: as above.
    let (w_ours, w_real) = unsafe {
        (
            u32_fn(h, c"waveOutGetNumDevs")(),
            u32_fn(real, c"waveOutGetNumDevs")(),
        )
    };
    assert_eq!(w_ours, w_real, "waveOutGetNumDevs");

    let log = tmp.join("wartales-mp").join("shim.log");
    let start = Instant::now();
    let text = loop {
        let text = std::fs::read_to_string(&log).unwrap_or_default();
        if text.contains("worker: started") || start.elapsed() > Duration::from_secs(5) {
            break text;
        }
        std::thread::sleep(Duration::from_millis(20));
    };
    let bound = format!("forwards: {} exports bound, 0 missing", theirs.len());
    for want in [
        "attach: winmm.dll proxy loaded",
        &bound,
        "hook kernelbase!CreateFileW: installed at ",
        "hook kernelbase!CreateFileA: installed at ",
        "worker: started",
    ] {
        assert!(text.contains(want), "shim.log lacks {want:?}:\n{text}");
    }

    // The CreateFileW hook: an unpatchable hlboot.dat is handed out untouched,
    // and a second open does not patch again.
    let game = tmp.join("game");
    std::fs::create_dir_all(&game).unwrap();
    let boot = game.join("hlboot.dat");
    std::fs::write(&boot, b"HLB\x09not a supported image").unwrap();
    for _ in 0..2 {
        assert_eq!(
            std::fs::read(&boot).unwrap(),
            b"HLB\x09not a supported image"
        );
    }
    let text = std::fs::read_to_string(&log).unwrap();
    for want in [
        "bytecode: open ",
        "patch: bytecode format 9, want 4 or 5, image left untouched",
        "bytecode: handing out the untouched original",
        "bytecode: already refused in this process",
    ] {
        assert!(text.contains(want), "shim.log lacks {want:?}:\n{text}");
    }
    let _ = std::fs::remove_dir_all(&tmp);
}
