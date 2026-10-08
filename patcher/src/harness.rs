// Copyright (c) 2026 Morgott. Licensed under CC BY-NC 4.0.
//
// Local co-op test harness: a command file channel (TEST BUILD ONLY: compiled
// with the cargo feature `harness`, i.e. `shim\build.ps1 -Test`; marker
// WMP_TEST_SEAM; the release dll carries none of it, shim\check.ps1 proves it).
//
// One appended function, `harnessTick()`, called at op 0 of hxd.App.mainLoop
// (every frame, title screen and game alike):
//
//   state 0 (first frame): WARTALES_MP_TEST_INSTANCE unset -> state 1 (off,
//     one global read per frame from then on); else dir =
//     %LOCALAPPDATA%\wartales-mp\harness\ (the driver creates it), state 2.
//   state 2, every 6th frame, inside a Trap:
//     if exists(dir+"cmd.txt"): c = getContent, deleteFile;
//     c = "<seq> <verb> [arg]"   (the driver writes cmd.tmp, then renames)
//       console <line>  $Game.inst.console: PREFS.admin forced true around
//                       resetCommands + runCommand (Battle/Cheats console
//                       modes are enabled only for get_isAdmin() && canCheat(),
//                       canCheat() is true for an admin), then restored and the
//                       command set rebuilt (also when the command throws). Ack "dispatched": the console
//                       shows a bad command on its own screen only.
//       dump            JSON of the game state -> dir+"state.json"
//     dir+"ack.txt" = "<seq> ok|dispatched|unknown|nogame", and a shim.log line.
//   An exception: "harness: error <exc>" and ack "<seq> error".
//
// Validated before editing; a mismatch skips the pass (logged).

use super::asm::{push_fn, Asm, Regs};
use super::diag::static_fn;
use super::job_xp::str_global;
use super::*;
use hlbc::types::ValBool;

struct Ctx {
    void_t: RefType,
    i32_t: RefType,
    bool_t: RefType,
    dyn_t: RefType,
    str_t: RefType,
    null_i32_t: RefType,
    get_env: RefFun,
    exists: RefFun,
    get_content: RefFun,
    save_content: RefFun,
    delete_file: RefFun,
    println: RefFun,
    std_string: RefFun,
    add: RefFun,
    index_of: RefFun,
    substr: RefFun,
    json_print: RefFun,
    json_rep_t: RefType,
    main_loop_fi: usize,
    game_g: RefGlobal,
    game_cls_t: RefType,
    game_inst: RefField,
    game_t: RefType,
    main_g: RefGlobal,
    main_cls_t: RefType,
    prefs: (RefField, RefType),
    admin: RefField,
    console: (RefField, RefType),
    reset_commands: RefFun,
    run_command: RefFun,
    // dump paths
    g: GameF,
    unit_t: RefType,
    unit_name: RefField,
    unit_ap: RefField,
    unit_owner: (RefField, RefType),
    player_name: RefField,
    bunit_t: RefType,
    bunit_data: RefField,
    battle_t: RefType,
    battle_cur: RefField,
    state_t: RefType,
    army: (RefField, RefType),
    arr_len: RefField,
    arr_arr: (RefField, RefType),
    dbg: usize,
}

/// Game fields the dump reads.
struct GameF {
    mode: (RefField, RefType),
    bools: Vec<(&'static str, RefField)>,
    players_ready: RefField,
    host: (RefField, RefType),
    me: (RefField, RefType),
    battle: RefField,
    state: RefField,
}

fn want_sig(code: &Bytecode, f: RefFun, args: &[RefType], ret: RefType, what: &str) -> Result<()> {
    let (a, r) = sig(code, f)?;
    if a != args || r != ret {
        bail!("{what}: unexpected signature");
    }
    Ok(())
}

fn plan(code: &Bytecode) -> Result<Ctx> {
    let void_t = prim_type(code, "Void", |t| matches!(t, Type::Void))?;
    let i32_t = prim_type(code, "I32", |t| matches!(t, Type::I32))?;
    let bool_t = prim_type(code, "Bool", |t| matches!(t, Type::Bool))?;
    let dyn_t = prim_type(code, "Dyn", |t| matches!(t, Type::Dyn))?;
    let null_i32_t = prim_type(
        code,
        "Null<I32>",
        |t| matches!(t, Type::Null(x) if *x == i32_t),
    )?;
    let str_t = obj_type(code, "String")?;

    let get_env = static_fn(code, "$Sys", "getEnv")?.findex;
    want_sig(code, get_env, &[str_t], str_t, "Sys.getEnv")?;
    let println = static_fn(code, "$Sys", "println")?.findex;
    want_sig(code, println, &[dyn_t], void_t, "Sys.println")?;
    let exists = static_fn(code, "sys.$FileSystem", "exists")?.findex;
    want_sig(code, exists, &[str_t], bool_t, "FileSystem.exists")?;
    let delete_file = static_fn(code, "sys.$FileSystem", "deleteFile")?.findex;
    want_sig(code, delete_file, &[str_t], void_t, "FileSystem.deleteFile")?;
    let get_content = static_fn(code, "sys.io.$File", "getContent")?.findex;
    want_sig(code, get_content, &[str_t], str_t, "File.getContent")?;
    let save_content = static_fn(code, "sys.io.$File", "saveContent")?.findex;
    want_sig(
        code,
        save_content,
        &[str_t, str_t],
        void_t,
        "File.saveContent",
    )?;
    let std_string = static_fn(code, "$Std", "string")?.findex;
    want_sig(code, std_string, &[dyn_t], str_t, "Std.string")?;
    let add = static_fn(code, "$String", "__add__")?.findex;
    want_sig(code, add, &[str_t, str_t], str_t, "String.__add__")?;
    let index_of = method(code, str_t, "indexOf")?.findex;
    want_sig(
        code,
        index_of,
        &[str_t, str_t, null_i32_t],
        i32_t,
        "String.indexOf",
    )?;
    let substr = method(code, str_t, "substr")?.findex;
    want_sig(
        code,
        substr,
        &[str_t, i32_t, null_i32_t],
        str_t,
        "String.substr",
    )?;
    let json = static_fn(code, "haxe.format.$JsonPrinter", "print")?;
    let json_print = json.findex;
    let json_args = fun_args(code, json);
    if json_args.len() != 3
        || json_args[0] != dyn_t
        || json_args[2] != str_t
        || sig(code, json_print)?.1 != str_t
    {
        bail!("JsonPrinter.print: unexpected signature");
    }
    let json_rep_t = json_args[1];
    let app_t = obj_type(code, "hxd.App")?;
    let ml = method(code, app_t, "mainLoop")?;
    if fun_args(code, ml) != [app_t] || ml.ops.is_empty() {
        bail!("hxd.App.mainLoop: unexpected shape");
    }
    if (0..ml.ops.len()).any(|i| jump_targets(ml, i).contains(&0)) {
        bail!("hxd.App.mainLoop: a jump targets op 0");
    }
    let main_loop_fi = fun_index(code, ml.findex)?;

    let game_t = obj_type(code, "Game")?;
    let (game_g, game_cls_t) = class_global(code, "Game")?;
    let game_inst = typed(code, game_cls_t, "inst", game_t)?;
    let (main_g, main_cls_t) = class_global(code, "Main")?;
    let prefs = field(code, main_cls_t, "PREFS")?;
    let Type::Virtual { fields } = &code.types[prefs.1 .0] else {
        bail!("Main.PREFS is not a virtual");
    };
    let admin = fields
        .iter()
        .position(|f| s(code, f.name) == "admin" && f.t == bool_t)
        .map(RefField)
        .context("PREFS.admin: no Bool field")?;
    let console = field(code, game_t, "console")?;
    let con_t = obj_type(code, "debug.Console")?;
    if console.1 != con_t {
        bail!("Game.console is not a debug.Console");
    }
    let reset_commands = method(code, con_t, "resetCommands")?.findex;
    want_sig(
        code,
        reset_commands,
        &[con_t],
        void_t,
        "Console.resetCommands",
    )?;
    let run_command = method(code, con_t, "runCommand")?.findex;
    want_sig(
        code,
        run_command,
        &[con_t, str_t],
        void_t,
        "Console.runCommand",
    )?;

    let mut bools = vec![];
    for n in [
        "isAuth",
        "isMulti",
        "isCoopGame",
        "connectedToHost",
        "isLoading",
        "hasGameplayStarted",
        "fading",
    ] {
        bools.push((n, typed(code, game_t, n, bool_t)?));
    }
    let player_t = obj_type(code, "ent.BasePlayer")?;
    let battle_t = obj_type(code, "battle.Battle")?;
    let state_t = obj_type(code, "st.GameState")?;
    let g = GameF {
        mode: field(code, game_t, "mode")?,
        bools,
        players_ready: typed(code, game_t, "playersReady", i32_t)?,
        host: field(code, game_t, "host")?,
        me: (typed(code, game_t, "me", player_t)?, player_t),
        battle: typed(code, game_t, "battle", battle_t)?,
        state: typed(code, game_t, "state", state_t)?,
    };
    let unit_t = obj_type(code, "st.Unit")?;
    let bunit_t = obj_type(code, "battle.Unit")?;
    let arr_t = obj_type(code, "hl.types.ArrayObj")?;
    let arr_arr = field(code, arr_t, "array")?;
    if !matches!(code.types[arr_arr.1 .0], Type::Array) {
        bail!("ArrayObj.array is not a native array");
    }
    Ok(Ctx {
        void_t,
        i32_t,
        bool_t,
        dyn_t,
        str_t,
        null_i32_t,
        get_env,
        exists,
        get_content,
        save_content,
        delete_file,
        println,
        std_string,
        add,
        index_of,
        substr,
        json_print,
        json_rep_t,
        main_loop_fi,
        game_g,
        game_cls_t,
        game_inst,
        game_t,
        main_g,
        main_cls_t,
        prefs,
        admin,
        console,
        reset_commands,
        run_command,
        g,
        unit_t,
        unit_name: typed(code, unit_t, "name", str_t)?,
        unit_ap: typed(code, unit_t, "aptitudePoints", i32_t)?,
        unit_owner: (typed(code, unit_t, "owner", player_t)?, player_t),
        player_name: typed(code, player_t, "name", str_t)?,
        bunit_t,
        bunit_data: typed(code, bunit_t, "data", unit_t)?,
        battle_t,
        battle_cur: typed(code, battle_t, "currentUnit", bunit_t)?,
        state_t,
        army: (typed(code, state_t, "army", arr_t)?, arr_t),
        arr_len: typed(code, arr_t, "length", i32_t)?,
        arr_arr,
        dbg: debug_file(code, "hxd/App.hx")?,
    })
}

/// Label names for generated code (a handful per build).
fn lbl(prefix: &str, n: &mut usize) -> &'static str {
    *n += 1;
    Box::leak(format!("{prefix}{n}").into_boxed_str())
}

/// Builder of the dump JSON: `j = j + piece`.
struct J<'a> {
    a: &'a mut Asm,
    c: &'a Ctx,
    j: Reg,
    s: Reg,
    /// JSON value temporaries: `t` the String, `u`/`f` print's null space/replacer.
    t: Reg,
    u: Reg,
    f: Reg,
    d: Reg,
    n: usize,
}

impl J<'_> {
    fn lit(&mut self, code: &mut Bytecode, text: &'static str) {
        let g = str_global(code, self.c.str_t, text);
        self.a.op(Opcode::GetGlobal {
            dst: self.s,
            global: g,
        });
        self.cat(self.s);
    }
    fn cat(&mut self, r: Reg) {
        self.a.op(Opcode::Call2 {
            dst: self.j,
            fun: self.c.add,
            arg0: self.j,
            arg1: r,
        });
    }
    /// Std.string of a plain value (i32/bool): boxed first.
    fn val(&mut self, r: Reg) {
        self.a.op(Opcode::ToDyn {
            dst: self.d,
            src: r,
        });
        self.a.op(Opcode::Call1 {
            dst: self.s,
            fun: self.c.std_string,
            arg0: self.d,
        });
        self.cat(self.s);
    }
    /// `p.name` as a JSON string (or null) for a BasePlayer register.
    fn player(&mut self, code: &mut Bytecode, p: Reg, tmp_s: Reg) {
        let null = lbl("pn", &mut self.n);
        let end = lbl("pe", &mut self.n);
        self.a.jmp(Opcode::JNull { reg: p, offset: 0 }, null);
        self.a.op(Opcode::Field {
            dst: tmp_s,
            obj: p,
            field: self.c.player_name,
        });
        self.str_q(tmp_s);
        self.a.jmp(Opcode::JAlways { offset: 0 }, end);
        self.a.label(null);
        self.lit(code, "null");
        self.a.label(end);
        self.a.op(Opcode::Label);
    }
    /// A String register as a JSON string, escaped; null as null.
    fn str_q(&mut self, r: Reg) {
        self.a.op(Opcode::Mov {
            dst: self.t,
            src: r,
        });
        self.quoted();
    }
    /// Std.string of an object register as a JSON string; null as null.
    fn obj_q(&mut self, r: Reg) {
        self.a.op(Opcode::Mov {
            dst: self.d,
            src: r,
        });
        self.a.op(Opcode::Null { dst: self.t });
        let null = lbl("on", &mut self.n);
        self.a.jmp(Opcode::JNull { reg: r, offset: 0 }, null);
        self.a.op(Opcode::Call1 {
            dst: self.t,
            fun: self.c.std_string,
            arg0: self.d,
        });
        self.a.label(null);
        self.quoted();
    }
    /// `t` (String or null) as a JSON value: haxe.format.JsonPrinter.print,
    /// the game's own serializer (full escaping, null as null).
    fn quoted(&mut self) {
        self.a.op(Opcode::Mov {
            dst: self.d,
            src: self.t,
        });
        self.a.op(Opcode::Null { dst: self.f });
        self.a.op(Opcode::Null { dst: self.u });
        self.a.op(Opcode::Call3 {
            dst: self.t,
            fun: self.c.json_print,
            arg0: self.d,
            arg1: self.f,
            arg2: self.u,
        });
        self.cat(self.t);
    }
}
fn build(code: &mut Bytecode, c: &Ctx) -> Result<RefFun> {
    let st_g = add_global(code, c.i32_t);
    let tick_g = add_global(code, c.i32_t);
    let dir_g = add_global(code, c.str_t);
    // PREFS.admin as it was before a console command, and whether it still
    // needs restoring (the command threw): the handler puts it back.
    let admin_g = add_global(code, c.bool_t);
    let pending_g = add_global(code, c.bool_t);
    let console_g = add_global(code, c.console.1);
    let k = |code: &mut Bytecode, v: i32| int_const(code, v);
    let (k0, k1, k2, k6, k8) = (k(code, 0), k(code, 1), k(code, 2), k(code, 6), k(code, 8));
    let s_inst = str_global(code, c.str_t, "WARTALES_MP_TEST_INSTANCE");
    let s_la = str_global(code, c.str_t, "LOCALAPPDATA");
    let s_sub = str_global(code, c.str_t, "\\wartales-mp\\harness\\");
    let s_on = str_global(code, c.str_t, "harness: WMP_TEST_SEAM instance ");
    let s_cmd = str_global(code, c.str_t, "cmd.txt");
    let s_ack = str_global(code, c.str_t, "ack.txt");
    let s_state = str_global(code, c.str_t, "state.json");
    let s_sp = str_global(code, c.str_t, " ");
    let s_console = str_global(code, c.str_t, "console ");
    let s_dump = str_global(code, c.str_t, "dump");
    let s_ok = str_global(code, c.str_t, "ok");
    let s_dispatched = str_global(code, c.str_t, "dispatched");
    let s_unknown = str_global(code, c.str_t, "unknown");
    let s_nogame = str_global(code, c.str_t, "nogame");
    let s_ackp = str_global(code, c.str_t, "harness: ack ");
    let s_err = str_global(code, c.str_t, "harness: error ");
    let s_errack = str_global(code, c.str_t, " error");

    let mut r = Regs(vec![]);
    let void = r.r(c.void_t);
    let i = r.r(c.i32_t);
    let kk = r.r(c.i32_t);
    let b = r.r(c.bool_t);
    let s = r.r(c.str_t);
    let s2 = r.r(c.str_t);
    let dir = r.r(c.str_t);
    let path = r.r(c.str_t);
    let text = r.r(c.str_t);
    let seq = r.r(c.str_t);
    let rest = r.r(c.str_t);
    let res = r.r(c.str_t);
    let j = r.r(c.str_t);
    let ni = r.r(c.null_i32_t);
    let d = r.r(c.dyn_t);
    let exc = r.r(c.dyn_t);
    let exc2 = r.r(c.dyn_t);
    let gcls = r.r(c.game_cls_t);
    let g = r.r(c.game_t);
    let con = r.r(c.console.1);
    let mcls = r.r(c.main_cls_t);
    let prefs = r.r(c.prefs.1);
    let old = r.r(c.bool_t);
    let mode = r.r(c.g.mode.1);
    let host = r.r(c.g.host.1);
    let me = r.r(c.g.me.1);
    let battle = r.r(c.battle_t);
    let bu = r.r(c.bunit_t);
    let unit = r.r(c.unit_t);
    let owner = r.r(c.unit_owner.1);
    let gs = r.r(c.state_t);
    let army = r.r(c.army.1);
    let arr = r.r(c.arr_arr.1);
    let n = r.r(c.i32_t);
    let ix = r.r(c.i32_t);
    let jrep = r.r(c.json_rep_t);

    let mut a = Asm::new();
    // --- state machine, outside the trap
    a.op(Opcode::GetGlobal {
        dst: i,
        global: st_g,
    });
    a.op(Opcode::Int { dst: kk, ptr: k2 });
    a.jmp(
        Opcode::JEq {
            a: i,
            b: kk,
            offset: 0,
        },
        "on",
    );
    a.op(Opcode::Int { dst: kk, ptr: k1 });
    a.jmp(
        Opcode::JEq {
            a: i,
            b: kk,
            offset: 0,
        },
        "ret",
    );
    a.op(Opcode::SetGlobal {
        global: st_g,
        src: kk,
    });
    a.op(Opcode::GetGlobal {
        dst: s,
        global: s_inst,
    });
    a.op(Opcode::Call1 {
        dst: seq,
        fun: c.get_env,
        arg0: s,
    });
    a.jmp(
        Opcode::JNull {
            reg: seq,
            offset: 0,
        },
        "ret",
    );
    a.op(Opcode::GetGlobal {
        dst: s,
        global: s_la,
    });
    a.op(Opcode::Call1 {
        dst: dir,
        fun: c.get_env,
        arg0: s,
    });
    a.jmp(
        Opcode::JNull {
            reg: dir,
            offset: 0,
        },
        "ret",
    );
    a.op(Opcode::GetGlobal {
        dst: s,
        global: s_sub,
    });
    a.op(Opcode::Call2 {
        dst: dir,
        fun: c.add,
        arg0: dir,
        arg1: s,
    });
    a.op(Opcode::SetGlobal {
        global: dir_g,
        src: dir,
    });
    a.op(Opcode::Int { dst: kk, ptr: k2 });
    a.op(Opcode::SetGlobal {
        global: st_g,
        src: kk,
    });
    a.op(Opcode::GetGlobal {
        dst: s,
        global: s_on,
    });
    a.op(Opcode::Call2 {
        dst: s,
        fun: c.add,
        arg0: s,
        arg1: seq,
    });
    a.op(Opcode::GetGlobal {
        dst: s2,
        global: s_sp,
    });
    a.op(Opcode::Call2 {
        dst: s,
        fun: c.add,
        arg0: s,
        arg1: s2,
    });
    a.op(Opcode::Call2 {
        dst: s,
        fun: c.add,
        arg0: s,
        arg1: dir,
    });
    a.op(Opcode::Mov { dst: d, src: s });
    a.op(Opcode::Call1 {
        dst: void,
        fun: c.println,
        arg0: d,
    });
    a.label("on");
    a.op(Opcode::GetGlobal {
        dst: i,
        global: tick_g,
    });
    a.op(Opcode::Incr { dst: i });
    a.op(Opcode::SetGlobal {
        global: tick_g,
        src: i,
    });
    a.op(Opcode::Int { dst: kk, ptr: k6 });
    a.jmp(
        Opcode::JSLt {
            a: i,
            b: kk,
            offset: 0,
        },
        "ret",
    );
    a.op(Opcode::Int { dst: kk, ptr: k0 });
    a.op(Opcode::SetGlobal {
        global: tick_g,
        src: kk,
    });
    a.op(Opcode::Null { dst: seq });
    a.op(Opcode::Null { dst: ni });

    // --- the command, inside a trap
    a.jmp(Opcode::Trap { exc, offset: 0 }, "catch");
    a.op(Opcode::GetGlobal {
        dst: dir,
        global: dir_g,
    });
    a.op(Opcode::GetGlobal {
        dst: s,
        global: s_cmd,
    });
    a.op(Opcode::Call2 {
        dst: path,
        fun: c.add,
        arg0: dir,
        arg1: s,
    });
    a.op(Opcode::Call1 {
        dst: b,
        fun: c.exists,
        arg0: path,
    });
    a.jmp(Opcode::JFalse { cond: b, offset: 0 }, "end");
    a.op(Opcode::Call1 {
        dst: text,
        fun: c.get_content,
        arg0: path,
    });
    a.op(Opcode::Call1 {
        dst: void,
        fun: c.delete_file,
        arg0: path,
    });
    a.op(Opcode::GetGlobal {
        dst: s,
        global: s_sp,
    });
    a.op(Opcode::Call3 {
        dst: i,
        fun: c.index_of,
        arg0: text,
        arg1: s,
        arg2: ni,
    });
    a.op(Opcode::Int { dst: kk, ptr: k1 });
    a.jmp(
        Opcode::JSLt {
            a: i,
            b: kk,
            offset: 0,
        },
        "end",
    );
    a.op(Opcode::Int { dst: kk, ptr: k0 });
    a.op(Opcode::ToDyn { dst: ni, src: i });
    a.op(Opcode::Call3 {
        dst: seq,
        fun: c.substr,
        arg0: text,
        arg1: kk,
        arg2: ni,
    });
    a.op(Opcode::Null { dst: ni });
    a.op(Opcode::Incr { dst: i });
    a.op(Opcode::Call3 {
        dst: rest,
        fun: c.substr,
        arg0: text,
        arg1: i,
        arg2: ni,
    });
    a.op(Opcode::GetGlobal {
        dst: res,
        global: s_unknown,
    });
    // verb: console
    a.op(Opcode::GetGlobal {
        dst: s,
        global: s_console,
    });
    a.op(Opcode::Call3 {
        dst: i,
        fun: c.index_of,
        arg0: rest,
        arg1: s,
        arg2: ni,
    });
    a.op(Opcode::Int { dst: kk, ptr: k0 });
    a.jmp(
        Opcode::JNotEq {
            a: i,
            b: kk,
            offset: 0,
        },
        "not_console",
    );
    a.op(Opcode::GetGlobal {
        dst: res,
        global: s_nogame,
    });
    a.op(Opcode::GetGlobal {
        dst: gcls,
        global: c.game_g,
    });
    a.op(Opcode::Field {
        dst: g,
        obj: gcls,
        field: c.game_inst,
    });
    a.jmp(Opcode::JNull { reg: g, offset: 0 }, "ack");
    a.op(Opcode::Field {
        dst: con,
        obj: g,
        field: c.console.0,
    });
    a.jmp(
        Opcode::JNull {
            reg: con,
            offset: 0,
        },
        "ack",
    );
    a.op(Opcode::Int { dst: kk, ptr: k8 });
    a.op(Opcode::Call3 {
        dst: s,
        fun: c.substr,
        arg0: rest,
        arg1: kk,
        arg2: ni,
    });
    a.op(Opcode::GetGlobal {
        dst: mcls,
        global: c.main_g,
    });
    a.op(Opcode::Field {
        dst: prefs,
        obj: mcls,
        field: c.prefs.0,
    });
    a.op(Opcode::NullCheck { reg: prefs });
    a.op(Opcode::Field {
        dst: old,
        obj: prefs,
        field: c.admin,
    });
    a.op(Opcode::SetGlobal {
        global: admin_g,
        src: old,
    });
    a.op(Opcode::Bool {
        dst: b,
        value: ValBool(true),
    });
    a.op(Opcode::SetGlobal {
        global: pending_g,
        src: b,
    });
    a.op(Opcode::SetGlobal {
        global: console_g,
        src: con,
    });
    a.op(Opcode::SetField {
        obj: prefs,
        field: c.admin,
        src: b,
    });
    a.op(Opcode::Call1 {
        dst: void,
        fun: c.reset_commands,
        arg0: con,
    });
    a.op(Opcode::Call2 {
        dst: void,
        fun: c.run_command,
        arg0: con,
        arg1: s,
    });
    a.op(Opcode::SetField {
        obj: prefs,
        field: c.admin,
        src: old,
    });
    a.op(Opcode::Bool {
        dst: b,
        value: ValBool(false),
    });
    a.op(Opcode::SetGlobal {
        global: pending_g,
        src: b,
    });
    a.op(Opcode::Call1 {
        dst: void,
        fun: c.reset_commands,
        arg0: con,
    });
    // The console reports a bad command on its own screen, not to us.
    a.op(Opcode::GetGlobal {
        dst: res,
        global: s_dispatched,
    });
    a.jmp(Opcode::JAlways { offset: 0 }, "ack");
    a.label("not_console");
    // verb: dump
    a.op(Opcode::GetGlobal {
        dst: s,
        global: s_dump,
    });
    a.op(Opcode::Call3 {
        dst: i,
        fun: c.index_of,
        arg0: rest,
        arg1: s,
        arg2: ni,
    });
    a.op(Opcode::Int { dst: kk, ptr: k0 });
    a.jmp(
        Opcode::JNotEq {
            a: i,
            b: kk,
            offset: 0,
        },
        "ack",
    );
    {
        let s_open = str_global(code, c.str_t, "{\"seq\":\"");
        a.op(Opcode::GetGlobal {
            dst: j,
            global: s_open,
        });
        let mut w = J {
            a: &mut a,
            c,
            j,
            s: s2,
            t: path,
            u: res,
            f: jrep,
            d,
            n: 0,
        };
        w.cat(seq);
        w.lit(code, "\",\"game\":");
        w.a.op(Opcode::GetGlobal {
            dst: gcls,
            global: c.game_g,
        });
        w.a.op(Opcode::Field {
            dst: g,
            obj: gcls,
            field: c.game_inst,
        });
        w.a.jmp(Opcode::JNotNull { reg: g, offset: 0 }, "has_game");
        w.lit(code, "false}");
        w.a.jmp(Opcode::JAlways { offset: 0 }, "write");
        w.a.label("has_game");
        w.lit(code, "true,\"mode\":");
        w.a.op(Opcode::Field {
            dst: mode,
            obj: g,
            field: c.g.mode.0,
        });
        w.obj_q(mode);
        for (name, f) in &c.g.bools {
            let key: &'static str = Box::leak(format!(",\"{name}\":").into_boxed_str());
            w.lit(code, key);
            w.a.op(Opcode::Field {
                dst: b,
                obj: g,
                field: *f,
            });
            w.val(b);
        }
        w.lit(code, ",\"playersReady\":");
        w.a.op(Opcode::Field {
            dst: i,
            obj: g,
            field: c.g.players_ready,
        });
        w.val(i);
        w.lit(code, ",\"host\":");
        w.a.op(Opcode::Field {
            dst: host,
            obj: g,
            field: c.g.host.0,
        });
        w.obj_q(host);
        w.lit(code, ",\"me\":");
        w.a.op(Opcode::Field {
            dst: me,
            obj: g,
            field: c.g.me.0,
        });
        w.player(code, me, s);
        // battle: the unit whose turn it is
        w.lit(code, ",\"battle\":");
        w.a.op(Opcode::Field {
            dst: battle,
            obj: g,
            field: c.g.battle,
        });
        w.a.jmp(
            Opcode::JNotNull {
                reg: battle,
                offset: 0,
            },
            "has_battle",
        );
        w.lit(code, "null");
        w.a.jmp(Opcode::JAlways { offset: 0 }, "units");
        w.a.label("has_battle");
        w.lit(code, "{\"currentUnit\":");
        w.a.op(Opcode::Field {
            dst: bu,
            obj: battle,
            field: c.battle_cur,
        });
        w.a.jmp(Opcode::JNull { reg: bu, offset: 0 }, "no_cur");
        w.a.op(Opcode::Field {
            dst: unit,
            obj: bu,
            field: c.bunit_data,
        });
        w.a.jmp(
            Opcode::JNull {
                reg: unit,
                offset: 0,
            },
            "no_cur",
        );
        w.lit(code, "{\"name\":");
        w.a.op(Opcode::Field {
            dst: s,
            obj: unit,
            field: c.unit_name,
        });
        w.str_q(s);
        w.lit(code, ",\"owner\":");
        w.a.op(Opcode::Field {
            dst: owner,
            obj: unit,
            field: c.unit_owner.0,
        });
        w.player(code, owner, s);
        w.lit(code, "}}");
        w.a.jmp(Opcode::JAlways { offset: 0 }, "units");
        w.a.label("no_cur");
        w.lit(code, "null}");
        // the army: name, owner, aptitude points
        w.a.label("units");
        w.lit(code, ",\"army\":[");
        w.a.op(Opcode::Field {
            dst: gs,
            obj: g,
            field: c.g.state,
        });
        w.a.jmp(Opcode::JNull { reg: gs, offset: 0 }, "army_end");
        w.a.op(Opcode::Field {
            dst: army,
            obj: gs,
            field: c.army.0,
        });
        w.a.jmp(
            Opcode::JNull {
                reg: army,
                offset: 0,
            },
            "army_end",
        );
        w.a.op(Opcode::Field {
            dst: n,
            obj: army,
            field: c.arr_len,
        });
        w.a.op(Opcode::Field {
            dst: arr,
            obj: army,
            field: c.arr_arr.0,
        });
        w.a.op(Opcode::Int { dst: ix, ptr: k0 });
        w.a.loop_head("army_loop");
        w.a.jmp(
            Opcode::JSGte {
                a: ix,
                b: n,
                offset: 0,
            },
            "army_end",
        );
        w.a.op(Opcode::Int { dst: kk, ptr: k0 });
        w.a.jmp(
            Opcode::JEq {
                a: ix,
                b: kk,
                offset: 0,
            },
            "first",
        );
        w.lit(code, ",");
        w.a.label("first");
        w.a.op(Opcode::GetArray {
            dst: d,
            array: arr,
            index: ix,
        });
        w.a.op(Opcode::SafeCast { dst: unit, src: d });
        w.lit(code, "{\"name\":");
        w.a.op(Opcode::Field {
            dst: s,
            obj: unit,
            field: c.unit_name,
        });
        w.str_q(s);
        w.lit(code, ",\"owner\":");
        w.a.op(Opcode::Field {
            dst: owner,
            obj: unit,
            field: c.unit_owner.0,
        });
        w.player(code, owner, s);
        w.lit(code, ",\"aptitudePoints\":");
        w.a.op(Opcode::Field {
            dst: i,
            obj: unit,
            field: c.unit_ap,
        });
        w.val(i);
        w.lit(code, "}");
        w.a.op(Opcode::Incr { dst: ix });
        w.a.jmp(Opcode::JAlways { offset: 0 }, "army_loop");
        w.a.label("army_end");
        w.lit(code, "]}");
    }
    a.label("write");
    a.op(Opcode::GetGlobal {
        dst: s,
        global: s_state,
    });
    a.op(Opcode::Call2 {
        dst: path,
        fun: c.add,
        arg0: dir,
        arg1: s,
    });
    a.op(Opcode::Call2 {
        dst: void,
        fun: c.save_content,
        arg0: path,
        arg1: j,
    });
    a.op(Opcode::GetGlobal {
        dst: res,
        global: s_ok,
    });
    // ack: "<seq> <result>" to ack.txt and shim.log
    a.label("ack");
    a.op(Opcode::GetGlobal {
        dst: s,
        global: s_sp,
    });
    a.op(Opcode::Call2 {
        dst: s,
        fun: c.add,
        arg0: seq,
        arg1: s,
    });
    a.op(Opcode::Call2 {
        dst: s,
        fun: c.add,
        arg0: s,
        arg1: res,
    });
    a.op(Opcode::GetGlobal {
        dst: s2,
        global: s_ack,
    });
    a.op(Opcode::Call2 {
        dst: path,
        fun: c.add,
        arg0: dir,
        arg1: s2,
    });
    a.op(Opcode::Call2 {
        dst: void,
        fun: c.save_content,
        arg0: path,
        arg1: s,
    });
    a.op(Opcode::GetGlobal {
        dst: s2,
        global: s_ackp,
    });
    a.op(Opcode::Call2 {
        dst: s,
        fun: c.add,
        arg0: s2,
        arg1: s,
    });
    a.op(Opcode::Mov { dst: d, src: s });
    a.op(Opcode::Call1 {
        dst: void,
        fun: c.println,
        arg0: d,
    });
    a.label("end");
    a.op(Opcode::EndTrap { exc });
    a.label("ret");
    a.op(Opcode::Ret { ret: void });
    // --- handler: log, then try an error ack (its own trap: never rethrow)
    a.label("catch");
    a.op(Opcode::Call1 {
        dst: s,
        fun: c.std_string,
        arg0: exc,
    });
    a.op(Opcode::GetGlobal {
        dst: s2,
        global: s_err,
    });
    a.op(Opcode::Call2 {
        dst: s,
        fun: c.add,
        arg0: s2,
        arg1: s,
    });
    a.op(Opcode::Mov { dst: d, src: s });
    a.op(Opcode::Call1 {
        dst: void,
        fun: c.println,
        arg0: d,
    });
    a.jmp(
        Opcode::Trap {
            exc: exc2,
            offset: 0,
        },
        "catch2",
    );
    a.op(Opcode::GetGlobal {
        dst: b,
        global: pending_g,
    });
    a.jmp(Opcode::JFalse { cond: b, offset: 0 }, "no_restore");
    a.op(Opcode::GetGlobal {
        dst: mcls,
        global: c.main_g,
    });
    a.op(Opcode::Field {
        dst: prefs,
        obj: mcls,
        field: c.prefs.0,
    });
    a.op(Opcode::GetGlobal {
        dst: old,
        global: admin_g,
    });
    a.op(Opcode::SetField {
        obj: prefs,
        field: c.admin,
        src: old,
    });
    a.op(Opcode::Bool {
        dst: b,
        value: ValBool(false),
    });
    a.op(Opcode::SetGlobal {
        global: pending_g,
        src: b,
    });
    // ...and the command set built while admin was forced goes too.
    a.op(Opcode::GetGlobal {
        dst: con,
        global: console_g,
    });
    a.jmp(
        Opcode::JNull {
            reg: con,
            offset: 0,
        },
        "no_restore",
    );
    a.op(Opcode::Call1 {
        dst: void,
        fun: c.reset_commands,
        arg0: con,
    });
    a.label("no_restore");
    a.op(Opcode::GetGlobal {
        dst: s,
        global: s_errack,
    });
    a.op(Opcode::Call2 {
        dst: s,
        fun: c.add,
        arg0: seq,
        arg1: s,
    });
    a.op(Opcode::GetGlobal {
        dst: dir,
        global: dir_g,
    });
    a.op(Opcode::GetGlobal {
        dst: s2,
        global: s_ack,
    });
    a.op(Opcode::Call2 {
        dst: path,
        fun: c.add,
        arg0: dir,
        arg1: s2,
    });
    a.op(Opcode::Call2 {
        dst: void,
        fun: c.save_content,
        arg0: path,
        arg1: s,
    });
    a.op(Opcode::EndTrap { exc: exc2 });
    a.op(Opcode::Ret { ret: void });
    a.label("catch2");
    a.op(Opcode::Ret { ret: void });

    push_fn(code, vec![], c.void_t, r.0, a.finish(), c.dbg)
}

fn apply(code: &mut Bytecode, c: Ctx) -> Result<()> {
    let tick = build(code, &c)?;
    let f = &mut code.functions[c.main_loop_fi];
    let void = new_reg(f, c.void_t);
    insert_ops(
        f,
        0,
        vec![Opcode::Call0 {
            dst: void,
            fun: tick,
        }],
    );
    eprintln!(
        "patched harness (TEST BUILD, WMP_TEST_SEAM): hxd.App.mainLoop fn@{} calls harnessTick fn@{}",
        f.findex.0, tick.0
    );
    Ok(())
}

/// Adds the test harness command channel, or leaves `code` untouched and logs why.
pub(crate) fn patch_harness(code: &mut Bytecode) {
    let snap = asm::Snap::take(code);
    let r = plan(code).and_then(|c| apply(code, c));
    if let Err(e) = r {
        snap.restore(code);
        crate::skipped(format!("harness skipped: {e:#}"));
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::asm::testutil::{check_flow, check_types, game, read, traps_ok, write};

    /// Prints the debug console's command names per mode (for the docs):
    /// `cargo test --release --features harness --target-dir target-test list_console_commands -- --ignored --nocapture`
    #[test]
    #[ignore]
    fn list_console_commands() {
        let Some(image) = game() else { return };
        let code = read(&image);
        let mode_t = obj_type(&code, "debug.ConsoleMode").unwrap();
        let add = method(&code, mode_t, "addCommand").unwrap().findex;
        let strs: std::collections::HashMap<usize, String> = code
            .constants
            .iter()
            .flatten()
            .filter_map(|c| match c.fields[..] {
                [si, _] => Some((c.global.0, code.strings.get(si)?.as_str().to_string())),
                _ => None,
            })
            .collect();
        for f in &code.functions {
            let Some(Type::Obj(o)) = f.parent.map(|p| &code.types[p.0]) else {
                continue;
            };
            let cls = s(&code, o.name);
            if !cls.starts_with("debug.") || s(&code, f.name) != "__constructor__" {
                continue;
            }
            let mut names = vec![];
            for (j, op) in f.ops.iter().enumerate() {
                let Some((fun, args)) = crate::diag::call_of(op) else {
                    continue;
                };
                if fun != add || args.len() < 2 {
                    continue;
                }
                let r = args[1];
                let name = f.ops[..j].iter().rev().find_map(|o| match o {
                    Opcode::GetGlobal { dst, global } if *dst == r => {
                        Some(strs.get(&global.0).cloned().unwrap_or("?".into()))
                    }
                    _ => None,
                });
                names.push(name.unwrap_or("?".into()));
            }
            if !names.is_empty() {
                println!("{cls}: {}", names.join(" "));
            }
        }
    }

    #[test]
    fn patches_installed_game() {
        let Some(image) = game() else { return };
        let orig = read(&image);
        let ml = plan(&orig).expect("plan").main_loop_fi;
        let mut code = read(&image);
        patch_harness(&mut code);
        let back = read(&write(&code));
        let tick = back.functions.last().unwrap();
        check_flow(tick);
        check_types(&back, tick, 0..tick.ops.len());
        assert_eq!(traps_ok(tick), 2);
        let (a, b) = (&orig.functions[ml], &back.functions[ml]);
        assert_eq!(b.ops.len(), a.ops.len() + 1);
        assert!(matches!(b.ops[0], Opcode::Call0 { fun, .. } if fun == tick.findex));
        crate::asm::testutil::shifted(a, b, 0, 1);
        for (i, (x, y)) in orig.functions.iter().zip(&back.functions).enumerate() {
            if i != ml {
                assert!(crate::asm::testutil::same(x, y), "fn#{i} changed");
            }
        }
    }
}
