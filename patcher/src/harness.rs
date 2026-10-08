// Copyright (c) 2026 Morgott. Licensed under CC BY-NC 4.0.
//
// Local co-op test harness: a command file channel (TEST BUILD ONLY: compiled
// with the cargo feature `harness`, i.e. `shim\build.ps1 -Test`; marker
// WMP_TEST_SEAM; the release dll carries none of it, shim\check.ps1 proves it).
// docs/coop-harness-plan.md section 8 lists the verbs.
//
// Two appended functions. `harnessTick()`, called at op 0 of hxd.App.mainLoop
// (every frame, title screen and game alike):
//
//   state 0 (first frame): WARTALES_MP_TEST_INSTANCE unset -> state 1 (off,
//     one global read per frame from then on); else dir =
//     %LOCALAPPDATA%\wartales-mp\harness\ (the driver creates it), state 2, and
//     the window goes windowed: Game.PREFS.displayMode = 0 (in memory) +
//     GraphicsControl.applyDisplayMode(), the options screen's own call.
//   state 2, every 6th frame, inside a Trap:
//     if exists(dir+"cmd.txt"): c = getContent, deleteFile;
//     c = "<seq> <verb> [arg]"   (the driver writes cmd.tmp, then renames)
//       console <line>  the debug console of Game.inst, else of TitleScreen.inst
//                       (AppBase.console): PREFS.admin forced true around
//                       resetCommands + runCommand (Battle/Cheats modes need
//                       get_isAdmin() && canCheat()), restored and the command
//                       set rebuilt after (also when it throws). Ack "dispatched".
//       dump            JSON of the game state -> dir+"state.json"
//       host            the newest save (LoadGame.loadGames) through a real
//                       LoadGame window in a Pause.doLoad mode: on the title
//                       screen (solo mode: every save of this player)
//                       convertGame() for a solo save, else loadGame() (co-op
//                       lobby + LoadMultiGame, its join code is dump's
//                       lobby.code); in game (co-op mode: this game's saves)
//                       loadGame(), the in-game reload.
//       join <code>     the title screen's join-by-code submit closure (the
//                       only caller of Lobby.joinCode).
//       slot            LobbyState.addSlot() (the lobby's "+": a loaded save
//                       admits a new player only into an opened slot).
//       start           LoadMultiGame.startGame() of the open lobby window.
//       backup <n>      onClick of Pause.backups[n] (the pause menu's backup
//                       button n, a st.SaveKey) of the open Pause window.
//     dir+"ack.txt" = "<seq> ok|dispatched|unknown|nogame|nosave|nowindow", and
//     a shim.log line.
//   An exception: "harness: error <exc>" and ack "<seq> error".
//
// `harnessFind(t)`: the first open window of runtime type `t` in
// TitleScreen.inst.ui, Game.inst.globalUI or Game.inst.mode (BaseUI.windows).
//
// `harnessUid(me)`, after mpman.Api.getUser's `makeSteamUser` (the local
// player, built once): both games run on one Steam account and would join as
// one player ("S" + account id hex). Instance N plays as the Session id
// "X" + N + id.substr(1) (User.make: userMap knows it), which its master
// renders as its lobby member id, as it renders every player's own id
// (internal/master idOf).
// `host` stamps the save's playerId with this id.
//
// Validated before editing; a mismatch skips the pass (logged).

use super::asm::{push_fn, Asm, Regs};
use super::diag::static_fn;
use super::job_xp::str_global;
use super::*;
use hlbc::types::{RefInt, ValBool};

/// Short forms of the ops the harness emits.
trait Ops {
    fn fld(&mut self, dst: Reg, obj: Reg, field: RefField);
    fn setf(&mut self, obj: Reg, field: RefField, src: Reg);
    fn gg(&mut self, dst: Reg, global: RefGlobal);
    fn call(&mut self, dst: Reg, fun: RefFun, args: &[Reg]);
    fn int(&mut self, dst: Reg, ptr: RefInt);
    fn if_null(&mut self, reg: Reg, l: &'static str);
    fn if_set(&mut self, reg: Reg, l: &'static str);
    fn if_false(&mut self, cond: Reg, l: &'static str);
    fn go(&mut self, l: &'static str);
}

impl Ops for Asm {
    fn fld(&mut self, dst: Reg, obj: Reg, field: RefField) {
        self.op(Opcode::Field { dst, obj, field });
    }
    fn setf(&mut self, obj: Reg, field: RefField, src: Reg) {
        self.op(Opcode::SetField { obj, field, src });
    }
    fn gg(&mut self, dst: Reg, global: RefGlobal) {
        self.op(Opcode::GetGlobal { dst, global });
    }
    fn call(&mut self, dst: Reg, fun: RefFun, args: &[Reg]) {
        self.op(match *args {
            [] => Opcode::Call0 { dst, fun },
            [arg0] => Opcode::Call1 { dst, fun, arg0 },
            [arg0, arg1] => Opcode::Call2 {
                dst,
                fun,
                arg0,
                arg1,
            },
            [arg0, arg1, arg2] => Opcode::Call3 {
                dst,
                fun,
                arg0,
                arg1,
                arg2,
            },
            _ => Opcode::CallN {
                dst,
                fun,
                args: args.to_vec(),
            },
        });
    }
    fn int(&mut self, dst: Reg, ptr: RefInt) {
        self.op(Opcode::Int { dst, ptr });
    }
    fn if_null(&mut self, reg: Reg, l: &'static str) {
        self.jmp(Opcode::JNull { reg, offset: 0 }, l);
    }
    fn if_set(&mut self, reg: Reg, l: &'static str) {
        self.jmp(Opcode::JNotNull { reg, offset: 0 }, l);
    }
    fn if_false(&mut self, cond: Reg, l: &'static str) {
        self.jmp(Opcode::JFalse { cond, offset: 0 }, l);
    }
    fn go(&mut self, l: &'static str) {
        self.jmp(Opcode::JAlways { offset: 0 }, l);
    }
}

struct Ctx {
    void_t: RefType,
    i32_t: RefType,
    f64_t: RefType,
    bool_t: RefType,
    dyn_t: RefType,
    str_t: RefType,
    type_t: RefType,
    null_i32_t: RefType,
    arr_t: RefType,
    get_env: RefFun,
    exists: RefFun,
    get_content: RefFun,
    save_content: RefFun,
    delete_file: RefFun,
    println: RefFun,
    std_string: RefFun,
    parse_int: RefFun,
    add: RefFun,
    index_of: RefFun,
    substr: RefFun,
    json_print: RefFun,
    json_rep_t: RefType,
    main_loop_fi: usize,
    dbg: usize,
    /// mpman.Api.getUser and the op after its `makeSteamUser` call.
    get_user: (usize, usize, Reg),
    user_t: RefType,
    user_id: RefField,
    user_name: RefField,
    user_make: RefFun,
    get_user_fn: RefFun,
    save_player: RefField,
    game_g: RefGlobal,
    game_cls_t: RefType,
    game_inst: RefField,
    game_t: RefType,
    game_prefs: (RefField, RefType),
    display_mode: RefField,
    apply_display: RefFun,
    title: (RefGlobal, RefType, RefField, RefType),
    title_ui: (RefField, RefType),
    title_join: RefFun,
    main_g: RefGlobal,
    main_cls_t: RefType,
    prefs: (RefField, RefType),
    admin: RefField,
    console: (RefField, RefType),
    reset_commands: RefFun,
    run_command: RefFun,
    // windows
    win_t: RefType,
    windows: RefField,
    global_ui: (RefField, RefType),
    // host / start / backup
    load_t: RefType,
    load_mode_t: RefType,
    /// Pause.doLoad's LoadGame modes: co-op (lists the running game's saves)
    /// and solo (lists every save of this player).
    load_multi_mode: RefGlobal,
    load_solo_mode: RefGlobal,
    load_ctor: RefFun,
    load_games: RefFun,
    load_current: (RefField, RefType),
    save_header: (RefField, RefType),
    header_multi: (RefField, RefType),
    load_game: RefFun,
    convert_game: RefFun,
    lmg_t: RefType,
    start_game: RefFun,
    add_slot: RefFun,
    pause_t: RefType,
    pause_backups: RefField,
    elem_t: RefType,
    on_click: (RefField, RefType),
    // dump
    g: GameF,
    lobby: (RefGlobal, RefType, RefField, RefType),
    lobby_state: (RefField, RefType),
    short_code: RefField,
    lobby_players: (RefField, RefType),
    proxy_array: (RefField, RefType),
    unit_t: RefType,
    unit_name: RefField,
    unit_ap: RefField,
    unit_owner: (RefField, RefType),
    player_name: RefField,
    player_connected: RefField,
    bunit_t: RefType,
    bunit_data: RefField,
    bunit_played: RefField,
    active_skills: RefFun,
    can_use: RefFun,
    skill_t: RefType,
    skill_id: RefField,
    battle_t: RefType,
    battle_cur: RefField,
    battle_player_turn: RefField,
    state_t: RefType,
    army: RefField,
    arr_len: RefField,
    arr_arr: (RefField, RefType),
    game_ui: (RefField, RefType),
    game_inv: (RefField, RefType),
    chest_inv: (RefField, RefType),
    obj_visible: RefField,
    obj_abs: (RefField, RefField),
    flow_size: (RefField, RefField),
    debrief_win_t: RefType,
    debrief: (RefField, RefType),
    loot: (RefField, RefType),
    all_items: RefFun,
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
    let f64_t = prim_type(code, "F64", |t| matches!(t, Type::F64))?;
    let bool_t = prim_type(code, "Bool", |t| matches!(t, Type::Bool))?;
    let dyn_t = prim_type(code, "Dyn", |t| matches!(t, Type::Dyn))?;
    let type_t = prim_type(code, "Type", |t| matches!(t, Type::Type))?;
    let null_i32_t = prim_type(
        code,
        "Null<I32>",
        |t| matches!(t, Type::Null(x) if *x == i32_t),
    )?;
    let str_t = obj_type(code, "String")?;
    let arr_t = obj_type(code, "hl.types.ArrayObj")?;
    let sfn = |cls: &str, name: &str, args: &[RefType], ret: RefType| -> Result<RefFun> {
        let f = static_fn(code, cls, name)?.findex;
        want_sig(code, f, args, ret, name)?;
        Ok(f)
    };
    let mfn = |t: RefType, name: &str, args: &[RefType], ret: RefType| -> Result<RefFun> {
        let f = method(code, t, name)?.findex;
        want_sig(code, f, args, ret, name)?;
        Ok(f)
    };

    let get_env = sfn("$Sys", "getEnv", &[str_t], str_t)?;
    let println = sfn("$Sys", "println", &[dyn_t], void_t)?;
    let exists = sfn("sys.$FileSystem", "exists", &[str_t], bool_t)?;
    let delete_file = sfn("sys.$FileSystem", "deleteFile", &[str_t], void_t)?;
    let get_content = sfn("sys.io.$File", "getContent", &[str_t], str_t)?;
    let save_content = sfn("sys.io.$File", "saveContent", &[str_t, str_t], void_t)?;
    let std_string = sfn("$Std", "string", &[dyn_t], str_t)?;
    let parse_int = sfn("$Std", "parseInt", &[str_t], null_i32_t)?;
    let add = sfn("$String", "__add__", &[str_t, str_t], str_t)?;
    let index_of = mfn(str_t, "indexOf", &[str_t, str_t, null_i32_t], i32_t)?;
    let substr = mfn(str_t, "substr", &[str_t, i32_t, null_i32_t], str_t)?;
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

    // mpman.Api.getUser: `__ME = makeSteamUser(steam.Api.getUser())`, once.
    let user_t = obj_type(code, "mpman.User")?;
    let gu = static_fn(code, "mpman.$Api", "getUser")?;
    let msu = static_fn(code, "mpman.$Api", "makeSteamUser")?.findex;
    let sites: Vec<(usize, Reg)> = gu
        .ops
        .iter()
        .enumerate()
        .filter_map(|(i, o)| match o {
            Opcode::Call1 { dst, fun, .. } if *fun == msu => Some((i + 1, *dst)),
            _ => None,
        })
        .collect();
    let [(user_at, user_reg)] = sites[..] else {
        bail!(
            "mpman.Api.getUser: expected one makeSteamUser call, found {}",
            sites.len()
        );
    };
    if gu.regs[user_reg.0 as usize] != user_t || sig(code, gu.findex)?.1 != user_t {
        bail!("mpman.Api.getUser: unexpected types");
    }
    let gu_findex = gu.findex;
    let get_user = (fun_index(code, gu.findex)?, user_at, user_reg);
    let user_id = typed(code, user_t, "id", str_t)?;
    let user_name = typed(code, user_t, "name", str_t)?;
    let user_make = sfn("mpman.$User", "make", &[str_t, str_t], user_t)?;

    let game_t = obj_type(code, "Game")?;
    let (game_g, game_cls_t) = class_global(code, "Game")?;
    let game_inst = typed(code, game_cls_t, "inst", game_t)?;
    let game_prefs = field(code, game_cls_t, "PREFS")?;
    let display_mode = field_of_virtual(code, game_prefs.1, "displayMode")?;
    if display_mode.1 != i32_t {
        bail!("Game.PREFS.displayMode is not an Int");
    }
    let apply_display = sfn("gfx.$GraphicsControl", "applyDisplayMode", &[], void_t)?;
    let (main_g, main_cls_t) = class_global(code, "Main")?;
    let prefs = field(code, main_cls_t, "PREFS")?;
    let admin = field_of_virtual(code, prefs.1, "admin")?;
    if admin.1 != bool_t {
        bail!("PREFS.admin: no Bool field");
    }
    // Game and TitleScreen are both AppBase: one field index for the console.
    let appbase_t = obj_type(code, "AppBase")?;
    let console = field(code, appbase_t, "console")?;
    let con_t = obj_type(code, "debug.Console")?;
    let title_t = obj_type(code, "ui.win.TitleScreen")?;
    if console.1 != con_t || !is_sub(code, game_t, appbase_t) || !is_sub(code, title_t, appbase_t) {
        bail!("AppBase.console: unexpected shape");
    }
    let reset_commands = mfn(con_t, "resetCommands", &[con_t], void_t)?;
    let run_command = mfn(con_t, "runCommand", &[con_t, str_t], void_t)?;
    let (title_g, title_cls_t) = class_global(code, "ui.win.TitleScreen")?;
    let title_inst = typed(code, title_cls_t, "inst", title_t)?;
    let title_ui = field(code, title_t, "ui")?;

    // Windows: BaseUI.windows of the title screen's GlobalUI, the game's
    // globalUI and its mode (GameMode is a BaseUI too).
    let win_t = obj_type(code, "ui.Window")?;
    let base_ui_t = obj_type(code, "ui.BaseUI")?;
    let windows = typed(code, base_ui_t, "windows", arr_t)?;
    let global_ui = field(code, game_t, "globalUI")?;
    let mode = field(code, game_t, "mode")?;
    for t in [title_ui.1, global_ui.1, mode.1] {
        if !is_sub(code, t, base_ui_t) {
            bail!("a window list owner is not a ui.BaseUI");
        }
    }

    // The title screen's join-by-code submit: the only caller of Lobby.joinCode.
    let join_code = static_fn(code, "$Lobby", "joinCode")?.findex;
    let joins: Vec<RefFun> = code
        .functions
        .iter()
        .filter(|f| calls(f, join_code))
        .map(|f| f.findex)
        .collect();
    let [title_join] = joins[..] else {
        bail!("Lobby.joinCode: expected one caller, found {}", joins.len());
    };
    want_sig(code, title_join, &[str_t], void_t, "join-by-code submit")?;

    // host: LoadGame (mode, ctor, list, current save, load/convert).
    let load_t = obj_type(code, "ui.win.LoadGame")?;
    let ctor = method(code, load_t, "__constructor__")?;
    let load_ctor = ctor.findex;
    let ca = fun_args(code, ctor);
    let load_mode_t = *ca.get(1).context("LoadGame ctor: no mode")?;
    want_sig(
        code,
        load_ctor,
        &[load_t, load_mode_t],
        void_t,
        "LoadGame ctor",
    )?;
    let load_games = sfn("ui.win.$LoadGame", "loadGames", &[load_mode_t], arr_t)?;
    // Pause.doLoad: `isMulti ? <multi mode> : <solo mode>` into the LoadGame ctor.
    let pause_t = obj_type(code, "ui.win.Pause")?;
    let do_load = method(code, pause_t, "doLoad")?;
    let modes: Vec<RefGlobal> = do_load
        .ops
        .iter()
        .filter_map(|o| match o {
            Opcode::GetGlobal { dst, global } if do_load.regs[dst.0 as usize] == load_mode_t => {
                Some(*global)
            }
            _ => None,
        })
        .collect();
    let [load_multi_mode, load_solo_mode] = modes[..] else {
        bail!(
            "Pause.doLoad: expected two LoadGame modes, found {}",
            modes.len()
        );
    };
    if !calls(do_load, load_ctor) {
        bail!("Pause.doLoad: no LoadGame ctor call");
    }
    let load_current = field(code, load_t, "current")?;
    let save_header = field_of_virtual(code, load_current.1, "header")?;
    let header_multi = field_of_virtual(code, save_header.1, "multi")?;
    let save_player = field_of_virtual(code, save_header.1, "playerId")?;
    if save_player.1 != str_t {
        bail!("save header playerId is not a String");
    }
    let load_game = mfn(load_t, "loadGame", &[load_t], void_t)?;
    let convert_game = mfn(load_t, "convertGame", &[load_t], void_t)?;
    let lmg_t = obj_type(code, "ui.win.LoadMultiGame")?;
    let start_game = mfn(lmg_t, "startGame", &[lmg_t], void_t)?;
    let pause_backups = typed(code, pause_t, "backups", arr_t)?;
    let elem_t = obj_type(code, "ui.comp.Element")?;
    let on_click = field(code, elem_t, "onClick")?;
    if !matches!(&code.types[on_click.1 .0], Type::Fun(f) if f.args.is_empty() && f.ret == void_t) {
        bail!("Element.onClick is not () -> Void");
    }
    for t in [load_t, lmg_t, pause_t] {
        if !is_sub(code, t, win_t) {
            bail!("a harness window is not a ui.Window");
        }
    }

    // dump
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
        mode,
        bools,
        players_ready: typed(code, game_t, "playersReady", i32_t)?,
        host: field(code, game_t, "host")?,
        me: (typed(code, game_t, "me", player_t)?, player_t),
        battle: typed(code, game_t, "battle", battle_t)?,
        state: typed(code, game_t, "state", state_t)?,
    };
    let lobby_t = obj_type(code, "Lobby")?;
    let (lobby_g, lobby_cls_t) = class_global(code, "Lobby")?;
    let lobby_inst = typed(code, lobby_cls_t, "inst", lobby_t)?;
    let lobby_state = field(code, lobby_t, "state")?;
    let proxy_t = obj_type(code, "hxbit.ArrayProxyData")?;
    let lobby_players = typed(code, lobby_state.1, "players", proxy_t)?;
    let add_slot = mfn(lobby_state.1, "addSlot", &[lobby_state.1], void_t)?;
    let proxy_array = field(code, proxy_t, "array")?;
    let unit_t = obj_type(code, "st.Unit")?;
    let bunit_t = obj_type(code, "battle.Unit")?;
    let active_skills = mfn(bunit_t, "getActiveSkills", &[bunit_t], arr_t)?;
    let cu = method(code, bunit_t, "canUseSkill")?;
    let can_use = cu.findex;
    let skill_t = *fun_args(code, cu).get(1).context("canUseSkill: no skill")?;
    want_sig(code, can_use, &[bunit_t, skill_t], bool_t, "canUseSkill")?;
    let skill_id = field_of_virtual(code, skill_t, "id")?;
    if skill_id.1 != str_t {
        bail!("skill id is not a String");
    }
    let arr_arr = field(code, arr_t, "array")?;
    if !matches!(code.types[arr_arr.1 .0], Type::Array) {
        bail!("ArrayObj.array is not a native array");
    }
    let obj_t = obj_type(code, "h2d.Object")?;
    let flow_t = obj_type(code, "h2d.Flow")?;
    let game_ui = field(code, game_t, "ui")?;
    let game_inv = field(code, game_ui.1, "gameInventory")?;
    let chest_inv = field(code, game_inv.1, "chestInventory")?;
    if !is_sub(code, chest_inv.1, flow_t) || !is_sub(code, flow_t, obj_t) {
        bail!("GameInventory.chestInventory is not an h2d.Flow");
    }
    let debrief_win_t = obj_type(code, "ui.win.Debrief")?;
    let debrief = field(code, debrief_win_t, "debrief")?;
    let inv_t = obj_type(code, "st.Inventory")?;
    let loot = typed(code, debrief.1, "loot", inv_t)?;
    let f = |t: RefType, n: &str| typed(code, t, n, f64_t);
    Ok(Ctx {
        void_t,
        i32_t,
        f64_t,
        bool_t,
        dyn_t,
        str_t,
        type_t,
        null_i32_t,
        arr_t,
        get_env,
        exists,
        get_content,
        save_content,
        delete_file,
        println,
        std_string,
        parse_int,
        add,
        index_of,
        substr,
        json_print,
        json_rep_t,
        main_loop_fi,
        dbg: debug_file(code, "hxd/App.hx")?,
        get_user,
        user_t,
        user_id,
        user_name,
        user_make,
        get_user_fn: gu_findex,
        save_player: save_player.0,
        game_g,
        game_cls_t,
        game_inst,
        game_t,
        game_prefs,
        display_mode: display_mode.0,
        apply_display,
        title: (title_g, title_cls_t, title_inst, title_t),
        title_ui,
        title_join,
        main_g,
        main_cls_t,
        prefs,
        admin: admin.0,
        console,
        reset_commands,
        run_command,
        win_t,
        windows,
        global_ui,
        load_t,
        load_mode_t,
        load_multi_mode,
        load_solo_mode,
        load_ctor,
        load_games,
        load_current,
        save_header,
        header_multi,
        load_game,
        convert_game,
        lmg_t,
        start_game,
        add_slot,
        pause_t,
        pause_backups,
        elem_t,
        on_click,
        g,
        lobby: (lobby_g, lobby_cls_t, lobby_inst, lobby_t),
        lobby_state,
        short_code: typed(code, lobby_state.1, "shortCode", str_t)?,
        lobby_players: (lobby_players, proxy_t),
        proxy_array,
        unit_t,
        unit_name: typed(code, unit_t, "name", str_t)?,
        unit_ap: typed(code, unit_t, "aptitudePoints", i32_t)?,
        unit_owner: (typed(code, unit_t, "owner", player_t)?, player_t),
        player_name: typed(code, player_t, "name", str_t)?,
        player_connected: typed(code, player_t, "connected", bool_t)?,
        bunit_t,
        bunit_data: typed(code, bunit_t, "data", unit_t)?,
        bunit_played: typed(code, bunit_t, "apSkillPlayed", proxy_t)?,
        active_skills,
        can_use,
        skill_t,
        skill_id: skill_id.0,
        battle_t,
        battle_cur: typed(code, battle_t, "currentUnit", bunit_t)?,
        battle_player_turn: typed(code, battle_t, "isPlayerTurn", bool_t)?,
        state_t,
        army: typed(code, state_t, "army", arr_t)?,
        arr_len: typed(code, arr_t, "length", i32_t)?,
        arr_arr,
        game_ui,
        game_inv,
        chest_inv,
        obj_visible: typed(code, obj_t, "visible", bool_t)?,
        obj_abs: (f(obj_t, "absX")?, f(obj_t, "absY")?),
        flow_size: (
            f(flow_t, "calculatedWidth")?,
            f(flow_t, "calculatedHeight")?,
        ),
        debrief_win_t,
        debrief,
        loot: (loot, inv_t),
        all_items: mfn(inv_t, "getAllItems", &[inv_t], arr_t)?,
    })
}

/// Label names for generated code (a handful per build).
fn lbl(prefix: &str, n: &mut usize) -> &'static str {
    *n += 1;
    Box::leak(format!("{prefix}{n}").into_boxed_str())
}

/// `harnessFind(want: Type) -> ui.Window`: the first open window whose runtime
/// type is `want`, or null.
fn build_find(code: &mut Bytecode, c: &Ctx) -> Result<RefFun> {
    let k0 = int_const(code, 0);
    let mut r = Regs(vec![]);
    let want = r.r(c.type_t);
    let tcls = r.r(c.title.1);
    let ts = r.r(c.title.3);
    let tui = r.r(c.title_ui.1);
    let gcls = r.r(c.game_cls_t);
    let g = r.r(c.game_t);
    let gui = r.r(c.global_ui.1);
    let mode = r.r(c.g.mode.1);
    let arr = r.r(c.arr_t);
    let raw = r.r(c.arr_arr.1);
    let n = r.r(c.i32_t);
    let ix = r.r(c.i32_t);
    let d = r.r(c.dyn_t);
    let t = r.r(c.type_t);
    let w = r.r(c.win_t);
    let mut a = Asm::new();
    let mut k = 0;
    // One search of `owner.windows`; falls through when nothing matched.
    let search = |a: &mut Asm, owner: Reg, k: &mut usize| {
        let (next, top) = (lbl("fn", k), lbl("ft", k));
        a.if_null(owner, next);
        a.fld(arr, owner, c.windows);
        a.if_null(arr, next);
        a.fld(n, arr, c.arr_len);
        a.fld(raw, arr, c.arr_arr.0);
        a.int(ix, k0);
        a.loop_head(top);
        a.jmp(
            Opcode::JSGte {
                a: ix,
                b: n,
                offset: 0,
            },
            next,
        );
        a.op(Opcode::GetArray {
            dst: d,
            array: raw,
            index: ix,
        });
        a.op(Opcode::Incr { dst: ix });
        a.if_null(d, top);
        a.op(Opcode::GetType { dst: t, src: d });
        a.jmp(
            Opcode::JNotEq {
                a: t,
                b: want,
                offset: 0,
            },
            top,
        );
        a.op(Opcode::SafeCast { dst: w, src: d });
        a.op(Opcode::Ret { ret: w });
        a.label(next);
    };
    a.gg(tcls, c.title.0);
    a.fld(ts, tcls, c.title.2);
    a.if_null(ts, "game");
    a.fld(tui, ts, c.title_ui.0);
    search(&mut a, tui, &mut k);
    a.label("game");
    a.gg(gcls, c.game_g);
    a.fld(g, gcls, c.game_inst);
    a.if_null(g, "none");
    a.fld(gui, g, c.global_ui.0);
    search(&mut a, gui, &mut k);
    a.fld(mode, g, c.g.mode.0);
    search(&mut a, mode, &mut k);
    a.label("none");
    a.op(Opcode::Null { dst: w });
    a.op(Opcode::Ret { ret: w });
    push_fn(code, vec![c.type_t], c.win_t, r.0, a.finish(), c.dbg)
}

/// `harnessUid(me: mpman.User) -> mpman.User`: with the harness on, the local
/// player is `User.make("X" + N + me.id.substr(1), me.name)` (registered in
/// mpman.User.userMap under that id, so `lobby.owner == getUser()` holds).
fn build_uid(code: &mut Bytecode, c: &Ctx) -> Result<RefFun> {
    let k1 = int_const(code, 1);
    let s_inst = str_global(code, c.str_t, "WARTALES_MP_TEST_INSTANCE");
    let s_x = str_global(code, c.str_t, "X");
    let s_log = str_global(code, c.str_t, "harness: player id ");
    let mut r = Regs(vec![]);
    let u = r.r(c.user_t);
    let void = r.r(c.void_t);
    let e = r.r(c.str_t);
    let s = r.r(c.str_t);
    let id = r.r(c.str_t);
    let ni = r.r(c.null_i32_t);
    let k = r.r(c.i32_t);
    let d = r.r(c.dyn_t);
    let mut a = Asm::new();
    a.if_null(u, "ret");
    a.gg(s, s_inst);
    a.call(e, c.get_env, &[s]);
    a.if_null(e, "ret");
    a.fld(id, u, c.user_id);
    a.if_null(id, "ret");
    a.int(k, k1);
    a.op(Opcode::Null { dst: ni });
    a.call(id, c.substr, &[id, k, ni]);
    a.gg(s, s_x);
    a.call(s, c.add, &[s, e]);
    a.call(s, c.add, &[s, id]);
    a.fld(id, u, c.user_name);
    a.call(u, c.user_make, &[s, id]);
    a.gg(id, s_log);
    a.call(s, c.add, &[id, s]);
    a.op(Opcode::Mov { dst: d, src: s });
    a.call(void, c.println, &[d]);
    a.label("ret");
    a.op(Opcode::Ret { ret: u });
    push_fn(code, vec![c.user_t], c.user_t, r.0, a.finish(), c.dbg)
}

/// Builder of the dump JSON: `j = j + piece`.
struct J<'a> {
    a: Asm,
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
        self.a.gg(self.s, g);
        self.cat(self.s);
    }
    fn cat(&mut self, r: Reg) {
        self.a.call(self.j, self.c.add, &[self.j, r]);
    }
    /// Std.string of a plain value (i32/f64/bool): boxed first.
    fn val(&mut self, r: Reg) {
        self.a.op(Opcode::ToDyn {
            dst: self.d,
            src: r,
        });
        self.a.call(self.s, self.c.std_string, &[self.d]);
        self.cat(self.s);
    }
    /// `,"key":` then `obj.field` as a plain value, `tmp` typed as the field.
    fn kv(&mut self, code: &mut Bytecode, key: &str, obj: Reg, field: RefField, tmp: Reg) {
        let key: &'static str = Box::leak(format!(",\"{key}\":").into_boxed_str());
        self.lit(code, key);
        self.a.fld(tmp, obj, field);
        self.val(tmp);
    }
    /// `p.name` as a JSON string (or null) for a BasePlayer register.
    fn player(&mut self, code: &mut Bytecode, p: Reg, tmp_s: Reg) {
        let null = lbl("pn", &mut self.n);
        let end = lbl("pe", &mut self.n);
        self.a.if_null(p, null);
        self.a.fld(tmp_s, p, self.c.player_name);
        self.str_q(tmp_s);
        self.a.go(end);
        self.a.label(null);
        self.lit(code, "null");
        self.a.label(end);
        self.a.op(Opcode::Label);
    }
    /// `,"connected":` of a BasePlayer register (null without one).
    fn connected(&mut self, code: &mut Bytecode, p: Reg, tmp_b: Reg) {
        let null = lbl("cn", &mut self.n);
        let end = lbl("ce", &mut self.n);
        self.lit(code, ",\"connected\":");
        self.a.if_null(p, null);
        self.a.fld(tmp_b, p, self.c.player_connected);
        self.val(tmp_b);
        self.a.go(end);
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
        self.a.if_null(r, null);
        self.a.call(self.t, self.c.std_string, &[self.d]);
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
        self.a
            .call(self.t, self.c.json_print, &[self.d, self.f, self.u]);
        self.cat(self.t);
    }
}

fn build(code: &mut Bytecode, c: &Ctx, find: RefFun) -> Result<RefFun> {
    let st_g = add_global(code, c.i32_t);
    let tick_g = add_global(code, c.i32_t);
    let dir_g = add_global(code, c.str_t);
    // PREFS.admin as it was before a console command, and whether it still
    // needs restoring (the command threw): the handler puts it back.
    let admin_g = add_global(code, c.bool_t);
    let pending_g = add_global(code, c.bool_t);
    let console_g = add_global(code, c.console.1);
    let k = |code: &mut Bytecode, v: i32| int_const(code, v);
    let (k0, k1, k2, k5, k6, k7, k8) = (
        k(code, 0),
        k(code, 1),
        k(code, 2),
        k(code, 5),
        k(code, 6),
        k(code, 7),
        k(code, 8),
    );
    let mut gs = |v: &'static str| str_global(code, c.str_t, v);
    let s_inst = gs("WARTALES_MP_TEST_INSTANCE");
    let s_la = gs("LOCALAPPDATA");
    let s_sub = gs("\\wartales-mp\\harness\\");
    let s_on = gs("harness: WMP_TEST_SEAM instance ");
    let s_cmd = gs("cmd.txt");
    let s_ack = gs("ack.txt");
    let s_state = gs("state.json");
    let s_sp = gs(" ");
    let s_console = gs("console ");
    let s_dump = gs("dump");
    let s_host = gs("host");
    let s_join = gs("join ");
    let s_start = gs("start");
    let s_slot = gs("slot");
    let s_backup = gs("backup ");
    let s_ok = gs("ok");
    let s_dispatched = gs("dispatched");
    let s_unknown = gs("unknown");
    let s_nogame = gs("nogame");
    let s_nosave = gs("nosave");
    let s_nowindow = gs("nowindow");
    let s_ackp = gs("harness: ack ");
    let s_err = gs("harness: error ");
    let s_errack = gs(" error");
    let s_open = gs("{\"seq\":\"");

    let mut r = Regs(vec![]);
    let void = r.r(c.void_t);
    let i = r.r(c.i32_t);
    let kk = r.r(c.i32_t);
    let b = r.r(c.bool_t);
    let fl = r.r(c.f64_t);
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
    let ty = r.r(c.type_t);
    let gcls = r.r(c.game_cls_t);
    let g = r.r(c.game_t);
    let tcls = r.r(c.title.1);
    let ts = r.r(c.title.3);
    let con = r.r(c.console.1);
    let mcls = r.r(c.main_cls_t);
    let prefs = r.r(c.prefs.1);
    let gprefs = r.r(c.game_prefs.1);
    let old = r.r(c.bool_t);
    let win = r.r(c.win_t);
    let lmode = r.r(c.load_mode_t);
    let lw = r.r(c.load_t);
    let cur = r.r(c.load_current.1);
    let hdr = r.r(c.save_header.1);
    let multi = r.r(c.header_multi.1);
    let lmg = r.r(c.lmg_t);
    let usr = r.r(c.user_t);
    let pause = r.r(c.pause_t);
    let elem = r.r(c.elem_t);
    let click = r.r(c.on_click.1);
    let mode = r.r(c.g.mode.1);
    let host = r.r(c.g.host.1);
    let me = r.r(c.g.me.1);
    let lcls = r.r(c.lobby.1);
    let lob = r.r(c.lobby.3);
    let lst = r.r(c.lobby_state.1);
    let proxy = r.r(c.lobby_players.1);
    let parr = r.r(c.proxy_array.1);
    let battle = r.r(c.battle_t);
    let bu = r.r(c.bunit_t);
    let skill = r.r(c.skill_t);
    let unit = r.r(c.unit_t);
    let owner = r.r(c.unit_owner.1);
    let gst = r.r(c.state_t);
    let list = r.r(c.arr_t);
    let arr = r.r(c.arr_arr.1);
    let n = r.r(c.i32_t);
    let ix = r.r(c.i32_t);
    let jrep = r.r(c.json_rep_t);
    let gui = r.r(c.game_ui.1);
    let ginv = r.r(c.game_inv.1);
    let chest = r.r(c.chest_inv.1);
    let dwin = r.r(c.debrief_win_t);
    let dbr = r.r(c.debrief.1);
    let inv = r.r(c.loot.1);

    let mut a = Asm::new();
    // --- state machine, outside the trap
    a.gg(i, st_g);
    a.int(kk, k2);
    a.jmp(
        Opcode::JEq {
            a: i,
            b: kk,
            offset: 0,
        },
        "on",
    );
    a.int(kk, k1);
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
    a.gg(s, s_inst);
    a.call(seq, c.get_env, &[s]);
    a.if_null(seq, "ret");
    a.gg(s, s_la);
    a.call(dir, c.get_env, &[s]);
    a.if_null(dir, "ret");
    a.gg(s, s_sub);
    a.call(dir, c.add, &[dir, s]);
    a.op(Opcode::SetGlobal {
        global: dir_g,
        src: dir,
    });
    a.int(kk, k2);
    a.op(Opcode::SetGlobal {
        global: st_g,
        src: kk,
    });
    a.gg(s, s_on);
    a.call(s, c.add, &[s, seq]);
    a.gg(s2, s_sp);
    a.call(s, c.add, &[s, s2]);
    a.call(s, c.add, &[s, dir]);
    a.op(Opcode::Mov { dst: d, src: s });
    a.call(void, c.println, &[d]);
    // windowed, in memory only (prefs.sav is never edited by the harness)
    a.gg(gcls, c.game_g);
    a.fld(gprefs, gcls, c.game_prefs.0);
    a.if_null(gprefs, "on");
    a.int(kk, k0);
    a.setf(gprefs, c.display_mode, kk);
    a.call(void, c.apply_display, &[]);
    a.label("on");
    a.gg(i, tick_g);
    a.op(Opcode::Incr { dst: i });
    a.op(Opcode::SetGlobal {
        global: tick_g,
        src: i,
    });
    a.int(kk, k6);
    a.jmp(
        Opcode::JSLt {
            a: i,
            b: kk,
            offset: 0,
        },
        "ret",
    );
    a.int(kk, k0);
    a.op(Opcode::SetGlobal {
        global: tick_g,
        src: kk,
    });
    a.op(Opcode::Null { dst: seq });
    a.op(Opcode::Null { dst: ni });

    // --- the command, inside a trap
    a.jmp(Opcode::Trap { exc, offset: 0 }, "catch");
    a.gg(dir, dir_g);
    a.gg(s, s_cmd);
    a.call(path, c.add, &[dir, s]);
    a.call(b, c.exists, &[path]);
    a.if_false(b, "end");
    a.call(text, c.get_content, &[path]);
    a.call(void, c.delete_file, &[path]);
    a.gg(s, s_sp);
    a.call(i, c.index_of, &[text, s, ni]);
    a.int(kk, k1);
    a.jmp(
        Opcode::JSLt {
            a: i,
            b: kk,
            offset: 0,
        },
        "end",
    );
    a.int(kk, k0);
    a.op(Opcode::ToDyn { dst: ni, src: i });
    a.call(seq, c.substr, &[text, kk, ni]);
    a.op(Opcode::Null { dst: ni });
    a.op(Opcode::Incr { dst: i });
    a.call(rest, c.substr, &[text, i, ni]);
    a.gg(res, s_unknown);
    // `rest` starts with the verb, else on to `next`; `arg` = rest.substr(len).
    let verb = |a: &mut Asm, v: RefGlobal, next: &'static str, arg: Option<RefInt>| {
        a.gg(s, v);
        a.call(i, c.index_of, &[rest, s, ni]);
        a.int(kk, k0);
        a.jmp(
            Opcode::JNotEq {
                a: i,
                b: kk,
                offset: 0,
            },
            next,
        );
        if let Some(len) = arg {
            a.int(kk, len);
            a.call(s, c.substr, &[rest, kk, ni]);
        }
    };
    // verb: console <line>
    verb(&mut a, s_console, "not_console", Some(k8));
    a.gg(res, s_nogame);
    a.gg(gcls, c.game_g);
    a.fld(g, gcls, c.game_inst);
    a.if_null(g, "con_title");
    a.fld(con, g, c.console.0);
    a.go("con_run");
    a.label("con_title");
    a.gg(tcls, c.title.0);
    a.fld(ts, tcls, c.title.2);
    a.if_null(ts, "ack");
    a.fld(con, ts, c.console.0);
    a.label("con_run");
    a.if_null(con, "ack");
    a.gg(mcls, c.main_g);
    a.fld(prefs, mcls, c.prefs.0);
    a.op(Opcode::NullCheck { reg: prefs });
    a.fld(old, prefs, c.admin);
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
    a.setf(prefs, c.admin, b);
    a.call(void, c.reset_commands, &[con]);
    a.call(void, c.run_command, &[con, s]);
    a.setf(prefs, c.admin, old);
    a.op(Opcode::Bool {
        dst: b,
        value: ValBool(false),
    });
    a.op(Opcode::SetGlobal {
        global: pending_g,
        src: b,
    });
    a.call(void, c.reset_commands, &[con]);
    // The console reports a bad command on its own screen, not to us.
    a.gg(res, s_dispatched);
    a.go("ack");
    a.label("not_console");

    // verb: host  (title: lobby from the newest save; game: in-game reload)
    verb(&mut a, s_host, "not_host", None);
    a.gg(res, s_nosave);
    a.gg(lmode, c.load_solo_mode);
    a.gg(gcls, c.game_g);
    a.fld(g, gcls, c.game_inst);
    a.if_null(g, "host_list");
    a.gg(lmode, c.load_multi_mode);
    a.label("host_list");
    a.call(list, c.load_games, &[lmode]);
    a.if_null(list, "ack");
    a.fld(n, list, c.arr_len);
    a.int(kk, k1);
    a.jmp(
        Opcode::JSLt {
            a: n,
            b: kk,
            offset: 0,
        },
        "ack",
    );
    a.fld(arr, list, c.arr_arr.0);
    a.int(ix, k0);
    a.op(Opcode::GetArray {
        dst: d,
        array: arr,
        index: ix,
    });
    a.op(Opcode::ToVirtual { dst: cur, src: d });
    a.op(Opcode::New { dst: lw });
    a.call(void, c.load_ctor, &[lw, lmode]);
    a.setf(lw, c.load_current.0, cur);
    a.gg(res, s_ok);
    // The save is the harness host's own: its playerId (whom
    // LoadMultiGame.isHostIn looks for among the lobby members) is our id.
    a.fld(hdr, cur, c.save_header.0);
    a.if_null(hdr, "host_load");
    a.call(usr, c.get_user_fn, &[]);
    a.if_null(usr, "host_load");
    a.fld(s, usr, c.user_id);
    a.setf(hdr, c.save_player, s);
    a.if_set(g, "host_load");
    a.fld(multi, hdr, c.header_multi.0);
    a.if_set(multi, "host_load");
    a.call(void, c.convert_game, &[lw]);
    a.go("ack");
    a.label("host_load");
    a.call(void, c.load_game, &[lw]);
    a.go("ack");
    a.label("not_host");

    // verb: join <code>  (title screen)
    verb(&mut a, s_join, "not_join", Some(k5));
    a.call(void, c.title_join, &[s]);
    a.gg(res, s_ok);
    a.go("ack");
    a.label("not_join");

    // verb: start  (host, lobby window)
    verb(&mut a, s_start, "not_start", None);
    a.gg(res, s_nowindow);
    a.op(Opcode::Type {
        dst: ty,
        ty: c.lmg_t,
    });
    a.call(win, find, &[ty]);
    a.if_null(win, "ack");
    a.op(Opcode::SafeCast { dst: lmg, src: win });
    a.call(void, c.start_game, &[lmg]);
    a.gg(res, s_ok);
    a.go("ack");
    a.label("not_start");

    // verb: slot  (host: open one player slot in the lobby of a loaded save)
    verb(&mut a, s_slot, "not_slot", None);
    a.gg(res, s_nowindow);
    a.gg(lcls, c.lobby.0);
    a.fld(lob, lcls, c.lobby.2);
    a.if_null(lob, "ack");
    a.fld(lst, lob, c.lobby_state.0);
    a.if_null(lst, "ack");
    a.call(void, c.add_slot, &[lst]);
    a.gg(res, s_ok);
    a.go("ack");
    a.label("not_slot");

    // verb: backup <n>  (the open pause menu's backup button n)
    verb(&mut a, s_backup, "not_backup", Some(k7));
    a.gg(res, s_nowindow);
    a.op(Opcode::Type {
        dst: ty,
        ty: c.pause_t,
    });
    a.call(win, find, &[ty]);
    a.if_null(win, "ack");
    a.op(Opcode::SafeCast {
        dst: pause,
        src: win,
    });
    a.gg(res, s_unknown);
    a.call(ni, c.parse_int, &[s]);
    a.if_null(ni, "ack");
    a.op(Opcode::SafeCast { dst: ix, src: ni });
    a.op(Opcode::Null { dst: ni });
    a.fld(list, pause, c.pause_backups);
    a.if_null(list, "ack");
    a.fld(n, list, c.arr_len);
    a.jmp(
        Opcode::JSGte {
            a: ix,
            b: n,
            offset: 0,
        },
        "ack",
    );
    a.int(kk, k0);
    a.jmp(
        Opcode::JSLt {
            a: ix,
            b: kk,
            offset: 0,
        },
        "ack",
    );
    a.fld(arr, list, c.arr_arr.0);
    a.op(Opcode::GetArray {
        dst: d,
        array: arr,
        index: ix,
    });
    a.op(Opcode::SafeCast { dst: elem, src: d });
    a.if_null(elem, "ack");
    a.fld(click, elem, c.on_click.0);
    a.if_null(click, "ack");
    a.op(Opcode::CallClosure {
        dst: void,
        fun: click,
        args: vec![],
    });
    a.gg(res, s_ok);
    a.go("ack");
    a.label("not_backup");

    // verb: dump
    verb(&mut a, s_dump, "ack", None);
    a.gg(j, s_open);
    let mut w = J {
        a,
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
    // lobby: join code and players (title screen and game)
    w.lit(code, "\",\"lobby\":");
    w.a.gg(lcls, c.lobby.0);
    w.a.fld(lob, lcls, c.lobby.2);
    w.a.if_null(lob, "no_lobby");
    w.a.fld(lst, lob, c.lobby_state.0);
    w.a.if_null(lst, "no_lobby");
    w.lit(code, "{\"code\":");
    w.a.fld(s, lst, c.short_code);
    w.str_q(s);
    w.lit(code, ",\"players\":");
    w.a.fld(proxy, lst, c.lobby_players.0);
    w.a.op(Opcode::Null { dst: parr });
    w.a.if_null(proxy, "lob_p");
    w.a.fld(parr, proxy, c.proxy_array.0);
    w.a.label("lob_p");
    w.obj_q(parr);
    w.lit(code, "}");
    w.a.go("lobby_end");
    w.a.label("no_lobby");
    w.lit(code, "null");
    w.a.label("lobby_end");
    w.lit(code, ",\"game\":");
    w.a.gg(gcls, c.game_g);
    w.a.fld(g, gcls, c.game_inst);
    w.a.if_set(g, "has_game");
    w.lit(code, "false}");
    w.a.go("write");
    w.a.label("has_game");
    w.lit(code, "true,\"mode\":");
    w.a.fld(mode, g, c.g.mode.0);
    w.obj_q(mode);
    for (name, f) in &c.g.bools {
        w.kv(code, name, g, *f, b);
    }
    w.kv(code, "playersReady", g, c.g.players_ready, i);
    w.lit(code, ",\"host\":");
    w.a.fld(host, g, c.g.host.0);
    w.obj_q(host);
    w.lit(code, ",\"me\":");
    w.a.fld(me, g, c.g.me.0);
    w.player(code, me, s);
    // battle: whose turn, and that unit's skills as this machine sees them
    w.lit(code, ",\"battle\":");
    w.a.fld(battle, g, c.g.battle);
    w.a.if_set(battle, "has_battle");
    w.lit(code, "null");
    w.a.go("units");
    w.a.label("has_battle");
    w.lit(code, "{\"isPlayerTurn\":");
    w.a.fld(b, battle, c.battle_player_turn);
    w.val(b);
    w.lit(code, ",\"currentUnit\":");
    w.a.fld(bu, battle, c.battle_cur);
    w.a.if_null(bu, "no_cur");
    w.a.fld(unit, bu, c.bunit_data);
    w.a.if_null(unit, "no_cur");
    w.lit(code, "{\"name\":");
    w.a.fld(s, unit, c.unit_name);
    w.str_q(s);
    w.lit(code, ",\"owner\":");
    w.a.fld(owner, unit, c.unit_owner.0);
    w.player(code, owner, s);
    w.connected(code, owner, b);
    w.lit(code, ",\"apSkillPlayed\":");
    w.a.fld(proxy, bu, c.bunit_played);
    w.a.op(Opcode::Null { dst: parr });
    w.a.if_null(proxy, "cur_p");
    w.a.fld(parr, proxy, c.proxy_array.0);
    w.a.label("cur_p");
    w.obj_q(parr);
    w.lit(code, ",\"skills\":[");
    w.a.call(list, c.active_skills, &[bu]);
    w.a.if_null(list, "skills_end");
    w.a.fld(n, list, c.arr_len);
    w.a.fld(arr, list, c.arr_arr.0);
    w.a.int(ix, k0);
    w.a.loop_head("skills_loop");
    w.a.jmp(
        Opcode::JSGte {
            a: ix,
            b: n,
            offset: 0,
        },
        "skills_end",
    );
    w.a.int(kk, k0);
    w.a.jmp(
        Opcode::JEq {
            a: ix,
            b: kk,
            offset: 0,
        },
        "skill_first",
    );
    w.lit(code, ",");
    w.a.label("skill_first");
    w.a.op(Opcode::GetArray {
        dst: d,
        array: arr,
        index: ix,
    });
    w.a.op(Opcode::Incr { dst: ix });
    w.a.op(Opcode::ToVirtual { dst: skill, src: d });
    w.lit(code, "{\"id\":");
    w.a.fld(s, skill, c.skill_id);
    w.str_q(s);
    w.lit(code, ",\"ok\":");
    w.a.call(b, c.can_use, &[bu, skill]);
    w.val(b);
    w.lit(code, "}");
    w.a.go("skills_loop");
    w.a.label("skills_end");
    w.lit(code, "]}}");
    w.a.go("units");
    w.a.label("no_cur");
    w.lit(code, "null}");
    // the army: name, owner (connected), aptitude points
    w.a.label("units");
    w.lit(code, ",\"army\":[");
    w.a.fld(gst, g, c.g.state);
    w.a.if_null(gst, "army_end");
    w.a.fld(list, gst, c.army);
    w.a.if_null(list, "army_end");
    w.a.fld(n, list, c.arr_len);
    w.a.fld(arr, list, c.arr_arr.0);
    w.a.int(ix, k0);
    w.a.loop_head("army_loop");
    w.a.jmp(
        Opcode::JSGte {
            a: ix,
            b: n,
            offset: 0,
        },
        "army_end",
    );
    w.a.int(kk, k0);
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
    w.a.fld(s, unit, c.unit_name);
    w.str_q(s);
    w.lit(code, ",\"owner\":");
    w.a.fld(owner, unit, c.unit_owner.0);
    w.player(code, owner, s);
    w.connected(code, owner, b);
    w.kv(code, "aptitudePoints", unit, c.unit_ap, i);
    w.lit(code, "}");
    w.a.op(Opcode::Incr { dst: ix });
    w.a.go("army_loop");
    w.a.label("army_end");
    // the co-op chest panel of the inventory side panel: screen rect
    w.lit(code, "],\"chest\":");
    w.a.fld(gui, g, c.game_ui.0);
    w.a.if_null(gui, "no_chest");
    w.a.fld(ginv, gui, c.game_inv.0);
    w.a.if_null(ginv, "no_chest");
    w.a.fld(chest, ginv, c.chest_inv.0);
    w.a.if_null(chest, "no_chest");
    w.lit(code, "{\"visible\":");
    w.a.fld(b, chest, c.obj_visible);
    w.val(b);
    w.kv(code, "x", chest, c.obj_abs.0, fl);
    w.kv(code, "y", chest, c.obj_abs.1, fl);
    w.kv(code, "w", chest, c.flow_size.0, fl);
    w.kv(code, "h", chest, c.flow_size.1, fl);
    w.lit(code, "}");
    w.a.go("chest_end");
    w.a.label("no_chest");
    w.lit(code, "null");
    w.a.label("chest_end");
    // the post-battle loot pool (open Debrief window): Std.string of the stacks
    w.lit(code, ",\"loot\":");
    w.a.op(Opcode::Type {
        dst: ty,
        ty: c.debrief_win_t,
    });
    w.a.call(win, find, &[ty]);
    w.a.op(Opcode::Null { dst: list });
    w.a.if_null(win, "loot_q");
    w.a.op(Opcode::SafeCast {
        dst: dwin,
        src: win,
    });
    w.a.fld(dbr, dwin, c.debrief.0);
    w.a.if_null(dbr, "loot_q");
    w.a.fld(inv, dbr, c.loot.0);
    w.a.if_null(inv, "loot_q");
    w.a.call(list, c.all_items, &[inv]);
    w.a.label("loot_q");
    w.obj_q(list);
    w.lit(code, "}");
    let mut a = w.a;
    a.label("write");
    a.gg(s, s_state);
    a.call(path, c.add, &[dir, s]);
    a.call(void, c.save_content, &[path, j]);
    a.gg(res, s_ok);
    // ack: "<seq> <result>" to ack.txt and shim.log
    a.label("ack");
    a.gg(s, s_sp);
    a.call(s, c.add, &[seq, s]);
    a.call(s, c.add, &[s, res]);
    a.gg(s2, s_ack);
    a.call(path, c.add, &[dir, s2]);
    a.call(void, c.save_content, &[path, s]);
    a.gg(s2, s_ackp);
    a.call(s, c.add, &[s2, s]);
    a.op(Opcode::Mov { dst: d, src: s });
    a.call(void, c.println, &[d]);
    a.label("end");
    a.op(Opcode::EndTrap { exc });
    a.label("ret");
    a.op(Opcode::Ret { ret: void });
    // --- handler: log, then try an error ack (its own trap: never rethrow)
    a.label("catch");
    a.call(s, c.std_string, &[exc]);
    a.gg(s2, s_err);
    a.call(s, c.add, &[s2, s]);
    a.op(Opcode::Mov { dst: d, src: s });
    a.call(void, c.println, &[d]);
    a.jmp(
        Opcode::Trap {
            exc: exc2,
            offset: 0,
        },
        "catch2",
    );
    a.gg(b, pending_g);
    a.if_false(b, "no_restore");
    a.gg(mcls, c.main_g);
    a.fld(prefs, mcls, c.prefs.0);
    a.gg(old, admin_g);
    a.setf(prefs, c.admin, old);
    a.op(Opcode::Bool {
        dst: b,
        value: ValBool(false),
    });
    a.op(Opcode::SetGlobal {
        global: pending_g,
        src: b,
    });
    // ...and the command set built while admin was forced goes too.
    a.gg(con, console_g);
    a.if_null(con, "no_restore");
    a.call(void, c.reset_commands, &[con]);
    a.label("no_restore");
    a.gg(s, s_errack);
    a.call(s, c.add, &[seq, s]);
    a.gg(dir, dir_g);
    a.gg(s2, s_ack);
    a.call(path, c.add, &[dir, s2]);
    a.call(void, c.save_content, &[path, s]);
    a.op(Opcode::EndTrap { exc: exc2 });
    a.op(Opcode::Ret { ret: void });
    a.label("catch2");
    a.op(Opcode::Ret { ret: void });

    push_fn(code, vec![], c.void_t, r.0, a.finish(), c.dbg)
}

fn apply(code: &mut Bytecode, c: Ctx) -> Result<()> {
    let uid = build_uid(code, &c)?;
    let find = build_find(code, &c)?;
    let tick = build(code, &c, find)?;
    let (gu_fi, at, me) = c.get_user;
    insert_ops(
        &mut code.functions[gu_fi],
        at,
        vec![Opcode::Call1 {
            dst: me,
            fun: uid,
            arg0: me,
        }],
    );
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
        let p = plan(&orig).expect("plan");
        let (ml, (gu, at, _)) = (p.main_loop_fi, p.get_user);
        let mut code = read(&image);
        patch_harness(&mut code);
        let back = read(&write(&code));
        let n = back.functions.len();
        let [uid, find, tick] = [n - 3, n - 2, n - 1].map(|i| &back.functions[i]);
        for f in [uid, find, tick] {
            check_flow(f);
            check_types(&back, f, 0..f.ops.len());
            assert_eq!(traps_ok(f), if f.findex == tick.findex { 2 } else { 0 });
        }
        let (a, b) = (&orig.functions[ml], &back.functions[ml]);
        assert!(matches!(b.ops[0], Opcode::Call0 { fun, .. } if fun == tick.findex));
        crate::asm::testutil::shifted(a, b, 0, 1);
        let (a, b) = (&orig.functions[gu], &back.functions[gu]);
        assert!(matches!(b.ops[at], Opcode::Call1 { fun, .. } if fun == uid.findex));
        assert!(!matches!(b.ops[at - 1], Opcode::NullCheck { .. }));
        check_types(&back, b, at..at + 1);
        crate::asm::testutil::shifted(a, b, at, 1);
        for (i, (x, y)) in orig.functions.iter().zip(&back.functions).enumerate() {
            if i != ml && i != gu {
                assert!(crate::asm::testutil::same(x, y), "fn#{i} changed");
            }
        }
    }
}
