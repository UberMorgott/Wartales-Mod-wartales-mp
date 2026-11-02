# single-dll checklist

Design: [design-single-dll.md](design-single-dll.md). Branch `single-dll`, worktree
`E:\DEV\Wartales\mp-sd`. Tick as done; a continuation resumes from the first open box.

## Done on main before the branch

- [x] helper crate (Rust) linked as the one staticlib; patcher C ABI inside (7896543)
- [x] Rust ports on main: applog conn log code uid modver nat sendq wsx relay sdrbridge install hlpatch gendef hlpatchgen (5ff866e..1fc10e8)
- [x] link port: branch worktree-agent-a89d44eb2e00a3506 commit 2170700 (not on main)
- [x] punch port (kcp, conv per stream): branch worktree-agent-ac61f52afbc3c1215 commit 08ada98 (not on main)

## Steps

- [ ] 0. worktree + branch `single-dll`; merge link + punch ports
- [ ] 1. lint gate `tools/lint.ps1` (fmt, clippy pedantic -D warnings, cargo-deny, undocumented_unsafe_blocks); cdylib `winmm` skeleton: build.rs thunks + .def, DllMain, log; export table == C build
- [ ] 2. bytecode path: CreateFileW hook, needles, passes, splash, hl_sys_print game log
- [ ] 3. Steam/SDR natives + lobby advert; shimcheck scenarios as Rust tests
- [ ] 4. master_config pass (loopback + ssl:false); master/relay/link/punch/nat in-process; drop install/TLS/ring
- [ ] 5. delete Go, C shim, MinHook, embed/UPX; docs (README, DESIGN, TECHNICAL)
- [ ] 6. harness: code join, host save load + rejoin, basic play, punch route; no exe / no wartales-mp.exe process / no %ProgramData% certs
- [ ] 7. every patcher pass -> evidence (table below)
- [ ] 8. merge to main; final dist\winmm.dll size + SHA256

## Found in old code

| What | Impact | Fixed how |
| --- | --- | --- |
| `TcpStream` shutdown does not wake a blocked recv on Windows (Rust port; Go used closesocket) | close() could hang a reader | `conn::close_tcp` = shutdown + CancelIoEx (1fc10e8) |
| nat: `Verified` kept after the WAN address changed (Go comment says it must reset) | stale "verified" endpoint picks direct for a new address | reset on change (Rust nat port) |
| nat: STUN attribute without padding panicked (Go) | helper crash on a malformed STUN reply | bounds-checked (Rust nat port) |

## Pass -> evidence

| Pass | Evidence | Status |
| --- | --- | --- |
